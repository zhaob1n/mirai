// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Display-only replay of a principal variation.
//!
//! Clones the board and plays the line, passes included, so simple ko matches
//! [`Board::play`]. Superko needs the position's hash history and is not consulted.
//! The record is not touched.

use crate::{Board, Color, Point, Rules};

/// A line replayed onto a clone of the starting board.
#[derive(Clone, Debug)]
pub struct Preview {
    pub board: Board,
    /// 1-based step of the move that last occupied each point. Untouched points are 0.
    ///
    /// A capture leaves that step on an empty point. Drawers skip a number where the
    /// board is empty, which is what the desktop preview has always done.
    pub numbers: Box<[u16]>,
    /// Accepted prefix length, including passes. The move that stopped the line is not
    /// counted.
    pub played: u32,
}

/// Replays `pv` from `to_play` onto a clone of `board`.
///
/// A pass advances the colour and the count but occupies nothing. It is a real
/// [`Board::play`], so a simple-ko ban clears. The desktop helper used to skip `play`
/// on a pass and leave that ban standing, which stopped a legal recapture; both
/// frontends now share the rules. An off-board or illegal move still stops the line;
/// stones already placed stay.
pub fn replay(board: &Board, to_play: Color, rules: &Rules, pv: &[Point]) -> Preview {
    let size = board.size;
    let mut board = board.clone();
    let mut color = to_play;
    let mut numbers = vec![0u16; size.points()];
    let mut step = 0u16;
    let mut played = 0u32;
    for &point in pv {
        step += 1;
        if point.is_pass() {
            board.play(color, point, rules).expect("a pass is legal");
            color = color.other();
        } else if !size.contains(point) || board.play(color, point, rules).is_err() {
            break;
        } else {
            numbers[point.index()] = step;
            color = color.other();
        }
        played += 1;
    }
    Preview {
        board,
        numbers: numbers.into_boxed_slice(),
        played,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuleSet, Size};

    /// A pass must advance the count without occupying a point, and an illegal
    /// continuation must leave the stones already placed — including a capture the
    /// rules actually take.
    #[test]
    fn replay_numbers_passes_and_stops_when_the_line_becomes_illegal() {
        let size = Size::square(9);
        let rules = RuleSet::default().rules();
        let board = Board::new(size);
        let p = |x: u8, y: u8| size.point(x, y);

        let played = replay(
            &board,
            Color::Black,
            &rules,
            &[p(2, 2), Point::PASS, p(6, 6)],
        );
        assert_eq!(played.played, 3);
        assert_eq!(played.board.at(p(2, 2)), Some(Color::Black));
        assert_eq!(played.numbers[p(2, 2).index()], 1);
        assert_eq!(
            played.board.at(p(6, 6)),
            Some(Color::Black),
            "a pass flips the colour, so the third move is Black again"
        );
        assert_eq!(
            played.numbers[p(6, 6).index()],
            3,
            "the pass still consumes a number"
        );
        assert_eq!(
            played.numbers.iter().filter(|&&n| n == 2).count(),
            0,
            "a pass occupies nothing"
        );
        assert!(
            board.stones().iter().all(Option::is_none),
            "replay clones; the source board stays empty"
        );

        let stopped = replay(&board, Color::Black, &rules, &[p(2, 2), p(2, 2), p(4, 4)]);
        assert_eq!(stopped.played, 1);
        assert_eq!(stopped.board.at(p(2, 2)), Some(Color::Black));
        assert_eq!(stopped.numbers[p(2, 2).index()], 1);
        assert_eq!(
            stopped.board.at(p(4, 4)),
            None,
            "an occupied point stops the line"
        );
        assert_eq!(stopped.numbers[p(4, 4).index()], 0);

        let off = Point(size.points() as u16);
        let off_board = replay(&board, Color::Black, &rules, &[p(1, 1), off, p(3, 3)]);
        assert_eq!(off_board.played, 1);
        assert_eq!(off_board.board.at(p(1, 1)), Some(Color::Black));
        assert_eq!(off_board.board.at(p(3, 3)), None);
        assert_eq!(off_board.numbers[p(3, 3).index()], 0);

        let white = replay(&board, Color::White, &rules, &[p(1, 1), p(2, 2)]);
        assert_eq!(white.played, 2);
        assert_eq!(white.board.at(p(1, 1)), Some(Color::White));
        assert_eq!(white.numbers[p(1, 1).index()], 1);
        assert_eq!(white.board.at(p(2, 2)), Some(Color::Black));
        assert_eq!(white.numbers[p(2, 2).index()], 2);

        // White at tengen, surrounded. The last Black move captures; a painter that
        // only dropped stones would leave White on the board.
        let captured = replay(
            &board,
            Color::Black,
            &rules,
            &[
                p(3, 4),
                p(4, 4),
                p(5, 4),
                Point::PASS,
                p(4, 3),
                Point::PASS,
                p(4, 5),
            ],
        );
        assert_eq!(captured.played, 7);
        assert_eq!(
            captured.board.at(p(4, 4)),
            None,
            "the surrounded stone is captured"
        );
        assert_eq!(captured.board.at(p(4, 5)), Some(Color::Black));
        assert_eq!(captured.numbers[p(4, 5).index()], 7);
        // The number is left in the vec; drawers skip an empty point, which is what
        // the old in-snapshot replay did too.
        assert_eq!(captured.numbers[p(4, 4).index()], 2);
        assert!(
            captured.numbers.iter().all(|&n| n != 4 && n != 6),
            "passes occupy nothing"
        );
    }

    /// A pass is a real move: [`Board::play`] clears simple ko, and the replay must too.
    /// The desktop helper only flipped the colour, so a recapture after two passes looked
    /// illegal.
    #[test]
    fn a_pass_clears_simple_ko_so_the_recapture_plays() {
        let size = Size::square(9);
        let rules = RuleSet::Chinese.rules();
        let mut board = Board::new(size);
        for (x, y) in [(2, 4), (3, 3), (3, 5)] {
            board.set(size.point(x, y), Some(Color::Black));
        }
        for (x, y) in [(3, 4), (4, 3), (5, 4), (4, 5)] {
            board.set(size.point(x, y), Some(Color::White));
        }
        let target = size.point(3, 4);
        let take = size.point(4, 4);

        let mut probe = board.clone();
        probe.play(Color::Black, take, &rules).unwrap();
        assert_eq!(probe.ko_ban(), Some(target));
        probe.play(Color::White, Point::PASS, &rules).unwrap();
        assert_eq!(
            probe.ko_ban(),
            None,
            "Board::play clears simple ko on a pass"
        );

        let immediate = replay(&board, Color::Black, &rules, &[take, target]);
        assert_eq!(immediate.played, 1, "the ko recapture is illegal at once");
        assert_eq!(immediate.board.ko_ban(), Some(target));
        assert_eq!(immediate.board.at(take), Some(Color::Black));
        assert_eq!(immediate.board.at(target), None);

        let answered = replay(
            &board,
            Color::Black,
            &rules,
            &[take, Point::PASS, Point::PASS, target],
        );
        assert_eq!(answered.played, 4);
        assert_eq!(answered.board.at(target), Some(Color::White));
        assert_eq!(
            answered.board.at(take),
            None,
            "white recaptures after the passes"
        );
        assert_eq!(answered.numbers[take.index()], 1);
        assert_eq!(answered.numbers[target.index()], 4);
        assert!(answered.numbers.iter().all(|&n| n != 2 && n != 3));
        assert_eq!(board.ko_ban(), None, "replay clones; the source ban stays");
    }
}
