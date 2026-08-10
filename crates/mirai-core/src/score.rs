// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Final-position scoring: dead-stone bookkeeping, area counting and territory counting.
//!
//! Both scoring modes start from the same decomposition of the board into connected
//! components: chains of one colour and maximal empty regions. A region belongs to a
//! colour when every chain on its border is that colour; a region with both colours on
//! its border (a dame, or the shared liberties of a seki) belongs to nobody.

use smallvec::SmallVec;

use crate::board::Board;
use crate::point::{Color, Point, Size};
use crate::rules::{Rules, Scoring, Tax, Whb};

/// The stones the scorer lifts off the board before counting, indexed by
/// [`Point::index`]. A default-constructed set has an empty vector and reports nothing
/// dead, which is what an unanalysed position wants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeadSet {
    pub dead: Vec<bool>,
}

impl DeadSet {
    pub fn empty(size: Size) -> DeadSet {
        DeadSet {
            dead: vec![false; size.points()],
        }
    }

    /// Derives dead stones from a KataGo ownership array (Black-positive, in [`Point`]
    /// order).
    ///
    /// A stone is a candidate when its point's ownership has the sign of the *other*
    /// colour by at least `threshold` (0.4 is the value mirai uses). A chain is marked
    /// dead only when a strict majority of its stones are candidates, which keeps
    /// half-settled boundaries from producing speckled, half-dead groups. Empty points
    /// are never dead.
    pub fn from_ownership(board: &Board, ownership: &[f32], threshold: f32) -> DeadSet {
        let size = board.size;
        let mut out = DeadSet::empty(size);
        let stones = board.stones();
        let mut seen = vec![false; size.points()];
        for i in 0..size.points() {
            if seen[i] {
                continue;
            }
            seen[i] = true;
            let Some(color) = stones[i] else { continue };
            let chain = board.chain(Point(i as u16));
            let mut qualifying = 0usize;
            for &p in &chain {
                seen[p.index()] = true;
                let own = ownership.get(p.index()).copied().unwrap_or(0.0);
                // `own * sign` is positive when the point is owned by the stone's own
                // colour, so this is "opposite sign and at least `threshold` strong".
                if own * color.sign() <= -threshold {
                    qualifying += 1;
                }
            }
            if qualifying * 2 > chain.len() {
                for &p in &chain {
                    out.dead[p.index()] = true;
                }
            }
        }
        out
    }

    /// Flips the whole chain at `p` between dead and alive. No-op on an empty point.
    pub fn toggle_chain(&mut self, board: &Board, p: Point) {
        if board.at(p).is_none() {
            return;
        }
        if self.dead.len() < board.size.points() {
            self.dead.resize(board.size.points(), false);
        }
        let now = !self.is_dead(p);
        for q in board.chain(p) {
            self.dead[q.index()] = now;
        }
    }

    #[inline]
    pub fn is_dead(&self, p: Point) -> bool {
        !p.is_pass() && self.dead.get(p.index()).copied().unwrap_or(false)
    }

    #[inline]
    pub fn any(&self) -> bool {
        self.dead.iter().any(|&d| d)
    }
}

/// A scored position.
///
/// `territory` holds `Some(color)` for every point that counts for that colour without
/// carrying one of its live stones — empty points and the points under dead stones — and
/// `None` for neutral points and for points holding live stones.
#[derive(Clone, Debug)]
pub struct ScoreResult {
    pub black: f32,
    pub white: f32,
    pub territory: Box<[Option<Color>]>,
    /// The count is a best effort rather than an exact application of the ruleset;
    /// the UI labels such a result "estimated". Set for [`Tax::All`] only.
    pub approximate: bool,
}

impl ScoreResult {
    #[inline]
    pub fn margin(&self) -> f32 {
        self.black - self.white
    }

    /// SGF-style result: `"B+3.5"`, `"W+0.5"`, or `"0"` for a jigo.
    pub fn result_string(&self) -> String {
        let m = self.margin();
        if m > 0.0 {
            format!("B+{m}")
        } else if m < 0.0 {
            format!("W+{}", -m)
        } else {
            "0".to_string()
        }
    }
}

/// A maximal connected run of identically-occupied points: a chain when `color` is
/// `Some`, an empty region when it is `None`.
struct Component {
    color: Option<Color>,
    size: u32,
    /// Ids of the adjacent components of a different value; for a region these are
    /// exactly the chains on its border.
    adjacent: SmallVec<[u32; 8]>,
}

/// An owned region this small is eye space rather than territory. Used only to keep the
/// seki test from firing on a live group that happens to sit next to an unfilled dame.
const MAX_EYE: u32 = 2;

/// Area or territory count of a final position.
///
/// Under **area** scoring each colour gets its live stones plus the regions only it
/// borders; the points under dead stones therefore go to whoever owns the region they
/// sit in. White gets `komi`, plus the handicap compensation named by [`Rules::whb`].
///
/// Under **territory** scoring each colour gets the regions only its live stones border
/// (which already include the points freed by removing the opponent's dead stones) minus
/// its own dead stones and the prisoners it lost during the game. That is the
/// conventional Japanese count with both scores shifted down by the same constant — the
/// total number of prisoners on the board — so [`ScoreResult::margin`] and
/// [`ScoreResult::result_string`] are exactly conventional.
pub fn score(
    board: &Board,
    rules: &Rules,
    komi: f32,
    handicap_stones: u8,
    dead: &DeadSet,
) -> ScoreResult {
    let size = board.size;
    let n = size.points();
    let stones = board.stones();

    // Working copy with the dead stones lifted off.
    let mut cells: Vec<Option<Color>> = Vec::with_capacity(n);
    let mut dead_count = [0u32; 2];
    for (i, &stone) in stones.iter().enumerate() {
        match stone {
            Some(c) if dead.dead.get(i).copied().unwrap_or(false) => {
                dead_count[c.index()] += 1;
                cells.push(None);
            }
            other => cells.push(other),
        }
    }

    let (comp_of, comps) = components(size, &cells);

    // A region belongs to a colour when every chain bordering it is that colour.
    let owner: Vec<Option<Color>> = comps
        .iter()
        .map(|c| {
            if c.color.is_some() {
                return None;
            }
            let mut owner = None;
            for &a in &c.adjacent {
                let Some(col) = comps[a as usize].color else {
                    continue;
                };
                match owner {
                    None => owner = Some(col),
                    Some(o) if o != col => return None,
                    _ => {}
                }
            }
            owner
        })
        .collect();

    let mut territory: Box<[Option<Color>]> = vec![None; n].into_boxed_slice();
    for i in 0..n {
        let c = comp_of[i] as usize;
        if comps[c].color.is_none() {
            territory[i] = owner[c];
        }
    }

    // Seki: a chain that shares an empty region with the enemy and owns no region bigger
    // than eye space. The bare "shares a region with the enemy" test the rules describe
    // would also catch every settled group standing next to an unfilled dame, and would
    // then tax its territory, so the size condition is required to tell a real seki from
    // an ordinary boundary.
    let in_seki: Vec<bool> = comps
        .iter()
        .map(|c| {
            let Some(col) = c.color else { return false };
            let mut shares_region = false;
            let mut only_eyes = true;
            for &a in &c.adjacent {
                let nb = &comps[a as usize];
                if nb.color.is_some() {
                    continue;
                }
                match owner[a as usize] {
                    None => shares_region = true,
                    Some(o) if o == col && nb.size > MAX_EYE => only_eyes = false,
                    Some(_) => {}
                }
            }
            shares_region && only_eyes
        })
        .collect();

    // Japanese and Korean rules give no territory for an eye inside a seki. KataGo models
    // that as one point of tax per eye-space group, which is what we apply.
    let taxing = rules.tax != Tax::None && rules.scoring == Scoring::Territory;

    let mut stone_pts = [0f32; 2];
    let mut region_pts = [0f32; 2];
    let mut tax = [0f32; 2];
    for (id, c) in comps.iter().enumerate() {
        match c.color {
            Some(col) => stone_pts[col.index()] += c.size as f32,
            None => {
                let Some(col) = owner[id] else { continue };
                region_pts[col.index()] += c.size as f32;
                if taxing && c.adjacent.iter().all(|&a| in_seki[a as usize]) {
                    tax[col.index()] += 1.0;
                }
            }
        }
    }

    let (black, mut white) = match rules.scoring {
        Scoring::Area => {
            let compensation = match rules.whb {
                Whb::Zero => 0,
                Whb::N => handicap_stones,
                Whb::NMinusOne => handicap_stones.saturating_sub(1),
            };
            (
                stone_pts[0] + region_pts[0],
                stone_pts[1] + region_pts[1] + compensation as f32,
            )
        }
        // Conventional Japanese counting: territory PLUS the prisoners you hold — the
        // opponent stones you captured during the game, and their dead stones sitting in
        // your area. (`region_pts` already contains the points those dead stones occupy,
        // because they were lifted before the flood fill; the stones themselves are the
        // extra prisoner.) Counting it the other way round — subtracting the prisoners you
        // lost — yields the same margin but shows both players a total lower than any
        // Japanese scorer would display.
        //
        // No handicap compensation: it is an area-scoring correction, and the territory
        // rulesets all specify `Whb::Zero` anyway.
        Scoring::Territory => (
            region_pts[0] - tax[0]
                + dead_count[Color::White.index()] as f32
                + board.captures[Color::Black.index()] as f32,
            region_pts[1] - tax[1]
                + dead_count[Color::Black.index()] as f32
                + board.captures[Color::White.index()] as f32,
        ),
    };
    white += komi;
    // The button is worth half a point to whoever takes it. mirai never plays it, so it
    // is modelled as a flat half point for White.
    if rules.has_button {
        white += 0.5;
    }

    ScoreResult {
        black,
        white,
        territory,
        approximate: rules.tax == Tax::All,
    }
}

/// Labels every point with the id of its connected component and records, for each
/// component, its value, its size and the components of a different value touching it.
fn components(size: Size, cells: &[Option<Color>]) -> (Vec<u32>, Vec<Component>) {
    let n = size.points();
    let mut comp_of = vec![u32::MAX; n];
    let mut comps: Vec<Component> = Vec::new();
    let mut stack: Vec<u16> = Vec::new();
    for start in 0..n {
        if comp_of[start] != u32::MAX {
            continue;
        }
        let id = comps.len() as u32;
        let color = cells[start];
        let mut count = 0;
        comp_of[start] = id;
        stack.push(start as u16);
        while let Some(cur) = stack.pop() {
            count += 1;
            for nb in size.neighbors(Point(cur)) {
                if comp_of[nb.index()] == u32::MAX && cells[nb.index()] == color {
                    comp_of[nb.index()] = id;
                    stack.push(nb.0);
                }
            }
        }
        comps.push(Component {
            color,
            size: count,
            adjacent: SmallVec::new(),
        });
    }
    for i in 0..n {
        let a = comp_of[i] as usize;
        for nb in size.neighbors(Point(i as u16)) {
            let b = comp_of[nb.index()];
            if b != a as u32 && !comps[a].adjacent.contains(&b) {
                comps[a].adjacent.push(b);
            }
        }
    }
    (comp_of, comps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RuleSet;

    /// Builds a board from a diagram: `X` black, `O` white, anything else empty.
    /// Whitespace is ignored, so the rows can be spaced out for readability.
    fn board_from(rows: &[&str]) -> Board {
        let h = rows.len() as u8;
        let w = rows[0].chars().filter(|c| !c.is_whitespace()).count() as u8;
        let size = Size::new(w, h).expect("diagram size");
        let mut b = Board::new(size);
        for (y, row) in rows.iter().enumerate() {
            let cells = row.chars().filter(|c| !c.is_whitespace());
            assert_eq!(cells.clone().count(), w as usize, "ragged diagram row {y}");
            for (x, ch) in cells.enumerate() {
                let c = match ch {
                    'X' => Some(Color::Black),
                    'O' => Some(Color::White),
                    _ => continue,
                };
                b.set(size.point(x as u8, y as u8), c);
            }
        }
        b
    }

    /// A finished 9x9 game. `y` grows downwards, so row 0 is the top.
    ///
    /// ```text
    ///      0 1 2 3 4 5 6 7 8
    ///   0  O X . . . . . . .     O(0,0) is captured by the move below
    ///   1  . O O . . . . . .     the two O are dead in Black's area
    ///   2  . . . . X . . . .
    ///   3  X X X X . X X X X     (4,3) is the one dame
    ///   4  O O O O O O O O O
    ///   5  . . . . . . . . .
    ///   6  . . . . . . . . .
    ///   7  . . . . . . . . .
    ///   8  . . . . . . . . .
    /// ```
    ///
    /// After Black plays (0,1) the White stone at (0,0) has no liberty and is captured,
    /// leaving `captures = [1, 0]` and Black stones at (1,0) and (0,1).
    ///
    /// Working copy (dead White removed) — Black owns 24 points: the singleton at (0,0)
    /// plus the 23 remaining empty points of rows 0..2. White owns the 36 points of rows
    /// 5..8. (4,3) touches both colours and is neutral. 11 Black stones, 9 White stones.
    fn endgame() -> (Board, DeadSet) {
        let mut b = board_from(&[
            "O X . . . . . . .",
            ". O O . . . . . .",
            ". . . . X . . . .",
            "X X X X . X X X X",
            "O O O O O O O O O",
            ". . . . . . . . .",
            ". . . . . . . . .",
            ". . . . . . . . .",
            ". . . . . . . . .",
        ]);
        let size = b.size;
        b.play(Color::Black, size.point(0, 1), &RuleSet::Chinese.rules())
            .expect("capture at (0,1)");
        assert_eq!(b.captures, [1, 0]);
        assert_eq!(b.at(size.point(0, 0)), None);

        let mut dead = DeadSet::empty(size);
        dead.toggle_chain(&b, size.point(1, 1));
        assert!(dead.is_dead(size.point(2, 1)), "the whole chain is dead");
        (b, dead)
    }

    #[test]
    fn endgame_area_and_territory() {
        let (b, dead) = endgame();
        let size = b.size;

        // Chinese: Black 11 stones + 24 points, White 9 stones + 36 points + 7.5 komi.
        let chinese = score(&b, &RuleSet::Chinese.rules(), 7.5, 0, &dead);
        assert_eq!((chinese.black, chinese.white), (35.0, 52.5));
        assert_eq!(chinese.result_string(), "W+17.5");
        assert!(!chinese.approximate);
        // Every point except the single dame belongs to somebody.
        assert_eq!(chinese.black + chinese.white - 7.5, 80.0);

        // Japanese, counted the way a human counts: Black 24 territory + 2 dead White
        // stones + 1 prisoner taken = 27. White 36 territory + 0 dead + 0 prisoners
        // + 6.5 komi = 42.5. These are the totals a Japanese scorer displays.
        let japanese = score(&b, &RuleSet::Japanese.rules(), 6.5, 0, &dead);
        assert_eq!((japanese.black, japanese.white), (27.0, 42.5));
        assert_eq!(japanese.result_string(), "W+15.5");
        assert!(!japanese.approximate);

        // The dame is nobody's, the points under the dead stones are Black's, and a live
        // stone is never territory.
        for r in [&chinese, &japanese] {
            assert_eq!(r.territory[size.point(4, 3).index()], None);
            assert_eq!(r.territory[size.point(1, 1).index()], Some(Color::Black));
            assert_eq!(r.territory[size.point(2, 1).index()], Some(Color::Black));
            assert_eq!(r.territory[size.point(0, 0).index()], Some(Color::Black));
            assert_eq!(r.territory[size.point(6, 6).index()], Some(Color::White));
            assert_eq!(r.territory[size.point(0, 3).index()], None);
            assert_eq!(r.territory[size.point(4, 2).index()], None);
        }

        // Stone scoring is Tax::All, which we only estimate.
        let stones = score(&b, &RuleSet::StoneScoring.rules(), 7.5, 0, &dead);
        assert!(stones.approximate);
    }

    #[test]
    fn handicap_compensation_variants() {
        let (b, dead) = endgame();
        let mut totals = Vec::new();
        for whb in [Whb::Zero, Whb::N, Whb::NMinusOne] {
            let mut rules = RuleSet::Chinese.rules();
            rules.whb = whb;
            let r = score(&b, &rules, 7.5, 4, &dead);
            assert_eq!(r.black, 35.0, "compensation never touches Black");
            totals.push(r.white);
        }
        assert_eq!(totals, vec![52.5, 56.5, 55.5]);

        // Territory scoring ignores the compensation entirely.
        let mut japanese = RuleSet::Japanese.rules();
        japanese.whb = Whb::N;
        assert_eq!(score(&b, &japanese, 6.5, 4, &dead).white, 42.5);
    }

    #[test]
    fn toggle_chain_flips_whole_chain_and_score() {
        let (b, mut dead) = endgame();
        let size = b.size;
        let rules = RuleSet::Chinese.rules();
        assert!(dead.any());
        assert_eq!(score(&b, &rules, 7.5, 0, &dead).black, 35.0);

        // Reviving the pair from either of its two stones brings both back: White gains
        // the two stones, and Black's 23-point region turns neutral because White now
        // borders it. The singleton at (0,0) is still Black's.
        dead.toggle_chain(&b, size.point(2, 1));
        assert!(!dead.is_dead(size.point(1, 1)));
        assert!(!dead.any());
        let revived = score(&b, &rules, 7.5, 0, &dead);
        assert_eq!((revived.black, revived.white), (12.0, 54.5));
        assert_eq!(revived.territory[size.point(3, 1).index()], None);

        // And killing them again restores the original count.
        dead.toggle_chain(&b, size.point(1, 1));
        let again = score(&b, &rules, 7.5, 0, &dead);
        assert_eq!((again.black, again.white), (35.0, 52.5));

        // An empty point has no chain to toggle.
        dead.toggle_chain(&b, size.point(8, 8));
        assert!(!dead.is_dead(size.point(8, 8)));
    }

    /// A White group with one eye, wholly surrounded by Black.
    ///
    /// ```text
    ///    . X X X .
    ///    X O O O X
    ///    X O . O X
    ///    X O O O X
    ///    . X X X .
    /// ```
    fn surrounded_white() -> Board {
        board_from(&[
            ". X X X .",
            "X O O O X",
            "X O . O X",
            "X O O O X",
            ". X X X .",
        ])
    }

    #[test]
    fn from_ownership_marks_a_surrounded_group() {
        let b = surrounded_white();
        let size = b.size;
        let dead = DeadSet::from_ownership(&b, &vec![0.95; size.points()], 0.4);
        for y in 1..4 {
            for x in 1..4 {
                let p = size.point(x, y);
                assert_eq!(
                    dead.is_dead(p),
                    b.at(p).is_some(),
                    "White stone dead, the eye at (2,2) is not"
                );
            }
        }
        // Black's own stones agree with the ownership, so they live.
        assert!(!dead.is_dead(size.point(1, 0)));
        assert!(dead.any());
    }

    #[test]
    fn from_ownership_needs_a_confident_majority() {
        let b = surrounded_white();
        let size = b.size;
        let white: Vec<Point> = (0..size.points())
            .map(|i| Point(i as u16))
            .filter(|&p| b.at(p) == Some(Color::White))
            .collect();
        assert_eq!(white.len(), 8);

        // Strong Black ownership, but only on half the chain: not a majority.
        let mut own = vec![0.0f32; size.points()];
        for &p in &white[..4] {
            own[p.index()] = 0.9;
        }
        assert!(!DeadSet::from_ownership(&b, &own, 0.4).any());

        // One more stone tips it over.
        own[white[4].index()] = 0.9;
        let dead = DeadSet::from_ownership(&b, &own, 0.4);
        assert!(white.iter().all(|&p| dead.is_dead(p)), "the chain goes as one");

        // Unanimous but weak ownership stays below the threshold.
        let weak = vec![0.3f32; size.points()];
        assert!(!DeadSet::from_ownership(&b, &weak, 0.4).any());

        // Ownership that agrees with the stones never kills them.
        let mut agreeing = vec![0.9f32; size.points()];
        for &p in &white {
            agreeing[p.index()] = -0.9;
        }
        assert!(!DeadSet::from_ownership(&b, &agreeing, 0.4).any());
    }

    #[test]
    fn result_string_covers_both_wins_and_a_jigo() {
        let res = |black: f32, white: f32| ScoreResult {
            black,
            white,
            territory: Vec::new().into_boxed_slice(),
            approximate: false,
        };
        assert_eq!(res(45.0, 41.5).result_string(), "B+3.5");
        assert_eq!(res(40.0, 40.5).result_string(), "W+0.5");
        assert_eq!(res(40.0, 40.0).result_string(), "0");
        assert_eq!(res(45.0, 41.5).margin(), 3.5);
        assert_eq!(res(40.0, 47.0).result_string(), "W+7");
    }
}
