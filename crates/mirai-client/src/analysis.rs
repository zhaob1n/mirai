// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Turning "the position the user is looking at" into an [`AnalyzeReq`], and measuring how
//! fast the answer is coming back.
//!
//! Both frontends need exactly this and nothing frontend-shaped is involved, so it lives
//! here rather than twice.

use std::time::Instant;

use mirai_core::{
    Board, Candidate, Color, DeadSet, GameTree, NodeAnalysis, NodeId, Point, Scoring,
};
use mirai_engine::{AnalyzeReq, Report, Want, dq_own};

/// How many principal-variation moves to ask for. Long enough to read a sequence out on the
/// board, short enough that the reports stay small at 10 Hz.
pub const PV_LEN: u8 = 15;

/// Ownership magnitude above which a stone counts as dead, matching KataGo's own default.
const DEAD_OWNERSHIP_THRESHOLD: f32 = 0.4;

/// The dead set KataGo's quantised ownership map implies for `board`.
///
/// A map of the wrong length is a report for another position; it marks nothing dead
/// rather than indexing off the end of the board.
pub fn dead_from_ownership(board: &Board, ownership: &[i8]) -> DeadSet {
    if ownership.len() != board.size.points() {
        return DeadSet::empty(board.size);
    }
    let owner: Vec<f32> = ownership.iter().copied().map(dq_own).collect();
    DeadSet::from_ownership(board, &owner, DEAD_OWNERSHIP_THRESHOLD)
}

/// Builds the analysis request for one node of `tree`.
///
/// Takes `&mut GameTree` because resolving the position populates the tree's position cache;
/// a shared borrow would replay the whole line on every keystroke.
///
/// The last setup or `PL` node on the path is a reconstruction boundary: the request
/// snapshots that board as unique row-major `initial_stones` and only sends real moves
/// after it. Territory scoring folds the boundary's prisoner counts into `komi_x2` so
/// KataGo, which would otherwise start from zero captures, matches the record. Area
/// scoring and ordinary move-only games keep an unadjusted move list.
pub fn request_for_node(
    tree: &mut GameTree,
    id: NodeId,
    want: Want,
    max_visits: u32,
) -> AnalyzeReq {
    let mut req = AnalyzeReq::new(tree.info.size, tree.info.rules, tree.info.komi);

    let path = tree.path_to(id);
    let boundary = path
        .iter()
        .rev()
        .copied()
        .find(|&nid| is_reconstruction_boundary(tree, nid));

    if let Some(boundary) = boundary {
        let snap = snapshot_boundary(tree, boundary);
        req.initial_player = Some(snap.initial_player);
        req.initial_stones = snap.initial_stones;
        req.komi_x2 = snap.komi_x2;
        for &nid in path.iter().skip_while(|&&nid| nid != boundary).skip(1) {
            if let Some(mv) = tree.node(nid).mv {
                req.moves.push(mv);
            }
        }
    } else {
        for nid in path {
            if let Some(mv) = tree.node(nid).mv {
                req.moves.push(mv);
            }
        }
        if req.moves.is_empty() {
            req.initial_player = Some(tree.position(id).to_play);
        }
    }

    req.want = want;
    req.max_visits = Some(max_visits);
    req.pv_len = Some(PV_LEN);
    req
}

/// A setup or `PL` node: the reconstruction boundary [`request_for_node`] snapshots.
pub(crate) fn is_reconstruction_boundary(tree: &GameTree, id: NodeId) -> bool {
    let node = tree.node(id);
    !node.setup.is_empty() || node.to_play_override.is_some()
}

/// Stones, side to move, and territory-adjusted komi at a boundary node.
///
/// This is the only place those three are derived. A whole-game plan snapshots each boundary
/// once and reuses the result; [`request_for_node`] does the same for a single node.
pub(crate) struct BoundarySnapshot {
    pub initial_player: Color,
    pub initial_stones: Vec<(Color, Point)>,
    pub komi_x2: i16,
}

pub(crate) fn snapshot_boundary(tree: &mut GameTree, boundary: NodeId) -> BoundarySnapshot {
    let mut komi_x2 = AnalyzeReq::new(tree.info.size, tree.info.rules, tree.info.komi).komi_x2;
    let (to_play, stones, captures) = {
        let pos = tree.position(boundary);
        let stones = pos
            .board
            .stones()
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|c| (c, Point(i as u16))))
            .collect::<Vec<_>>();
        (pos.to_play, stones, pos.board.captures)
    };
    if tree.info.rules.rules().scoring == Scoring::Territory && captures != [0, 0] {
        let black_captures = captures[Color::Black.index()];
        let white_captures = captures[Color::White.index()];
        let adjusted =
            i32::from(komi_x2) + 2 * (i32::from(white_captures) - i32::from(black_captures));
        komi_x2 = adjusted.clamp(i16::MIN.into(), i16::MAX.into()) as i16;
    }
    BoundarySnapshot {
        initial_player: to_play,
        initial_stones: stones,
        komi_x2,
    }
}

/// Converts a wire report into the Black-perspective [`NodeAnalysis`] the tree stores.
pub fn analysis_of(report: &Report, max_candidates: usize) -> NodeAnalysis {
    NodeAnalysis {
        visits: report.root.visits,
        winrate: report.root.winrate_f32(),
        score_lead: report.root.score_lead_f32(),
        score_stdev: report.root.score_stdev_f32(),
        candidates: report
            .moves
            .iter()
            .take(max_candidates.max(1))
            .map(|m| Candidate {
                mv: m.mv,
                visits: m.visits,
                winrate: m.winrate_f32(),
                score_lead: m.score_lead_f32(),
                prior: m.prior_f32(),
                pv: m.pv.clone(),
                utility: Some(m.utility_f32()),
            })
            .collect(),
        ownership: report
            .ownership
            .as_ref()
            .map(|o| o.clone().into_boxed_slice()),
    }
}

/// Measures how fast a search is running, in visits per second.
///
/// KataGo reports no timing of its own — a report carries a visit count and nothing else —
/// so the rate is measured here from the visit delta between consecutive reports over the
/// wall time between them. Reports arrive at ~10 Hz and a single interval is noisy, so the
/// samples are exponentially smoothed; the readout would otherwise be unreadable.
#[derive(Clone, Copy)]
pub struct SpeedMeter {
    mark: Instant,
    visits: u32,
    rate: f32,
}

/// Weight of the newest sample. At the default 100 ms report interval this settles within
/// about a second and still tracks a real slowdown.
const SPEED_SMOOTHING: f32 = 0.35;

impl SpeedMeter {
    /// A meter for a search dispatched at `now`, which has reported nothing yet.
    pub fn started(now: Instant) -> SpeedMeter {
        SpeedMeter {
            mark: now,
            visits: 0,
            rate: 0.0,
        }
    }

    /// The smoothed rate, or `None` before any report showed progress.
    pub fn rate(&self) -> Option<f32> {
        (self.rate > 0.0).then_some(self.rate)
    }

    /// Folds in a report of `visits` total visits observed at `now`.
    ///
    /// A report with no new visits is ignored rather than counted as a zero-rate sample:
    /// the final report is echoed as `Done`, and a search that has hit its visit cap would
    /// otherwise decay its own last reading towards zero.
    pub fn sample(&mut self, visits: u32, now: Instant) {
        let dt = now.saturating_duration_since(self.mark).as_secs_f32();
        if visits <= self.visits || dt < 0.001 {
            return;
        }
        let sample = (visits - self.visits) as f32 / dt;
        self.rate = if self.rate > 0.0 {
            self.rate + SPEED_SMOOTHING * (sample - self.rate)
        } else {
            sample
        };
        self.mark = now;
        self.visits = visits;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mirai_core::{GameInfo, RuleSet, Size};

    use super::*;

    fn tree(size: u8) -> GameTree {
        GameTree::new(GameInfo {
            size: Size { w: size, h: size },
            rules: RuleSet::Japanese,
            komi: 6.5,
            ..GameInfo::default()
        })
    }

    fn p(size: u8, x: u16, y: u16) -> Point {
        Point(y * size as u16 + x)
    }

    #[test]
    fn a_plain_move_list_needs_no_initial_stones() {
        let mut t = tree(19);
        let root = t.root();
        let first = t.play(root, Color::Black, p(19, 3, 3)).expect("legal");
        let second = t.play(first, Color::White, p(19, 15, 15)).expect("legal");

        let req = request_for_node(&mut t, second, Want::empty(), 100);

        assert!(req.initial_stones.is_empty());
        assert_eq!(req.moves.len(), 2);
        assert_eq!(req.moves[0], (Color::Black, p(19, 3, 3)));
        assert_eq!(req.initial_player, None, "a move list implies the player");
        assert_eq!(req.max_visits, Some(100));
        assert_eq!(req.pv_len, Some(PV_LEN));
    }

    #[test]
    fn root_setup_becomes_initial_stones_and_names_the_player() {
        let mut t = tree(19);
        let root = t.root();
        t.node_mut(root).setup.add_black.push(p(19, 3, 3));
        t.node_mut(root).setup.add_white.push(p(19, 15, 15));

        let req = request_for_node(&mut t, root, Want::empty(), 50);

        assert_eq!(req.initial_stones.len(), 2);
        assert!(req.moves.is_empty());
        // With no moves the request must say whose turn it is, because the wire format
        // cannot infer it from an empty move list.
        assert_eq!(req.initial_player, Some(Color::Black));
        assert_eq!(req.komi(), 6.5, "a capture-free setup must not touch komi");
    }

    #[test]
    fn a_cleared_setup_point_does_not_survive_as_an_initial_stone() {
        let mut t = tree(19);
        let root = t.root();
        t.node_mut(root).setup.add_black.push(p(19, 3, 3));
        t.node_mut(root).setup.add_black.push(p(19, 4, 4));
        t.node_mut(root).setup.add_empty.push(p(19, 4, 4));

        let req = request_for_node(&mut t, root, Want::empty(), 50);

        assert_eq!(req.initial_stones, vec![(Color::Black, p(19, 3, 3))]);
    }

    /// A mid-game setup is a reconstruction boundary: earlier stones become the
    /// snapshot, and real moves after it stay in the move list so ko history survives.
    #[test]
    fn setup_after_a_move_keeps_later_moves() {
        let mut t = tree(9);
        let root = t.root();
        let first = t.play(root, Color::Black, p(9, 2, 2)).expect("legal");
        let second = t.play(first, Color::White, p(9, 6, 6)).expect("legal");
        let setup = t.add_child(second);
        t.node_mut(setup).setup.add_black.push(p(9, 4, 4));
        t.node_mut(setup).setup.add_white.push(p(9, 0, 0));
        let later = t.play(setup, Color::Black, p(9, 3, 3)).expect("legal");

        let at_setup = request_for_node(&mut t, setup, Want::empty(), 50);
        assert!(
            at_setup.moves.is_empty(),
            "pre-boundary history is the snapshot"
        );
        assert_eq!(at_setup.initial_player, Some(Color::Black));
        assert_eq!(
            at_setup.initial_stones,
            vec![
                (Color::White, p(9, 0, 0)),
                (Color::Black, p(9, 2, 2)),
                (Color::Black, p(9, 4, 4)),
                (Color::White, p(9, 6, 6)),
            ]
        );

        let req = request_for_node(&mut t, later, Want::empty(), 50);
        assert_eq!(req.initial_stones, at_setup.initial_stones);
        assert_eq!(req.initial_player, Some(Color::Black));
        assert_eq!(req.moves, vec![(Color::Black, p(9, 3, 3))]);
        assert_eq!(req.to_play(), Color::White);
        assert_eq!(req.max_visits, Some(50));
        assert_eq!(req.pv_len, Some(PV_LEN));
    }

    /// B[D4]; PL[B]; B[C3] — same-colour consecutive moves, no fabricated pass.
    #[test]
    fn pl_after_a_move_then_a_real_move_keeps_post_boundary_history() {
        let mut t = tree(9);
        let root = t.root();
        let d4 = p(9, 3, 3);
        let c3 = p(9, 2, 2);
        let first = t.play(root, Color::Black, d4).expect("legal");
        let pl = t.add_child(first);
        t.node_mut(pl).to_play_override = Some(Color::Black);
        let third = t.play(pl, Color::Black, c3).expect("legal");

        let at_pl = request_for_node(&mut t, pl, Want::empty(), 50);
        assert_eq!(at_pl.initial_player, Some(Color::Black));
        assert!(at_pl.moves.is_empty());
        assert_eq!(at_pl.initial_stones, vec![(Color::Black, d4)]);
        assert_eq!(at_pl.to_play(), Color::Black);

        let req = request_for_node(&mut t, third, Want::empty(), 50);
        assert_eq!(req.initial_player, Some(Color::Black));
        assert_eq!(req.initial_stones, vec![(Color::Black, d4)]);
        assert_eq!(req.moves, vec![(Color::Black, c3)]);
        assert!(
            req.moves.iter().all(|&(_, pt)| !pt.is_pass()),
            "a PL switch must not invent a pass"
        );
        assert_eq!(req.to_play(), Color::White);

        let pos = t.position(third);
        assert_eq!(pos.to_play, Color::White);
        assert_eq!(pos.board.at(d4), Some(Color::Black));
        assert_eq!(pos.board.at(c3), Some(Color::Black));
    }

    /// `(;SZ[9]AB[aa];AW[aa][bb];AE[bb];B[cc])` — last setup wins, no duplicate stones.
    #[test]
    fn cumulative_setup_overwrites_and_erases_before_the_move_list() {
        let mut t = tree(9);
        let root = t.root();
        let aa = p(9, 0, 0);
        let bb = p(9, 1, 1);
        let cc = p(9, 2, 2);
        t.node_mut(root).setup.add_black.push(aa);
        let aw = t.add_child(root);
        t.node_mut(aw).setup.add_white.extend([aa, bb]);
        let ae = t.add_child(aw);
        t.node_mut(ae).setup.add_empty.push(bb);
        let mv = t.play(ae, Color::Black, cc).expect("legal");

        let req = request_for_node(&mut t, mv, Want::empty(), 50);
        assert_eq!(req.initial_stones, vec![(Color::White, aa)]);
        assert_eq!(req.moves, vec![(Color::Black, cc)]);
        assert_eq!(req.initial_player, Some(Color::Black));
    }

    #[test]
    fn territory_komi_uses_boundary_captures_not_the_target() {
        let mut t = tree(9);
        let root = t.root();
        let b1 = t.play(root, Color::Black, p(9, 1, 0)).expect("legal");
        let w1 = t.play(b1, Color::White, p(9, 0, 0)).expect("legal");
        let cap = t.play(w1, Color::Black, p(9, 0, 1)).expect("legal");
        let pl = t.add_child(cap);
        t.node_mut(pl).to_play_override = Some(Color::White);
        let w2 = t.play(pl, Color::White, p(9, 8, 7)).expect("legal");
        let b2 = t.play(w2, Color::Black, p(9, 8, 8)).expect("legal");
        let later = t.play(b2, Color::White, p(9, 7, 8)).expect("legal");

        assert_eq!(t.position(pl).board.captures, [1, 0]);
        assert_eq!(t.position(later).board.captures, [1, 1]);

        let before = request_for_node(&mut t, cap, Want::empty(), 50);
        assert!(before.initial_stones.is_empty());
        assert_eq!(before.moves.len(), 3);
        assert_eq!(before.komi_x2, 13, "a move list already has the captures");

        let at_pl = request_for_node(&mut t, pl, Want::empty(), 50);
        let at_later = request_for_node(&mut t, later, Want::empty(), 50);
        // KM 6.5 → 13; boundary prisoners are Black 1, White 0 → komi_x2 11.
        assert_eq!(at_pl.komi_x2, 11);
        assert_eq!(
            at_later.komi_x2, 11,
            "post-boundary captures must not enter komi"
        );
        assert_eq!(t.info.komi, 6.5, "the record's KM must not be rewritten");

        let mut area = t.clone();
        area.info.rules = RuleSet::Chinese;
        let area_req = request_for_node(&mut area, later, Want::empty(), 50);
        assert_eq!(area_req.komi_x2, 13);
        assert_eq!(area_req.moves, at_later.moves);
    }

    #[test]
    fn the_request_carries_the_records_own_rules() {
        let mut t = GameTree::new(GameInfo {
            size: Size { w: 13, h: 13 },
            rules: RuleSet::Chinese,
            komi: 7.5,
            ..GameInfo::default()
        });
        let root = t.root();

        let req = request_for_node(&mut t, root, Want::OWNERSHIP, 1);

        assert_eq!(req.size, Size { w: 13, h: 13 });
        assert_eq!(req.rules, RuleSet::Chinese);
        assert_eq!(req.komi(), 7.5);
        assert_eq!(req.want, Want::OWNERSHIP);
    }

    #[test]
    fn speed_meter_measures_visits_per_second() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        assert_eq!(m.rate(), None, "nothing has been reported yet");

        // 600 visits in 0.5 s, counting from dispatch.
        m.sample(600, t0 + Duration::from_millis(500));
        assert!((m.rate().unwrap() - 1200.0).abs() < 1.0, "{:?}", m.rate());

        // A steady 1200/s must stay at 1200/s however it is smoothed.
        for i in 1..=10 {
            m.sample(
                600 + i * 120,
                t0 + Duration::from_millis(500 + i as u64 * 100),
            );
        }
        assert!((m.rate().unwrap() - 1200.0).abs() < 1.0, "{:?}", m.rate());
    }

    #[test]
    fn a_repeated_final_report_does_not_zero_the_rate() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        m.sample(1000, t0 + Duration::from_millis(500));
        let running = m.rate().unwrap();

        // The engine echoes its last report as `Done`: same visit count, later arrival.
        m.sample(1000, t0 + Duration::from_millis(900));
        assert_eq!(m.rate(), Some(running));
    }

    #[test]
    fn the_meter_tracks_a_slowdown() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        let mut visits = 0;
        let mut at = t0;
        for _ in 0..10 {
            visits += 200;
            at += Duration::from_millis(100);
            m.sample(visits, at);
        }
        assert!((m.rate().unwrap() - 2000.0).abs() < 1.0, "{:?}", m.rate());

        for _ in 0..20 {
            visits += 20;
            at += Duration::from_millis(100);
            m.sample(visits, at);
        }
        assert!((m.rate().unwrap() - 200.0).abs() < 20.0, "{:?}", m.rate());
    }
}
