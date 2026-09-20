// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Analysing a whole game: build every request, then run them with bounded concurrency.
//!
//! The split is deliberate. Building a request needs `&mut GameTree` (the position cache), so
//! it cannot happen concurrently; running one needs nothing but an [`Engine`]. Doing the tree
//! work up front turns a whole-game sweep into a plain bounded-concurrency problem and keeps
//! the borrow out of the async part entirely.
//!
//! Concurrency is capped because each in-flight request is one server-opened unidirectional
//! stream and one KataGo search slot. Beyond the server's analysis threads, more requests buy
//! nothing and only lengthen the queue the user is waiting on.

use std::sync::Arc;

use mirai_core::{Color, GameTree, NodeId, Point};
use mirai_engine::{AnalyzeReq, Engine, EngineError, Report, Want};

use crate::analysis;

/// One position of the main line, and the request that will analyse it.
pub struct Planned {
    pub node: NodeId,
    /// Move number, for progress and for placing the result on a graph.
    pub turn: u16,
    pub req: AnalyzeReq,
}

/// What one planned position produced.
pub struct Analysed {
    pub node: NodeId,
    pub turn: u16,
    pub result: Result<Arc<Report>, EngineError>,
}

/// Builds one request per main-line position, root included.
///
/// The root is included because the win rate before the first move is the baseline every
/// later delta is measured against.
pub fn plan_mainline(tree: &mut GameTree, want: Want, visits: u32) -> Vec<Planned> {
    let line = tree.main_line();
    line.into_iter()
        .map(|node| {
            let turn = tree.move_number(node);
            Planned {
                node,
                turn,
                req: analysis::request_for_node(tree, node, want, visits),
            }
        })
        .collect()
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
            let Some((index, Planned { node, turn, req })) = queue.next() else {
                break;
            };
            // `subscribe` is synchronous by contract, so the whole query lives in one task
            // and its slot is held for exactly as long as the query.
            let sub = engine.subscribe(req);
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
            plan[3].req.moves.len(),
            3,
            "each request carries its own history"
        );
        assert_eq!(plan[3].req.max_visits, Some(100));
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

    #[test]
    fn in_flight_scales_with_threads_and_saturates_at_sixteen() {
        assert_eq!(in_flight(1), 2);
        assert_eq!(in_flight(8), 16);
        assert_eq!(in_flight(20), 16);
        assert_eq!(in_flight(0), 2);
    }
}
