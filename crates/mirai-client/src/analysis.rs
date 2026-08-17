// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Turning "the position the user is looking at" into an [`AnalyzeReq`], and measuring how
//! fast the answer is coming back.
//!
//! Both frontends need exactly this and nothing frontend-shaped is involved, so it lives
//! here rather than twice.

use std::time::Instant;

use mirai_core::{Candidate, Color, GameTree, NodeAnalysis, NodeId, Point};
use mirai_engine::{AnalyzeReq, Report, Want};

/// How many principal-variation moves to ask for. Long enough to read a sequence out on the
/// board, short enough that the reports stay small at 10 Hz.
pub const PV_LEN: u8 = 15;

/// Builds the analysis request for one node of `tree`.
///
/// Takes `&mut GameTree` because resolving the position populates the tree's position cache;
/// a shared borrow would replay the whole line on every keystroke.
pub fn request_for_node(
    tree: &mut GameTree,
    id: NodeId,
    want: Want,
    max_visits: u32,
) -> AnalyzeReq {
    let mut req = AnalyzeReq::new(tree.info.size, tree.info.rules, tree.info.komi);

    // Setup stones become `initialStones` only when they precede every move, which is what
    // real SGFs do. Anything later cannot be expressed as initial stones plus a move list,
    // so that case falls back to the replayed board.
    let mut setup: Vec<(Color, Point)> = Vec::new();
    let mut moves: Vec<(Color, Point)> = Vec::new();
    let mut late_setup = false;
    for nid in tree.path_to(id) {
        let node = tree.node(nid);
        if !node.setup.is_empty() {
            if moves.is_empty() {
                for &p in &node.setup.add_black {
                    setup.push((Color::Black, p));
                }
                for &p in &node.setup.add_white {
                    setup.push((Color::White, p));
                }
                setup.retain(|(_, p)| !node.setup.add_empty.contains(p));
            } else {
                late_setup = true;
            }
        }
        if let Some((c, p)) = node.mv {
            moves.push((c, p));
        }
    }

    if late_setup {
        // Correct, just without move history.
        let pos = tree.position(id);
        req.initial_player = Some(pos.to_play);
        req.initial_stones = pos
            .board
            .stones()
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|c| (c, Point(i as u16))))
            .collect();
    } else {
        req.initial_stones = setup;
        if moves.is_empty() {
            req.initial_player = Some(tree.position(id).to_play);
        }
        req.moves = moves;
    }

    req.want = want;
    req.max_visits = Some(max_visits);
    req.pv_len = Some(PV_LEN);
    req
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
/// wall time between them. The first report only sets that baseline: treating it as a rate
/// from dispatch would charge every visit already sitting in the snapshot — a cache-hot
/// resume after Space, or the tail of the previous search — to a few milliseconds.
/// Reports arrive at ~10 Hz and a single interval is noisy, so later samples are
/// exponentially smoothed; the readout would otherwise be unreadable.
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

    /// The smoothed rate, or `None` until two reports have shown progress.
    pub fn rate(&self) -> Option<f32> {
        (self.rate > 0.0).then_some(self.rate)
    }

    /// Folds in a report of `visits` total visits observed at `now`.
    ///
    /// The first report that shows progress only records the visit count and time. A
    /// report with no new visits is ignored rather than counted as a zero-rate sample:
    /// the final report is echoed as `Done`, and a search that has hit its visit cap
    /// would otherwise decay its own last reading towards zero.
    pub fn sample(&mut self, visits: u32, now: Instant) {
        if visits <= self.visits {
            return;
        }
        let dt = now.saturating_duration_since(self.mark).as_secs_f32();
        if self.visits > 0 && dt >= 0.001 {
            let sample = (visits - self.visits) as f32 / dt;
            self.rate = if self.rate > 0.0 {
                self.rate + SPEED_SMOOTHING * (sample - self.rate)
            } else {
                sample
            };
        }
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

    /// Setup after a move cannot be expressed as initial stones plus a move list, so the
    /// request must carry the replayed board instead — correct, just without history.
    #[test]
    fn setup_after_a_move_falls_back_to_the_whole_board() {
        let mut t = tree(9);
        let root = t.root();
        let first = t.play(root, Color::Black, p(9, 2, 2)).expect("legal");
        let second = t.play(first, Color::White, p(9, 6, 6)).expect("legal");
        t.node_mut(second).setup.add_black.push(p(9, 4, 4));

        let req = request_for_node(&mut t, second, Want::empty(), 50);

        assert!(req.moves.is_empty(), "history must not be sent as moves");
        assert_eq!(req.initial_stones.len(), 3);
        assert_eq!(req.initial_player, Some(Color::Black));
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

        // First report only marks the baseline; a rate needs two snapshots.
        m.sample(600, t0 + Duration::from_millis(500));
        assert_eq!(m.rate(), None, "one report is not an interval");

        // 120 visits in 0.1 s after that.
        m.sample(720, t0 + Duration::from_millis(600));
        assert!((m.rate().unwrap() - 1200.0).abs() < 1.0, "{:?}", m.rate());

        // A steady 1200/s must stay at 1200/s however it is smoothed.
        for i in 1..=10 {
            m.sample(
                720 + i * 120,
                t0 + Duration::from_millis(600 + i as u64 * 100),
            );
        }
        assert!((m.rate().unwrap() - 1200.0).abs() < 1.0, "{:?}", m.rate());
    }

    #[test]
    fn a_repeated_final_report_does_not_zero_the_rate() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        m.sample(1000, t0 + Duration::from_millis(500));
        m.sample(1200, t0 + Duration::from_millis(600));
        let running = m.rate().unwrap();

        // The engine echoes its last report as `Done`: same visit count, later arrival.
        m.sample(1200, t0 + Duration::from_millis(900));
        assert_eq!(m.rate(), Some(running));
    }

    #[test]
    fn a_cache_hot_first_report_is_not_a_rate() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        // Resume after Space: first snapshot lands in 10 ms with thousands of visits
        // already in the tree. That is a baseline, not 1.2M visits/s.
        m.sample(12_000, t0 + Duration::from_millis(10));
        assert_eq!(m.rate(), None);

        m.sample(12_200, t0 + Duration::from_millis(110));
        assert!((m.rate().unwrap() - 2000.0).abs() < 1.0, "{:?}", m.rate());
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
