// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Board geometry, rules, scoring, game tree and SGF for mirai.
//!
//! Conventions that hold across the whole workspace:
//!
//! * Points are KataGo-ordered: `index = y * width + x`, `y = 0` is the **top** row
//!   (see [`point`]). This matches the ordering of KataGo's `ownership` / `policy` arrays.
//! * Every engine-derived value is stored from **Black's** perspective; the UI converts to
//!   side-to-move at display time. Utility and `utilityLcb` flip by sign, like score lead.

pub mod board;
pub mod clock;
pub mod handicap;
pub mod point;
pub mod rules;
pub mod score;
pub mod sgf;
pub mod tree;

pub use board::{Board, Captured, IllegalMove};
pub use clock::{TimeControl, clock_text, think_budget};
pub use handicap::fixed_handicap;
pub use point::{COLUMNS, Color, MAX_DIM, MIN_DIM, Point, Size};
pub use rules::{Ko, RuleSet, Rules, Scoring, Tax, Whb};
pub use score::{DeadSet, ScoreResult, score};
pub use sgf::SgfError;
pub use tree::{
    Candidate, GameInfo, GameTree, MarkKind, Marks, Node, NodeAnalysis, NodeId, PlayerInfo,
    Position, Setup,
};

/// A tiny deterministic PRNG (SplitMix64), used for Zobrist tables and play-mode sampling.
/// Deterministic across runs and machines, which the Zobrist contract requires.
#[derive(Clone, Debug)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    #[inline]
    pub const fn new(seed: u64) -> SplitMix64 {
        SplitMix64(seed)
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// A `std::io::Write` sink that refuses to grow a `Vec` past `limit` — the
/// decompression-bomb guard both the SGF analysis blob and the MRP/1 frame codec need.
pub struct BoundedWriter<'a> {
    out: &'a mut Vec<u8>,
    limit: usize,
    /// Names the payload in the error, e.g. `"frame"` or `"analysis"`.
    what: &'static str,
}

impl<'a> BoundedWriter<'a> {
    pub fn new(out: &'a mut Vec<u8>, limit: usize, what: &'static str) -> BoundedWriter<'a> {
        BoundedWriter { out, limit, what }
    }
}

impl std::io::Write for BoundedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.out.len() + buf.len() > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("decompressed {} exceeds the {} limit", self.what, self.what),
            ));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
