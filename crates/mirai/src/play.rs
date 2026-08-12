// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Play mode. Step 11.
//!
//! A game against the engine is a thin layer over the same [`AppState`] a review session
//! uses: the moves land in the ordinary game tree, so everything the analysis UI already
//! draws keeps working while a game is running. This module owns only the parts a review
//! does not have — whose turn it is, the clocks, the AI's subscription, and the
//! end-of-game scoring.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk::glib;
use gtk::prelude::ObjectExt;
use mirai_core::{
    Color, DeadSet, GameInfo, GameTree, NodeId, Point, SplitMix64, TimeControl, fixed_handicap,
    score, think_budget,
};
use mirai_engine::{MoveInfo, Report, SubEvent, Want};

use crate::app::AppState;
use crate::widgets::BoardView;
use crate::window_shell::MiraiWindow;

fn with_play<R>(
    weak: &glib::WeakRef<MiraiWindow>,
    f: impl FnOnce(&PlayController) -> R,
) -> Option<R> {
    let window = weak.upgrade()?;
    window.with_ui(|ui| f(&ui.play))
}

/// Default KataGo human-SL profile offered in the UI.
///
/// One constant because the New Game dialog and Preferences both prefill it, and they used
/// to disagree — a user who set strength in one place saw a different rank in the other.
pub const DEFAULT_HUMAN_PROFILE: &str = "rank_5k";

/// How strong the AI plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Strength {
    Visits(u32),
    TimeMs(u32),
    /// Only offered when `EngineDesc::has_human_model` is true.
    Human {
        profile: String,
    },
}

impl Strength {
    pub fn visits(&self) -> Option<u32> {
        match self {
            Strength::Visits(v) => Some(*v),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayState {
    /// No game in progress; the board is a review session.
    Idle,
    HumanTurn,
    AiThinking,
    Scoring,
    Over(String),
}

/// The parameters a new game starts from.
#[derive(Clone, Debug)]
pub struct GameSetup {
    pub size: mirai_core::Size,
    pub rules: mirai_core::RuleSet,
    pub komi: f32,
    pub handicap: u8,
    /// `None` means the human plays both colours (self-play review).
    pub human: Option<Color>,
    pub tc: TimeControl,
    pub strength: Strength,
}

/// Both clocks at one node, so undo can put them back.
#[derive(Clone, Copy, Debug)]
struct ClockSnap {
    remaining: [f32; 2],
    byo_left: [u8; 2],
    in_byo: [bool; 2],
}

pub struct PlaySession {
    pub human: Option<Color>,
    pub tc: TimeControl,
    pub remaining: [f32; 2],
    pub byo_left: [u8; 2],
    pub strength: Strength,
    pub resign_streak: u8,
    pub state: PlayState,
    pub dead: DeadSet,

    /// Per colour: the clock is counting a byo-yomi period rather than main time.
    in_byo: [bool; 2],
    clocks: HashMap<NodeId, ClockSnap>,
    /// Why the game ended, in prose.
    reason: String,
    /// A result the rules did not produce — resignation or a lost flag.
    forced_result: Option<String>,
    /// The text the score dialog and the status line show.
    summary: String,
}

impl PlaySession {
    /// The colour the engine plays, if any.
    #[inline]
    pub fn ai(&self) -> Option<Color> {
        self.human.map(Color::other)
    }

    fn snap(&self) -> ClockSnap {
        ClockSnap {
            remaining: self.remaining,
            byo_left: self.byo_left,
            in_byo: self.in_byo,
        }
    }

    fn restore(&mut self, s: ClockSnap) {
        self.remaining = s.remaining;
        self.byo_left = s.byo_left;
        self.in_byo = s.in_byo;
    }
}

/// Owns the play session and drives the AI's turns.
pub struct PlayController {
    state: AppState,
    window: glib::WeakRef<MiraiWindow>,
    session: RefCell<Option<PlaySession>>,
    thinking: RefCell<Option<glib::JoinHandle<()>>>,
    ticker: Cell<Option<glib::SourceId>>,
    board: RefCell<Option<BoardView>>,
    analyse_hook: RefCell<Option<Box<dyn Fn()>>>,
    rng: RefCell<SplitMix64>,
    last_tick: Cell<Instant>,
}

impl PlayController {
    pub fn new(state: &AppState, window: &MiraiWindow) -> PlayController {
        PlayController {
            state: state.clone(),
            window: window.downgrade(),
            session: RefCell::new(None),
            thinking: RefCell::new(None),
            ticker: Cell::new(None),
            board: RefCell::new(None),
            analyse_hook: RefCell::new(None),
            rng: RefCell::new(SplitMix64::new(seed_from_clock())),
            last_tick: Cell::new(Instant::now()),
        }
    }

    pub fn is_active(&self) -> bool {
        self.session.borrow().is_some()
    }

    /// Whether the current session is a person playing against the engine.
    pub fn is_human_vs_engine(&self) -> bool {
        self.session
            .borrow()
            .as_ref()
            .is_some_and(|session| session.human.is_some())
    }

    pub fn play_state(&self) -> PlayState {
        self.session
            .borrow()
            .as_ref()
            .map(|s| s.state.clone())
            .unwrap_or(PlayState::Idle)
    }

    /// Gives the controller the board it scores on: it installs a click hook that toggles
    /// dead groups while the game is being counted, and draws the territory overlay.
    pub fn attach_board(&self, board: &BoardView) {
        *self.board.borrow_mut() = Some(board.clone());
        let weak = self.window.clone();
        board.set_click_hook(Some(Box::new(move |p: Point| {
            with_play(&weak, |play| play.on_board_click(p)).unwrap_or(false)
        })));
    }

    /// What the "Analyse game" button in the result dialog runs. The window wires this to
    /// its `BatchAnalysis` so the banner and the blunder strip come along.
    pub fn set_analyse_hook(&self, f: impl Fn() + 'static) {
        *self.analyse_hook.borrow_mut() = Some(Box::new(f));
    }

    // -- lifecycle ----------------------------------------------------------------------

    pub fn start(&self, setup: GameSetup) {
        self.stop();

        let mut info = GameInfo::new(setup.size, setup.rules);
        info.komi = setup.komi;
        info.date = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%Y-%m-%d").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let engine_name = self
            .state
            .engine_desc()
            .map(|d| d.name)
            .unwrap_or_else(|| "KataGo".to_string());
        for c in [Color::Black, Color::White] {
            info.players[c.index()].name = match setup.human {
                Some(h) if h == c => "You".to_string(),
                Some(_) => engine_name.clone(),
                None => "You".to_string(),
            };
        }

        let stones = fixed_handicap(setup.size, setup.handicap);
        info.handicap = if stones.len() >= 2 { setup.handicap } else { 0 };

        let mut tree = GameTree::new(info);
        let root = tree.root();
        if !stones.is_empty() {
            for p in stones {
                tree.set_setup_stone(root, p, Some(Color::Black));
            }
        }
        self.state.set_tree(tree, None);

        let in_byo = setup.tc.main_s == 0 && setup.tc.byo_periods > 0;
        let start_time = if in_byo {
            setup.tc.byo_period_s as f32
        } else {
            setup.tc.main_s as f32
        };
        let byo_left = if in_byo {
            setup.tc.byo_periods.saturating_sub(1)
        } else {
            setup.tc.byo_periods
        };
        let session = PlaySession {
            human: setup.human,
            tc: setup.tc,
            remaining: [start_time; 2],
            byo_left: [byo_left; 2],
            strength: setup.strength,
            resign_streak: 0,
            state: PlayState::HumanTurn,
            dead: DeadSet::empty(setup.size),
            in_byo: [in_byo; 2],
            clocks: HashMap::new(),
            reason: String::new(),
            forced_result: None,
            summary: String::new(),
        };
        *self.session.borrow_mut() = Some(session);
        self.state.notify_play_changed();
        *self.rng.borrow_mut() = SplitMix64::new(seed_from_clock());
        self.clear_overlay();
        self.snapshot_clock(self.state.cursor());
        self.start_ticker();
        self.advance();
    }

    pub fn stop(&self) {
        self.abort_thinking();
        if let Some(id) = self.ticker.take() {
            id.remove();
        }
        let had = self.session.borrow_mut().take().is_some();
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

    /// Hands the turn to whoever is next, starting the AI when it is its move.
    fn advance(&self) {
        let Some(ai) = self.session.borrow().as_ref().and_then(PlaySession::ai) else {
            self.set_state(PlayState::HumanTurn);
            return;
        };
        if self.state.to_play() == ai {
            self.set_state(PlayState::AiThinking);
            self.ai_turn(ai);
        } else {
            self.set_state(PlayState::HumanTurn);
        }
    }

    fn set_state(&self, new: PlayState) {
        {
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            if s.state == new {
                return;
            }
            s.state = new;
        }
        self.state.notify_play_changed();
    }

    // -- moves --------------------------------------------------------------------------

    /// Called after the human plays; starts the AI's reply when it is the AI's turn.
    ///
    /// Tolerates both call styles: if the caller already pushed the move into the tree
    /// (the cursor sits on it) it is not played twice.
    pub fn on_human_move(&self, mv: (Color, Point)) {
        if !matches!(self.play_state(), PlayState::HumanTurn) {
            return;
        }
        let cursor = self.state.cursor();
        let already = self.state.tree().node(cursor).mv == Some(mv);
        if !already {
            if self.state.to_play() != mv.0 {
                return;
            }
            if let Err(error) = self.state.play_move(mv.0, mv.1) {
                self.state.toast_illegal_move(error);
                return;
            }
        }
        self.after_move(mv.0);
    }

    pub fn pass(&self) {
        if !matches!(self.play_state(), PlayState::HumanTurn) {
            return;
        }
        let c = self.state.to_play();
        self.on_human_move((c, Point::PASS));
    }

    pub fn resign(&self) {
        if !self.is_active() || matches!(self.play_state(), PlayState::Scoring) {
            return;
        }
        let loser = self
            .session
            .borrow()
            .as_ref()
            .and_then(|s| s.human)
            .unwrap_or_else(|| self.state.to_play());
        self.end_game(
            Some(format!("{}+R", loser.other().katago())),
            format!("{} resigned.", side_name(loser)),
        );
    }

    /// Retracts the AI's move and the human's, restores the clocks and cancels any
    /// in-flight search.
    pub fn undo(&self) {
        if !self.is_active() {
            return;
        }
        self.abort_thinking();
        self.clear_overlay();

        let human = self.session.borrow().as_ref().and_then(|s| s.human);
        let mut removed = 0;
        while removed < 2 {
            let cursor = self.state.cursor();
            let Some(parent) = self.state.tree().parent(cursor) else {
                break;
            };
            // Move the cursor off the node first: deleting the node the widgets are
            // looking at would leave them holding a stale id.
            self.state.set_cursor(parent);
            self.state.with_tree_mut(|t| t.delete_branch(cursor));
            removed += 1;
            match human {
                Some(h) if self.state.to_play() != h => continue,
                _ => break,
            }
        }
        if removed == 0 {
            return;
        }

        let cursor = self.state.cursor();
        let size = self.state.tree().info.size;
        {
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            if let Some(&snap) = s.clocks.get(&cursor) {
                s.restore(snap);
            }
            // Ids are handed out in creation order, so everything newer than the node we
            // rewound to belongs to the branch that just went away.
            let keep = cursor.0;
            s.clocks.retain(|id, _| id.0 <= keep);
            s.resign_streak = 0;
            s.dead = DeadSet::empty(size);
            s.forced_result = None;
            s.reason.clear();
            s.summary.clear();
            s.state = PlayState::HumanTurn;
        }
        self.state.with_tree_mut(|t| t.info.result.clear());
        self.state.set_status(String::new());
        self.state.notify_play_changed();
        self.advance();
    }

    /// Clock bookkeeping and end-of-game detection after `color` completed a move.
    fn after_move(&self, color: Color) {
        {
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            let i = color.index();
            if s.in_byo[i] {
                s.remaining[i] = s.tc.byo_period_s as f32;
            } else {
                s.remaining[i] += s.tc.increment_s as f32;
            }
        }
        self.snapshot_clock(self.state.cursor());
        if self.two_passes() {
            self.end_game(None, "Both players passed.".to_string());
            return;
        }
        self.advance();
    }

    fn snapshot_clock(&self, id: NodeId) {
        let mut guard = self.session.borrow_mut();
        let Some(s) = guard.as_mut() else { return };
        let snap = s.snap();
        s.clocks.insert(id, snap);
    }

    fn two_passes(&self) -> bool {
        let tree = self.state.tree();
        let cursor = self.state.cursor();
        let is_pass = |id: NodeId| matches!(tree.node(id).mv, Some((_, p)) if p.is_pass());
        is_pass(cursor) && tree.parent(cursor).is_some_and(is_pass)
    }

    // -- the AI's turn ------------------------------------------------------------------

    fn ai_turn(&self, ai: Color) {
        let Some(engine) = self.state.engine() else {
            self.state
                .toast("No engine is running — start one in Preferences");
            self.set_state(PlayState::HumanTurn);
            return;
        };

        let (strength, budget_ms) = {
            let guard = self.session.borrow();
            let Some(s) = guard.as_ref() else { return };
            let seconds = think_budget(&s.tc, s.remaining[ai.index()], s.byo_left[ai.index()]);
            let ms = match &s.strength {
                Strength::TimeMs(t) => Some(*t),
                _ => seconds.map(|sec| (sec * 1000.0).max(100.0) as u32),
            };
            (s.strength.clone(), ms)
        };

        let mut req = self
            .state
            .request_for_cursor(Some(strength.visits().unwrap_or(u32::MAX)), Want::OWNERSHIP);
        req.max_time_ms = budget_ms;
        req.priority = 8;
        req.report_every_ms = Some(200);
        req.overrides = vec![("wideRootNoise".to_string(), "0.0".to_string())];
        if let Strength::Human { profile } = &strength {
            req.overrides
                .push(("humanSLProfile".to_string(), profile.clone()));
        }

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
                            play.set_state(PlayState::HumanTurn);
                        });
                        return;
                    }
                }
            }
            with_play(&weak, |play| {
                play.state.set_status(String::new());
                match done {
                    Some(report) => play.apply_ai_move(ai, &report),
                    None => play.set_state(PlayState::HumanTurn),
                }
            });
        });
        if let Some(old) = self.thinking.borrow_mut().replace(handle) {
            old.abort();
        }
    }

    fn apply_ai_move(&self, ai: Color, report: &Report) {
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

        let position = self.state.position();
        let winrate = report.root.winrate_for(ai);
        let resign = {
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            let (streak, resign) = resign_check(
                winrate,
                threshold,
                s.resign_streak,
                required,
                position.move_number,
                position.board.size.points(),
            );
            s.resign_streak = streak;
            resign
        };
        if resign {
            self.end_game(
                Some(format!("{}+R", ai.other().katago())),
                format!("{} resigned.", side_name(ai)),
            );
            return;
        }

        let choice = {
            let mut rng = self.rng.borrow_mut();
            select_move_index(&report.moves, temperature, &mut rng)
        };
        let mv = match choice {
            Some(i) => report.moves[i].mv,
            // No legal candidate at all — the only thing left is to pass.
            None => Point::PASS,
        };
        if !mv.is_pass()
            && let Err(e) = self.state.play_move(ai, mv)
        {
            tracing::warn!(%e, "the engine suggested an illegal move; passing instead");
            let _ = self.state.play_move(ai, Point::PASS);
            self.after_move(ai);
            return;
        }
        if mv.is_pass() && self.state.play_move(ai, Point::PASS).is_err() {
            return;
        }
        self.after_move(ai);
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

        let mut flagged = None;
        {
            let active = match self.play_state() {
                PlayState::HumanTurn | PlayState::AiThinking => self.state.to_play(),
                _ => return,
            };
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            if s.tc.is_unlimited() {
                return;
            }
            let i = active.index();
            let tc = s.tc;
            if tick_clock(
                &tc,
                &mut s.remaining[i],
                &mut s.byo_left[i],
                &mut s.in_byo[i],
                dt,
            ) == Tick::Flag
            {
                flagged = Some(active);
            }
        }
        self.state.notify_play_changed();
        if let Some(loser) = flagged {
            self.end_game(
                Some(format!("{}+T", loser.other().katago())),
                format!("{} lost on time.", side_name(loser)),
            );
        }
    }

    /// `(black, white)` clock text for the header, or `None` when untimed.
    pub fn clocks(&self) -> Option<(String, String)> {
        let s = self.session.borrow();
        let s = s.as_ref()?;
        if s.tc.is_unlimited() {
            return None;
        }
        let text = |i: usize| {
            let t = crate::util::clock_text(s.remaining[i]);
            if s.in_byo[i] {
                format!("{t} ({})", s.byo_left[i] as u16 + 1)
            } else {
                t
            }
        };
        Some((text(0), text(1)))
    }

    // -- scoring ------------------------------------------------------------------------

    /// Ends the game and counts it. `forced` is the result the rules cannot produce —
    /// a resignation or a lost flag; `None` means the count decides.
    fn end_game(&self, forced: Option<String>, reason: String) {
        self.abort_thinking();
        {
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            s.state = PlayState::Scoring;
            s.resign_streak = 0;
            s.reason = reason;
            s.forced_result = forced.clone();
        }
        if let Some(r) = &forced {
            let r = r.clone();
            self.state.with_tree_mut(|t| t.info.result = r);
        }
        self.state.set_status("Counting…".to_string());
        self.state.notify_play_changed();

        let Some(engine) = self.state.engine() else {
            self.finish_scoring(None);
            return;
        };
        let mut req = self.state.request_for_cursor(Some(400), Want::OWNERSHIP);
        req.max_time_ms = None;
        req.report_every_ms = None;
        req.priority = 8;
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
            with_play(&weak, |play| play.finish_scoring(ownership));
        });
        if let Some(old) = self.thinking.borrow_mut().replace(handle) {
            old.abort();
        }
    }

    fn finish_scoring(&self, ownership: Option<Vec<i8>>) {
        let position = self.state.position();
        let dead = match &ownership {
            Some(raw) => {
                let f: Vec<f32> = raw.iter().map(|&v| mirai_proto::types::dq_own(v)).collect();
                DeadSet::from_ownership(&position.board, &f, 0.4)
            }
            None => DeadSet::empty(position.board.size),
        };
        let forced = {
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            s.dead = dead;
            s.forced_result.clone()
        };
        // A resignation or a lost flag settles the result, so the count is only an
        // estimate and there is nothing left to mark. A game that ended in two passes
        // stays in `Scoring`, where clicking a group still changes the count.
        if let Some(result) = forced {
            self.rescore();
            self.set_state(PlayState::Over(result));
        } else {
            self.rescore();
        }

        let summary = self
            .session
            .borrow()
            .as_ref()
            .map(|s| s.summary.clone())
            .unwrap_or_default();
        let board = self.board.borrow().clone();
        if let Some(board) = board {
            let weak = self.window.clone();
            crate::dialogs::show_score_with(&board, &self.state, &summary, move || {
                with_play(&weak, PlayController::run_analyse);
            });
        }
    }

    /// Recounts the current position from the session's dead set. Purely local: no query,
    /// so toggling a group is instant.
    fn rescore(&self) {
        let position = self.state.position();
        let (rules, komi, handicap) = {
            let t = self.state.tree();
            (t.info.rules, t.info.komi, t.info.handicap)
        };
        let (result, territory, dead) = {
            let mut guard = self.session.borrow_mut();
            let Some(s) = guard.as_mut() else { return };
            let counted = score(&position.board, &rules.rules(), komi, handicap, &s.dead);
            let counted_str = counted.result_string();
            let summary = match &s.forced_result {
                Some(forced) => format!(
                    "{}\n\n{}\n\nCount without the resignation: {} ({:.1} — {:.1}{})",
                    s.reason,
                    result_phrase(forced),
                    result_phrase(&counted_str),
                    counted.black,
                    counted.white,
                    if counted.approximate {
                        ", estimated"
                    } else {
                        ""
                    },
                ),
                None => format!(
                    "{}\n\n{}\n\nBlack {:.1} — White {:.1}{}",
                    s.reason,
                    result_phrase(&counted_str),
                    counted.black,
                    counted.white,
                    if counted.approximate {
                        " (estimated)"
                    } else {
                        ""
                    },
                ),
            };
            s.summary = summary.clone();
            let result = s.forced_result.clone().unwrap_or(counted_str);
            (result, counted.territory, s.dead.clone())
        };

        self.state.with_tree_mut(|t| t.info.result = result.clone());
        if let Some(b) = self.board.borrow().as_ref() {
            b.set_score_overlay(Some(dead), Some(territory));
        }
        let status = if self.play_state() == PlayState::Scoring {
            format!("{} — click a group to mark it dead", result_phrase(&result))
        } else {
            result_phrase(&result)
        };
        self.state.set_status(status);
        self.state.notify_play_changed();
    }

    /// Every primary board click while a game is running comes through here, so the
    /// controller can keep the clocks and the turn order honest. Outside a game it
    /// returns `false` and the board plays the move itself, as in review mode.
    fn on_board_click(&self, p: Point) -> bool {
        match self.play_state() {
            PlayState::Idle => false,
            PlayState::HumanTurn => {
                let color = self.state.to_play();
                self.on_human_move((color, p));
                true
            }
            // The engine is searching, or the game is over: the board is read-only.
            PlayState::AiThinking | PlayState::Over(_) => true,
            PlayState::Scoring => {
                if p.is_pass() {
                    return true;
                }
                let position = self.state.position();
                {
                    let mut guard = self.session.borrow_mut();
                    let Some(s) = guard.as_mut() else {
                        return false;
                    };
                    if position.board.at(p).is_none() {
                        return true;
                    }
                    s.dead.toggle_chain(&position.board, p);
                }
                self.rescore();
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

// -- pure logic, unit-tested ------------------------------------------------------------

/// One 100 ms clock step's outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tick {
    Running,
    /// The clock rolled over into (another) byo-yomi period.
    PeriodConsumed,
    /// Out of time.
    Flag,
}

/// Advances one player's clock by `dt` seconds.
///
/// `byo_left` counts the periods that have *not* been entered yet, so main time running
/// out consumes one and starts counting it; the flag falls when a period expires with
/// none left.
pub(crate) fn tick_clock(
    tc: &TimeControl,
    remaining: &mut f32,
    byo_left: &mut u8,
    in_byo: &mut bool,
    dt: f32,
) -> Tick {
    *remaining -= dt;
    if *remaining > 0.0 {
        return Tick::Running;
    }
    if *byo_left > 0 {
        *byo_left -= 1;
        *in_byo = true;
        *remaining = tc.byo_period_s as f32;
        Tick::PeriodConsumed
    } else {
        *remaining = 0.0;
        Tick::Flag
    }
}

/// Updates the AI's resignation streak and says whether it should resign now.
///
/// Both conditions must hold: `required` consecutive turns under `threshold`, and enough
/// stones on the board that a hopeless-looking opening cannot end the game.
pub(crate) fn resign_check(
    winrate_for_ai: f32,
    threshold: f32,
    streak_before: u8,
    required: u8,
    move_number: u16,
    board_points: usize,
) -> (u8, bool) {
    if winrate_for_ai >= threshold {
        return (0, false);
    }
    let streak = streak_before.saturating_add(1);
    let past_opening = move_number as usize > board_points / 4;
    (streak, required > 0 && streak >= required && past_opening)
}

/// Picks the AI's move out of a final report.
///
/// At temperature 0 that is KataGo's own choice (`order == 0`). Above 0 the candidates are
/// sampled proportionally to `play_value^(1/t)`, so a small temperature still almost
/// always plays the best move while a large one flattens towards uniform.
pub(crate) fn select_move_index(
    moves: &[MoveInfo],
    temperature: f32,
    rng: &mut SplitMix64,
) -> Option<usize> {
    if moves.is_empty() {
        return None;
    }
    let best = moves.iter().position(|m| m.order == 0).unwrap_or(0);
    if temperature <= 0.0 {
        return Some(best);
    }
    let exponent = 1.0 / temperature as f64;
    let weights: Vec<f64> = moves
        .iter()
        .map(|m| (m.play_value as f64).powf(exponent))
        .collect();
    let total: f64 = weights.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return Some(best);
    }
    let mut r = rng.next_f64() * total;
    for (i, w) in weights.iter().enumerate() {
        r -= w;
        if r <= 0.0 {
            return Some(i);
        }
    }
    Some(best)
}

fn seed_from_clock() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}

fn side_name(c: Color) -> &'static str {
    match c {
        Color::Black => "Black",
        Color::White => "White",
    }
}

/// `"B+R"` → `"Black wins by resignation"`.
fn result_phrase(result: &str) -> String {
    if result.is_empty() {
        return "No result".to_string();
    }
    if result == "0" || result.eq_ignore_ascii_case("draw") {
        return "Jigo — a draw".to_string();
    }
    let (winner, margin) = match result.split_once('+') {
        Some((w, m)) => (w, m),
        None => return result.to_string(),
    };
    let who = match Color::from_letter(winner) {
        Some(c) => side_name(c),
        None => return result.to_string(),
    };
    match margin {
        "R" | "r" => format!("{who} wins by resignation"),
        "T" | "t" => format!("{who} wins on time"),
        "F" | "f" => format!("{who} wins by forfeit"),
        m => format!("{who} wins by {m}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_core::Point;

    fn mv(order: u8, play_value: u32) -> MoveInfo {
        MoveInfo {
            mv: Point(order as u16),
            visits: 100,
            edge_visits: 100,
            winrate: 32768,
            prior: 1000,
            lcb: 0,
            utility: 0,
            utility_lcb: 0,
            score_lead: 0,
            score_selfplay: 0,
            score_stdev: 0,
            order,
            play_value,
            pv: Vec::new(),
            pv_visits: Vec::new(),
        }
    }

    #[test]
    fn temperature_zero_always_plays_the_engines_choice() {
        // `order == 0` is deliberately not first in the slice, so a lazy `moves[0]` fails.
        let moves = vec![mv(1, 900), mv(0, 1000), mv(2, 5)];
        let mut rng = SplitMix64::new(1);
        for _ in 0..100 {
            assert_eq!(select_move_index(&moves, 0.0, &mut rng), Some(1));
        }
        assert_eq!(select_move_index(&moves, -1.0, &mut rng), Some(1));
    }

    #[test]
    fn high_temperature_spreads_the_choice_over_the_candidates() {
        let moves = vec![mv(0, 1000), mv(1, 400), mv(2, 20)];
        let mut rng = SplitMix64::new(0x1234_5678);
        let mut counts = [0usize; 3];
        for _ in 0..1000 {
            let i = select_move_index(&moves, 4.0, &mut rng).expect("a candidate");
            counts[i] += 1;
        }
        assert_eq!(counts.iter().sum::<usize>(), 1000);
        // Weights at t = 4 are 1000^0.25 : 400^0.25 : 20^0.25 = 5.62 : 4.47 : 2.11,
        // i.e. roughly 46% / 37% / 17%.
        assert!(
            counts[0] > counts[1],
            "best move should still lead: {counts:?}"
        );
        assert!(
            counts[1] > counts[2],
            "ordering should follow the weights: {counts:?}"
        );
        assert!(
            counts[1] >= 250 && counts[1] <= 500,
            "second best: {counts:?}"
        );
        assert!(
            counts[2] >= 80,
            "the weakest move must still show up: {counts:?}"
        );
        // The distribution is deterministic for this seed.
        assert_eq!(counts, [472, 366, 162]);
    }

    #[test]
    fn a_lone_bad_report_does_not_resign() {
        // One turn below the threshold, three required.
        let (streak, resign) = resign_check(0.01, 0.05, 0, 3, 200, 361);
        assert_eq!(streak, 1);
        assert!(!resign);
    }

    #[test]
    fn three_hopeless_turns_past_the_opening_resign() {
        let mut streak = 0;
        let mut resign = false;
        for _ in 0..3 {
            (streak, resign) = resign_check(0.01, 0.05, streak, 3, 200, 361);
        }
        assert_eq!(streak, 3);
        assert!(resign);
    }

    #[test]
    fn the_move_number_gate_holds_the_resignation_back() {
        // Same three hopeless turns, but only move 20 on a 19x19 board (gate is 90).
        let mut streak = 0;
        let mut resign = false;
        for _ in 0..3 {
            (streak, resign) = resign_check(0.01, 0.05, streak, 3, 20, 361);
        }
        assert_eq!(streak, 3);
        assert!(!resign, "the opening gate must suppress the resignation");
    }

    #[test]
    fn one_good_report_clears_the_streak() {
        let (streak, _) = resign_check(0.01, 0.05, 2, 3, 200, 361);
        assert_eq!(streak, 3);
        let (streak, resign) = resign_check(0.40, 0.05, 2, 3, 200, 361);
        assert_eq!(streak, 0);
        assert!(!resign);
    }

    #[test]
    fn main_time_running_out_consumes_a_period() {
        let tc = TimeControl {
            main_s: 10,
            byo_periods: 3,
            byo_period_s: 30,
            increment_s: 0,
        };
        let (mut remaining, mut byo_left, mut in_byo) = (10.0f32, 3u8, false);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 5.0),
            Tick::Running
        );
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 5.0),
            Tick::PeriodConsumed
        );
        assert!(in_byo);
        assert_eq!(byo_left, 2);
        assert_eq!(remaining, 30.0);
    }

    #[test]
    fn the_last_period_expiring_loses_on_time() {
        let tc = TimeControl {
            main_s: 0,
            byo_periods: 3,
            byo_period_s: 30,
            increment_s: 0,
        };
        // Same initial state `start` builds for a byo-yomi-only control.
        let (mut remaining, mut byo_left, mut in_byo) = (30.0f32, 2u8, true);
        // Three periods are available in total; the first is already being counted.
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 30.0),
            Tick::PeriodConsumed
        );
        assert_eq!(byo_left, 1);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 30.0),
            Tick::PeriodConsumed
        );
        assert_eq!(byo_left, 0);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 30.0),
            Tick::Flag
        );
        assert_eq!(remaining, 0.0);
    }

    #[test]
    fn absolute_time_without_periods_flags_immediately() {
        let tc = TimeControl {
            main_s: 60,
            byo_periods: 0,
            byo_period_s: 0,
            increment_s: 0,
        };
        let (mut remaining, mut byo_left, mut in_byo) = (0.5f32, 0u8, false);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 1.0),
            Tick::Flag
        );
        assert!(!in_byo);
    }

    #[test]
    fn result_phrases_read_like_english() {
        assert_eq!(result_phrase("B+R"), "Black wins by resignation");
        assert_eq!(result_phrase("W+T"), "White wins on time");
        assert_eq!(result_phrase("B+7.5"), "Black wins by 7.5");
        assert_eq!(result_phrase("0"), "Jigo — a draw");
    }
}
