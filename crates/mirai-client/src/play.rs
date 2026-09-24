// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Playing a game against the engine, without a UI.
//!
//! The GTK controller and the HarmonyOS view model both drive this. Clocks, resignation,
//! move sampling and scoring live here; frontends own timers, dialogs and drawing.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use mirai_core::{
    Color, DeadSet, GameInfo, GameTree, NodeId, Point, SplitMix64, TimeControl, clock_text,
    fixed_handicap, score, think_budget,
};
use mirai_engine::{AnalyzeReq, MoveInfo, Report, Want};

use crate::{GameSession, dead_from_ownership};

pub const DEFAULT_HUMAN_PROFILE: &str = "rank_5k";

/// How strong the AI plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Strength {
    Visits(u32),
    TimeMs(u32),
    Human { profile: String },
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
    pub reason: String,
    pub forced_result: Option<String>,
    pub summary: String,
    /// Last SGF result (`"B+R"`, `"W+3.5"`, `"0"`), written by [`Play::rescore`].
    result: String,
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

pub fn result_phrase(result: &str) -> String {
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
        Some(c) => c.name(),
        None => return result.to_string(),
    };
    match margin {
        "R" | "r" => format!("{who} wins by resignation"),
        "T" | "t" => format!("{who} wins on time"),
        "F" | "f" => format!("{who} wins by forfeit"),
        m => format!("{who} wins by {m}"),
    }
}

/// Frontend-agnostic play session sitting on a [`GameSession`].
pub struct Play {
    session: Option<PlaySession>,
    rng: SplitMix64,
}

impl Default for Play {
    fn default() -> Play {
        Play {
            session: None,
            rng: SplitMix64::new(seed_from_clock()),
        }
    }
}

impl Play {
    pub fn new() -> Play {
        Play::default()
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

    /// The result as English prose, e.g. "Black wins by resignation". `None` before the
    /// game has been counted at all.
    pub fn result_phrase(&self) -> Option<String> {
        let s = self.session.as_ref()?;
        (!s.result.is_empty()).then(|| result_phrase(&s.result))
    }

    pub fn human(&self) -> Option<Color> {
        self.session.as_ref().and_then(|s| s.human)
    }

    pub fn summary(&self) -> String {
        self.session
            .as_ref()
            .map(|s| s.summary.clone())
            .unwrap_or_default()
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
                Some(h) if h == c => "You".to_string(),
                Some(_) => engine_name.to_string(),
                None => "You".to_string(),
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
        } else {
            setup.tc.main_s as f32
        };
        let byo_left = if in_byo {
            setup.tc.byo_periods.saturating_sub(1)
        } else {
            setup.tc.byo_periods
        };
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
            reason: String::new(),
            forced_result: None,
            summary: String::new(),
            result: String::new(),
        });
        self.rng = SplitMix64::new(seed_from_clock());
        self.snapshot_clock(game.cursor());
        self.advance(game);
    }

    pub fn stop(&mut self) {
        self.session = None;
    }

    fn snapshot_clock(&mut self, id: NodeId) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let snap = s.snap();
        s.clocks.insert(id, snap);
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
        let to_play = game.to_play();
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

    pub fn ai_request(&mut self, game: &mut GameSession) -> Option<AnalyzeReq> {
        let s = self.session.as_ref()?;
        let ai = s.ai()?;
        let seconds = think_budget(&s.tc, s.remaining[ai.index()], s.byo_left[ai.index()]);
        let ms = match &s.strength {
            Strength::TimeMs(t) => Some(*t),
            _ => seconds.map(|sec| (sec * 1000.0).max(100.0) as u32),
        };
        let mut req =
            game.request_for_cursor(Want::OWNERSHIP, s.strength.visits().unwrap_or(u32::MAX));
        req.max_time_ms = ms;
        req.priority = 8;
        req.report_every_ms = Some(200);
        req.overrides = vec![("wideRootNoise".to_string(), "0.0".to_string())];
        if let Strength::Human { profile } = &s.strength {
            req.overrides
                .push(("humanSLProfile".to_string(), profile.clone()));
        }
        Some(req)
    }

    pub fn human_play(&mut self, game: &mut GameSession, p: Point) -> Result<(), String> {
        if !matches!(self.state(), PlayState::HumanTurn) {
            return Err("not your turn".into());
        }
        game.play(p).map_err(|e| e.to_string())?;
        let color = game
            .tree()
            .node(game.cursor())
            .mv
            .map(|(c, _)| c)
            .unwrap_or(Color::Black);
        self.after_move(game, color);
        Ok(())
    }

    pub fn pass(&mut self, game: &mut GameSession) -> Result<(), String> {
        self.human_play(game, Point::PASS)
    }

    pub fn resign(&mut self, game: &mut GameSession) {
        if !self.is_active() || matches!(self.state(), PlayState::Scoring) {
            return;
        }
        let loser = self
            .session
            .as_ref()
            .and_then(|s| s.human)
            .unwrap_or_else(|| game.to_play());
        self.end_game(
            game,
            Some(format!("{}+R", loser.other().katago())),
            format!("{} resigned.", loser.name()),
        );
    }

    pub fn undo(&mut self, game: &mut GameSession) {
        if !self.is_active() {
            return;
        }
        let human = self.session.as_ref().and_then(|s| s.human);
        let mut removed = 0;
        while removed < 2 {
            let cursor = game.cursor();
            if !game.delete_branch_at(cursor) {
                break;
            }
            removed += 1;
            match human {
                Some(h) if game.to_play() != h => continue,
                _ => break,
            }
        }
        if removed == 0 {
            return;
        }
        let cursor = game.cursor();
        let size = game.tree().info.size;
        if let Some(s) = self.session.as_mut() {
            if let Some(snap) = s.clocks.get(&cursor).copied() {
                s.restore(snap);
            }
            s.resign_streak = 0;
            s.dead = DeadSet::empty(size);
            s.forced_result = None;
            s.reason.clear();
            s.summary.clear();
            s.result.clear();
            s.state = PlayState::HumanTurn;
        }
        game.set_result(String::new());
        self.advance(game);
    }

    fn after_move(&mut self, game: &mut GameSession, color: Color) {
        if let Some(s) = self.session.as_mut() {
            let i = color.index();
            if s.in_byo[i] {
                s.remaining[i] = s.tc.byo_period_s as f32;
            } else {
                s.remaining[i] += s.tc.increment_s as f32;
            }
        }
        self.snapshot_clock(game.cursor());
        if Self::two_passes(game) {
            self.end_game(game, None, "Both players passed.".to_string());
            return;
        }
        self.advance(game);
    }

    pub fn apply_ai_report(
        &mut self,
        game: &mut GameSession,
        report: &Report,
        temperature: f32,
        resign_threshold: f32,
        resign_required: u8,
    ) {
        if !matches!(self.state(), PlayState::AiThinking) {
            return;
        }
        let Some(ai) = self.ai() else {
            return;
        };
        let position_move = game.position().move_number;
        let board_points = game.position().board.size.points();
        let winrate = report.root.winrate_for(ai);
        let resign = {
            let Some(s) = self.session.as_mut() else {
                return;
            };
            let (streak, resign) = resign_check(
                winrate,
                resign_threshold,
                s.resign_streak,
                resign_required,
                position_move,
                board_points,
            );
            s.resign_streak = streak;
            resign
        };
        if resign {
            self.end_game(
                game,
                Some(format!("{}+R", ai.other().katago())),
                format!("{} resigned.", ai.name()),
            );
            return;
        }
        let choice = select_move_index(&report.moves, temperature, &mut self.rng);
        let mv = match choice {
            Some(i) => report.moves[i].mv,
            None => Point::PASS,
        };
        if game.play(mv).is_err() && mv != Point::PASS {
            let _ = game.play(Point::PASS);
        }
        self.after_move(game, ai);
    }

    pub fn tick(&mut self, game: &mut GameSession, dt: f32) -> bool {
        let active = match self.state() {
            PlayState::HumanTurn | PlayState::AiThinking => game.to_play(),
            PlayState::Idle | PlayState::AiStalled(_) | PlayState::Scoring | PlayState::Over(_) => {
                return false;
            }
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

    pub fn flag(&mut self, game: &mut GameSession, loser: Color) {
        self.end_game(
            game,
            Some(format!("{}+T", loser.other().katago())),
            format!("{} lost on time.", loser.name()),
        );
    }

    fn end_game(&mut self, game: &mut GameSession, forced: Option<String>, reason: String) {
        if let Some(s) = self.session.as_mut() {
            s.state = PlayState::Scoring;
            s.resign_streak = 0;
            s.reason = reason;
            s.forced_result = forced.clone();
        }
        if let Some(r) = &forced {
            game.set_result(r.clone());
        }
        if forced.is_some() {
            self.rescore(game);
            if let Some(s) = self.session.as_mut()
                && let Some(result) = s.forced_result.clone()
            {
                s.state = PlayState::Over(result);
            }
        } else {
            self.rescore(game);
        }
    }

    pub fn apply_ownership(&mut self, game: &mut GameSession, ownership: Option<&[i8]>) {
        let board = game.position().board.clone();
        let dead = match ownership {
            Some(raw) => dead_from_ownership(&board, raw),
            None => DeadSet::empty(board.size),
        };
        if let Some(s) = self.session.as_mut() {
            s.dead = dead;
        }
        let forced = self.session.as_ref().and_then(|s| s.forced_result.clone());
        self.rescore(game);
        if let Some(result) = forced
            && let Some(s) = self.session.as_mut()
        {
            s.state = PlayState::Over(result);
        }
    }

    pub fn toggle_dead(&mut self, game: &mut GameSession, p: Point) {
        if !matches!(self.state(), PlayState::Scoring) || p.is_pass() {
            return;
        }
        let board = game.position().board.clone();
        if board.at(p).is_none() {
            return;
        }
        if let Some(s) = self.session.as_mut() {
            s.dead.toggle_chain(&board, p);
        }
        self.rescore(game);
    }

    fn rescore(&mut self, game: &mut GameSession) {
        let position = game.position().clone();
        let (rules, komi, handicap) = {
            let t = game.tree();
            (t.info.rules, t.info.komi, t.info.handicap)
        };
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let counted = score(&position.board, &rules.rules(), komi, handicap, &s.dead);
        s.territory = counted.territory.clone();
        let counted_str = counted.result_string();
        s.summary = match &s.forced_result {
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
        let result = s.forced_result.clone().unwrap_or(counted_str);
        s.result = result.clone();
        game.set_result(result);
    }

    pub fn scoring_request(&mut self, game: &mut GameSession) -> AnalyzeReq {
        let mut req = game.request_for_cursor(Want::OWNERSHIP, 400);
        req.max_time_ms = None;
        req.report_every_ms = None;
        req.priority = 8;
        req
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
    fn result_phrases_read_like_english() {
        assert_eq!(result_phrase("B+R"), "Black wins by resignation");
        assert_eq!(result_phrase("W+T"), "White wins on time");
        assert_eq!(result_phrase("B+7.5"), "Black wins by 7.5");
        assert_eq!(result_phrase("0"), "Jigo — a draw");
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
}
