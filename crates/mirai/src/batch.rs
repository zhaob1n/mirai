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
use std::collections::VecDeque;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use mirai_core::{Color, GameTree, NodeId, Point};
use mirai_engine::{AnalyzeReq, Engine, Want};

use crate::app::{AppState, Change, NodeRef, TreeEpoch};
use crate::window_shell::MiraiWindow;

/// A move that cost its own player win rate, as measured by the sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blunder {
    pub node: NodeRef,
    pub move_number: u16,
    pub player: Color,
    /// Win-rate loss for `player`, in `0.0..=1.0`.
    pub drop: f32,
    pub played: Point,
    /// The engine's first choice at the parent, when it differs from what was played.
    pub best: Option<Point>,
}

pub fn in_flight(analysis_threads: u16) -> usize {
    mirai_client::batch::in_flight(analysis_threads)
}

pub fn blunders(tree: &GameTree, epoch: TreeEpoch) -> Vec<Blunder> {
    mirai_client::batch::blunders(tree)
        .into_iter()
        .map(|b| Blunder {
            node: NodeRef { epoch, id: b.node },
            move_number: b.move_number,
            player: b.player,
            drop: b.drop,
            played: b.played,
            best: b.best,
        })
        .collect()
}

enum BatchMessage {
    Item(NodeRef, Result<Arc<mirai_engine::Report>, String>),
    Finished,
}

struct RuntimeTask(tokio::task::JoinHandle<()>);

impl Drop for RuntimeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Drives one bounded-concurrency sweep. The window owns this value uniquely; one coordinator
/// task owns the queue and all in-flight subscriptions.
pub struct BatchAnalysis {
    state: AppState,
    window: glib::WeakRef<MiraiWindow>,
    banner: adw::Banner,
    running: Cell<bool>,
    task: RefCell<Option<glib::JoinHandle<()>>>,
    total: Cell<u32>,
    done: Cell<u32>,
    analysed: Cell<u32>,
}

impl BatchAnalysis {
    pub fn new(state: &AppState, window: &MiraiWindow) -> BatchAnalysis {
        let banner = adw::Banner::builder()
            .revealed(false)
            .button_label("Cancel")
            .build();
        let this = BatchAnalysis {
            state: state.clone(),
            window: window.downgrade(),
            banner,
            running: Cell::new(false),
            task: RefCell::new(None),
            total: Cell::new(0),
            done: Cell::new(0),
            analysed: Cell::new(0),
        };
        let weak = this.window.clone();
        this.banner.connect_button_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.with_ui(|ui| ui.batch.cancel());
            }
        });
        this
    }

    pub fn banner(&self) -> &adw::Banner {
        &self.banner
    }

    pub fn start(&self) {
        if self.running.get() {
            self.state.toast("Whole-game analysis is already running");
            return;
        }
        let Some(engine) = self.state.engine() else {
            self.state
                .toast("No engine — start one in Preferences first");
            return;
        };
        let nodes: Vec<_> = self
            .state
            .tree()
            .main_line()
            .into_iter()
            .map(|id| self.state.node_ref(id))
            .collect();
        if nodes.len() < 2 {
            self.state.toast("Nothing to analyse yet");
            return;
        }

        let (visits, max_candidates) = {
            let cfg = self.state.config();
            (
                cfg.analysis.batch_visits,
                cfg.analysis.stored_suggestion_limit(),
            )
        };
        let workers = in_flight(engine.describe().analysis_threads).min(nodes.len());
        let requests: VecDeque<_> = nodes
            .into_iter()
            .filter_map(|node| {
                let id = self.state.resolve_node(node)?;
                let mut req = self
                    .state
                    .request_for_node(id, Some(visits), Want::OWNERSHIP);
                req.report_every_ms = None;
                req.priority = 0;
                Some((node, req))
            })
            .collect();

        self.running.set(true);
        self.done.set(0);
        self.analysed.set(0);
        self.total.set(requests.len() as u32);
        self.update_banner();
        self.banner.set_revealed(true);
        self.state.set_busy(true);
        self.state.notify_batch_progress(0, self.total.get());

        if let Some(old) = self.task.borrow_mut().take() {
            old.abort();
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime_task = self
            .state
            .runtime()
            .spawn(run_batch(engine, requests, workers, tx));
        let runtime_task = RuntimeTask(runtime_task);
        let weak = self.window.clone();
        let handle = glib::spawn_future_local(async move {
            let _runtime_task = runtime_task;
            while let Some(message) = rx.recv().await {
                let Some(window) = weak.upgrade() else {
                    return;
                };
                let keep_going = window
                    .with_ui(|ui| ui.batch.handle_message(message, max_candidates))
                    .unwrap_or(false);
                if !keep_going {
                    return;
                }
            }
        });
        *self.task.borrow_mut() = Some(handle);
    }

    pub fn cancel(&self) {
        if !self.running.replace(false) {
            return;
        }
        if let Some(task) = self.task.borrow_mut().take() {
            task.abort();
        }
        self.banner.set_revealed(false);
        self.state.set_busy(false);
        self.state
            .notify_batch_progress(self.done.get(), self.total.get());
        self.state.changed(Change::Tree);
        self.state.toast(format!(
            "Analysis cancelled after {} positions",
            self.done.get()
        ));
    }

    fn handle_message(&self, message: BatchMessage, max_candidates: usize) -> bool {
        if !self.running.get() {
            return false;
        }
        match message {
            BatchMessage::Item(node, Ok(report)) => {
                let id = self.state.resolve_node(node);
                if let Some(id) = id {
                    let analysis = crate::util::analysis_of(&report, max_candidates);
                    self.state
                        .with_tree_cached(|tree| tree.set_analysis(id, Some(analysis)));
                }
                self.record_progress(id.is_some());
                true
            }
            BatchMessage::Item(_, Err(message)) => {
                tracing::warn!(%message, "whole-game analysis query failed");
                self.fail(message);
                false
            }
            BatchMessage::Finished => {
                self.finish();
                false
            }
        }
    }

    fn update_banner(&self) {
        self.banner.set_title(&format!(
            "Analysing {}/{}…",
            self.done.get(),
            self.total.get()
        ));
    }

    fn record_progress(&self, stored: bool) {
        self.done.set(self.done.get() + 1);
        if stored {
            self.analysed.set(self.analysed.get() + 1);
        }
        self.update_banner();
        self.state
            .notify_batch_progress(self.done.get(), self.total.get());
    }

    fn finish(&self) {
        let analysed = self.analysed.get();
        self.running.set(false);
        self.banner.set_revealed(false);
        self.state.set_busy(false);
        self.state.changed(Change::Tree);

        let n = blunders(&self.state.tree(), self.state.tree_epoch()).len();
        self.state.toast(match n {
            0 => format!("Analysed {analysed} positions — no blunders"),
            1 => format!("Analysed {analysed} positions — 1 blunder"),
            n => format!("Analysed {analysed} positions — {n} blunders"),
        });
    }

    fn fail(&self, message: String) {
        self.running.set(false);
        self.banner.set_revealed(false);
        self.state.set_busy(false);
        self.state
            .notify_batch_progress(self.done.get(), self.total.get());
        self.state.changed(Change::Tree);
        self.state.toast(format!("Analysis stopped: {message}"));
    }
}

impl Drop for BatchAnalysis {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

async fn run_batch(
    engine: Arc<dyn Engine>,
    mut queue: VecDeque<(NodeRef, AnalyzeReq)>,
    workers: usize,
    tx: tokio::sync::mpsc::UnboundedSender<BatchMessage>,
) {
    let mut in_flight = tokio::task::JoinSet::new();
    for _ in 0..workers {
        let Some((node, req)) = queue.pop_front() else {
            break;
        };
        let engine = Arc::clone(&engine);
        in_flight.spawn(async move {
            let result = engine
                .subscribe(req)
                .finish()
                .await
                .map_err(|e| e.to_string());
            (node, result)
        });
    }

    while let Some(joined) = in_flight.join_next().await {
        let (node, result) = match joined {
            Ok(result) => result,
            Err(e) => {
                let _ = tx.send(BatchMessage::Item(
                    NodeRef {
                        epoch: TreeEpoch(u64::MAX),
                        id: NodeId(0),
                    },
                    Err(e.to_string()),
                ));
                return;
            }
        };
        let failed = result.is_err();
        if tx.send(BatchMessage::Item(node, result)).is_err() || failed {
            return;
        }
        if let Some((node, req)) = queue.pop_front() {
            let engine = Arc::clone(&engine);
            in_flight.spawn(async move {
                let result = engine
                    .subscribe(req)
                    .finish()
                    .await
                    .map_err(|e| e.to_string());
                (node, result)
            });
        }
    }
    let _ = tx.send(BatchMessage::Finished);
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

        let found = blunders(&tree, TreeEpoch(7));
        assert_eq!(found.len(), 1, "{found:?}");
        let b = found[0];
        assert_eq!(
            b.node,
            NodeRef {
                epoch: TreeEpoch(7),
                id: ids[2]
            }
        );
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
        assert!(blunders(&tree, TreeEpoch(0)).is_empty());
        // And a drop below the noise floor is not a blunder either.
        tree.set_analysis(ids[2], Some(analysis(0.51, None)));
        assert!(blunders(&tree, TreeEpoch(0)).is_empty());
    }

    #[test]
    fn nodes_without_analysis_are_skipped() {
        let (mut tree, _size, ids) = game();
        // Move 3 has an analysis but its parent (move 2) does not, so nothing on the line
        // can be judged yet. Move 3 itself is a fine move for Black (0.90 → 0.95), so once
        // move 2 is filled in only White's mistake shows up.
        tree.set_analysis(ids[1], Some(analysis(0.50, None)));
        tree.set_analysis(ids[3], Some(analysis(0.95, None)));
        assert!(blunders(&tree, TreeEpoch(0)).is_empty());

        tree.set_analysis(ids[2], Some(analysis(0.90, None)));
        let found = blunders(&tree, TreeEpoch(7));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].node,
            NodeRef {
                epoch: TreeEpoch(7),
                id: ids[2]
            }
        );
    }

    #[test]
    fn best_is_none_when_the_engine_agreed_with_the_move_played() {
        let (mut tree, size, ids) = game();
        let played = size.from_gtp("Q16").unwrap();
        tree.set_analysis(ids[1], Some(analysis(0.50, Some(played))));
        tree.set_analysis(ids[2], Some(analysis(0.95, None)));
        let found = blunders(&tree, TreeEpoch(0));
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
