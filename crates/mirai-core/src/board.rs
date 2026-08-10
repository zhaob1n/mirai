// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Board state and move legality.
//!
//! The board only knows about *simple* ko (via [`Board::ko_ban`]); positional and
//! situational superko need the hash history of a whole game and are enforced by
//! [`crate::tree::GameTree`].

use std::sync::LazyLock;

use smallvec::SmallVec;

use crate::SplitMix64;
use crate::point::{Color, Point, Size};
use crate::rules::{Ko, Rules};

#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum IllegalMove {
    #[error("point is already occupied")]
    Occupied,
    #[error("move is suicide")]
    Suicide,
    #[error("move violates the ko rule")]
    Ko,
    #[error("point is off the board")]
    OffBoard,
}

/// Stones removed by a move.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Captured {
    pub points: SmallVec<[Point; 8]>,
}

impl Captured {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.points.len()
    }
}

/// The largest board KataGo's stock binary supports, and therefore ours.
const MAX_POINTS: usize = 361;

/// Mixed into the position hash when White is to move, so that a situational-superko
/// comparison distinguishes the two turn parities while a positional one can still
/// ignore them with a single XOR (see [`Board::situational_hash`]).
pub const WHITE_TO_MOVE_HASH: u64 = 0x4C57_9E24_1B3F_D8A7;

/// Zobrist table, seeded from a fixed constant so hashes are reproducible across runs
/// and machines.
static ZOBRIST: LazyLock<[[u64; MAX_POINTS]; 2]> = LazyLock::new(|| {
    let mut rng = SplitMix64::new(0x9E37_79B9_7F4A_7C15);
    let mut table = [[0u64; MAX_POINTS]; 2];
    for color in table.iter_mut() {
        for slot in color.iter_mut() {
            *slot = rng.next_u64();
        }
    }
    table
});

/// A 384-bit set over point indices, so flood fills need no heap-allocated visited map.
#[derive(Clone, Copy)]
struct Bits([u64; 6]);

impl Bits {
    const EMPTY: Bits = Bits([0; 6]);

    /// Inserts `i`, returning `true` when it was not already present.
    #[inline]
    fn insert(&mut self, i: usize) -> bool {
        let bit = 1u64 << (i % 64);
        let word = &mut self.0[i / 64];
        let fresh = *word & bit == 0;
        *word |= bit;
        fresh
    }
}

/// Minimal stack interface so the flood fill can run on the board's reusable scratch
/// buffer (`&mut self` paths) or on an inline `SmallVec` (`&self` paths) without ever
/// allocating.
trait Stack {
    fn clear(&mut self);
    fn push(&mut self, v: u16);
    fn pop(&mut self) -> Option<u16>;
}

impl Stack for Vec<u16> {
    #[inline]
    fn clear(&mut self) {
        Vec::clear(self)
    }
    #[inline]
    fn push(&mut self, v: u16) {
        Vec::push(self, v)
    }
    #[inline]
    fn pop(&mut self) -> Option<u16> {
        Vec::pop(self)
    }
}

impl Stack for SmallVec<[u16; 64]> {
    #[inline]
    fn clear(&mut self) {
        SmallVec::clear(self)
    }
    #[inline]
    fn push(&mut self, v: u16) {
        SmallVec::push(self, v)
    }
    #[inline]
    fn pop(&mut self) -> Option<u16> {
        SmallVec::pop(self)
    }
}

#[derive(Clone, Debug)]
pub struct Board {
    pub size: Size,
    stones: Box<[Option<Color>]>,
    pub captures: [u16; 2],
    zobrist: u64,
    ko_ban: Option<Point>,
    /// Flood-fill stack, kept across moves so `play` allocates nothing.
    scratch: Vec<u16>,
}

impl Board {
    pub fn new(size: Size) -> Board {
        let n = size.points();
        Board {
            size,
            stones: vec![None; n].into_boxed_slice(),
            captures: [0; 2],
            zobrist: 0,
            ko_ban: None,
            scratch: Vec::with_capacity(n),
        }
    }

    #[inline]
    pub fn stones(&self) -> &[Option<Color>] {
        &self.stones
    }

    #[inline]
    pub fn at(&self, p: Point) -> Option<Color> {
        if self.size.contains(p) {
            self.stones[p.index()]
        } else {
            None
        }
    }

    /// Position hash, ignoring whose turn it is.
    #[inline]
    pub fn zobrist(&self) -> u64 {
        self.zobrist
    }

    /// Position hash including the side to move, for situational superko.
    #[inline]
    pub fn situational_hash(&self, to_play: Color) -> u64 {
        match to_play {
            Color::Black => self.zobrist,
            Color::White => self.zobrist ^ WHITE_TO_MOVE_HASH,
        }
    }

    /// The point forbidden by simple ko, if any.
    #[inline]
    pub fn ko_ban(&self) -> Option<Point> {
        self.ko_ban
    }

    #[inline]
    pub fn stone_count(&self, color: Color) -> u32 {
        self.stones.iter().filter(|s| **s == Some(color)).count() as u32
    }

    /// Places or clears a stone without any legality or capture logic, for SGF setup
    /// properties (`AB` / `AW` / `AE`) and board editing. Clears any pending ko ban.
    pub fn set(&mut self, p: Point, c: Option<Color>) {
        if !self.size.contains(p) {
            return;
        }
        self.put(p, c);
        self.ko_ban = None;
    }

    /// All stones connected to `p` (empty when `p` is empty or off the board).
    pub fn chain(&self, p: Point) -> SmallVec<[Point; 32]> {
        let mut members = SmallVec::new();
        if !self.size.contains(p) {
            return members;
        }
        let mut stack: SmallVec<[u16; 64]> = SmallVec::new();
        walk(&self.stones, self.size, p, &mut stack, &mut members);
        members
    }

    /// Liberty count of the chain at `p` (0 when `p` is empty or off the board).
    pub fn liberties(&self, p: Point) -> u32 {
        if !self.size.contains(p) {
            return 0;
        }
        let mut stack: SmallVec<[u16; 64]> = SmallVec::new();
        let mut members = SmallVec::new();
        walk(&self.stones, self.size, p, &mut stack, &mut members)
    }

    /// Non-mutating legality test, equivalent to `play` succeeding. Does not consider
    /// superko, which needs the game's hash history.
    pub fn is_legal(&self, color: Color, p: Point, rules: &Rules) -> bool {
        if p.is_pass() {
            return true;
        }
        if !self.size.contains(p) || self.stones[p.index()].is_some() {
            return false;
        }
        if rules.ko == Ko::Simple && self.ko_ban == Some(p) {
            return false;
        }
        let mut stack: SmallVec<[u16; 64]> = SmallVec::new();
        let mut members = SmallVec::new();
        let mut has_empty_neighbor = false;
        let mut friendly_neighbor = false;
        let mut friendly_escape = false;
        for nb in self.size.neighbors(p) {
            match self.stones[nb.index()] {
                None => has_empty_neighbor = true,
                Some(c) if c == color => {
                    friendly_neighbor = true;
                    // One of this chain's liberties is `p` itself; a second one means the
                    // played stone lives.
                    if walk(&self.stones, self.size, nb, &mut stack, &mut members) >= 2 {
                        friendly_escape = true;
                    }
                }
                Some(_) => {
                    // An enemy chain whose only liberty is `p` is captured by the move.
                    if walk(&self.stones, self.size, nb, &mut stack, &mut members) == 1 {
                        return true;
                    }
                }
            }
        }
        if has_empty_neighbor || friendly_escape {
            return true;
        }
        // Suicide: legal only when the rules allow multi-stone suicide and the resulting
        // chain is bigger than one stone.
        rules.multi_stone_suicide && friendly_neighbor
    }

    /// Plays a move, updating captures, the position hash and the ko ban.
    ///
    /// `Point::PASS` is always legal, captures nothing and clears the ko ban.
    pub fn play(
        &mut self,
        color: Color,
        point: Point,
        rules: &Rules,
    ) -> Result<Captured, IllegalMove> {
        let mut captured = Captured::default();
        if point.is_pass() {
            self.ko_ban = None;
            return Ok(captured);
        }
        if !self.size.contains(point) {
            return Err(IllegalMove::OffBoard);
        }
        if self.stones[point.index()].is_some() {
            return Err(IllegalMove::Occupied);
        }
        if rules.ko == Ko::Simple && self.ko_ban == Some(point) {
            return Err(IllegalMove::Ko);
        }

        // Borrow the scratch stack out of `self` so the fill can run against `&self.stones`
        // while the board is being mutated. `take` leaves an empty Vec, so no allocation.
        let mut stack = std::mem::take(&mut self.scratch);
        let mut members: SmallVec<[Point; 32]> = SmallVec::new();

        self.put(point, Some(color));

        for nb in self.size.neighbors(point) {
            if self.stones[nb.index()] == Some(color.other())
                && walk(&self.stones, self.size, nb, &mut stack, &mut members) == 0
            {
                self.captures[color.index()] += members.len() as u16;
                for i in 0..members.len() {
                    let m = members[i];
                    self.put(m, None);
                    captured.points.push(m);
                }
            }
        }

        let liberties = walk(&self.stones, self.size, point, &mut stack, &mut members);
        let mut result = Ok(());
        if !captured.points.is_empty() {
            // A capture that takes exactly one stone with a single-stone, single-liberty
            // reply is the classic ko shape; nothing else can repeat immediately.
            self.ko_ban = if captured.points.len() == 1 && members.len() == 1 && liberties == 1 {
                Some(captured.points[0])
            } else {
                None
            };
        } else if liberties == 0 {
            if rules.multi_stone_suicide && members.len() > 1 {
                self.captures[color.other().index()] += members.len() as u16;
                for i in 0..members.len() {
                    let m = members[i];
                    self.put(m, None);
                    captured.points.push(m);
                }
                self.ko_ban = None;
            } else {
                // Nothing was captured, so undoing the placement restores the board.
                self.put(point, None);
                result = Err(IllegalMove::Suicide);
            }
        } else {
            self.ko_ban = None;
        }

        self.scratch = stack;
        result.map(|()| captured)
    }

    #[inline]
    fn put(&mut self, p: Point, c: Option<Color>) {
        let i = p.index();
        let table = &*ZOBRIST;
        if let Some(old) = self.stones[i] {
            self.zobrist ^= table[old.index()][i];
        }
        self.stones[i] = c;
        if let Some(new) = c {
            self.zobrist ^= table[new.index()][i];
        }
    }
}

/// Explicit-stack flood fill over the chain at `start`. Fills `members` with the chain's
/// stones and returns its liberty count. Both buffers are cleared first.
fn walk<S: Stack>(
    stones: &[Option<Color>],
    size: Size,
    start: Point,
    stack: &mut S,
    members: &mut SmallVec<[Point; 32]>,
) -> u32 {
    members.clear();
    stack.clear();
    let Some(color) = stones[start.index()] else {
        return 0;
    };
    let mut seen = Bits::EMPTY;
    let mut libs = Bits::EMPTY;
    let mut n_libs = 0;
    seen.insert(start.index());
    stack.push(start.0);
    while let Some(cur) = stack.pop() {
        let cur = Point(cur);
        members.push(cur);
        for nb in size.neighbors(cur) {
            match stones[nb.index()] {
                None => {
                    if libs.insert(nb.index()) {
                        n_libs += 1;
                    }
                }
                Some(c) if c == color => {
                    if seen.insert(nb.index()) {
                        stack.push(nb.0);
                    }
                }
                Some(_) => {}
            }
        }
    }
    n_libs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RuleSet;

    fn b9() -> Board {
        Board::new(Size::square(9))
    }

    fn play(board: &mut Board, rules: &Rules, color: Color, gtp: &str) -> Captured {
        let p = board.size.from_gtp(gtp).expect("coordinate");
        assert!(
            board.is_legal(color, p, rules),
            "is_legal disagrees with play for {gtp}"
        );
        board.play(color, p, rules).expect("legal move")
    }

    #[test]
    fn capture_updates_counts_and_hash() {
        let rules = RuleSet::Chinese.rules();
        let mut board = b9();
        let empty = board.zobrist();
        play(&mut board, &rules, Color::Black, "D4");
        play(&mut board, &rules, Color::White, "D3");
        play(&mut board, &rules, Color::Black, "C3");
        play(&mut board, &rules, Color::White, "A1");
        play(&mut board, &rules, Color::Black, "E3");
        play(&mut board, &rules, Color::White, "A2");
        let captured = play(&mut board, &rules, Color::Black, "D2");
        assert_eq!(captured.points.len(), 1);
        assert_eq!(board.at(board.size.from_gtp("D3").unwrap()), None);
        assert_eq!(board.captures[Color::Black.index()], 1);
        assert_ne!(board.zobrist(), empty);
    }

    /// The textbook ko shape: a lone White stone whose only liberty is the point Black
    /// plays, and a Black stone left with exactly one liberty afterwards.
    #[test]
    fn simple_ko_is_banned_for_exactly_one_move() {
        let rules = RuleSet::Chinese.rules();
        let mut board = b9();
        let s = board.size;
        for (x, y) in [(2, 4), (3, 3), (3, 5)] {
            board.set(s.point(x, y), Some(Color::Black));
        }
        for (x, y) in [(3, 4), (4, 3), (5, 4), (4, 5)] {
            board.set(s.point(x, y), Some(Color::White));
        }
        let target = s.point(3, 4);
        let take = s.point(4, 4);
        assert_eq!(board.liberties(target), 1);

        assert!(board.is_legal(Color::Black, take, &rules));
        let taken = board.play(Color::Black, take, &rules).expect("capture");
        assert_eq!(&taken.points[..], &[target]);
        assert_eq!(board.ko_ban(), Some(target));

        // White may not take back immediately...
        assert!(!board.is_legal(Color::White, target, &rules));
        assert_eq!(
            board.play(Color::White, target, &rules),
            Err(IllegalMove::Ko)
        );
        // ...but one move later the ban is gone.
        play(&mut board, &rules, Color::White, "A9");
        assert_eq!(board.ko_ban(), None);
        assert!(board.is_legal(Color::White, target, &rules));
        let retaken = board.play(Color::White, target, &rules).expect("recapture");
        assert_eq!(&retaken.points[..], &[take]);
    }

    #[test]
    fn single_stone_suicide_illegal_under_every_ruleset() {
        for rs in RuleSet::ALL {
            let rules = rs.rules();
            let mut board = b9();
            // White surrounds A1 (the bottom-left corner).
            for mv in ["A2", "B1"] {
                let p = board.size.from_gtp(mv).unwrap();
                board.set(p, Some(Color::White));
            }
            let a1 = board.size.from_gtp("A1").unwrap();
            assert!(
                !board.is_legal(Color::Black, a1, &rules),
                "{rs:?} allowed single-stone suicide"
            );
            assert_eq!(
                board.play(Color::Black, a1, &rules),
                Err(IllegalMove::Suicide),
                "{rs:?} allowed single-stone suicide"
            );
        }
    }

    #[test]
    fn multi_stone_suicide_follows_the_ruleset() {
        // Black fills its own two-stone eye space at A1/A2, surrounded by White.
        let build = || {
            let mut board = b9();
            for mv in ["A3", "B1", "B2"] {
                let p = board.size.from_gtp(mv).unwrap();
                board.set(p, Some(Color::White));
            }
            let p = board.size.from_gtp("A1").unwrap();
            board.set(p, Some(Color::Black));
            board
        };
        for rs in [RuleSet::NewZealand, RuleSet::TrompTaylor] {
            let rules = rs.rules();
            let mut board = build();
            let a2 = board.size.from_gtp("A2").unwrap();
            assert!(board.is_legal(Color::Black, a2, &rules), "{rs:?}");
            let captured = board.play(Color::Black, a2, &rules).expect("legal");
            assert_eq!(captured.points.len(), 2, "{rs:?} must remove both stones");
            assert_eq!(board.at(a2), None);
            assert_eq!(board.captures[Color::White.index()], 2);
        }
        for rs in [RuleSet::Chinese, RuleSet::Japanese, RuleSet::Aga] {
            let rules = rs.rules();
            let mut board = build();
            let a2 = board.size.from_gtp("A2").unwrap();
            assert!(!board.is_legal(Color::Black, a2, &rules), "{rs:?}");
            assert_eq!(
                board.play(Color::Black, a2, &rules),
                Err(IllegalMove::Suicide),
                "{rs:?}"
            );
        }
    }

    #[test]
    fn pass_is_legal_and_clears_ko() {
        let rules = RuleSet::Chinese.rules();
        let mut board = b9();
        let before = board.zobrist();
        let captured = board.play(Color::Black, Point::PASS, &rules).expect("pass");
        assert!(captured.is_empty());
        assert_eq!(board.zobrist(), before);
        assert_eq!(board.ko_ban(), None);
    }

    #[test]
    fn hashes_are_order_independent_and_turn_aware() {
        let rules = RuleSet::Chinese.rules();
        let mut a = b9();
        play(&mut a, &rules, Color::Black, "D4");
        play(&mut a, &rules, Color::White, "G7");
        let mut b = b9();
        b.set(b.size.from_gtp("G7").unwrap(), Some(Color::White));
        b.set(b.size.from_gtp("D4").unwrap(), Some(Color::Black));
        assert_eq!(a.zobrist(), b.zobrist());
        assert_ne!(
            a.situational_hash(Color::Black),
            a.situational_hash(Color::White)
        );
    }

    #[test]
    fn chain_and_liberties() {
        let mut board = b9();
        for mv in ["D4", "D5", "D6"] {
            let p = board.size.from_gtp(mv).unwrap();
            board.set(p, Some(Color::Black));
        }
        let d5 = board.size.from_gtp("D5").unwrap();
        assert_eq!(board.chain(d5).len(), 3);
        // Three stones in a line have eight liberties.
        assert_eq!(board.liberties(d5), 8);
        assert!(board.chain(board.size.from_gtp("A1").unwrap()).is_empty());
    }

    /// `is_legal` is the promise the GUI trusts before it lets the user click, so it must
    /// agree with `play` on every point of every position — including the awkward ones a
    /// random game produces (snapbacks, self-atari, multi-stone suicide, ko).
    #[test]
    fn is_legal_agrees_with_play_over_random_games() {
        for rs in [RuleSet::Chinese, RuleSet::NewZealand, RuleSet::TrompTaylor] {
            let rules = rs.rules();
            let mut rng = crate::SplitMix64::new(0xC0FF_EE00_1234_5678);
            for _game in 0..8 {
                let mut board = Board::new(Size::square(7));
                let mut color = Color::Black;
                for _ply in 0..120 {
                    let n = board.size.points();
                    for i in 0..n {
                        let p = Point(i as u16);
                        let claim = board.is_legal(color, p, &rules);
                        let mut probe = board.clone();
                        let truth = probe.play(color, p, &rules).is_ok();
                        assert_eq!(claim, truth, "{rs:?} disagreed on {p:?}");
                    }
                    // Play a random legal move, or pass when there is none.
                    let start = (rng.next_u64() % n as u64) as usize;
                    let choice = (0..n)
                        .map(|k| Point(((start + k) % n) as u16))
                        .find(|&p| board.is_legal(color, p, &rules));
                    match choice {
                        Some(p) => board.play(color, p, &rules).expect("legal"),
                        None => board.play(color, Point::PASS, &rules).expect("pass"),
                    };
                    color = color.other();
                }
            }
        }
    }
}
