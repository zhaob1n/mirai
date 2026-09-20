// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Whole-game analysis. Step 12.
//!
//! The sweep itself is `mirai_client::batch::sweep`: `min(analysis_threads * 2, 16)`
//! positions are kept in flight, because KataGo batches its neural-net evaluations across
//! concurrent queries and one query at a time leaves most of the GPU idle. What lives here
//! is the GTK half — the banner, the progress toasts, and writing each result onto the tree
//! as it lands so a cancelled sweep keeps what it already found.
//!
//! Cancelling drops [`RuntimeTask`], which aborts the tokio task, which drops the sweep's
//! `JoinSet` and with it every live `Subscription`. That is the only cancellation mechanism
//! there is (INV-3).

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use mirai_client::batch::Flow;
use mirai_core::{Color, GameTree, NodeId, Point};
use mirai_engine::{Engine, Want};

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
    Item(NodeId, Result<Arc<mirai_engine::Report>, String>),
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
    /// The tree the running sweep was planned against; a result that arrives after the
    /// record was replaced must not land on the new one.
    epoch: Cell<TreeEpoch>,
    start_position_revision: Cell<u64>,
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
            epoch: Cell::new(state.tree_epoch()),
            start_position_revision: Cell::new(state.position_revision()),
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
        let (visits, max_candidates) = {
            let cfg = self.state.config();
            (
                cfg.analysis.batch_visits,
                cfg.analysis.stored_suggestion_limit(),
            )
        };
        let epoch = self.state.tree_epoch();
        let mut plan = self
            .state
            .with_tree_cached(|t| mirai_client::batch::plan_mainline(t, Want::OWNERSHIP, visits));
        if plan.len() < 2 {
            self.state.toast("Nothing to analyse yet");
            return;
        }
        // A sweep is background work: it must not preempt the live search the user is
        // watching, and nobody reads its intermediate reports.
        for planned in &mut plan {
            planned.req.report_every_ms = None;
            planned.req.priority = 0;
        }
        let workers = in_flight(engine.describe().analysis_threads).min(plan.len());

        self.running.set(true);
        self.epoch.set(epoch);
        self.start_position_revision
            .set(self.state.position_revision());
        self.done.set(0);
        self.analysed.set(0);
        self.total.set(plan.len() as u32);
        self.update_banner();
        self.banner.set_revealed(true);
        self.state.set_busy(true);
        self.state.notify_batch_progress(0, self.total.get());

        if let Some(old) = self.task.borrow_mut().take() {
            old.abort();
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime_task = self.state.runtime().spawn(async move {
            let mut failed = false;
            mirai_client::batch::sweep(engine.as_ref(), plan, workers, |analysed, _, _| {
                let item = analysed.result.clone().map_err(|e| e.to_string());
                failed |= item.is_err();
                // One dead search stops the sweep here: the desktop reports it on the
                // banner rather than leaving a silent hole in the graph.
                if tx.send(BatchMessage::Item(analysed.node, item)).is_err() || failed {
                    Flow::Stop
                } else {
                    Flow::Continue
                }
            })
            .await;
            if !failed {
                let _ = tx.send(BatchMessage::Finished);
            }
        });
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
        if !self.running.get()
            || self.state.position_revision() != self.start_position_revision.get()
            || self.state.tree_epoch() != self.epoch.get()
        {
            return false;
        }
        match message {
            BatchMessage::Item(node, Ok(report)) => {
                let id = self.state.resolve_node(NodeRef {
                    epoch: self.epoch.get(),
                    id: node,
                });
                let stored = id.is_some_and(|id| {
                    let analysis = crate::util::analysis_of(&report, max_candidates);
                    self.state.set_analysis_at(
                        id,
                        self.start_position_revision.get(),
                        Some(analysis),
                    )
                });
                self.record_progress(stored);
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
                    utility: None,
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

    /// `mirai_client::batch::blunders` is tested in its own crate; what the desktop wrapper
    /// adds is the epoch stamp that keeps a stale node id from resolving after the tree is
    /// replaced (INV-10).
    #[test]
    fn every_blunder_carries_the_epoch_it_was_found_in() {
        let (mut tree, size, ids) = game();
        let best = size.from_gtp("D16").unwrap();
        tree.set_analysis(ids[1], Some(analysis(0.50, Some(best))));
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
        assert_eq!(found[0].player, Color::White);
        assert_eq!(found[0].best, Some(best));
    }
}
