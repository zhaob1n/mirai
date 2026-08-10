// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Fixed handicap placement.

use smallvec::SmallVec;

use crate::point::{Point, Size};

/// The standard fixed handicap points for `n` stones (2..=9) on a square board of 7, 9,
/// 11, ... 19 lines. Returns empty for `n` outside `2..=9`, for non-square boards, for
/// boards smaller than 7x7 and for even-sided boards (which have no tengen or side
/// midpoints and therefore no standard placement).
///
/// Order is the conventional one: the four corners first (lower left, upper right, upper
/// left, lower right), then the side midpoints (left, right, bottom, top), with tengen
/// taken last on odd stone counts.
pub fn fixed_handicap(size: Size, n: u8) -> SmallVec<[Point; 9]> {
    let mut out = SmallVec::new();
    if !(2..=9).contains(&n) || !size.is_square() || size.w < 7 || size.w.is_multiple_of(2) {
        return out;
    }
    let w = size.w;
    let off = if w >= 13 { 3 } else { 2 };
    let lo = off;
    let hi = w - 1 - off;
    let mid = (w - 1) / 2;
    if hi <= lo || mid <= lo {
        return out;
    }

    let p = |x: u8, y: u8| size.point(x, y);
    // `y = 0` is the top row, so "lower" means the larger y.
    let corners = [
        p(lo, hi), // lower left
        p(hi, lo), // upper right
        p(lo, lo), // upper left
        p(hi, hi), // lower right
    ];
    let sides = [
        p(lo, mid), // left
        p(hi, mid), // right
        p(mid, hi), // bottom
        p(mid, lo), // top
    ];
    let tengen = p(mid, mid);

    let corner_count = n.min(4) as usize;
    out.extend_from_slice(&corners[..corner_count]);
    if n >= 5 {
        let side_count = match n {
            5 => 0,
            6 | 7 => 2,
            _ => 4,
        };
        out.extend_from_slice(&sides[..side_count]);
        if n % 2 == 1 {
            out.push(tengen);
        }
    }
    debug_assert_eq!(out.len(), n as usize);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gtp(size: Size, pts: &[Point]) -> Vec<String> {
        pts.iter().map(|&p| size.to_gtp(p).to_string()).collect()
    }

    #[test]
    fn nine_stones_on_19x19_are_the_star_points() {
        let size = Size::square(19);
        let h = fixed_handicap(size, 9);
        assert_eq!(h.len(), 9);
        let mut got = gtp(size, &h);
        got.sort();
        let mut want = gtp(size, &size.star_points());
        want.sort();
        assert_eq!(got, want);
        // The conventional order starts lower left, upper right, upper left, lower right.
        assert_eq!(&gtp(size, &h)[..4], &["D4", "Q16", "D16", "Q4"]);
        assert_eq!(gtp(size, &h)[8], "K10");
    }

    #[test]
    fn counts_and_shapes() {
        let size = Size::square(19);
        assert_eq!(gtp(size, &fixed_handicap(size, 2)), ["D4", "Q16"]);
        assert_eq!(gtp(size, &fixed_handicap(size, 5)), ["D4", "Q16", "D16", "Q4", "K10"]);
        assert_eq!(
            gtp(size, &fixed_handicap(size, 6)),
            ["D4", "Q16", "D16", "Q4", "D10", "Q10"]
        );
        assert_eq!(fixed_handicap(size, 7).len(), 7);
        assert_eq!(fixed_handicap(size, 8).len(), 8);
        // 9x9 uses the 3-3 offset...
        let small = Size::square(9);
        assert_eq!(gtp(small, &fixed_handicap(small, 4)), ["C3", "G7", "C7", "G3"]);
        assert_eq!(gtp(small, &fixed_handicap(small, 5))[4], "E5");
        assert_eq!(fixed_handicap(Size::square(13), 9).len(), 9);
        assert_eq!(fixed_handicap(Size::square(7), 9).len(), 9);
    }

    #[test]
    fn rejects_unsupported_boards_and_counts() {
        assert!(fixed_handicap(Size::square(5), 2).is_empty());
        assert!(fixed_handicap(Size::square(19), 1).is_empty());
        assert!(fixed_handicap(Size::square(19), 10).is_empty());
        assert!(fixed_handicap(Size::new(19, 13).unwrap(), 2).is_empty());
    }
}
