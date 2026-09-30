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
//! subscription and its status text, the territory overlay, and the
//! result dialog. Every action mutates `Play` and then calls [`PlayController::sync`],

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::ObjectExt;
use mirai_client::play::{Count, GameEnd, PlayAnchor, PlayError};
use mirai_core::{Color, IllegalMove, Point};
use mirai_engine::{Engine, Report, SubEvent};

use crate::app::AppState;
use crate::i18n;
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
    /// The engine current when the turn stalled, if any. Weak, not strong: the
    /// pool's entries are weak and KataGo exits with the last window using it, so
    /// a profile switch while stalled must not keep the old process alive. A dead
    /// weak counts as a different engine, and the turn retries when a new one is ready.
    stalled_engine: RefCell<Option<Weak<dyn Engine>>>,
    last_tick: Cell<Instant>,
}
impl PlayController {
    pub fn new(state: &AppState, window: &MiraiWindow) -> PlayController {
        PlayController {
            state: state.clone(),
            window: window.downgrade(),
            play: RefCell::new(mirai_client::Play::with_human_name(
                // Translators: the human player's name written into the game record.
                i18n::gettext("You"),
            )),
            thinking: RefCell::new(None),
            ticker: Cell::new(None),
            board: RefCell::new(None),
            analyse_hook: RefCell::new(None),
            started: Cell::new(Started::Nothing),
            stalled_engine: RefCell::new(None),
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

    /// Gives the controller the board it scores on so it can draw the territory overlay.
    /// Board clicks are installed once by the window, not here.
    pub fn attach_board(&self, board: &BoardView) {
        *self.board.borrow_mut() = Some(board.clone());
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
                .start(game, setup, &engine_name, &date);
            game.set_edit_history_enabled(false);
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
        self.stalled_engine.borrow_mut().take();
        self.clear_overlay();
        if had {
            self.state
                .with_session_mut(|game| game.set_edit_history_enabled(true));
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
            PlayState::Idle | PlayState::HumanTurn | PlayState::AiStalled(_) => {
                self.started.set(Started::Nothing);
            }
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
            PlayState::AiStalled(reason) => reason.clone(),
            PlayState::Scoring => {
                let Some(phrase) = self.play.borrow().result().map(i18n::result_phrase) else {
                    return;
                };
                // Translators: {result} is a sentence such as "Black wins by 7.5".
                i18n::gettext_f(
                    "{result} — click a group to mark it dead",
                    &[("result", phrase.as_str())],
                )
            }
            PlayState::Over(_) => {
                let Some(phrase) = self.play.borrow().result().map(i18n::result_phrase) else {
                    return;
                };
                phrase
            }
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
            // Refused because the view is not at the play head; the board is locked then.
            Err(PlayError::NotYourTurn) => {}
            Err(PlayError::Illegal(IllegalMove::Occupied)) => {}
            Err(PlayError::Illegal(error)) => self.state.toast(i18n::illegal_move(error)),
        }
    }

    pub fn resign(&self) {
        if !self.is_active() || matches!(self.play_state(), PlayState::Scoring | PlayState::Over(_))
        {
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
        let undone = self
            .state
            .with_session_mut(|game| self.play.borrow_mut().undo(game));
        if !undone {
            return;
        }
        self.abort_thinking();
        self.clear_overlay();
        self.started.set(Started::Nothing);
        self.state.set_status(String::new());
        self.sync();
    }

    // -- the AI's turn ------------------------------------------------------------------

    fn ai_turn(&self) {
        let Some(engine) = self.state.engine() else {
            self.fail_ai(i18n::gettext(
                "No engine is running — start one in Preferences",
            ));
            return;
        };
        if self.play.borrow().ai().is_none() {
            return;
        }
        let Some((req, anchor)) = self.state.with_session_mut(|game| {
            let mut play = self.play.borrow_mut();
            let req = play.ai_request(game)?;
            Some((req, play.request_anchor()?))
        }) else {
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
                        let visits = r.root.visits;
                        let shown = crate::util::si_visits(visits);
                        if with_play(&weak, |play| {
                            if play.play.borrow().request_anchor() == Some(anchor) {
                                play.state.set_status(
                                    // Translators: {visits} is a visit count, possibly abbreviated (1.2k).
                                    i18n::ngettext_f(
                                        "Thinking… {visits} visit",
                                        "Thinking… {visits} visits",
                                        visits as u64,
                                        &[("visits", shown.as_str())],
                                    ),
                                );
                            }
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
                        let reason = i18n::engine_error(&e);
                        with_play(&weak, |play| {
                            let current = play.play.borrow().request_anchor() == Some(anchor);
                            if !current {
                                return;
                            }
                            // Clear the engine before stalling, so the engine-changed
                            // hook cannot retry this dead engine immediately.
                            play.state.on_engine_error(e);
                            let current = play.play.borrow().request_anchor() == Some(anchor);
                            if current {
                                play.fail_ai(reason);
                            }
                        });
                        return;
                    }
                }
            }
            with_play(&weak, |play| {
                let current = play.play.borrow().request_anchor() == Some(anchor);
                if !current {
                    return;
                }
                match done {
                    Some(report) => play.apply_ai_move(anchor, &report),
                    None => {
                        play.fail_ai(i18n::gettext("The engine ended the search without a move"))
                    }
                }
            });
        });
        if let Some(old) = self.thinking.borrow_mut().replace(handle) {
            old.abort();
        }
    }

    /// The search produced nothing usable. Leave the position alone — passing for
    /// the engine would be a move it did not choose — and wait to be asked again.
    fn fail_ai(&self, reason: impl Into<String>) {
        self.abort_thinking();
        self.play.borrow_mut().ai_failed(reason);
        *self.stalled_engine.borrow_mut() = self.state.engine().as_ref().map(Arc::downgrade);
        self.started.set(Started::Nothing);
        self.sync();
    }

    /// Asks the engine for this move again. A no-op unless the turn is stalled.
    pub(crate) fn retry(&self) {
        if !matches!(self.play_state(), PlayState::AiStalled(_)) {
            return;
        }
        self.play.borrow_mut().retry_ai();
        self.state.set_status(String::new());
        self.sync();
    }

    /// A stalled turn resumes when a different engine becomes ready. Called from
    /// the window's `Change::Engine` arm, which also fires when a profile is saved
    /// or deleted without the running engine changing. The same remote engine coming
    /// back after a dropped link is `Change::Reconnected`, which calls [`Self::retry`].
    pub(crate) fn retry_if_engine_ready(&self) {
        if !matches!(self.play_state(), PlayState::AiStalled(_)) {
            return;
        }
        let Some(engine) = self.state.engine() else {
            return;
        };
        let current = Arc::downgrade(&engine);
        if self
            .stalled_engine
            .borrow()
            .as_ref()
            .is_some_and(|old| Weak::ptr_eq(old, &current))
        {
            return;
        }
        self.retry();
    }

    fn apply_ai_move(&self, anchor: PlayAnchor, report: &Report) {
        if self.play.borrow().request_anchor() != Some(anchor)
            || !matches!(self.play_state(), PlayState::AiThinking)
        {
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
        let applied = self.state.with_session_mut(|game| {
            self.play.borrow_mut().apply_ai_report_for(
                game,
                anchor,
                report,
                temperature,
                threshold,
                required,
            )
        });
        if applied {
            self.started.set(Started::Nothing);
            self.sync();
        } else {
            self.fail_ai(i18n::gettext("The engine ended the search without a move"));
        }
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

        let running = self.play.borrow().is_clock_running();
        if !running {
            return;
        }

        let flagged = self
            .state
            .with_session_mut(|game| self.play.borrow_mut().tick(game, dt));
        self.state.notify_play_changed();
        if flagged {
            self.abort_thinking();
            self.state.with_session_mut(|game| {
                let mut play = self.play.borrow_mut();
                if let Some(active) = play.side_to_move(game) {
                    play.flag(game, active);
                }
            });
            self.sync();
        }
    }

    /// `(black, white)` clock text for the header, or `None` when untimed.
    pub fn clocks(&self) -> Option<(String, String)> {
        self.play.borrow().clocks()
    }

    /// The side actually playing, not the colour at the viewed cursor.
    pub(crate) fn side_to_move(&self) -> Option<Color> {
        self.state
            .with_session_mut(|game| self.play.borrow().side_to_move(game))
    }

    /// Whether a click on the board would be the human's move: their turn, viewed at
    /// the play head. Reviewing an earlier position is read-only.
    pub(crate) fn human_can_move(&self) -> bool {
        self.state
            .with_session_mut(|game| self.play.borrow().accepts_human_move(game))
    }

    // -- scoring ------------------------------------------------------------------------

    /// Asks the engine for an ownership map so the count starts from KataGo's own idea of
    /// which stones are dead. Without an engine the count runs on the board alone.
    fn begin_count(&self) {
        self.abort_thinking();
        let Some(engine) = self.state.engine() else {
            let anchor = self
                .state
                .with_session_mut(|game| self.play.borrow_mut().start_count(game));
            if let Some(anchor) = anchor {
                self.finish_count(anchor, None);
            }
            return;
        };
        self.state.set_status(i18n::gettext("Counting…"));
        let Some((req, anchor)) = self.state.with_session_mut(|game| {
            let mut play = self.play.borrow_mut();
            let req = play.scoring_request(game)?;
            Some((req, play.score_anchor()?))
        }) else {
            return;
        };
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
            with_play(&weak, |play| play.finish_count(anchor, ownership));
        });
        if let Some(old) = self.thinking.borrow_mut().replace(handle) {
            old.abort();
        }
    }

    fn finish_count(&self, anchor: PlayAnchor, ownership: Option<Vec<i8>>) {
        let applied = self.state.with_session_mut(|game| {
            self.play
                .borrow_mut()
                .apply_ownership_for(game, anchor, ownership.as_deref())
        });
        if !applied {
            return;
        }
        self.sync();

        let summary = score_summary(&self.play.borrow());
        let board = self.board.borrow().clone();
        if let Some(board) = board {
            let weak = self.window.clone();
            crate::dialogs::show_score_with(
                &board,
                &self.state,
                &summary,
                {
                    let weak = weak.clone();
                    move || {
                        with_play(&weak, |play| {
                            play.stop();
                            play.run_analyse();
                        });
                    }
                },
                move || {
                    with_play(&weak, PlayController::stop);
                },
            );
        }
    }

    /// Primary-board clicks in an active game come through here so clocks and
    /// turn order stay honest. Scoring toggles dead stones; every other state
    /// leaves the board read-only. The window calls this only while a game runs.
    pub(crate) fn on_board_click(&self, p: Point) {
        match self.play_state() {
            PlayState::HumanTurn => self.human_move(p),
            // Searching, stalled, or over: the board is read-only. Undo and Retry
            // are the way out of a stall, not a click.
            PlayState::Idle
            | PlayState::AiThinking
            | PlayState::AiStalled(_)
            | PlayState::Over(_) => {}
            PlayState::Scoring => {
                self.state
                    .with_session_mut(|game| self.play.borrow_mut().toggle_dead(game, p));
                self.sync();
            }
        }
    }

    fn run_analyse(&self) {
        let hook = self.analyse_hook.borrow();
        match hook.as_ref() {
            Some(f) => f(),
            None => self.state.toast(i18n::gettext(
                "Whole-game analysis is not available from here",
            )),
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

fn end_sentence(end: GameEnd) -> String {
    match end {
        GameEnd::BothPassed => i18n::gettext("Both players passed."),
        GameEnd::Resigned(Color::Black) => i18n::gettext("Black resigned."),
        GameEnd::Resigned(Color::White) => i18n::gettext("White resigned."),
        GameEnd::LostOnTime(Color::Black) => i18n::gettext("Black lost on time."),
        GameEnd::LostOnTime(Color::White) => i18n::gettext("White lost on time."),
    }
}

fn count_line(end: Option<GameEnd>, count: &Count) -> String {
    let black = format!("{:.1}", count.black);
    let white = format!("{:.1}", count.white);
    let forced = matches!(end, Some(GameEnd::Resigned(_) | GameEnd::LostOnTime(_)));
    if forced {
        let result = i18n::result_phrase(&count.result);
        if count.approximate {
            // Translators: {result} is a sentence such as "Black wins by 7.5"; {black} and {white} are point totals.
            i18n::gettext_f(
                "Count on the board: {result} ({black} — {white}, estimated)",
                &[
                    ("result", result.as_str()),
                    ("black", black.as_str()),
                    ("white", white.as_str()),
                ],
            )
        } else {
            // Translators: {result} is a sentence such as "Black wins by 7.5"; {black} and {white} are point totals.
            i18n::gettext_f(
                "Count on the board: {result} ({black} — {white})",
                &[
                    ("result", result.as_str()),
                    ("black", black.as_str()),
                    ("white", white.as_str()),
                ],
            )
        }
    } else if count.approximate {
        i18n::gettext_f(
            "Black {black} — White {white} (estimated)",
            &[("black", black.as_str()), ("white", white.as_str())],
        )
    } else {
        i18n::gettext_f(
            "Black {black} — White {white}",
            &[("black", black.as_str()), ("white", white.as_str())],
        )
    }
}

/// Reason, result and board count, as the score dialog shows them. Empty before a count.
fn score_summary(play: &mirai_client::Play) -> String {
    let Some(count) = play.count() else {
        return String::new();
    };
    let mut parts = Vec::with_capacity(3);
    if let Some(end) = play.end() {
        parts.push(end_sentence(end));
    }
    if let Some(result) = play.result() {
        parts.push(i18n::result_phrase(result));
    }
    parts.push(count_line(play.end(), count));
    parts.join("\n\n")
}
