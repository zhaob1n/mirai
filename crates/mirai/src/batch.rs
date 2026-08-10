// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Whole-game analysis. Step 12.
//!
//! The sweep is a bounded-concurrency queue: `min(analysis_threads * 2, 16)` positions are
//! kept in flight, because KataGo batches its neural-net evaluations across concurrent
//! queries and one query at a time leaves most of the GPU idle. Every worker is a
//! `glib::JoinHandle`; aborting one drops its `Subscription`, which terminates the query
//! inside the engine. That is the only cancellation mechanism there is.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use mirai_core::{Color, GameTree, NodeId, Point};
use mirai_engine::{AnalyzeReq, Engine, Want};

use crate::app::{AppState, signal};

/// A move that cost its own player win rate, as measured by the sweep.
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

/// Anything smaller than this is search noise, not a mistake worth listing. It is also
/// where the winrate graph's blunder strip starts colouring bars.
pub const BLUNDER_MIN_DROP: f32 = 0.02;

/// How many positions to keep in flight for an engine with `analysis_threads` threads.
pub fn in_flight(analysis_threads: u16) -> usize {
    ((analysis_threads as usize).max(1) * 2).min(16)
}

/// A stored (Black-perspective) win rate seen from `c`'s point of view.
#[inline]
fn winrate_for(black_winrate: f32, c: Color) -> f32 {
    match c {
        Color::Black => black_winrate,
        Color::White => 1.0 - black_winrate,
    }
}

/// Extracts the blunders of the main line from the analyses already stored in the tree.
///
/// Each move is judged from the perspective of the player who made it: the win rate the
/// position had for them before the move, minus the win rate it has for them after. Nodes
/// whose own or whose parent's analysis is missing are skipped — an unanalysed gap is not
/// evidence of a mistake.
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
        let (Some(before), Some(after)) = (tree.node(parent).analysis.as_ref(), node.analysis.as_ref())
        else {
            continue;
        };
        let drop = winrate_for(before.winrate, player) - winrate_for(after.winrate, player);
        if drop.is_nan() || drop < BLUNDER_MIN_DROP {
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

/// A callback run once, with the whole blunder list, when a sweep finishes.
type BlunderHook = Box<dyn Fn(Vec<Blunder>)>;

/// Drives the sweep and fills each main-line node's `NodeAnalysis`. The banner is owned
/// here but mounted by the window.
pub struct BatchAnalysis {
    state: AppState,
    banner: adw::Banner,
    running: Cell<bool>,
    tasks: RefCell<Vec<glib::JoinHandle<()>>>,
    /// Nodes still to analyse, in reverse order so workers can `pop`.
    queue: RefCell<Vec<NodeId>>,
    total: Cell<u32>,
    done: Cell<u32>,
    /// Workers that have not yet drained the queue.
    live_workers: Cell<usize>,
    /// Bumped by every start and every stop, so a worker can tell it has been superseded.
    generation: Cell<u64>,
    on_finished: RefCell<Vec<BlunderHook>>,
}

impl BatchAnalysis {
    pub fn new(state: &AppState) -> Rc<BatchAnalysis> {
        let banner = adw::Banner::builder()
            .revealed(false)
            .button_label("Cancel")
            .build();
        let this = Rc::new(BatchAnalysis {
            state: state.clone(),
            banner,
            running: Cell::new(false),
            tasks: RefCell::new(Vec::new()),
            queue: RefCell::new(Vec::new()),
            total: Cell::new(0),
            done: Cell::new(0),
            live_workers: Cell::new(0),
            generation: Cell::new(0),
            on_finished: RefCell::new(Vec::new()),
        });
        let weak = Rc::downgrade(&this);
        this.banner.connect_button_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.cancel();
            }
        });
        this
    }

    pub fn banner(&self) -> &adw::Banner {
        &self.banner
    }

    /// Registers a callback for the blunder list produced by a completed sweep.
    pub fn connect_finished(&self, f: impl Fn(Vec<Blunder>) + 'static) {
        self.on_finished.borrow_mut().push(Box::new(f));
    }

    pub fn start(self: &Rc<Self>) {
        if self.running.get() {
            self.state.toast("Whole-game analysis is already running");
            return;
        }
        let Some(engine) = self.state.engine() else {
            self.state.toast("No engine — start one in Preferences first");
            return;
        };
        let nodes = self.state.tree().main_line();
        if nodes.len() < 2 {
            self.state.toast("Nothing to analyse yet");
            return;
        }

        let (visits, max_candidates) = {
            let cfg = self.state.config();
            (
                cfg.analysis.batch_visits,
                cfg.analysis.max_suggestions as usize,
            )
        };
        let workers = in_flight(engine.describe().analysis_threads).min(nodes.len());

        self.generation.set(self.generation.get().wrapping_add(1));
        let generation = self.generation.get();
        self.running.set(true);
        self.done.set(0);
        self.total.set(nodes.len() as u32);
        self.live_workers.set(workers);
        {
            let mut queue = self.queue.borrow_mut();
            *queue = nodes;
            queue.reverse();
        }
        self.update_banner();
        self.banner.set_revealed(true);
        self.state.set_busy(true);
        self.state.notify_batch_progress(0, self.total.get());

        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let weak = Rc::downgrade(self);
                let engine = Arc::clone(&engine);
                glib::spawn_future_local(async move {
                    run_worker(weak, engine, generation, visits, max_candidates).await;
                })
            })
            .collect();
        *self.tasks.borrow_mut() = handles;
    }

    /// User-requested stop: kills every in-flight query and hides the banner. Whatever was
    /// analysed before the cancel stays in the tree.
    pub fn cancel(&self) {
        if !self.running.get() {
            return;
        }
        self.teardown();
        self.state.notify_batch_progress(self.done.get(), self.total.get());
        self.state.emit_by_name::<()>(signal::TREE_CHANGED, &[]);
        self.state
            .toast(format!("Analysis cancelled after {} positions", self.done.get()));
    }

    /// Stops the workers and puts the UI back to rest, without reporting anything.
    fn teardown(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.running.set(false);
        self.live_workers.set(0);
        self.queue.borrow_mut().clear();
        for task in self.tasks.borrow_mut().drain(..) {
            task.abort();
        }
        self.banner.set_revealed(false);
        self.state.set_busy(false);
    }

    fn update_banner(&self) {
        self.banner.set_title(&format!(
            "Analysing {}/{}…",
            self.done.get(),
            self.total.get()
        ));
    }

    /// Called by a worker after each stored result.
    fn record_progress(&self) {
        self.done.set(self.done.get() + 1);
        self.update_banner();
        self.state
            .notify_batch_progress(self.done.get(), self.total.get());
    }

    /// A worker drained the queue; the last one out finishes the sweep.
    fn worker_finished(&self) {
        let left = self.live_workers.get().saturating_sub(1);
        self.live_workers.set(left);
        if left > 0 {
            return;
        }
        let analysed = self.done.get();
        self.teardown();

        // One redraw for the whole sweep rather than one per node. Emitted directly so the
        // document is not flagged as modified: nothing the user typed has changed.
        self.state.emit_by_name::<()>(signal::TREE_CHANGED, &[]);

        let rows = blunders(&self.state.tree());
        self.state.toast(match rows.len() {
            0 => format!("Analysed {analysed} positions — no blunders"),
            1 => format!("Analysed {analysed} positions — 1 blunder"),
            n => format!("Analysed {analysed} positions — {n} blunders"),
        });
        for hook in self.on_finished.borrow().iter() {
            hook(rows.clone());
        }
    }

    /// Aborts the sweep because the engine failed; reports it once.
    fn fail(&self, message: String) {
        self.teardown();
        self.state.notify_batch_progress(self.done.get(), self.total.get());
        self.state.emit_by_name::<()>(signal::TREE_CHANGED, &[]);
        self.state.toast(format!("Analysis stopped: {message}"));
    }

    /// Whole-game analysis differs from live analysis only in its knobs; the position walk
    /// itself lives in `AppState` so the two can never disagree.
    fn request_for_node(&self, id: NodeId, max_visits: u32) -> AnalyzeReq {
        let mut req = self
            .state
            .request_for_node(id, Some(max_visits), Want::OWNERSHIP);
        req.report_every_ms = None;
        req.priority = 0;
        req
    }
}

/// One worker: take the next position, analyse it to completion, store it, repeat.
async fn run_worker(
    weak: Weak<BatchAnalysis>,
    engine: Arc<dyn Engine>,
    generation: u64,
    visits: u32,
    max_candidates: usize,
) {
    loop {
        let (batch, id, req) = {
            let Some(batch) = weak.upgrade() else { return };
            if batch.generation.get() != generation {
                return;
            }
            let Some(id) = batch.queue.borrow_mut().pop() else {
                break;
            };
            let req = batch.request_for_node(id, visits);
            (batch, id, req)
        };
        drop(batch);

        let result = engine.subscribe(req).finish().await;

        let Some(batch) = weak.upgrade() else { return };
        if batch.generation.get() != generation {
            return;
        }
        match result {
            Ok(report) => {
                let analysis = crate::util::analysis_of(&report, max_candidates);
                batch
                    .state
                    .with_tree_cached(|tree| tree.set_analysis(id, Some(analysis)));
                batch.record_progress();
            }
            Err(e) => {
                tracing::warn!(%e, "whole-game analysis query failed");
                batch.fail(e.to_string());
                return;
            }
        }
    }

    if let Some(batch) = weak.upgrade()
        && batch.generation.get() == generation
    {
        batch.worker_finished();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_core::{GameInfo, GameTree, NodeAnalysis, RuleSet, Size};

    fn analysis(black_winrate: f32, best: Option<Point>) -> NodeAnalysis {
        NodeAnalysis {
            visits: 1000,
            winrate: black_winrate,
            score_lead: 0.0,
            score_stdev: 1.0,
            candidates: best
                .map(|mv| mirai_core::Candidate {
                    mv,
                    visits: 900,
                    winrate: black_winrate,
                    score_lead: 0.0,
                    prior: 0.5,
                    pv: vec![mv],
                })
                .into_iter()
                .collect(),
            ownership: None,
        }
    }

    /// Root, then `B D4`, `W Q16`, `B Q4` on a 19×19 board.
    fn game() -> (GameTree, Size, Vec<NodeId>) {
        let size = Size::square(19);
        let mut tree = GameTree::new(GameInfo::new(size, RuleSet::Chinese));
        let mut ids = vec![tree.root()];
        for (c, gtp) in [
            (Color::Black, "D4"),
            (Color::White, "Q16"),
            (Color::Black, "Q4"),
        ] {
            let at = *ids.last().unwrap();
            ids.push(tree.play(at, c, size.from_gtp(gtp).unwrap()).unwrap());
        }
        (tree, size, ids)
    }

    #[test]
    fn white_blunder_is_measured_from_whites_perspective() {
        let (mut tree, size, ids) = game();
        let best = size.from_gtp("D16").unwrap();
        // Black 50 % before White's move, Black 90 % after it: White lost 40 points of
        // win rate even though the stored (Black-perspective) number went *up*.
        tree.set_analysis(ids[1], Some(analysis(0.50, Some(best))));
        tree.set_analysis(ids[2], Some(analysis(0.90, None)));

        let found = blunders(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
        let b = found[0];
        assert_eq!(b.node, ids[2]);
        assert_eq!(b.player, Color::White);
        assert_eq!(b.move_number, 2);
        assert_eq!(b.played, size.from_gtp("Q16").unwrap());
        assert_eq!(b.best, Some(best));
        assert!((b.drop - 0.40).abs() < 1e-6, "drop was {}", b.drop);
    }

    #[test]
    fn a_move_that_improves_the_movers_winrate_is_not_a_blunder() {
        let (mut tree, _size, ids) = game();
        // Black 50 % before White's move, Black 20 % after: White improved.
        tree.set_analysis(ids[1], Some(analysis(0.50, None)));
        tree.set_analysis(ids[2], Some(analysis(0.20, None)));
        assert!(blunders(&tree).is_empty());
        // And a drop below the noise floor is not a blunder either.
        tree.set_analysis(ids[2], Some(analysis(0.51, None)));
        assert!(blunders(&tree).is_empty());
    }

    #[test]
    fn nodes_without_analysis_are_skipped() {
        let (mut tree, _size, ids) = game();
        // Move 3 has an analysis but its parent (move 2) does not, so nothing on the line
        // can be judged yet. Move 3 itself is a fine move for Black (0.90 → 0.95), so once
        // move 2 is filled in only White's mistake shows up.
        tree.set_analysis(ids[1], Some(analysis(0.50, None)));
        tree.set_analysis(ids[3], Some(analysis(0.95, None)));
        assert!(blunders(&tree).is_empty());

        tree.set_analysis(ids[2], Some(analysis(0.90, None)));
        let found = blunders(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].node, ids[2]);
    }

    #[test]
    fn best_is_none_when_the_engine_agreed_with_the_move_played() {
        let (mut tree, size, ids) = game();
        let played = size.from_gtp("Q16").unwrap();
        tree.set_analysis(ids[1], Some(analysis(0.50, Some(played))));
        tree.set_analysis(ids[2], Some(analysis(0.95, None)));
        let found = blunders(&tree);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].best, None);
    }

    #[test]
    fn in_flight_scales_with_threads_and_saturates_at_sixteen() {
        assert_eq!(in_flight(1), 2);
        assert_eq!(in_flight(2), 4);
        assert_eq!(in_flight(8), 16);
        assert_eq!(in_flight(20), 16);
        // A degenerate description still gets one worker's worth of work.
        assert_eq!(in_flight(0), 2);
    }
}
