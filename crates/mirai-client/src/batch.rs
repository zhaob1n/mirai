// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Analysing a whole game: plan every position, then run them with bounded concurrency.
//!
//! The split is deliberate. Planning needs `&mut GameTree` (the position cache), so it
//! cannot happen concurrently; running one position needs nothing but an [`Engine`]. Doing
//! the tree work up front — one pass, a shared move list, a snapshot of each reconstruction
//! boundary — turns a whole-game sweep into a plain bounded-concurrency problem and keeps
//! the borrow out of the async part entirely. The [`AnalyzeReq`] is built only when the
//! sweep dispatches the position, so live memory stays proportional to the concurrency cap
//! rather than to the square of the game length.
//!
//! Concurrency is capped because each in-flight request is one server-opened unidirectional
//! stream and one KataGo search slot. Beyond the server's analysis threads, more requests buy
//! nothing and only lengthen the queue the user is waiting on.

use std::sync::Arc;

use mirai_core::{Color, GameTree, NodeId, Point, RuleSet, Size};
use mirai_engine::{AnalyzeReq, Engine, EngineError, Report, Want};

use crate::analysis::{self, BoundarySnapshot, PV_LEN};

/// Fields every position of one plan shares. One allocation, not one per node.
struct PlanBase {
    size: Size,
    rules: RuleSet,
    komi_x2: i16,
    moves: Arc<[(Color, Point)]>,
    want: Want,
    visits: u32,
}

/// One position of the main line, and what the sweep needs to build its request.
///
/// The request is not stored here. [`Planned::request`] copies this position's range out of
/// the shared move list, and [`sweep`] calls it only when it dispatches the position.
pub struct Planned {
    pub node: NodeId,
    /// Move number, for progress and for placing the result on a graph.
    pub turn: u16,
    shared: Arc<PlanBase>,
    /// `shared.moves[start..end]` is this position's move list.
    start: usize,
    end: usize,
    /// Last setup/`PL` node at or before this position, snapshotted once.
    boundary: Option<Arc<BoundarySnapshot>>,
    /// Set only when there is no boundary and the move range is empty:
    /// [`request_for_node`](crate::analysis::request_for_node) then names the side to move,
    /// which a move list would otherwise imply.
    bare_player: Option<Color>,
    /// Applied at materialisation. A sweep is background work, so the desktop leaves
    /// reporting off and priority at zero, and caps stored candidates.
    pub report_every_ms: Option<u16>,
    pub priority: i8,
    pub max_candidates: Option<u8>,
}

impl Planned {
    /// The request [`sweep`] sends. Copies this position's move range and boundary stones;
    /// the plan itself keeps one shared copy of each.
    pub fn request(&self) -> AnalyzeReq {
        let base = &self.shared;
        let mut req = AnalyzeReq {
            komi_x2: base.komi_x2,
            ..AnalyzeReq::new(base.size, base.rules, 0.0)
        };
        if let Some(boundary) = &self.boundary {
            req.initial_player = Some(boundary.initial_player);
            req.initial_stones = boundary.initial_stones.clone();
            req.komi_x2 = boundary.komi_x2;
        } else if self.start == self.end {
            req.initial_player = self.bare_player;
        }
        req.moves = self.shared.moves[self.start..self.end].to_vec();
        req.want = base.want;
        req.max_visits = Some(base.visits);
        req.pv_len = Some(PV_LEN);
        req.report_every_ms = self.report_every_ms;
        req.priority = self.priority;
        req.max_candidates = self.max_candidates;
        req
    }
}

/// What one planned position produced.
pub struct Analysed {
    pub node: NodeId,
    pub turn: u16,
    pub result: Result<Arc<Report>, EngineError>,
}

/// Plans one position per main-line node, root included.
///
/// The root is included because the win rate before the first move is the baseline every
/// later delta is measured against.
///
/// One downward pass records, per node, the reconstruction boundary
/// [`request_for_node`](crate::analysis::request_for_node) would use (last setup or `PL`
/// node so far, with its stones, side to move and territory komi adjustment) and a range
/// into a single move list. Building the [`AnalyzeReq`] is deferred to [`Planned::request`].
pub fn plan_mainline(tree: &mut GameTree, want: Want, visits: u32) -> Vec<Planned> {
    let line = tree.main_line();
    let template = AnalyzeReq::new(tree.info.size, tree.info.rules, tree.info.komi);
    let mut moves: Vec<(Color, Point)> = Vec::new();
    let mut raw = Vec::with_capacity(line.len());
    let mut boundary: Option<Arc<BoundarySnapshot>> = None;
    let mut boundary_end = 0usize;
    // Move-number base for the next node, matching [`GameTree::move_number`]: an `MN`
    // replaces the count at its own node and that node's own move is not added.
    let mut turn_base = 0u16;

    for id in line {
        // Stepping the cache along the line makes each boundary snapshot one `step`,
        // not a replay from the root. The old per-node `path_to` was the quadratic walk.
        let _ = tree.position(id);
        let is_boundary = analysis::is_reconstruction_boundary(tree, id);
        let (mv, move_number_override) = {
            let node = tree.node(id);
            (node.mv, node.move_number_override)
        };
        let turn = if let Some(m) = move_number_override {
            turn_base = m;
            m
        } else if mv.is_some() {
            turn_base = turn_base.saturating_add(1);
            turn_base
        } else {
            turn_base
        };

        if is_boundary {
            let snap = Arc::new(analysis::snapshot_boundary(tree, id));
            if let Some(mv) = mv {
                moves.push(mv);
            }
            boundary_end = moves.len();
            boundary = Some(snap);
            raw.push(RawPlanned {
                node: id,
                turn,
                start: boundary_end,
                end: boundary_end,
                boundary: boundary.clone(),
                bare_player: None,
            });
        } else {
            if let Some(mv) = mv {
                moves.push(mv);
            }
            let end = moves.len();
            let start = if boundary.is_some() { boundary_end } else { 0 };
            let bare_player = if boundary.is_none() && start == end {
                Some(tree.position(id).to_play)
            } else {
                None
            };
            raw.push(RawPlanned {
                node: id,
                turn,
                start,
                end,
                boundary: boundary.clone(),
                bare_player,
            });
        }
    }

    let shared = Arc::new(PlanBase {
        size: template.size,
        rules: template.rules,
        komi_x2: template.komi_x2,
        moves: moves.into(),
        want,
        visits,
    });
    raw.into_iter()
        .map(|raw| Planned {
            node: raw.node,
            turn: raw.turn,
            shared: Arc::clone(&shared),
            start: raw.start,
            end: raw.end,
            boundary: raw.boundary,
            bare_player: raw.bare_player,
            report_every_ms: None,
            priority: 0,
            max_candidates: None,
        })
        .collect()
}

struct RawPlanned {
    node: NodeId,
    turn: u16,
    start: usize,
    end: usize,
    boundary: Option<Arc<BoundarySnapshot>>,
    bare_player: Option<Color>,
}

/// What a sweep sink wants next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    Continue,
    Stop,
}

/// Runs `plan` through `engine`, at most `concurrency` at a time, handing each result to
/// `sink` as it lands along with the number finished so far and the total.
///
/// A failed position is reported, not fatal: one dead search must not discard the rest of
/// the game, so it is the sink that decides. Returning [`Flow::Stop`] abandons the rest —
/// the outstanding tasks are dropped, which drops their subscriptions and so cancels the
/// queries (INV-3).
///
/// The results come back in plan order regardless of completion order, so a caller can
/// index them by move number. A stopped sweep returns only what already finished, which
/// may be shorter than `plan`.
///
/// Each request is built here, at dispatch, from the plan's shared move list. Only the
/// in-flight positions own a copy.
pub async fn sweep<E, P>(
    engine: &E,
    plan: Vec<Planned>,
    concurrency: usize,
    mut sink: P,
) -> Vec<Analysed>
where
    E: Engine + ?Sized,
    P: FnMut(&Analysed, usize, usize) -> Flow,
{
    let total = plan.len();
    // Zero would starve the sweep; one is the honest meaning of "no concurrency".
    let concurrency = concurrency.max(1);
    let mut queue = plan.into_iter().enumerate();
    let mut running = tokio::task::JoinSet::new();
    let mut done: Vec<Option<Analysed>> = (0..total).map(|_| None).collect();
    let mut finished = 0;

    loop {
        // Refill before every await, never in a loop of its own. Dispatching the whole plan
        // first and only then joining is what a semaphore invites, and it defers the first
        // sink call until all but `concurrency` positions are done: the caller's progress
        // counter sits at zero and then jumps to the total.
        while running.len() < concurrency {
            let Some((index, planned)) = queue.next() else {
                break;
            };
            let node = planned.node;
            let turn = planned.turn;
            // `subscribe` is synchronous by contract, so the whole query lives in one task
            // and its slot is held for exactly as long as the query. The request is built
            // here, not in `plan_mainline`, so the queued tail does not own N histories.
            let sub = engine.subscribe(planned.request());
            running.spawn(async move {
                let result = sub.finish().await;
                (index, Analysed { node, turn, result })
            });
        }
        let Some(joined) = running.join_next().await else {
            break;
        };
        // A panicking analysis task would be a bug in this crate, not a server fault.
        let (index, analysed) = joined.expect("analysis task panicked");
        let slot = done[index].insert(analysed);
        finished += 1;
        if sink(slot, finished, total) == Flow::Stop {
            break;
        }
    }

    done.into_iter().flatten().collect()
}

/// Smallest drop that is still noise. The blunder list and the win-rate strip share this
/// floor: a move is listed or coloured only when the mover's loss is strictly above it.
pub const BLUNDER_MIN_DROP: f32 = 0.02;

/// `true` when `drop` is large enough to list or colour. NaN is not a blunder.
#[inline]
pub fn is_blunder_drop(drop: f32) -> bool {
    drop > BLUNDER_MIN_DROP
}

/// How many positions to keep in flight for an engine with `analysis_threads` threads.
pub fn in_flight(analysis_threads: u16) -> usize {
    ((analysis_threads as usize).max(1) * 2).min(16)
}

/// A move that cost its own player win rate, as measured by a stored sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blunder {
    pub node: NodeId,
    pub move_number: u16,
    pub player: Color,
    /// Win-rate loss for `player`, in `0.0..=1.0`.
    pub drop: f32,
    pub played: Point,
    /// The engine's first choice at the parent, when it differs from what was played.
    pub best: Option<Point>,
}

/// Extracts the blunders of the main line from analyses already stored in the tree.
///
/// Each move is judged from the perspective of the player who made it. Nodes whose own or
/// whose parent's analysis is missing are skipped — an unanalysed gap is not a mistake.
pub fn blunders(tree: &GameTree) -> Vec<Blunder> {
    let mut out = Vec::new();
    for id in tree.main_line() {
        let node = tree.node(id);
        let Some((player, played)) = node.mv else {
            continue;
        };
        let Some(parent) = node.parent else {
            continue;
        };
        let (Some(before), Some(after)) =
            (tree.node(parent).analysis.as_ref(), node.analysis.as_ref())
        else {
            continue;
        };
        let drop = player.winrate_for(before.winrate) - player.winrate_for(after.winrate);
        if !is_blunder_drop(drop) {
            continue;
        }
        out.push(Blunder {
            node: id,
            move_number: tree.move_number(id),
            player,
            drop,
            played,
            best: before
                .candidates
                .first()
                .map(|c| c.mv)
                .filter(|&best| best != played),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mirai_core::{Candidate, Color, GameInfo, NodeAnalysis, Point, RuleSet, Size};
    use mirai_engine::{CancelGuard, EngineDesc, SubEvent, Subscription};
    use tokio::sync::watch;

    use super::*;

    /// Answers every request immediately, and records how many were in flight at once.
    struct Tally {
        live: AtomicUsize,
        peak: AtomicUsize,
        served: AtomicUsize,
        /// Requests whose `max_visits` equals this fail, to exercise the per-position error.
        fail_at_visits: u32,
    }

    /// `Engine` is a foreign trait, so the shared handle has to be a local type rather than
    /// an `Arc<Tally>` directly.
    #[derive(Clone)]
    struct Counting(Arc<Tally>);

    impl Counting {
        fn new(fail_at_visits: u32) -> Counting {
            Counting(Arc::new(Tally {
                live: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                served: AtomicUsize::new(0),
                fail_at_visits,
            }))
        }
    }

    impl std::ops::Deref for Counting {
        type Target = Tally;
        fn deref(&self) -> &Tally {
            &self.0
        }
    }

    impl Engine for Counting {
        fn subscribe(&self, req: AnalyzeReq) -> Subscription {
            let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(live, Ordering::SeqCst);
            self.served.fetch_add(1, Ordering::SeqCst);

            if req.max_visits == Some(self.fail_at_visits) {
                self.live.fetch_sub(1, Ordering::SeqCst);
                return Subscription::failed(EngineError::Query("no".into()));
            }

            let mut report = Report::empty(req.moves.len() as u16, Color::Black);
            report.root.visits = req.max_visits.unwrap_or(0);
            let (tx, rx) = watch::channel(SubEvent::Done(Arc::new(report)));
            let this = self.0.clone();
            Subscription::new(
                rx,
                CancelGuard::new(move || {
                    this.live.fetch_sub(1, Ordering::SeqCst);
                    drop(tx);
                }),
            )
        }

        fn describe(&self) -> EngineDesc {
            EngineDesc::placeholder("counting")
        }
    }

    fn game(moves: usize) -> GameTree {
        let mut tree = GameTree::new(GameInfo {
            size: Size::square(19),
            rules: RuleSet::Chinese,
            komi: 7.5,
            ..GameInfo::default()
        });
        let mut at = tree.root();
        for i in 0..moves {
            let colour = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };
            let p = Size::square(19).point((i % 19) as u8, (i / 19) as u8);
            at = tree.play(at, colour, p).expect("legal");
        }
        tree
    }

    #[test]
    fn a_plan_covers_the_root_and_every_main_line_move() {
        let mut tree = game(5);
        let plan = plan_mainline(&mut tree, Want::empty(), 100);

        assert_eq!(plan.len(), 6, "root plus five moves");
        assert_eq!(plan[0].turn, 0, "the baseline is the empty board");
        assert_eq!(plan[5].turn, 5);
        assert_eq!(
            plan[3].request().moves.len(),
            3,
            "each request carries its own history"
        );
        assert_eq!(plan[3].request().max_visits, Some(100));
    }

    /// A variation must not be swept: the user asked about the game that was played.
    #[test]
    fn a_plan_ignores_variations() {
        let mut tree = game(3);
        let root = tree.root();
        tree.add_variation(root, Color::Black, Size::square(19).point(15, 15))
            .expect("legal");

        let plan = plan_mainline(&mut tree, Want::empty(), 10);

        assert_eq!(plan.len(), 4, "root plus the three main-line moves only");
    }

    #[tokio::test]
    async fn a_sweep_answers_every_position_in_order() {
        let engine = Counting::new(u32::MAX);
        let mut tree = game(9);
        let plan = plan_mainline(&mut tree, Want::empty(), 250);

        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        let out = sweep(&engine, plan, 4, move |_, done, total| {
            assert!(done <= total);
            counter.store(done, Ordering::SeqCst);
            Flow::Continue
        })
        .await;

        assert_eq!(out.len(), 10);
        assert_eq!(
            seen.load(Ordering::SeqCst),
            10,
            "progress must reach the total"
        );
        for (i, analysed) in out.iter().enumerate() {
            assert_eq!(analysed.turn as usize, i, "results must be in plan order");
            let report = analysed.result.as_ref().expect("analysis failed");
            assert_eq!(report.root.visits, 250);
        }
    }

    /// The sink drives a progress banner and a win-rate graph, so it must hear about each
    /// result while the rest of the plan is still queued. A sweep that dispatches everything
    /// before joining anything reports nothing until all but `concurrency` positions are
    /// done, which reads as 0/N for the whole search and then N/N at the end.
    #[tokio::test]
    async fn a_sweep_reports_a_result_before_dispatching_the_rest() {
        let engine = Counting::new(u32::MAX);
        let mut tree = game(31);
        let plan = plan_mainline(&mut tree, Want::empty(), 10);
        assert_eq!(plan.len(), 32);

        let tally = engine.clone();
        let out = sweep(&engine, plan, 4, move |_, finished, total| {
            let dispatched = tally.served.load(Ordering::SeqCst);
            assert!(
                dispatched <= finished + 4,
                "dispatched {dispatched} of {total} positions having reported {finished}"
            );
            Flow::Continue
        })
        .await;

        assert_eq!(out.len(), 32);
    }

    /// The cap is the point: it is one server stream and one search slot per request.
    #[tokio::test]
    async fn a_sweep_never_exceeds_its_concurrency() {
        let engine = Counting::new(u32::MAX);
        let mut tree = game(40);
        let plan = plan_mainline(&mut tree, Want::empty(), 10);

        let out = sweep(&engine, plan, 8, |_, _, _| Flow::Continue).await;

        assert_eq!(out.len(), 41);
        assert_eq!(engine.served.load(Ordering::SeqCst), 41);
        assert!(
            engine.peak.load(Ordering::SeqCst) <= 8,
            "ran {} at once with a cap of 8",
            engine.peak.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn one_failed_position_does_not_discard_the_others() {
        // Every request in this plan asks for 77 visits, so all of them fail; then a plan
        // that asks for something else succeeds. Both must return a full-length result.
        let engine = Counting::new(77);
        let mut tree = game(4);

        let failing = plan_mainline(&mut tree, Want::empty(), 77);
        let out = sweep(&engine, failing, 3, |_, _, _| Flow::Continue).await;
        assert_eq!(out.len(), 5);
        assert!(
            out.iter().all(|a| a.result.is_err()),
            "failures were swallowed"
        );

        let working = plan_mainline(&mut tree, Want::empty(), 78);
        let out = sweep(&engine, working, 3, |_, _, _| Flow::Continue).await;
        assert_eq!(out.len(), 5);
        assert!(out.iter().all(|a| a.result.is_ok()));
    }

    #[tokio::test]
    async fn a_concurrency_of_zero_still_makes_progress() {
        let engine = Counting::new(u32::MAX);
        let mut tree = game(3);
        let plan = plan_mainline(&mut tree, Want::empty(), 5);

        let out = sweep(&engine, plan, 0, |_, _, _| Flow::Continue).await;

        assert_eq!(out.len(), 4, "zero must behave as one, not deadlock");
        assert_eq!(engine.peak.load(Ordering::SeqCst), 1);
    }

    fn stored(black_winrate: f32, best: Option<Point>) -> NodeAnalysis {
        NodeAnalysis {
            visits: 1000,
            winrate: black_winrate,
            score_lead: 0.0,
            score_stdev: 1.0,
            candidates: best
                .map(|mv| Candidate {
                    mv,
                    visits: 900,
                    winrate: black_winrate,
                    score_lead: 0.0,
                    prior: 0.5,
                    pv: vec![mv],
                    utility: None,
                })
                .into_iter()
                .collect(),
            ownership: None,
        }
    }

    #[test]
    fn white_blunder_is_measured_from_whites_perspective() {
        let mut tree = game(3);
        let size = tree.info.size;
        let ids: Vec<_> = tree.main_line();
        let best = size.from_gtp("D16").unwrap();
        tree.set_analysis(ids[1], Some(stored(0.50, Some(best))));
        tree.set_analysis(ids[2], Some(stored(0.90, None)));

        let found = blunders(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].player, Color::White);
        assert_eq!(found[0].move_number, 2);
        assert_eq!(found[0].best, Some(best));
        assert!(
            (found[0].drop - 0.40).abs() < 1e-6,
            "drop was {}",
            found[0].drop
        );
    }

    #[test]
    fn a_move_that_improves_the_movers_winrate_is_not_a_blunder() {
        let mut tree = game(3);
        let ids: Vec<_> = tree.main_line();
        tree.set_analysis(ids[1], Some(stored(0.50, None)));
        tree.set_analysis(ids[2], Some(stored(0.20, None)));
        assert!(blunders(&tree).is_empty());
        tree.set_analysis(ids[2], Some(stored(0.51, None)));
        assert!(blunders(&tree).is_empty());
    }

    #[test]
    fn a_two_percent_drop_is_still_noise() {
        let mut tree = game(3);
        let ids: Vec<_> = tree.main_line();
        // White to play: Black 50 % → 52 % is a 2 % loss for White.
        tree.set_analysis(ids[1], Some(stored(0.50, None)));
        tree.set_analysis(ids[2], Some(stored(0.52, None)));
        assert!(blunders(&tree).is_empty());
        tree.set_analysis(ids[2], Some(stored(0.53, None)));
        assert_eq!(blunders(&tree).len(), 1);
    }

    #[test]
    fn an_unanalysed_parent_is_not_a_blunder() {
        let mut tree = game(3);
        let ids: Vec<_> = tree.main_line();
        tree.set_analysis(ids[1], Some(stored(0.50, None)));
        tree.set_analysis(ids[3], Some(stored(0.95, None)));
        assert!(blunders(&tree).is_empty());

        tree.set_analysis(ids[2], Some(stored(0.90, None)));
        let found = blunders(&tree);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].node, ids[2]);
    }

    fn pt(size: u8, x: u16, y: u16) -> Point {
        Point(y * u16::from(size) + x)
    }

    fn record(rules: RuleSet, size: u8) -> GameTree {
        GameTree::new(GameInfo {
            size: Size { w: size, h: size },
            rules,
            komi: rules.default_komi(),
            ..GameInfo::default()
        })
    }

    /// Setup at the root, a mid-game setup, `PL`, territory captures, passes, an `MN`
    /// override, and a setup node that also has a move. Every planned request must equal
    /// [`request_for_node`](crate::analysis::request_for_node).
    fn reconstruction_trees() -> Vec<GameTree> {
        let mut trees = Vec::new();

        let mut t = record(RuleSet::Japanese, 19);
        let root = t.root();
        t.node_mut(root).setup.add_black.push(pt(19, 3, 3));
        t.node_mut(root).setup.add_white.push(pt(19, 15, 15));
        t.info.handicap = 2;
        let w = t.play(root, Color::White, pt(19, 4, 4)).expect("legal");
        t.play(w, Color::Black, Point::PASS).expect("pass");
        trees.push(t);

        let mut t = record(RuleSet::Japanese, 9);
        let root = t.root();
        let first = t.play(root, Color::Black, pt(9, 2, 2)).expect("legal");
        let second = t.play(first, Color::White, pt(9, 6, 6)).expect("legal");
        let setup = t.add_child(second);
        t.node_mut(setup).setup.add_black.push(pt(9, 4, 4));
        t.node_mut(setup).setup.add_white.push(pt(9, 0, 0));
        let later = t.play(setup, Color::Black, pt(9, 3, 3)).expect("legal");
        t.play(later, Color::White, Point::PASS).expect("pass");
        trees.push(t);

        let mut t = record(RuleSet::Japanese, 9);
        let root = t.root();
        let first = t.play(root, Color::Black, pt(9, 3, 3)).expect("legal");
        let pl = t.add_child(first);
        t.node_mut(pl).to_play_override = Some(Color::Black);
        let third = t.play(pl, Color::Black, pt(9, 2, 2)).expect("legal");
        t.play(third, Color::White, pt(9, 5, 5)).expect("legal");
        trees.push(t);

        let mut t = record(RuleSet::Japanese, 9);
        let root = t.root();
        let b1 = t.play(root, Color::Black, pt(9, 1, 0)).expect("legal");
        let w1 = t.play(b1, Color::White, pt(9, 0, 0)).expect("legal");
        let cap = t.play(w1, Color::Black, pt(9, 0, 1)).expect("legal");
        let pl = t.add_child(cap);
        t.node_mut(pl).to_play_override = Some(Color::White);
        let w2 = t.play(pl, Color::White, pt(9, 8, 7)).expect("legal");
        let b2 = t.play(w2, Color::Black, pt(9, 8, 8)).expect("legal");
        t.play(b2, Color::White, pt(9, 7, 8)).expect("legal");
        trees.push(t);

        let mut area = trees.last().expect("territory tree").clone();
        area.info.rules = RuleSet::Chinese;
        trees.push(area);

        let mut t = record(RuleSet::Chinese, 9);
        let root = t.root();
        t.node_mut(root).setup.add_black.push(pt(9, 0, 0));
        let aw = t.add_child(root);
        t.node_mut(aw)
            .setup
            .add_white
            .extend([pt(9, 0, 0), pt(9, 1, 1)]);
        let ae = t.add_child(aw);
        t.node_mut(ae).setup.add_empty.push(pt(9, 1, 1));
        let mv = t.play(ae, Color::Black, pt(9, 2, 2)).expect("legal");
        t.node_mut(mv).move_number_override = Some(20);
        let after = t.play(mv, Color::White, Point::PASS).expect("pass");
        t.node_mut(after).move_number_override = Some(7);
        trees.push(t);

        let mut t = record(RuleSet::Japanese, 9);
        let both = t.add_child(t.root());
        t.node_mut(both).setup.add_black.push(pt(9, 1, 1));
        t.node_mut(both).mv = Some((Color::White, pt(9, 2, 2)));
        let quiet = t.add_child(both);
        t.node_mut(quiet).comment = "no move".into();
        t.play(quiet, Color::Black, pt(9, 3, 3)).expect("legal");
        trees.push(t);

        let mut t = record(RuleSet::Chinese, 19);
        let mut at = t.root();
        for i in 0..6 {
            let colour = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };
            at = t.play(at, colour, Point::PASS).expect("pass");
            if i == 2 {
                t.node_mut(at).move_number_override = Some(40);
            }
        }
        trees.push(t);

        trees
    }

    #[test]
    fn every_planned_request_matches_request_for_node() {
        for mut tree in reconstruction_trees() {
            let want = Want::OWNERSHIP;
            let visits = 40;
            let plan = plan_mainline(&mut tree, want, visits);
            let line = tree.main_line();
            assert_eq!(plan.len(), line.len());
            for (planned, &node) in plan.iter().zip(&line) {
                assert_eq!(planned.node, node);
                assert_eq!(planned.turn, tree.move_number(node));
                assert_eq!(
                    planned.request(),
                    crate::analysis::request_for_node(&mut tree, node, want, visits),
                    "diverged at turn {}",
                    planned.turn
                );
            }
        }
    }

    /// Each request used to own its own prefix of the move list, so planning a long game
    /// retained a quadratic number of move slots before a single search had started.
    #[test]
    fn a_plan_does_not_retain_a_move_list_per_position() {
        let mut tree = record(RuleSet::Chinese, 19);
        let mut at = tree.root();
        for i in 0..400 {
            let colour = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };
            at = tree.play(at, colour, Point::PASS).expect("pass is legal");
        }
        let plan = plan_mainline(&mut tree, Want::empty(), 10);
        let stored = plan[0].shared.moves.len();
        assert!(
            plan.iter().all(|p| Arc::ptr_eq(&p.shared, &plan[0].shared)),
            "each position kept its own move list"
        );
        assert!(
            stored < plan.len() * 4,
            "retained {stored} move slots for {} positions",
            plan.len()
        );
        assert_eq!(plan[400].request().moves.len(), 400);
    }

    #[test]
    fn in_flight_scales_with_threads_and_saturates_at_sixteen() {
        assert_eq!(in_flight(1), 2);
        assert_eq!(in_flight(8), 16);
        assert_eq!(in_flight(20), 16);
        assert_eq!(in_flight(0), 2);
    }
}
