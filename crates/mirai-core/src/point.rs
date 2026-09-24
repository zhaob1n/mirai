// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Board geometry.
//!
//! Point encoding is KataGo's: `index = y * width + x`, with **`y = 0` the TOP row**.
//! This is byte-identical to the ordering of KataGo's `ownership` / `policy` arrays, so
//! overlays need no index remapping.

use arrayvec::ArrayString;
use serde::{Deserialize, Serialize};

/// A board intersection, or [`Point::PASS`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Point(pub u16);

impl Point {
    pub const PASS: Point = Point(u16::MAX);

    #[inline]
    pub const fn is_pass(self) -> bool {
        self.0 == u16::MAX
    }

    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl core::fmt::Debug for Point {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_pass() {
            f.write_str("PASS")
        } else {
            write!(f, "P({})", self.0)
        }
    }
}

/// Board dimensions. Both dimensions are constrained to `2..=19` (KataGo's stock `MAX_LEN`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Size {
    pub w: u8,
    pub h: u8,
}

pub const MIN_DIM: u8 = 2;
pub const MAX_DIM: u8 = 19;

/// GTP column letters: the alphabet without `I`.
pub const COLUMNS: &[u8; 25] = b"ABCDEFGHJKLMNOPQRSTUVWXYZ";

impl Size {
    #[inline]
    pub const fn new(w: u8, h: u8) -> Option<Size> {
        if w < MIN_DIM || w > MAX_DIM || h < MIN_DIM || h > MAX_DIM {
            None
        } else {
            Some(Size { w, h })
        }
    }

    /// Convenience for square boards. Panics on an out-of-range dimension; use
    /// [`Size::new`] for untrusted input.
    #[inline]
    pub const fn square(n: u8) -> Size {
        match Size::new(n, n) {
            Some(s) => s,
            None => panic!("board dimension out of range"),
        }
    }

    #[inline]
    pub const fn points(self) -> usize {
        self.w as usize * self.h as usize
    }

    #[inline]
    pub const fn is_square(self) -> bool {
        self.w == self.h
    }

    #[inline]
    pub const fn contains(self, p: Point) -> bool {
        !p.is_pass() && (p.0 as usize) < self.points()
    }

    #[inline]
    pub const fn xy(self, p: Point) -> (u8, u8) {
        debug_assert!(!p.is_pass());
        ((p.0 % self.w as u16) as u8, (p.0 / self.w as u16) as u8)
    }

    #[inline]
    pub const fn point(self, x: u8, y: u8) -> Point {
        Point(y as u16 * self.w as u16 + x as u16)
    }

    /// `Some` when `(x, y)` is on the board.
    #[inline]
    pub const fn try_point(self, x: i32, y: i32) -> Option<Point> {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            None
        } else {
            Some(Point(y as u16 * self.w as u16 + x as u16))
        }
    }

    /// GTP / KataGo coordinate, e.g. `A19` for `Point(0)` on 19x19. `PASS` renders `"pass"`.
    ///
    /// A point this size does not contain renders `"?"`: labels are drawn from analysis a
    /// file or a server supplied, and a bad one must read as nonsense, not panic.
    pub fn to_gtp(self, p: Point) -> ArrayString<4> {
        let mut s = ArrayString::new();
        if p.is_pass() {
            s.push_str("pass");
            return s;
        }
        if !self.contains(p) {
            s.push('?');
            return s;
        }
        let (x, y) = self.xy(p);
        s.push(COLUMNS[x as usize] as char);
        let row = self.h - y;
        if row >= 10 {
            s.push((b'0' + row / 10) as char);
        }
        s.push((b'0' + row % 10) as char);
        s
    }

    /// Parses a GTP coordinate. Case-insensitive; accepts `pass`.
    pub fn from_gtp(self, s: &str) -> Option<Point> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("pass") {
            return Some(Point::PASS);
        }
        let b = s.as_bytes();
        let (&head, tail) = b.split_first()?;
        let col = COLUMNS
            .iter()
            .position(|&c| c == head.to_ascii_uppercase())?;
        if tail.is_empty() || !tail.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let mut row: u32 = 0;
        for &d in tail {
            row = row.checked_mul(10)?.checked_add((d - b'0') as u32)?;
            if row > 255 {
                return None;
            }
        }
        if row == 0 || row > self.h as u32 || col >= self.w as usize {
            return None;
        }
        let y = self.h - row as u8;
        Some(self.point(col as u8, y))
    }

    /// SGF coordinate bytes. Both SGF and our encoding put the origin at the top left,
    /// so this is a direct mapping with no flip.
    #[inline]
    pub fn to_sgf(self, p: Point) -> [u8; 2] {
        debug_assert!(!p.is_pass());
        let (x, y) = self.xy(p);
        [b'a' + x, b'a' + y]
    }

    /// Parses an SGF point value. An empty value is a pass; so is `"tt"` on boards <= 19.
    pub fn from_sgf(self, v: &[u8]) -> Option<Point> {
        if v.is_empty() {
            return Some(Point::PASS);
        }
        if v.len() != 2 {
            return None;
        }
        let x = sgf_axis(v[0])?;
        let y = sgf_axis(v[1])?;
        if x >= self.w || y >= self.h {
            // `tt` is the classic FF[3] pass on boards up to 19x19.
            if v == b"tt" && self.w <= 19 && self.h <= 19 {
                return Some(Point::PASS);
            }
            return None;
        }
        Some(self.point(x, y))
    }

    /// The four orthogonal neighbours of `p`, in a fixed order.
    #[inline]
    pub fn neighbors(self, p: Point) -> Neighbors {
        let (x, y) = self.xy(p);
        Neighbors {
            size: self,
            x: x as i32,
            y: y as i32,
            i: 0,
        }
    }

    /// Star point (hoshi) coordinates for this board, used for drawing and handicap.
    pub fn star_points(self) -> smallvec::SmallVec<[Point; 9]> {
        let mut out = smallvec::SmallVec::new();
        let edge = |n: u8| -> Option<u8> {
            match n {
                0..=6 => None,
                7..=12 => Some(2),
                _ => Some(3),
            }
        };
        let (Some(ex), Some(ey)) = (edge(self.w), edge(self.h)) else {
            return out;
        };
        // Corners are always drawn; the centre only on odd boards; the four side
        // midpoints only on boards large enough to carry them (19x19 gets nine).
        let cx = self.w / 2;
        let cy = self.h / 2;
        let odd = self.w % 2 == 1 && self.h % 2 == 1 && self.w >= 9 && self.h >= 9;
        let sides = odd && self.w >= 17 && self.h >= 17;
        let mut push = |p: Point| {
            if !out.contains(&p) {
                out.push(p);
            }
        };
        for &y in &[ey, self.h - 1 - ey] {
            for &x in &[ex, self.w - 1 - ex] {
                push(self.point(x, y));
            }
        }
        if odd {
            push(self.point(cx, cy));
        }
        if sides {
            push(self.point(ex, cy));
            push(self.point(self.w - 1 - ex, cy));
            push(self.point(cx, ey));
            push(self.point(cx, self.h - 1 - ey));
        }
        out
    }
}

#[inline]
fn sgf_axis(b: u8) -> Option<u8> {
    match b {
        b'a'..=b'z' => Some(b - b'a'),
        b'A'..=b'Z' => Some(b - b'A' + 26),
        _ => None,
    }
}

pub struct Neighbors {
    size: Size,
    x: i32,
    y: i32,
    i: u8,
}

impl Iterator for Neighbors {
    type Item = Point;
    #[inline]
    fn next(&mut self) -> Option<Point> {
        const D: [(i32, i32); 4] = [(0, -1), (-1, 0), (1, 0), (0, 1)];
        while (self.i as usize) < D.len() {
            let (dx, dy) = D[self.i as usize];
            self.i += 1;
            if let Some(p) = self.size.try_point(self.x + dx, self.y + dy) {
                return Some(p);
            }
        }
        None
    }
}

/// Stone / player colour.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Color {
    Black,
    White,
}

impl Color {
    #[inline]
    pub const fn other(self) -> Color {
        match self {
            Color::Black => Color::White,
            Color::White => Color::Black,
        }
    }

    /// KataGo's one-letter player name.
    #[inline]
    pub const fn katago(self) -> &'static str {
        match self {
            Color::Black => "B",
            Color::White => "W",
        }
    }

    #[inline]
    pub const fn index(self) -> usize {
        match self {
            Color::Black => 0,
            Color::White => 1,
        }
    }

    /// `+1` for Black, `-1` for White — the sign convention of KataGo's ownership array.
    #[inline]
    pub const fn sign(self) -> f32 {
        match self {
            Color::Black => 1.0,
            Color::White => -1.0,
        }
    }

    /// The player's English name, for labels and result phrases.
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Color::Black => "Black",
            Color::White => "White",
        }
    }

    /// The Black-perspective win rate `black_winrate` as this colour sees it (INV-2).
    ///
    /// Everything stored and transmitted is Black-perspective; this is the one conversion
    /// to side-to-move, and doing it twice is nearly invisible on screen.
    #[inline]
    pub const fn winrate_for(self, black_winrate: f32) -> f32 {
        match self {
            Color::Black => black_winrate,
            Color::White => 1.0 - black_winrate,
        }
    }

    /// The Black-perspective score lead `black_lead` as this colour sees it (INV-2).
    #[inline]
    pub const fn score_lead_for(self, black_lead: f32) -> f32 {
        black_lead * self.sign()
    }

    /// The Black-perspective KataGo utility `black_utility` as this colour sees it (INV-2).
    ///
    /// Utility is a signed quantity like score lead, not a probability: White's reading is
    /// the negation, not `1 - u`.
    #[inline]
    pub const fn utility_for(self, black_utility: f32) -> f32 {
        black_utility * self.sign()
    }

    pub fn from_letter(s: &str) -> Option<Color> {
        match s.trim() {
            "b" | "B" => Some(Color::Black),
            "w" | "W" => Some(Color::White),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtp_corners_19x19() {
        let s = Size::square(19);
        assert_eq!(&*s.to_gtp(Point(0)), "A19");
        assert_eq!(s.from_gtp("A19"), Some(Point(0)));
        assert_eq!(&*s.to_gtp(Point(360)), "T1");
        assert_eq!(s.from_gtp("t1"), Some(Point(360)));
        // No `I` column.
        assert_eq!(&*s.to_gtp(s.point(8, 0)), "J19");
        assert_eq!(s.from_gtp("I19"), None);
        assert_eq!(&*s.to_gtp(Point::PASS), "pass");
        assert_eq!(s.from_gtp("PASS"), Some(Point::PASS));
        assert_eq!(s.from_gtp("A20"), None);
        assert_eq!(s.from_gtp("A0"), None);
    }

    #[test]
    fn sgf_corners_19x19() {
        let s = Size::square(19);
        assert_eq!(&s.to_sgf(Point(0)), b"aa");
        assert_eq!(s.from_sgf(b"aa"), Some(Point(0)));
        assert_eq!(&s.to_sgf(Point(360)), b"ss");
        assert_eq!(s.from_sgf(b"ss"), Some(Point(360)));
        assert_eq!(s.from_sgf(b""), Some(Point::PASS));
        assert_eq!(s.from_sgf(b"tt"), Some(Point::PASS));
    }

    #[test]
    fn utility_flips_like_score_not_like_winrate() {
        assert_eq!(Color::White.utility_for(0.12), -0.12);
        assert_eq!(Color::Black.utility_for(0.12), 0.12);
        assert_eq!(Color::White.winrate_for(0.75), 0.25);
        assert_eq!(Color::White.score_lead_for(4.0), -4.0);
    }

    #[test]
    fn rectangular_geometry() {
        let s = Size::new(19, 13).unwrap();
        assert_eq!(s.points(), 247);
        // Row 0 is the top, which is row number `h`.
        assert_eq!(&*s.to_gtp(Point(0)), "A13");
        assert_eq!(&*s.to_gtp(s.point(18, 12)), "T1");
        assert_eq!(s.xy(s.point(5, 7)), (5, 7));
        assert_eq!(Size::new(20, 19), None);
        assert_eq!(Size::new(1, 19), None);
    }

    #[test]
    fn a_point_off_the_board_labels_as_a_placeholder() {
        // Analysis from a file or a server can name any `u16`; rendering it must not panic.
        let s = Size::new(19, 13).unwrap();
        assert_eq!(&*s.to_gtp(Point(247)), "?");
        assert_eq!(&*s.to_gtp(Point(Point::PASS.0 - 1)), "?");
        assert_eq!(&*Size::square(9).to_gtp(Point(80)), "J1");
        assert_eq!(&*Size::square(9).to_gtp(Point(81)), "?");
    }

    #[test]
    fn neighbors_clip_at_edges() {
        let s = Size::square(19);
        assert_eq!(s.neighbors(Point(0)).count(), 2);
        assert_eq!(s.neighbors(s.point(0, 5)).count(), 3);
        assert_eq!(s.neighbors(s.point(5, 5)).count(), 4);
    }

    #[test]
    fn star_points_19() {
        let s = Size::square(19);
        let st = s.star_points();
        assert_eq!(st.len(), 9);
        assert!(st.contains(&s.point(3, 3)));
        assert!(st.contains(&s.point(9, 9)));
        assert!(st.contains(&s.point(15, 15)));
        assert_eq!(Size::square(9).star_points().len(), 5);
        assert_eq!(Size::square(13).star_points().len(), 5);
    }
}
