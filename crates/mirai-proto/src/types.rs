// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! MRP/1 value types.
//!
//! Quantisation is part of the type: every float that crosses the wire is stored as a
//! fixed-point integer, and the conversion helpers live here so that the local KataGo
//! decoder and the remote server quantise identically. A live report is ~2.5 KB instead
//! of the ~45 KB of equivalent KataGo JSON.
//!
//! All values are from **Black's** perspective (mirai always launches KataGo with
//! `reportAnalysisWinratesAs = BLACK`).

use mirai_core::{Color, Point, RuleSet, Size};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const PROTO_VERSION: u16 = 1;

/// Sentinel stored in `Report::policy` for a move KataGo reported as illegal (`-1`).
pub const POLICY_ILLEGAL: u16 = u16::MAX;

pub const LCB_SCALE: f64 = 16384.0;
pub const UTILITY_SCALE: f64 = 8192.0;
pub const SCORE_SCALE: f64 = 32.0;
pub const STDEV_SCALE: f64 = 32.0;

/// Steps per unit for [`RootInfo::raw_var_time_left`]. KataGo's `rawVarTimeLeft` is in "no
/// particular units" and runs to a few hundred, so a quarter-unit step covers it in `u16`.
///
/// It lives here with the other scales rather than beside its decoder: a peer decoding MRP/1
/// must be able to learn every scale from this crate alone.
pub const RAW_VAR_TIME_SCALE: f64 = 4.0;

/// Quantise a probability in `[0, 1]` to 16 bits.
#[inline]
pub fn q16(v: f64) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

/// Inverse of [`q16`].
#[inline]
pub fn dq16(v: u16) -> f32 {
    v as f32 / 65535.0
}

/// Quantise a signed value at `scale` steps per unit.
#[inline]
pub fn qs(v: f64, scale: f64) -> i16 {
    if v.is_nan() {
        return 0;
    }
    (v * scale).round().clamp(-32767.0, 32767.0) as i16
}

/// Inverse of [`qs`].
#[inline]
pub fn dqs(v: i16, scale: f64) -> f32 {
    (v as f64 / scale) as f32
}

/// Quantise a non-negative value at `scale` steps per unit.
#[inline]
pub fn qu(v: f64, scale: f64) -> u16 {
    if v.is_nan() {
        return 0;
    }
    (v * scale).round().clamp(0.0, 65535.0) as u16
}

/// Inverse of [`qu`].
#[inline]
pub fn dqu(v: u16, scale: f64) -> f32 {
    (v as f64 / scale) as f32
}

/// Quantise one ownership value in `[-1, 1]`.
#[inline]
pub fn q_own(v: f64) -> i8 {
    (v * 127.0).round().clamp(-127.0, 127.0) as i8
}

/// Inverse of [`q_own`].
#[inline]
pub fn dq_own(v: i8) -> f32 {
    v as f32 / 127.0
}

/// Quantise one policy value. KataGo reports `-1` for illegal moves.
#[inline]
pub fn q_policy(v: f64) -> u16 {
    if v < 0.0 {
        POLICY_ILLEGAL
    } else {
        (v * 65534.0).round().clamp(0.0, 65534.0) as u16
    }
}

/// Inverse of [`q_policy`]; `None` for an illegal move.
#[inline]
pub fn dq_policy(v: u16) -> Option<f32> {
    if v == POLICY_ILLEGAL {
        None
    } else {
        Some(v as f32 / 65534.0)
    }
}

bitflags::bitflags! {
    /// Optional extras a client wants in the reports.
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct Want: u8 {
        /// Fill [`Report::ownership`].
        const OWNERSHIP = 1;
        /// Fill [`Report::policy`].
        const POLICY = 2;
        /// Fill [`MoveInfo::pv_visits`].
        const PV_VISITS = 4;
        /// Reserved. `MoveInfo` has no per-move ownership field, so there is nowhere for the
        /// result to land; a v1 engine MUST NOT ask KataGo for it and burn search time on a
        /// value it will discard. Kept so the bit is not reused before a v2 adds the field.
        const MOVES_OWNERSHIP = 8;
        /// Advisory. The `raw_*` fields of [`RootInfo`] are filled whenever the engine
        /// reported them, whether or not this bit is set — KataGo has no switch for it.
        /// Set it for forward compatibility; never rely on it to suppress them.
        const ROOT_RAW = 16;
    }
}

impl Serialize for Want {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.bits())
    }
}

impl<'de> Deserialize<'de> for Want {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Want, D::Error> {
        Ok(Want::from_bits_truncate(u8::deserialize(d)?))
    }
}

/// A restriction on which moves the search may consider.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AvoidSpec {
    pub player: Color,
    pub moves: Vec<Point>,
    pub until_depth: u16,
    /// `true` turns this into an `allowMoves` entry instead of `avoidMoves`.
    pub allow: bool,
}

/// One position to analyse. Stateless: it carries the whole position.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AnalyzeReq {
    pub size: Size,
    pub rules: RuleSet,
    /// `komi * 2`; KataGo requires an integer or half-integer komi.
    pub komi_x2: i16,
    pub initial_stones: Vec<(Color, Point)>,
    pub moves: Vec<(Color, Point)>,
    pub initial_player: Option<Color>,
    pub max_visits: Option<u32>,
    pub max_time_ms: Option<u32>,
    pub pv_len: Option<u8>,
    pub want: Want,
    pub report_every_ms: Option<u16>,
    pub priority: i8,
    pub avoid: Vec<AvoidSpec>,
    pub overrides: Vec<(String, String)>,
}

impl AnalyzeReq {
    /// A request for the given position with everything optional left off.
    pub fn new(size: Size, rules: RuleSet, komi: f32) -> AnalyzeReq {
        AnalyzeReq {
            size,
            rules,
            komi_x2: (komi * 2.0).round() as i16,
            initial_stones: Vec::new(),
            moves: Vec::new(),
            initial_player: None,
            max_visits: None,
            max_time_ms: None,
            pv_len: None,
            want: Want::empty(),
            report_every_ms: None,
            priority: 0,
            avoid: Vec::new(),
            overrides: Vec::new(),
        }
    }

    #[inline]
    pub fn komi(&self) -> f32 {
        self.komi_x2 as f32 / 2.0
    }

    /// The colour to move at the analysed position.
    pub fn to_play(&self) -> Color {
        match self.moves.last() {
            Some(&(c, _)) => c.other(),
            None => self
                .initial_player
                .unwrap_or(if self.initial_stones.is_empty() {
                    Color::Black
                } else {
                    Color::White
                }),
        }
    }

    /// The turn number of the analysed position (0 = the position before the first move).
    #[inline]
    pub fn turn(&self) -> u16 {
        self.moves.len() as u16
    }
}

/// One candidate move. Every float is quantised; use the `*_f32` accessors to read it back.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MoveInfo {
    pub mv: Point,
    pub visits: u32,
    pub edge_visits: u32,
    /// `q16(winrate)`, Black's perspective.
    pub winrate: u16,
    /// `q16(prior)`.
    pub prior: u16,
    /// `qs(lcb, LCB_SCALE)`.
    pub lcb: i16,
    /// `qs(utility, UTILITY_SCALE)`.
    pub utility: i16,
    /// `qs(utilityLcb, UTILITY_SCALE)`.
    ///
    /// Unlike every other quantised field this one **saturates in normal use**: KataGo's LCB
    /// carries a confidence radius of `lcbStdevs * stdev / sqrt(ess)`, which is 3.5 utility at
    /// one visit and `2 * utilityRange * lcbStdevs` (−14 at the analysis defaults) at none, so
    /// a barely-searched child clips to ±3.99988. Measured on a 42-position sweep at 5 000
    /// visits, 18 % of candidates clip — every one of them a one-visit policy-tail move. A
    /// consumer may read a clipped value as "not searched"; it must not read it as a magnitude.
    pub utility_lcb: i16,
    /// `qs(scoreLead, SCORE_SCALE)`, Black's perspective, points.
    pub score_lead: i16,
    /// `qs(scoreSelfplay, SCORE_SCALE)`.
    pub score_selfplay: i16,
    /// `qu(scoreStdev, STDEV_SCALE)`.
    pub score_stdev: u16,
    pub order: u8,
    /// KataGo's `playSelectionValue`, rounded. Drives play-mode move sampling.
    pub play_value: u32,
    pub pv: Vec<Point>,
    /// Empty unless [`Want::PV_VISITS`] was requested.
    pub pv_visits: Vec<u32>,
}

impl MoveInfo {
    #[inline]
    pub fn winrate_f32(&self) -> f32 {
        dq16(self.winrate)
    }
    #[inline]
    pub fn prior_f32(&self) -> f32 {
        dq16(self.prior)
    }
    #[inline]
    pub fn lcb_f32(&self) -> f32 {
        dqs(self.lcb, LCB_SCALE)
    }
    #[inline]
    pub fn utility_f32(&self) -> f32 {
        dqs(self.utility, UTILITY_SCALE)
    }
    #[inline]
    pub fn utility_lcb_f32(&self) -> f32 {
        dqs(self.utility_lcb, UTILITY_SCALE)
    }
    #[inline]
    pub fn score_lead_f32(&self) -> f32 {
        dqs(self.score_lead, SCORE_SCALE)
    }
    #[inline]
    pub fn score_selfplay_f32(&self) -> f32 {
        dqs(self.score_selfplay, SCORE_SCALE)
    }
    #[inline]
    pub fn score_stdev_f32(&self) -> f32 {
        dqu(self.score_stdev, STDEV_SCALE)
    }

    /// Win rate as seen by `to_play` (the UI convention).
    #[inline]
    pub fn winrate_for(&self, to_play: Color) -> f32 {
        to_play.winrate_for(self.winrate_f32())
    }

    /// Score lead as seen by `to_play`, in points.
    #[inline]
    pub fn score_lead_for(&self, to_play: Color) -> f32 {
        to_play.score_lead_for(self.score_lead_f32())
    }

    /// KataGo utility as seen by `to_play`.
    #[inline]
    pub fn utility_for(&self, to_play: Color) -> f32 {
        to_play.utility_for(self.utility_f32())
    }

    /// Lower confidence bound on utility as seen by `to_play`.
    #[inline]
    pub fn utility_lcb_for(&self, to_play: Color) -> f32 {
        to_play.utility_for(self.utility_lcb_f32())
    }
}

/// Root statistics for the analysed position.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RootInfo {
    pub visits: u32,
    pub winrate: u16,
    pub score_lead: i16,
    pub score_selfplay: i16,
    pub score_stdev: u16,
    pub utility: i16,
    pub current_player: Color,
    /// Raw (single-evaluation) net output; only present with [`Want::ROOT_RAW`].
    pub raw_winrate: Option<u16>,
    pub raw_lead: Option<i16>,
    pub raw_var_time_left: Option<u16>,
}

impl RootInfo {
    #[inline]
    pub fn winrate_f32(&self) -> f32 {
        dq16(self.winrate)
    }
    #[inline]
    pub fn score_lead_f32(&self) -> f32 {
        dqs(self.score_lead, SCORE_SCALE)
    }
    #[inline]
    pub fn score_selfplay_f32(&self) -> f32 {
        dqs(self.score_selfplay, SCORE_SCALE)
    }
    #[inline]
    pub fn score_stdev_f32(&self) -> f32 {
        dqu(self.score_stdev, STDEV_SCALE)
    }
    #[inline]
    pub fn utility_f32(&self) -> f32 {
        dqs(self.utility, UTILITY_SCALE)
    }
    #[inline]
    pub fn winrate_for(&self, to_play: Color) -> f32 {
        to_play.winrate_for(self.winrate_f32())
    }
    #[inline]
    pub fn score_lead_for(&self, to_play: Color) -> f32 {
        to_play.score_lead_for(self.score_lead_f32())
    }
}

/// One analysis result for one position. Intermediate (`isDuringSearch`) reports and the
/// final report have the same shape.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Report {
    pub turn: u16,
    pub root: RootInfo,
    /// Ordered by KataGo's `order` field: `moves[0]` is the engine's choice.
    pub moves: Vec<MoveInfo>,
    /// `w * h` entries in row-major, top-left-origin order — the same order as [`Point`].
    pub ownership: Option<Vec<i8>>,
    /// `w * h + 1` entries; the last is pass. [`POLICY_ILLEGAL`] marks an illegal move.
    pub policy: Option<Vec<u16>>,
}

impl Report {
    /// An empty report for `turn`, used as the initial `watch` value.
    pub fn empty(turn: u16, current_player: Color) -> Report {
        Report {
            turn,
            root: RootInfo {
                visits: 0,
                winrate: q16(0.5),
                score_lead: 0,
                score_selfplay: 0,
                score_stdev: 0,
                utility: 0,
                current_player,
                raw_winrate: None,
                raw_lead: None,
                raw_var_time_left: None,
            },
            moves: Vec::new(),
            ownership: None,
            policy: None,
        }
    }

    /// The most-visited candidate's visit count, for relative-visit colouring.
    pub fn best_visits(&self) -> u32 {
        self.moves.iter().map(|m| m.visits).max().unwrap_or(0)
    }
}

/// What a server (or a local process) can analyse.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EngineDesc {
    pub name: String,
    pub katago_version: String,
    pub model: String,
    pub analysis_threads: u16,
    pub max_board: Size,
    pub has_human_model: bool,
}

impl EngineDesc {
    pub fn placeholder(name: impl Into<String>) -> EngineDesc {
        EngineDesc {
            name: name.into(),
            katago_version: String::new(),
            model: String::new(),
            analysis_threads: 1,
            max_board: Size::square(19),
            has_human_model: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantisation_round_trips_within_tolerance() {
        for &w in &[0.0f64, 0.0001, 0.5, 0.4837, 0.99991, 1.0] {
            assert!((dq16(q16(w)) as f64 - w).abs() <= 1e-4, "winrate {w}");
        }
        for &l in &[-1023.0f64, -7.5, -0.03, 0.0, 3.25, 512.125, 1023.0] {
            let back = dqs(qs(l, SCORE_SCALE), SCORE_SCALE) as f64;
            assert!((back - l).abs() <= 0.02, "lead {l} -> {back}");
        }
        for i in -127..=127i32 {
            let v = i as f64 / 127.0;
            assert!((dq_own(q_own(v)) as f64 - v).abs() <= 0.005);
        }
        assert_eq!(dq_policy(q_policy(-1.0)), None);
        assert!((dq_policy(q_policy(0.25)).unwrap() - 0.25).abs() < 1e-4);
    }

    #[test]
    fn quantisation_clamps_out_of_range() {
        assert_eq!(q16(2.0), 65535);
        assert_eq!(q16(-1.0), 0);
        assert_eq!(qs(f64::INFINITY, SCORE_SCALE), 32767);
        assert_eq!(qs(f64::NAN, SCORE_SCALE), 0);
        assert_eq!(qu(-5.0, STDEV_SCALE), 0);
    }

    #[test]
    fn want_serialises_as_one_byte() {
        let w = Want::OWNERSHIP | Want::PV_VISITS;
        let bytes = postcard::to_stdvec(&w).unwrap();
        assert_eq!(bytes, vec![5]);
        assert_eq!(postcard::from_bytes::<Want>(&bytes).unwrap(), w);
    }

    #[test]
    fn perspective_conversion_flips_for_white() {
        let mut m = MoveInfo {
            mv: Point(0),
            visits: 1,
            edge_visits: 1,
            winrate: q16(0.75),
            prior: 0,
            lcb: 0,
            utility: 0,
            utility_lcb: 0,
            score_lead: qs(4.0, SCORE_SCALE),
            score_selfplay: 0,
            score_stdev: 0,
            order: 0,
            play_value: 0,
            pv: Vec::new(),
            pv_visits: Vec::new(),
        };
        assert!((m.winrate_for(Color::Black) - 0.75).abs() < 1e-4);
        assert!((m.winrate_for(Color::White) - 0.25).abs() < 1e-4);
        assert!((m.score_lead_for(Color::White) + 4.0).abs() < 0.02);
        m.utility = qs(0.12, UTILITY_SCALE);
        m.utility_lcb = qs(0.08, UTILITY_SCALE);
        assert!((m.utility_for(Color::Black) - 0.12).abs() < 2e-4);
        assert!((m.utility_lcb_for(Color::White) + 0.08).abs() < 2e-4);
        m.winrate = q16(0.25);
        assert!((m.winrate_for(Color::White) - 0.75).abs() < 1e-4);
    }

    #[test]
    fn to_play_follows_the_move_list() {
        let mut r = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        assert_eq!(r.to_play(), Color::Black);
        r.moves.push((Color::Black, Point(0)));
        assert_eq!(r.to_play(), Color::White);
        assert_eq!(r.turn(), 1);
        r.moves.clear();
        r.initial_stones.push((Color::Black, Point(0)));
        assert_eq!(r.to_play(), Color::White);
        r.initial_player = Some(Color::Black);
        assert_eq!(r.to_play(), Color::Black);
    }
}
