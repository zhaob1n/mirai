// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Playing a game against the engine, without a UI.
//!
//! The GTK controller and the HarmonyOS view model both drive this. Clocks, resignation,
//! move sampling and scoring live here; frontends own timers, dialogs and drawing.
//!
//! The session remembers a play head — the node the game is at — separately from the view
//! cursor. Navigation may move the cursor; a move, an undo and a clock step still apply at
//! the head. An engine report is anchored to that node's epoch and id when the request is
//! built, and is ignored unless that anchor is still the head.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use mirai_core::{
    Color, DeadSet, GameInfo, GameTree, IllegalMove, NodeId, Point, SplitMix64, TimeControl,
    clock_text, fixed_handicap, score, think_budget,
};
use mirai_engine::{AnalyzeReq, MoveInfo, Report, Want};

use crate::{GameSession, dead_from_ownership};

pub const DEFAULT_HUMAN_PROFILE: &str = "rank_5k";

/// Visits for a human-like AI when no clock bounds the search. Without a bound the
/// request asks for `u32::MAX` visits and the engine's move never comes. The figure is
/// KataGo's `gtp_human5k_example.cfg`; there it only backs a move sampled from the human
/// policy, while mirai still picks from the search, so play is stronger than the rank.
const HUMAN_UNTIMED_VISITS: u32 = 40;

/// How strong the AI plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Strength {
    Visits(u32),
    TimeMs(u32),
    Human { profile: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayState {
    Idle,
    HumanTurn,
    AiThinking,
    /// The engine's turn could not be played. The string is the reason a frontend
    /// shows. Clocks do not run. Undo and resign still apply; [`Play::retry_ai`]
    /// goes back to [`AiThinking`](PlayState::AiThinking).
    AiStalled(String),
    Scoring,
    Over(String),
}

#[derive(Clone, Debug)]
pub struct GameSetup {
    pub size: mirai_core::Size,
    pub rules: mirai_core::RuleSet,
    pub komi: f32,
    pub handicap: u8,
    /// `None` means the human plays both colours.
    pub human: Option<Color>,
    pub tc: TimeControl,
    pub strength: Strength,
}

impl Default for GameSetup {
    fn default() -> GameSetup {
        GameSetup {
            size: mirai_core::Size::square(19),
            rules: mirai_core::RuleSet::Chinese,
            komi: mirai_core::RuleSet::Chinese.default_komi(),
            handicap: 0,
            human: Some(Color::Black),
            tc: TimeControl::UNLIMITED,
            strength: Strength::Visits(400),
        }
    }
}

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
    /// Last-counted territory, for a board overlay. Empty until the first rescore.
    territory: Box<[Option<Color>]>,
    in_byo: [bool; 2],
    clocks: HashMap<NodeId, ClockSnap>,
    pub end: Option<GameEnd>,
    pub forced_result: Option<String>,
    /// The last count, written by [`Play::rescore`].
    count: Option<Count>,
    /// The node the game is being played at. The view cursor may move; this does not.
    head: NodeId,
    /// [`GameSession::epoch`](crate::GameSession::epoch) when this game started. A replaced
    /// record must not be treated as this game, even when a node id collides.
    epoch: u64,
}

impl PlaySession {
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

/// One clock step's outcome.
///
/// A single `dt` may cross several byo-yomi periods. That is still
/// [`PeriodConsumed`] unless the overshoot also exhausts the last period:
/// [`Play::tick`] and the GTK ticker treat anything but [`Flag`] as "the game
/// continues", and the clock display is redrawn either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tick {
    Running,
    PeriodConsumed,
    Flag,
}

pub fn tick_clock(
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
    // The negative remainder is time already spent. Resetting to a full period
    // here would hand back the overshoot, so a stall spanning several periods
    // would cost only one and grant time.
    while *remaining <= 0.0 {
        if *byo_left == 0 {
            *remaining = 0.0;
            return Tick::Flag;
        }
        *byo_left -= 1;
        *in_byo = true;
        *remaining += tc.byo_period_s as f32;
    }
    Tick::PeriodConsumed
}

pub fn resign_check(
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

pub fn select_move_index(
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

/// Why a game stopped before it was counted. Each frontend words it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameEnd {
    BothPassed,
    Resigned(Color),
    LostOnTime(Color),
}

/// The board as last counted, whatever a resignation or a time loss decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Count {
    pub black: f32,
    pub white: f32,
    /// Some points were taken from the engine's ownership estimate, not settled.
    pub approximate: bool,
    /// The count as an SGF result, `"B+3.5"` or `"0"`.
    pub result: String,
}

/// An SGF result (`RE`) value as [`outcome`] reads it, for a frontend to word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    /// Nothing recorded.
    None,
    Draw,
    Resignation(Color),
    Time(Color),
    Forfeit(Color),
    /// A winner by the margin as written, e.g. `"7.5"`.
    Points(Color, &'a str),
    /// A winner by an unrecorded margin: `"B+"`.
    Win(Color),
    /// Something else, shown as written.
    Other(&'a str),
}

pub fn outcome(result: &str) -> Outcome<'_> {
    if result.is_empty() {
        return Outcome::None;
    }
    if result == "0" || result.eq_ignore_ascii_case("draw") {
        return Outcome::Draw;
    }
    let Some((winner, margin)) = result.split_once('+') else {
        return Outcome::Other(result);
    };
    let Some(who) = Color::from_letter(winner) else {
        return Outcome::Other(result);
    };
    match margin {
        "" => Outcome::Win(who),
        "R" | "r" => Outcome::Resignation(who),
        "T" | "t" => Outcome::Time(who),
        "F" | "f" => Outcome::Forfeit(who),
        m => Outcome::Points(who, m),
    }
}

/// Why a human move was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayError {
    NotYourTurn,
    Illegal(IllegalMove),
}

impl std::fmt::Display for PlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayError::NotYourTurn => f.write_str("not your turn"),
            PlayError::Illegal(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for PlayError {}

/// The English text, for a frontend that shows the error as it is.
impl From<PlayError> for String {
    fn from(e: PlayError) -> String {
        e.to_string()
    }
}

/// The position an engine request was built for.
///
/// A report is applied only when this still names the play head: the same document epoch,
/// the same node, and the generation of the request that is still outstanding. Navigation
/// does not retire it. Undo, a later request, or a replaced record does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayAnchor {
    epoch: u64,
    id: NodeId,
    generation: u64,
}

/// Frontend-agnostic play session sitting on a [`GameSession`].
pub struct Play {
    session: Option<PlaySession>,
    rng: SplitMix64,
    human_name: String,
    /// Bumped for every engine request and every action that retires one. A report carries
    /// the generation it was asked under; a later request invalidates the earlier report
    /// even when both positions have the same move number and the same side to move.
    request_gen: u64,
    ai_anchor: Option<PlayAnchor>,
    score_anchor: Option<PlayAnchor>,
}

impl Default for Play {
    fn default() -> Play {
        Play::with_human_name("You")
    }
}

impl Play {
    pub fn new() -> Play {
        Play::default()
    }

    /// A player that records the human as `name` rather than "You", for a frontend that
    /// localises it.
    pub fn with_human_name(name: impl Into<String>) -> Play {
        Play {
            session: None,
            rng: SplitMix64::new(seed_from_clock()),
            human_name: name.into(),
            request_gen: 0,
            ai_anchor: None,
            score_anchor: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    pub fn state(&self) -> PlayState {
        self.session
            .as_ref()
            .map(|s| s.state.clone())
            .unwrap_or(PlayState::Idle)
    }

    pub fn is_dead(&self, p: Point) -> bool {
        self.session.as_ref().is_some_and(|s| s.dead.is_dead(p))
    }

    pub fn ai(&self) -> Option<Color> {
        self.session.as_ref().and_then(PlaySession::ai)
    }

    pub fn dead(&self) -> Option<&DeadSet> {
        self.session.as_ref().map(|s| &s.dead)
    }

    pub fn territory(&self) -> Option<&[Option<Color>]> {
        self.session.as_ref().map(|s| s.territory.as_ref())
    }

    /// The game's SGF result: what a resignation or a time loss forced, else the count.
    /// `None` before the game has been counted at all.
    pub fn result(&self) -> Option<&str> {
        let s = self.session.as_ref()?;
        s.forced_result
            .as_deref()
            .or(s.count.as_ref().map(|c| c.result.as_str()))
    }

    /// Why the game stopped; `None` while it is being played.
    pub fn end(&self) -> Option<GameEnd> {
        self.session.as_ref()?.end
    }

    /// The board as last counted; `None` before the game has been counted.
    pub fn count(&self) -> Option<&Count> {
        self.session.as_ref()?.count.as_ref()
    }

    pub fn human(&self) -> Option<Color> {
        self.session.as_ref().and_then(|s| s.human)
    }

    pub fn clocks(&self) -> Option<(String, String)> {
        let s = self.session.as_ref()?;
        if s.tc.is_unlimited() {
            return None;
        }
        let text = |i: usize| {
            let t = clock_text(s.remaining[i]);
            if s.in_byo[i] {
                format!("{t} ({})", s.byo_left[i] as u16 + 1)
            } else {
                t
            }
        };
        Some((text(0), text(1)))
    }

    pub fn start(
        &mut self,
        game: &mut GameSession,
        setup: GameSetup,
        engine_name: &str,
        date: &str,
    ) {
        self.stop();
        let mut info = GameInfo::new(setup.size, setup.rules);
        info.komi = setup.komi;
        info.date = date.to_string();
        for c in [Color::Black, Color::White] {
            info.players[c.index()].name = match setup.human {
                Some(h) if h != c => engine_name.to_string(),
                _ => self.human_name.clone(),
            };
        }
        let stones = fixed_handicap(setup.size, setup.handicap);
        info.handicap = if stones.len() >= 2 { setup.handicap } else { 0 };
        let mut tree = GameTree::new(info);
        let root = tree.root();
        for p in stones {
            tree.set_setup_stone(root, p, Some(Color::Black));
        }
        game.adopt(tree, None);

        let in_byo = setup.tc.main_s == 0 && setup.tc.byo_periods > 0;
        let start_time = if in_byo {
            setup.tc.byo_period_s as f32
        } else if setup.tc.main_s == 0 {
            // Fischer adds the increment after a move. With no main time the
            // first move is played on that increment, or the opening tick flags.
            setup.tc.increment_s as f32
        } else {
            setup.tc.main_s as f32
        };
        let byo_left = if in_byo {
            setup.tc.byo_periods.saturating_sub(1)
        } else {
            setup.tc.byo_periods
        };
        let epoch = game.epoch();
        let head = game.cursor();
        self.session = Some(PlaySession {
            human: setup.human,
            tc: setup.tc,
            remaining: [start_time; 2],
            byo_left: [byo_left; 2],
            strength: setup.strength,
            resign_streak: 0,
            state: PlayState::HumanTurn,
            dead: DeadSet::empty(setup.size),
            territory: vec![None; setup.size.points()].into_boxed_slice(),
            in_byo: [in_byo; 2],
            clocks: HashMap::new(),
            end: None,
            forced_result: None,
            count: None,
            head,
            epoch,
        });
        self.rng = SplitMix64::new(seed_from_clock());
        self.snapshot_clock(game.cursor());
        self.advance(game);
    }

    pub fn stop(&mut self) {
        self.session = None;
        self.retire_requests();
    }

    fn snapshot_clock(&mut self, id: NodeId) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let snap = s.snap();
        s.clocks.insert(id, snap);
    }

    fn retire_requests(&mut self) {
        self.request_gen = self.request_gen.wrapping_add(1);
        self.ai_anchor = None;
        self.score_anchor = None;
    }

    fn issue_anchor(&mut self, epoch: u64, id: NodeId) -> PlayAnchor {
        self.request_gen = self.request_gen.wrapping_add(1);
        PlayAnchor {
            epoch,
            id,
            generation: self.request_gen,
        }
    }

    /// Still the request we asked for, and still the node the game is at.
    fn anchor_live(&self, game: &GameSession, anchor: PlayAnchor) -> bool {
        let Some(s) = self.session.as_ref() else {
            return false;
        };
        anchor.generation == self.request_gen
            && anchor.epoch == s.epoch
            && anchor.epoch == game.epoch()
            && anchor.id == s.head
            && game.tree().contains(anchor.id)
    }

    fn tree_is_ours(&self, game: &GameSession) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| game.epoch() == s.epoch && game.tree().contains(s.head))
    }

    fn at_head(&self, game: &GameSession) -> bool {
        self.tree_is_ours(game)
            && self
                .session
                .as_ref()
                .is_some_and(|s| game.cursor() == s.head)
    }

    /// Side to move at the play head. The view cursor is not consulted: a review
    /// position must not run the other clock or resign the other player.
    fn mover(&self, game: &mut GameSession) -> Option<Color> {
        let head = self
            .session
            .as_ref()
            .and_then(|s| (game.epoch() == s.epoch).then_some(s.head))?;
        if !game.tree().contains(head) {
            return None;
        }
        Some(game.tree_cached_mut().position(head).to_play)
    }

    fn two_passes(game: &GameSession) -> bool {
        let tree = game.tree();
        let cursor = game.cursor();
        let is_pass = |id: NodeId| matches!(tree.node(id).mv, Some((_, p)) if p.is_pass());
        is_pass(cursor) && tree.parent(cursor).is_some_and(is_pass)
    }

    fn advance(&mut self, game: &mut GameSession) {
        let Some(ai) = self.ai() else {
            if let Some(s) = self.session.as_mut() {
                s.state = PlayState::HumanTurn;
            }
            return;
        };
        let Some(to_play) = self.mover(game) else {
            return;
        };
        if let Some(s) = self.session.as_mut() {
            s.state = if to_play == ai {
                PlayState::AiThinking
            } else {
                PlayState::HumanTurn
            };
        }
    }

    pub fn needs_ai(&self) -> bool {
        matches!(self.state(), PlayState::AiThinking)
    }

    /// The search failed, or there is no engine. Only an in-progress AI turn
    /// stalls; a late failure after undo or a human move is ignored.
    pub fn ai_failed(&mut self, reason: impl Into<String>) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if !matches!(s.state, PlayState::AiThinking) {
            return;
        }
        s.state = PlayState::AiStalled(reason.into());
        self.retire_requests();
    }

    /// Asks for the engine's move again after [`Play::ai_failed`].
    pub fn retry_ai(&mut self) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if !matches!(s.state, PlayState::AiStalled(_)) {
            return;
        }
        s.state = PlayState::AiThinking;
    }

    /// The search for the play head, not for whatever node is on screen.
    ///
    /// Records the anchor [`Play::request_anchor`] returns. A later request, undo, or
    /// [`Play::stop`] retires it, so a report built for this call cannot land afterwards.
    pub fn ai_request(&mut self, game: &mut GameSession) -> Option<AnalyzeReq> {
        let s = self.session.as_ref()?;
        if !matches!(s.state, PlayState::AiThinking) {
            return None;
        }
        let ai = s.ai()?;
        let epoch = s.epoch;
        let head = s.head;
        let i = ai.index();
        let seconds = think_budget(&s.tc, s.remaining[i], s.byo_left[i], s.in_byo[i]);
        if game.epoch() != epoch || !game.tree().contains(head) {
            return None;
        }
        if game.tree_cached_mut().position(head).to_play != ai {
            return None;
        }
        let ms = match &s.strength {
            Strength::TimeMs(t) => Some(*t),
            _ => seconds.map(|sec| (sec * 1000.0).max(100.0) as u32),
        };
        let visits = match &s.strength {
            Strength::Visits(v) => *v,
            Strength::Human { .. } if ms.is_none() => HUMAN_UNTIMED_VISITS,
            // Time decides: the search ends on the clock, not on a visit count.
            _ => u32::MAX,
        };
        let mut req = game.request_for(head, Want::OWNERSHIP, visits);
        req.max_time_ms = ms;
        req.priority = 8;
        req.report_every_ms = Some(200);
        req.overrides = vec![("wideRootNoise".to_string(), "0.0".to_string())];
        if let Strength::Human { profile } = &s.strength {
            req.overrides
                .push(("humanSLProfile".to_string(), profile.clone()));
        }
        let anchor = self.issue_anchor(epoch, head);
        self.ai_anchor = Some(anchor);
        self.score_anchor = None;
        Some(req)
    }

    /// The anchor of the latest [`Play::ai_request`], if that request is still outstanding.
    pub fn request_anchor(&self) -> Option<PlayAnchor> {
        self.ai_anchor
    }

    /// The anchor of the latest [`Play::scoring_request`], if that count is still outstanding.
    pub fn score_anchor(&self) -> Option<PlayAnchor> {
        self.score_anchor
    }

    /// Whether either player's clock is running, without cloning a terminal result
    /// string on each timer tick.
    pub fn is_clock_running(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| matches!(s.state, PlayState::HumanTurn | PlayState::AiThinking))
    }

    /// Whose clock runs. `None` when no clock should: stalled, scoring, over, or the
    /// record this session started is no longer the one in `game`.
    pub fn side_to_move(&self, game: &mut GameSession) -> Option<Color> {
        self.is_clock_running().then(|| self.mover(game)).flatten()
    }

    /// Whether a human move would be accepted now: it is the human's turn and the view
    /// is at the play head. A frontend uses this to lock the board while reviewing.
    pub fn accepts_human_move(&self, game: &GameSession) -> bool {
        matches!(self.state(), PlayState::HumanTurn) && self.at_head(game)
    }

    pub fn human_play(&mut self, game: &mut GameSession, p: Point) -> Result<(), PlayError> {
        // A click while reviewing is a point on the viewed board, not a move at the
        // live position. Playing it either forks the game or drops a stone on the
        // wrong node; both are refused.
        if !matches!(self.state(), PlayState::HumanTurn) || !self.at_head(game) {
            return Err(PlayError::NotYourTurn);
        }
        game.play(p).map_err(PlayError::Illegal)?;
        let color = game
            .tree()
            .node(game.cursor())
            .mv
            .map(|(c, _)| c)
            .unwrap_or(Color::Black);
        self.after_move(game, color);
        Ok(())
    }

    pub fn pass(&mut self, game: &mut GameSession) -> Result<(), PlayError> {
        self.human_play(game, Point::PASS)
    }

    pub fn resign(&mut self, game: &mut GameSession) {
        // Scoring is the count the player is still adjusting. Over already has a
        // result — a timeout's `B+T` must not become a resignation.
        if !self.is_active()
            || matches!(self.state(), PlayState::Scoring | PlayState::Over(_))
            || !self.tree_is_ours(game)
        {
            return;
        }
        let human = self.session.as_ref().and_then(|s| s.human);
        let Some(loser) = human.or_else(|| self.mover(game)) else {
            return;
        };
        let head = self.session.as_ref().expect("active session").head;
        game.go_to(head);
        self.end_game(
            game,
            Some(format!("{}+R", loser.other().katago())),
            GameEnd::Resigned(loser),
        );
    }

    /// Retracts at the play head. Returns `false` at the initial position or after
    /// the record was replaced, so a frontend leaves an existing search/count alone.
    pub fn undo(&mut self, game: &mut GameSession) -> bool {
        let Some((human, mut head, epoch)) =
            self.session.as_ref().map(|s| (s.human, s.head, s.epoch))
        else {
            return false;
        };
        if game.epoch() != epoch || !game.tree().contains(head) {
            return false;
        }
        // Retract the last exchange at the play head. Deleting the view cursor
        // instead throws away every descendant the player had only stepped back to see.
        let mut removed = 0;
        while removed < 2 {
            let Some(parent) = game.tree().parent(head) else {
                break;
            };
            if !game.delete_branch_at(head) {
                break;
            }
            removed += 1;
            head = parent;
            let to_play = game.tree_cached_mut().position(head).to_play;
            match human {
                Some(h) if to_play != h => continue,
                _ => break,
            }
        }
        if removed == 0 {
            return false;
        }
        self.retire_requests();
        game.go_to(head);
        let size = game.tree().info.size;
        if let Some(s) = self.session.as_mut() {
            s.head = head;
            if let Some(snap) = s.clocks.get(&head).copied() {
                s.restore(snap);
            }
            s.resign_streak = 0;
            s.dead = DeadSet::empty(size);
            s.forced_result = None;
            s.end = None;
            s.count = None;
            s.state = PlayState::HumanTurn;
        }
        game.set_result(String::new());
        self.advance(game);
        true
    }

    fn after_move(&mut self, game: &mut GameSession, color: Color) {
        let id = game.cursor();
        if let Some(s) = self.session.as_mut() {
            s.head = id;
            let i = color.index();
            if s.in_byo[i] {
                s.remaining[i] = s.tc.byo_period_s as f32;
            } else {
                s.remaining[i] += s.tc.increment_s as f32;
            }
        }
        self.snapshot_clock(id);
        if Self::two_passes(game) {
            self.end_game(game, None, GameEnd::BothPassed);
            return;
        }
        self.advance(game);
    }

    /// Applies `report` only when `anchor` is still the outstanding request for the play head.
    ///
    /// The move is played at that node, never at the view cursor. The view then follows
    /// the move. A stale anchor, a replaced record, or a root that is not this engine's
    /// side to move changes nothing and returns `false`. The anchor names the request, so
    /// the report's turn is not compared: KataGo counts the request's moves, which after
    /// a setup node or an SGF `MN` is not the head's move number.
    pub fn apply_ai_report_for(
        &mut self,
        game: &mut GameSession,
        anchor: PlayAnchor,
        report: &Report,
        temperature: f32,
        resign_threshold: f32,
        resign_required: u8,
    ) -> bool {
        if !matches!(self.state(), PlayState::AiThinking)
            || self.ai_anchor != Some(anchor)
            || !self.anchor_live(game, anchor)
        {
            return false;
        }
        let Some(ai) = self.ai() else {
            return false;
        };
        if report.root.current_player != ai {
            return false;
        }
        let (move_number, board_points) = {
            let pos = game.tree_cached_mut().position(anchor.id);
            if pos.to_play != ai {
                return false;
            }
            (pos.move_number, pos.board.size.points())
        };
        let winrate = report.root.winrate_for(ai);
        let resign = {
            let Some(s) = self.session.as_mut() else {
                return false;
            };
            let (streak, resign) = resign_check(
                winrate,
                resign_threshold,
                s.resign_streak,
                resign_required,
                move_number,
                board_points,
            );
            s.resign_streak = streak;
            resign
        };
        if resign {
            game.go_to(anchor.id);
            self.end_game(
                game,
                Some(format!("{}+R", ai.other().katago())),
                GameEnd::Resigned(ai),
            );
            return true;
        }
        if game.cursor() != anchor.id {
            game.go_to(anchor.id);
        }
        let choice = select_move_index(&report.moves, temperature, &mut self.rng);
        let mv = match choice {
            Some(i) => report.moves[i].mv,
            None => Point::PASS,
        };
        if game.play(mv).is_err() && mv != Point::PASS {
            let _ = game.play(Point::PASS);
        }
        self.ai_anchor = None;
        self.after_move(game, ai);
        true
    }

    pub fn tick(&mut self, game: &mut GameSession, dt: f32) -> bool {
        if !self.is_clock_running() {
            return false;
        }
        let Some(active) = self.mover(game) else {
            return false;
        };
        let Some(s) = self.session.as_mut() else {
            return false;
        };
        if s.tc.is_unlimited() {
            return false;
        }
        let i = active.index();
        let tc = s.tc;
        tick_clock(
            &tc,
            &mut s.remaining[i],
            &mut s.byo_left[i],
            &mut s.in_byo[i],
            dt,
        ) == Tick::Flag
    }

    pub fn flag(&mut self, game: &mut GameSession, _claimed: Color) {
        if !self.is_clock_running() {
            return;
        }
        // The caller may have seen a review position. Only the clock at the play
        // head can time out; the viewed side does not determine the loser.
        let Some(loser) = self.mover(game) else {
            return;
        };
        let head = self.session.as_ref().expect("active session").head;
        game.go_to(head);
        self.end_game(
            game,
            Some(format!("{}+T", loser.other().katago())),
            GameEnd::LostOnTime(loser),
        );
    }

    fn end_game(&mut self, game: &mut GameSession, forced: Option<String>, end: GameEnd) {
        if let Some(s) = self.session.as_mut() {
            s.state = forced
                .as_ref()
                .map_or(PlayState::Scoring, |result| PlayState::Over(result.clone()));
            s.resign_streak = 0;
            s.end = Some(end);
            s.forced_result = forced;
        }
        // The search that produced this ending, and any count asked for an earlier
        // position, must not land afterwards.
        self.retire_requests();
        self.rescore(game);
    }

    /// Counts `ownership` only when `anchor` is still the outstanding scoring request.
    ///
    /// The board counted is the play head, not the view. A late count after undo or a
    /// replaced record returns `false` and leaves the record alone.
    pub fn apply_ownership_for(
        &mut self,
        game: &mut GameSession,
        anchor: PlayAnchor,
        ownership: Option<&[i8]>,
    ) -> bool {
        if !matches!(self.state(), PlayState::Scoring | PlayState::Over(_))
            || self.score_anchor != Some(anchor)
            || !self.anchor_live(game, anchor)
        {
            return false;
        }
        let board = &game.tree_cached_mut().position(anchor.id).board;
        let dead = match ownership {
            Some(raw) => dead_from_ownership(board, raw),
            None => DeadSet::empty(board.size),
        };
        if let Some(s) = self.session.as_mut() {
            s.dead = dead;
        }
        self.score_anchor = None;
        game.go_to(anchor.id);
        self.rescore(game);
        true
    }

    pub fn toggle_dead(&mut self, game: &mut GameSession, p: Point) {
        // While the engine's count is outstanding the board is read-only: its result
        // replaces the dead set and would silently drop a mark made meanwhile.
        if !matches!(self.state(), PlayState::Scoring)
            || self.score_anchor.is_some()
            || p.is_pass()
            || !self.at_head(game)
        {
            return;
        }
        let head = self.session.as_ref().expect("active session").head;
        let board = &game.tree_cached_mut().position(head).board;
        if board.at(p).is_none() {
            return;
        }
        if let Some(s) = self.session.as_mut() {
            s.dead.toggle_chain(board, p);
        }
        self.rescore(game);
    }

    fn rescore(&mut self, game: &mut GameSession) {
        if !self.tree_is_ours(game) {
            return;
        }
        let head = self.session.as_ref().expect("active session").head;
        let (rules, komi, handicap) = {
            let t = game.tree();
            (t.info.rules, t.info.komi, t.info.handicap)
        };
        let board = &game.tree_cached_mut().position(head).board;
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let counted = score(board, &rules.rules(), komi, handicap, &s.dead);
        let count = Count {
            black: counted.black,
            white: counted.white,
            approximate: counted.approximate,
            result: counted.result_string(),
        };
        let result = s
            .forced_result
            .clone()
            .unwrap_or_else(|| count.result.clone());
        s.territory = counted.territory;
        s.count = Some(count);
        game.set_result(result);
    }

    /// Starts a count at the play head without building a query (for no-engine scoring).
    pub fn start_count(&mut self, game: &GameSession) -> Option<PlayAnchor> {
        let s = self.session.as_ref()?;
        if !matches!(s.state, PlayState::Scoring | PlayState::Over(_))
            || game.epoch() != s.epoch
            || !game.tree().contains(s.head)
        {
            return None;
        }
        let anchor = self.issue_anchor(s.epoch, s.head);
        self.score_anchor = Some(anchor);
        self.ai_anchor = None;
        Some(anchor)
    }

    pub fn scoring_request(&mut self, game: &mut GameSession) -> Option<AnalyzeReq> {
        let anchor = self.start_count(game)?;
        let mut req = game.request_for(anchor.id, Want::OWNERSHIP, 400);
        req.max_time_ms = None;
        req.report_every_ms = None;
        req.priority = 8;
        Some(req)
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
        assert_eq!(counts, [472, 366, 162]);
    }

    #[test]
    fn a_lone_bad_report_does_not_resign() {
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
        let mut streak = 0;
        let mut resign = false;
        for _ in 0..3 {
            (streak, resign) = resign_check(0.01, 0.05, streak, 3, 20, 361);
        }
        assert_eq!(streak, 3);
        assert!(!resign);
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
        let (mut remaining, mut byo_left, mut in_byo) = (30.0f32, 2u8, true);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 30.0),
            Tick::PeriodConsumed
        );
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 30.0),
            Tick::PeriodConsumed
        );
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 30.0),
            Tick::Flag
        );
    }

    /// A stall whose dt spans several periods must spend each of them. Resetting to a
    /// full period on the first expiry granted the time the stall had already used.
    #[test]
    fn a_stall_spending_two_and_a_half_periods_leaves_half_a_period() {
        let tc = TimeControl {
            main_s: 0,
            byo_periods: 3,
            byo_period_s: 30,
            increment_s: 0,
        };
        // The current period plus two banked: three periods, 0.5 of the last left.
        let (mut remaining, mut byo_left, mut in_byo) = (30.0f32, 2u8, true);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 75.0),
            Tick::PeriodConsumed
        );
        assert!(in_byo);
        assert_eq!(byo_left, 0);
        assert_eq!(remaining, 15.0);
    }

    #[test]
    fn a_stall_longer_than_every_period_loses_on_time() {
        let tc = TimeControl {
            main_s: 0,
            byo_periods: 3,
            byo_period_s: 30,
            increment_s: 0,
        };
        let (mut remaining, mut byo_left, mut in_byo) = (30.0f32, 2u8, true);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 91.0),
            Tick::Flag
        );
        assert_eq!(remaining, 0.0);
        assert_eq!(byo_left, 0);
    }

    /// Main time that runs out mid-step does not start the first period full: the
    /// overshoot is already time spent in that period.
    #[test]
    fn main_time_overshoot_rolls_into_byo_yomi() {
        let tc = TimeControl {
            main_s: 10,
            byo_periods: 3,
            byo_period_s: 30,
            increment_s: 0,
        };
        let (mut remaining, mut byo_left, mut in_byo) = (10.0f32, 3u8, false);
        assert_eq!(
            tick_clock(&tc, &mut remaining, &mut byo_left, &mut in_byo, 25.0),
            Tick::PeriodConsumed
        );
        assert!(in_byo);
        assert_eq!(byo_left, 2);
        assert_eq!(remaining, 15.0);
    }

    #[test]
    fn sgf_results_are_read_into_outcomes() {
        assert_eq!(outcome("B+R"), Outcome::Resignation(Color::Black));
        assert_eq!(outcome("W+t"), Outcome::Time(Color::White));
        assert_eq!(outcome("B+7.5"), Outcome::Points(Color::Black, "7.5"));
        assert_eq!(outcome("W+"), Outcome::Win(Color::White));
        assert_eq!(outcome("0"), Outcome::Draw);
        assert_eq!(outcome("Draw"), Outcome::Draw);
        assert_eq!(outcome(""), Outcome::None);
        assert_eq!(outcome("Void"), Outcome::Other("Void"));
        assert_eq!(outcome("X+R"), Outcome::Other("X+R"));
    }

    #[test]
    fn starting_a_game_places_handicap_and_asks_white_to_play() {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        let setup = GameSetup {
            handicap: 2,
            human: Some(Color::White),
            ..Default::default()
        };
        play.start(&mut game, setup, "KataGo", "2026-08-17");
        assert_eq!(game.tree().info.handicap, 2);
        assert_eq!(game.to_play(), Color::White);
        assert_eq!(play.state(), PlayState::HumanTurn);
    }

    fn engine_to_move(tc: TimeControl) -> (GameSession, Play) {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        play.start(
            &mut game,
            GameSetup {
                human: Some(Color::White),
                tc,
                ..Default::default()
            },
            "KataGo",
            "2026-08-17",
        );
        (game, play)
    }

    #[test]
    fn an_engine_failure_stalls_until_retried() {
        let (mut game, mut play) = engine_to_move(TimeControl::UNLIMITED);
        assert_eq!(play.state(), PlayState::AiThinking);
        play.ai_failed("no engine");
        assert_eq!(play.state(), PlayState::AiStalled("no engine".into()));
        assert!(!play.needs_ai());
        let p = game.tree().info.size.point(3, 3);
        assert!(play.human_play(&mut game, p).is_err());
        play.retry_ai();
        assert_eq!(play.state(), PlayState::AiThinking);
        assert!(play.needs_ai());
    }

    #[test]
    fn undo_and_resign_work_from_a_stalled_turn() {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        play.start(&mut game, GameSetup::default(), "KataGo", "2026-08-17");
        let p = game.tree().info.size.point(3, 3);
        play.human_play(&mut game, p).expect("the opening move");
        play.ai_failed("search failed");
        play.undo(&mut game);
        assert_eq!(play.state(), PlayState::HumanTurn);
        assert_eq!(game.cursor(), game.tree().root());

        play.human_play(&mut game, p).expect("the opening move");
        play.ai_failed("search failed");
        play.resign(&mut game);
        assert!(matches!(play.state(), PlayState::Over(_)));
    }

    #[test]
    fn an_untimed_human_like_engine_still_has_a_bounded_search() {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        let setup = GameSetup {
            human: Some(Color::White),
            strength: Strength::Human {
                profile: DEFAULT_HUMAN_PROFILE.into(),
            },
            ..GameSetup::default()
        };
        play.start(&mut game, setup, "KataGo", "2026-08-17");
        let req = play.ai_request(&mut game).expect("the engine plays Black");
        assert_eq!(req.max_time_ms, None);
        assert_eq!(req.max_visits, Some(HUMAN_UNTIMED_VISITS));
    }

    #[test]
    fn the_clock_does_not_run_while_the_engine_is_stalled() {
        let tc = TimeControl {
            main_s: 60,
            byo_periods: 0,
            byo_period_s: 0,
            increment_s: 0,
        };
        let (mut game, mut play) = engine_to_move(tc);
        let before = play.clocks();
        assert!(!play.tick(&mut game, 1.0));
        let thinking = play.clocks();
        assert_ne!(before, thinking);
        play.ai_failed("no engine");
        let stalled = play.clocks();
        assert!(!play.tick(&mut game, 30.0));
        assert_eq!(play.clocks(), stalled);
        play.retry_ai();
        assert!(!play.tick(&mut game, 1.0));
        assert_ne!(play.clocks(), stalled);
    }

    /// The period clock is still positive while in byo-yomi, and `byo_left` counts
    /// only periods banked after the current one. Budgeting that remainder as main
    /// time (or treating a zero bank as "out of periods") makes the engine move at
    /// once instead of using the period.
    #[test]
    fn an_ai_already_in_byo_yomi_thinks_for_most_of_the_period() {
        let tc = TimeControl {
            main_s: 0,
            byo_periods: 1,
            byo_period_s: 30,
            increment_s: 0,
        };
        let (mut game, mut play) = engine_to_move(tc);
        let req = play.ai_request(&mut game).expect("the engine is to move");
        assert_eq!(req.max_time_ms, Some(27_000));
    }

    /// Zero main time with an increment is still a clock. Seeding from main time
    /// alone starts the bank at zero, so the first tick loses on time; hiding it
    /// as unlimited leaves the increment unused.
    #[test]
    fn increment_only_starts_on_the_increment_and_does_not_flag_immediately() {
        let tc = TimeControl {
            main_s: 0,
            byo_periods: 0,
            byo_period_s: 0,
            increment_s: 10,
        };
        let (mut game, mut play) = engine_to_move(tc);
        assert_eq!(
            play.clocks(),
            Some(("0:10".to_string(), "0:10".to_string()))
        );
        let req = play.ai_request(&mut game).expect("the engine is to move");
        assert_eq!(req.max_time_ms, Some(5_000));
        assert!(!play.tick(&mut game, 1.0));
        assert_eq!(
            play.clocks(),
            Some(("0:09".to_string(), "0:10".to_string()))
        );
    }

    fn report_at(game: &mut GameSession, p: Point) -> Report {
        let turn = game.position().move_number;
        let color = game.to_play();
        let mut report = Report::empty(turn, color);
        let mut candidate = mv(0, 100);
        candidate.mv = p;
        report.moves.push(candidate);
        report
    }

    #[test]
    fn navigating_during_a_search_plays_at_the_request_node_not_the_view() {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        play.start(&mut game, GameSetup::default(), "KataGo", "2026-08-17");
        let size = game.tree().info.size;
        let first = size.point(3, 3);
        let reply = size.point(15, 15);
        play.human_play(&mut game, first).unwrap();
        let head = game.cursor();
        let _req = play.ai_request(&mut game).unwrap();
        let anchor = play.request_anchor().unwrap();
        let report = report_at(&mut game, reply);

        game.go_first();
        assert_eq!(game.to_play(), Color::Black);
        let req = play.ai_request(&mut game).unwrap();
        assert_eq!(req.moves, vec![(Color::Black, first)]);
        let current = play.request_anchor().unwrap();
        assert!(!play.apply_ai_report_for(&mut game, anchor, &report, 0.0, 0.0, 3));
        assert!(play.apply_ai_report_for(&mut game, current, &report, 0.0, 0.0, 3));
        assert_eq!(game.tree().parent(game.cursor()), Some(head));
        assert_eq!(
            game.tree().node(game.cursor()).mv,
            Some((Color::White, reply))
        );
        assert_eq!(game.tree().children(game.tree().root()), &[head]);
        assert_eq!(play.state(), PlayState::HumanTurn);
        game.go_first();
        let extra = size.point(5, 5);
        assert!(matches!(
            play.human_play(&mut game, extra),
            Err(PlayError::NotYourTurn)
        ));
        assert_eq!(game.cursor(), game.tree().root());
        assert_eq!(game.tree().children(game.tree().root()), &[head]);
    }

    #[test]
    fn back_to_back_requests_only_accept_the_latest_anchor() {
        let (mut game, mut play) = engine_to_move(TimeControl::UNLIMITED);
        let p = game.tree().info.size.point(3, 3);
        let _old = play.ai_request(&mut game).unwrap();
        let first = play.request_anchor().unwrap();
        let _new = play.ai_request(&mut game).unwrap();
        let second = play.request_anchor().unwrap();
        let report = report_at(&mut game, p);

        assert!(!play.apply_ai_report_for(&mut game, first, &report, 0.0, 0.0, 3));
        assert_eq!(game.cursor(), game.tree().root());
        assert_eq!(play.state(), PlayState::AiThinking);
        assert!(play.apply_ai_report_for(&mut game, second, &report, 0.0, 0.0, 3));
        assert_eq!(game.tree().node(game.cursor()).mv, Some((Color::Black, p)));
        assert!(!play.apply_ai_report_for(&mut game, second, &report, 0.0, 0.0, 3));
    }

    #[test]
    fn a_replaced_record_cannot_receive_a_report_even_if_node_ids_collide() {
        let (mut game, mut play) = engine_to_move(TimeControl::UNLIMITED);
        let p = game.tree().info.size.point(3, 3);
        let _req = play.ai_request(&mut game).unwrap();
        let anchor = play.request_anchor().unwrap();
        let report = report_at(&mut game, p);
        let epoch = game.epoch();
        let info = game.tree().info.clone();
        game.adopt(GameTree::new(info), None);
        assert_ne!(game.epoch(), epoch);

        assert!(!play.apply_ai_report_for(&mut game, anchor, &report, 0.0, 0.0, 3));
        assert!(game.tree().children(game.tree().root()).is_empty());
        assert_eq!(game.tree().info.result, "");
    }

    /// KataGo's turn counts the moves the request carries, which an SGF `MN` on the head
    /// does not change; only the side to move has to match.
    #[test]
    fn a_report_needs_the_requested_player_not_the_heads_move_number() {
        let (mut game, mut play) = engine_to_move(TimeControl::UNLIMITED);
        let root = game.tree().root();
        game.tree_mut().node_mut(root).move_number_override = Some(40);
        let p = game.tree().info.size.point(3, 3);
        let req = play.ai_request(&mut game).unwrap();
        let anchor = play.request_anchor().unwrap();
        let mut report = report_at(&mut game, p);
        report.turn = req.moves.len() as u16;
        report.root.current_player = Color::White;
        assert!(!play.apply_ai_report_for(&mut game, anchor, &report, 0.0, 0.0, 3));
        assert_eq!(game.cursor(), game.tree().root());
        assert!(game.tree().children(game.tree().root()).is_empty());
        report.root.current_player = Color::Black;
        assert!(play.apply_ai_report_for(&mut game, anchor, &report, 0.0, 0.0, 3));
    }

    #[test]
    fn undo_while_reviewing_retracts_the_play_head_not_the_view_subtree() {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        play.start(&mut game, GameSetup::default(), "KataGo", "2026-08-17");
        game.set_edit_history_enabled(false);
        let size = game.tree().info.size;
        let (first, reply, last) = (size.point(3, 3), size.point(15, 15), size.point(4, 4));
        play.human_play(&mut game, first).unwrap();
        let first_id = game.cursor();
        let _req = play.ai_request(&mut game).unwrap();
        let anchor = play.request_anchor().unwrap();
        let report = report_at(&mut game, reply);
        assert!(play.apply_ai_report_for(&mut game, anchor, &report, 0.0, 0.0, 3));
        let reply_id = game.cursor();
        play.human_play(&mut game, last).unwrap();
        let last_id = game.cursor();

        game.go_to(first_id);
        play.undo(&mut game);
        assert_eq!(game.cursor(), reply_id);
        assert!(game.tree().contains(first_id));
        assert!(game.tree().contains(reply_id));
        assert!(!game.tree().contains(last_id));
        assert_eq!(game.tree().children(first_id), &[reply_id]);
        assert_eq!(play.state(), PlayState::HumanTurn);
    }

    #[test]
    fn navigation_does_not_change_the_clock_or_the_time_loss() {
        let tc = TimeControl {
            main_s: 60,
            byo_periods: 0,
            byo_period_s: 0,
            increment_s: 0,
        };
        let mut game = GameSession::blank();
        let mut play = Play::new();
        play.start(
            &mut game,
            GameSetup {
                tc,
                ..GameSetup::default()
            },
            "KataGo",
            "2026-08-17",
        );
        let p = game.tree().info.size.point(3, 3);
        play.human_play(&mut game, p).unwrap();
        game.go_first();
        assert_eq!(game.to_play(), Color::Black);
        assert_eq!(play.side_to_move(&mut game), Some(Color::White));
        assert!(!play.tick(&mut game, 1.0));
        assert_eq!(play.clocks(), Some(("1:00".into(), "0:59".into())));
        play.flag(&mut game, Color::Black); // the viewed player, not the timed-out player
        assert_eq!(play.end(), Some(GameEnd::LostOnTime(Color::White)));
        assert_eq!(game.tree().info.result, "B+T");
    }

    #[test]
    fn resignation_cannot_replace_a_finished_result_or_late_count() {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        play.start(&mut game, GameSetup::default(), "KataGo", "2026-08-17");
        let p = game.tree().info.size.point(3, 3);
        play.human_play(&mut game, p).unwrap();
        // White, the engine, is to move at the play head, so White loses on time.
        play.flag(&mut game, Color::White);
        assert_eq!(game.tree().info.result, "B+T");
        let head = game.cursor();
        let _req = play.scoring_request(&mut game).unwrap();
        let count_anchor = play.score_anchor().unwrap();
        game.go_first();
        play.resign(&mut game);
        play.flag(&mut game, Color::White);
        assert_eq!(game.tree().info.result, "B+T");
        assert_eq!(play.end(), Some(GameEnd::LostOnTime(Color::White)));
        assert!(play.apply_ownership_for(&mut game, count_anchor, None));
        assert_eq!(game.cursor(), head);
        assert_eq!(game.tree().info.result, "B+T");
        assert!(!play.apply_ownership_for(&mut game, count_anchor, None));
        play.undo(&mut game);
        assert_eq!(game.tree().info.result, "");
        assert!(!play.apply_ownership_for(&mut game, count_anchor, None));
        assert_eq!(game.tree().info.result, "");
        assert!(play.count().is_none());
    }

    #[test]
    fn a_mark_made_during_the_count_is_not_overwritten_by_it() {
        let mut game = GameSession::blank();
        let mut play = Play::new();
        let setup = GameSetup {
            human: None,
            ..GameSetup::default()
        };
        play.start(&mut game, setup, "KataGo", "2026-08-17");
        let p = game.tree().info.size.point(3, 3);
        play.human_play(&mut game, p).unwrap();
        play.pass(&mut game).unwrap();
        play.pass(&mut game).unwrap();
        assert_eq!(play.state(), PlayState::Scoring);

        let anchor = play.start_count(&game).unwrap();
        play.toggle_dead(&mut game, p);
        assert!(
            !play.is_dead(p),
            "the board is read-only while the engine counts"
        );
        assert!(play.apply_ownership_for(&mut game, anchor, None));
        play.toggle_dead(&mut game, p);
        assert!(play.is_dead(p), "after the count the player corrects it");
    }
}
