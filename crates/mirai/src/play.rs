// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Play mode. Step 11.
//!
//! A game against the engine is a thin layer over the same [`AppState`] a review session
//! uses: the moves land in the ordinary game tree, so everything the analysis UI already
//! draws keeps working while a game is running.
//!
//! The rules of a game — whose turn it is, the clocks, resignation, the move the engine
//! actually plays, and the count at the end — are [`mirai_client::Play`], shared with the
//! HarmonyOS client. What is here is the GTK half of it: the tick source, the engine
//! subscription and its status text, the board's click hook and territory overlay, and the
//! result dialog. Every action mutates `Play` and then calls [`PlayController::sync`],
//! which redraws and starts whatever the new state asks for.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::ObjectExt;
use mirai_core::Point;
use mirai_engine::{Report, SubEvent};

use crate::app::AppState;
use crate::widgets::BoardView;
use crate::window_shell::MiraiWindow;

pub use mirai_client::play::{DEFAULT_HUMAN_PROFILE, GameSetup, PlayState, Strength};

fn with_play<R>(
    weak: &glib::WeakRef<MiraiWindow>,
    f: impl FnOnce(&PlayController) -> R,
) -> Option<R> {
    let window = weak.upgrade()?;
    window.with_ui(|ui| f(&ui.play))
}

/// What the controller has already started for the state `Play` is in, so a redraw cannot
/// start a second search or a second count.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Started {
    Nothing,
    Search,
    Count,
}

/// Owns the play session and drives the AI's turns.
pub struct PlayController {
    state: AppState,
    window: glib::WeakRef<MiraiWindow>,
    play: RefCell<mirai_client::Play>,
    thinking: RefCell<Option<glib::JoinHandle<()>>>,
    ticker: Cell<Option<glib::SourceId>>,
    board: RefCell<Option<BoardView>>,
    analyse_hook: RefCell<Option<Box<dyn Fn()>>>,
    started: Cell<Started>,
    last_tick: Cell<Instant>,
}

impl PlayController {
    pub fn new(state: &AppState, window: &MiraiWindow) -> PlayController {
        PlayController {
            state: state.clone(),
            window: window.downgrade(),
            play: RefCell::new(mirai_client::Play::new()),
            thinking: RefCell::new(None),
            ticker: Cell::new(None),
            board: RefCell::new(None),
            analyse_hook: RefCell::new(None),
            started: Cell::new(Started::Nothing),
            last_tick: Cell::new(Instant::now()),
        }
    }

    pub fn is_active(&self) -> bool {
        self.play.borrow().is_active()
    }

    /// Whether the current session is a person playing against the engine.
    pub fn is_human_vs_engine(&self) -> bool {
        self.play.borrow().human().is_some()
    }

    pub fn play_state(&self) -> PlayState {
        self.play.borrow().state()
    }

    /// Gives the controller the board it scores on: it installs a click hook that toggles
    /// dead groups while the game is being counted, and draws the territory overlay.
    pub fn attach_board(&self, board: &BoardView) {
        *self.board.borrow_mut() = Some(board.clone());
        let weak = self.window.clone();
        board.set_click_hook(move |p: Point| {
            with_play(&weak, |play| play.on_board_click(p)).unwrap_or(false)
        });
    }

    /// What the "Analyse game" button in the result dialog runs. The window wires this to
    /// its `BatchAnalysis` so the banner and the blunder strip come along.
    pub fn set_analyse_hook(&self, f: impl Fn() + 'static) {
        *self.analyse_hook.borrow_mut() = Some(Box::new(f));
    }

    // -- lifecycle ----------------------------------------------------------------------

    pub fn start(&self, setup: GameSetup) {
        self.stop();

        let engine_name = self
            .state
            .engine_desc()
            .map(|d| d.name)
            .unwrap_or_else(|| "KataGo".to_string());
        let date = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%Y-%m-%d").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();

        self.state.with_session_mut(|game| {
            self.play
                .borrow_mut()
                .start(game, setup, &engine_name, &date)
        });
        self.clear_overlay();
        self.start_ticker();
        self.sync();
    }

    pub fn stop(&self) {
        self.abort_thinking();
        if let Some(id) = self.ticker.take() {
            id.remove();
        }
        let had = self.play.borrow().is_active();
        self.play.borrow_mut().stop();
        self.started.set(Started::Nothing);
        self.clear_overlay();
        if had {
            self.state.set_status(String::new());
            self.state.notify_play_changed();
        }
    }

    fn abort_thinking(&self) {
        if let Some(h) = self.thinking.borrow_mut().take() {
            h.abort();
        }
    }

    fn clear_overlay(&self) {
        if let Some(b) = self.board.borrow().as_ref() {
            b.set_score_overlay(None, None);
        }
    }

    /// Reflects whatever `Play` just decided: redraw, then start the AI's search or the
    /// end-of-game count when that is what the new state asks for.
    fn sync(&self) {
        let state = self.play_state();
        self.draw_overlay(&state);
        self.update_status(&state);
        self.state.notify_play_changed();
        match state {
            PlayState::Idle | PlayState::HumanTurn => self.started.set(Started::Nothing),
            PlayState::AiThinking => {
                if self.started.replace(Started::Search) != Started::Search {
                    self.ai_turn();
                }
            }
            PlayState::Scoring | PlayState::Over(_) => {
                if self.started.replace(Started::Count) != Started::Count {
                    self.begin_count();
                }
            }
        }
    }

    fn draw_overlay(&self, state: &PlayState) {
        let Some(board) = self.board.borrow().clone() else {
            return;
        };
        if !matches!(state, PlayState::Scoring | PlayState::Over(_)) {
            board.set_score_overlay(None, None);
            return;
        }
        let play = self.play.borrow();
        let (Some(dead), Some(territory)) = (play.dead(), play.territory()) else {
            return;
        };
        board.set_score_overlay(
            Some(dead.clone()),
            Some(territory.to_vec().into_boxed_slice()),
        );
    }

    fn update_status(&self, state: &PlayState) {
        let status = match state {
            PlayState::Idle | PlayState::HumanTurn => String::new(),
            // Owned by the search stream and the count; leave whatever they wrote.
            PlayState::AiThinking => return,
            PlayState::Scoring => match self.play.borrow().result_phrase() {
                Some(phrase) => format!("{phrase} — click a group to mark it dead"),
                None => return,
            },
            PlayState::Over(_) => match self.play.borrow().result_phrase() {
                Some(phrase) => phrase,
                None => return,
            },
        };
        self.state.set_status(status);
    }

    // -- moves --------------------------------------------------------------------------

    pub fn pass(&self) {
        self.human_move(Point::PASS);
    }

    fn human_move(&self, p: Point) {
        if !matches!(self.play_state(), PlayState::HumanTurn) {
            return;
        }
        let played = self
            .state
            .with_session_mut(|game| self.play.borrow_mut().human_play(game, p));
        match played {
            Ok(()) => self.sync(),
            Err(e) => self.state.toast(e),
        }
    }

    pub fn resign(&self) {
        if !self.is_active() || matches!(self.play_state(), PlayState::Scoring) {
            return;
        }
        self.abort_thinking();
        self.state
            .with_session_mut(|game| self.play.borrow_mut().resign(game));
        self.sync();
    }

    /// Retracts the AI's move and the human's, restores the clocks and cancels any
    /// in-flight search.
    pub fn undo(&self) {
        if !self.is_active() {
            return;
        }
        self.abort_thinking();
        self.clear_overlay();
        self.started.set(Started::Nothing);
        self.state
            .with_session_mut(|game| self.play.borrow_mut().undo(game));
        self.state.set_status(String::new());
        self.sync();
    }

    // -- the AI's turn ------------------------------------------------------------------

    fn ai_turn(&self) {
        let Some(engine) = self.state.engine() else {
            self.state
                .toast("No engine is running — start one in Preferences");
            self.started.set(Started::Nothing);
            return;
        };
        if self.play.borrow().ai().is_none() {
            return;
        }
        let Some(req) = self
            .state
            .with_session_mut(|game| self.play.borrow_mut().ai_request(game))
        else {
            return;
        };

        let mut sub = engine.subscribe(req);
        let weak = self.window.clone();
        let handle = glib::spawn_future_local(async move {
            let mut done: Option<Arc<Report>> = None;
            while let Some(event) = sub.next().await {
                match event {
                    SubEvent::Pending => {}
                    SubEvent::Report(r) => {
                        if with_play(&weak, |play| {
                            play.state.set_status(format!(
                                "Thinking… {} visits",
                                crate::util::si_visits(r.root.visits)
                            ));
                        })
                        .is_none()
                        {
                            return;
                        }
                    }
                    SubEvent::Done(r) => {
                        done = Some(r);
                        break;
                    }
                    SubEvent::Failed(e) => {
                        with_play(&weak, |play| {
                            play.state.set_status(String::new());
                            play.state.on_engine_error(e);
                            play.give_turn_back();
                        });
                        return;
                    }
                }
            }
            with_play(&weak, |play| {
                play.state.set_status(String::new());
                match done {
                    Some(report) => play.apply_ai_move(&report),
                    None => play.give_turn_back(),
                }
            });
        });
        if let Some(old) = self.thinking.borrow_mut().replace(handle) {
            old.abort();
        }
    }

    /// The search produced nothing usable. `Play` still believes it is thinking, so hand
    /// the turn back by passing for it would be wrong — leave the position alone and let
    /// the person move instead.
    fn give_turn_back(&self) {
        self.started.set(Started::Nothing);
        self.state.notify_play_changed();
    }

    fn apply_ai_move(&self, report: &Report) {
        if !matches!(self.play_state(), PlayState::AiThinking) {
            return;
        }
        let (temperature, threshold, required) = {
            let cfg = self.state.config();
            (
                cfg.play.temperature,
                cfg.play.resign_threshold,
                cfg.play.resign_streak,
            )
        };
        self.state.with_session_mut(|game| {
            self.play
                .borrow_mut()
                .apply_ai_report(game, report, temperature, threshold, required)
        });
        self.started.set(Started::Nothing);
        self.sync();
    }

    // -- clocks -------------------------------------------------------------------------

    fn start_ticker(&self) {
        if let Some(id) = self.ticker.take() {
            id.remove();
        }
        self.last_tick.set(Instant::now());
        let weak = self.window.clone();
        let id = glib::timeout_add_local(Duration::from_millis(100), move || {
            if with_play(&weak, PlayController::tick).is_none() {
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.ticker.set(Some(id));
    }

    fn tick(&self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick.get()).as_secs_f32();
        self.last_tick.set(now);

        let active = match self.play_state() {
            PlayState::HumanTurn | PlayState::AiThinking => self.state.to_play(),
            _ => return,
        };
        let flagged = self
            .state
            .with_session_mut(|game| self.play.borrow_mut().tick(game, dt));
        self.state.notify_play_changed();
        if flagged {
            self.abort_thinking();
            self.state
                .with_session_mut(|game| self.play.borrow_mut().flag(game, active));
            self.sync();
        }
    }

    /// `(black, white)` clock text for the header, or `None` when untimed.
    pub fn clocks(&self) -> Option<(String, String)> {
        self.play.borrow().clocks()
    }

    // -- scoring ------------------------------------------------------------------------

    /// Asks the engine for an ownership map so the count starts from KataGo's own idea of
    /// which stones are dead. Without an engine the count runs on the board alone.
    fn begin_count(&self) {
        self.abort_thinking();
        let Some(engine) = self.state.engine() else {
            self.finish_count(None);
            return;
        };
        self.state.set_status("Counting…".to_string());
        let req = self
            .state
            .with_session_mut(|game| self.play.borrow_mut().scoring_request(game));
        let mut sub = engine.subscribe(req);
        let weak = self.window.clone();
        let handle = glib::spawn_future_local(async move {
            let mut ownership = None;
            while let Some(event) = sub.next().await {
                match event {
                    SubEvent::Done(r) => {
                        ownership = r.ownership.clone();
                        break;
                    }
                    SubEvent::Failed(e) => {
                        tracing::warn!(%e, "scoring query failed; counting without ownership");
                        break;
                    }
                    _ => {}
                }
            }
            with_play(&weak, |play| play.finish_count(ownership));
        });
        if let Some(old) = self.thinking.borrow_mut().replace(handle) {
            old.abort();
        }
    }

    fn finish_count(&self, ownership: Option<Vec<i8>>) {
        self.state.with_session_mut(|game| {
            self.play
                .borrow_mut()
                .apply_ownership(game, ownership.as_deref())
        });
        self.sync();

        let summary = self.play.borrow().summary();
        let board = self.board.borrow().clone();
        if let Some(board) = board {
            let weak = self.window.clone();
            crate::dialogs::show_score_with(&board, &self.state, &summary, move || {
                with_play(&weak, PlayController::run_analyse);
            });
        }
    }

    /// Every primary board click while a game is running comes through here, so the
    /// controller can keep the clocks and the turn order honest. Outside a game it
    /// returns `false` and the board plays the move itself, as in review mode.
    fn on_board_click(&self, p: Point) -> bool {
        match self.play_state() {
            PlayState::Idle => false,
            PlayState::HumanTurn => {
                self.human_move(p);
                true
            }
            // The engine is searching, or the game is over: the board is read-only.
            PlayState::AiThinking | PlayState::Over(_) => true,
            PlayState::Scoring => {
                self.state
                    .with_session_mut(|game| self.play.borrow_mut().toggle_dead(game, p));
                self.sync();
                true
            }
        }
    }

    fn run_analyse(&self) {
        let hook = self.analyse_hook.borrow();
        match hook.as_ref() {
            Some(f) => f(),
            None => self
                .state
                .toast("Whole-game analysis is not available from here"),
        }
    }
}

impl Drop for PlayController {
    fn drop(&mut self) {
        if let Some(task) = self.thinking.get_mut().take() {
            task.abort();
        }
        if let Some(source) = self.ticker.take() {
            source.remove();
        }
    }
}
