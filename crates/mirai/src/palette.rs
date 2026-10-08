// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The one candidate colour table the board and the candidate list share.
//!
//! A candidate is coloured by **how much the move loses** against KataGo's own pick — the
//! engine's first choice is always the coolest stop, and a move walks from cyan through mint and
//! green into the three `Severity` hexes the win-rate graph paints a blunder tick with
//! (`widgets/winrate.rs`) as it gets worse. Rank is not the colour: `order` says which move the
//! engine would play, not by how much, and the list already carries the number in the badge.
//!
//! **Loss is the drop in KataGo's `utility`**, side-to-move, pick minus candidate. Win rate and
//! score are already blended inside that one number, the way the engine itself blends them, so
//! the live path does not consult either. The pick compared to itself is zero, so it stays
//! cyan. Absolute utility is not the colour: in a lost position every move including the pick
//! would be red.
//!
//! It is the *mean*, not `utilityLcb`. The lower bound was measured and rejected: it warms a
//! move for being thin as well as for being bad, which the grey floor below already says
//! better and without pretending to know by how much
//! (`docs/dev/CANDIDATE_COLOUR.md` §6).
//!
//! Records saved before MRAI v2 have no utility on the tree.
//! [`mirai_core::candidate_grade::grade_means`] is the old two-channel reading — worse of
//! win-rate and score-lead loss — used only as that fallback.
//!
//! How much search a move actually got is the board's *opacity* channel, and the list's Visits
//! column. Below [`mirai_core::candidate_grade::TRUSTED_VISITS`] the loss is grey, not a hue:
//! a 1-visit mean is a rumour. The engine's pick is the reference, never unknown,
//! and always occupies the coolest stop.

use mirai_core::candidate_grade::is_known;

/// Colour stops, from KataGo's own pick to a move that loses half a win of utility.
///
/// The cool end is LizzieYzy's best-move cyan family; the three warm stops are the exact hexes
/// `Severity` paints a blunder tick with (`widgets/winrate.rs`), so a candidate that shows orange
/// here is an orange tick on the win-rate graph once it is played.
pub const GRADE_RAMP: [[u8; 3]; 6] = [
    [0x00, 0xB8, 0xE6],
    [0x12, 0xC7, 0x9E],
    [0x3F, 0xC2, 0x4A],
    [0xE8, 0xC8, 0x38],
    [0xE8, 0x80, 0x2C],
    [0xE8, 0x50, 0x38],
];

/// Warm grey for an unsearched candidate, off the loss ramp so it cannot be read as cyan or as
/// a blunder tick.
pub const UNKNOWN_RGB: [u8; 3] = [0x9E, 0x98, 0x8C];

/// Sentinel [`colour_level`] returns for an unsearched candidate. Not a level on the ramp.
pub const UNKNOWN_LEVEL: u32 = u32::MAX;

/// How finely the ramp is cut between two adjacent stops.
///
/// The board and the list draw a grade at the same level, so a blob and its badge are one
/// colour. Rounding the badge to the nearest *stop* while the board interpolated was not
/// invisible, as once assumed: green and yellow are far apart, and a move between them was a
/// yellow-green blob beside a yellow badge. An eighth of a segment is a step no eye separates
/// on two discs, and it keeps the list's stylesheet to one rule per level.
pub const LEVELS_PER_STOP: u32 = 8;

/// The warmest level: the last stop of [`GRADE_RAMP`].
const LAST_LEVEL: u32 = (GRADE_RAMP.len() as u32 - 1) * LEVELS_PER_STOP;

/// RGB the board and list share: grey below [`mirai_core::candidate_grade::TRUSTED_VISITS`],
/// otherwise the loss ramp at `grade`'s level.
pub fn colour(grade: f32, rank: usize, visits: u32) -> [u8; 3] {
    if is_known(rank, visits) {
        level_rgb(grade_level(grade))
    } else {
        UNKNOWN_RGB
    }
}

/// Badge level for the same reading as [`colour`]. [`UNKNOWN_LEVEL`] when unsearched.
pub fn colour_level(grade: f32, rank: usize, visits: u32) -> u32 {
    if is_known(rank, visits) {
        grade_level(grade)
    } else {
        UNKNOWN_LEVEL
    }
}

/// The level a grade is drawn at: its position on the ramp, rounded to [`LEVELS_PER_STOP`].
fn grade_level(grade: f32) -> u32 {
    if grade.is_nan() || grade <= 0.0 {
        return 0;
    }
    ((grade * LEVELS_PER_STOP as f32 + 0.5) as u32).min(LAST_LEVEL)
}

/// The level a ramp stop sits on, for a mark that is exactly one stop — a blunder's badge.
pub fn stop_level(stop: u32) -> u32 {
    (stop * LEVELS_PER_STOP).min(LAST_LEVEL)
}

/// The colour of a level, interpolated between the two stops it falls between.
fn level_rgb(level: u32) -> [u8; 3] {
    let level = level.min(LAST_LEVEL);
    let i = (level / LEVELS_PER_STOP) as usize;
    let a = GRADE_RAMP[i];
    let Some(&b) = GRADE_RAMP.get(i + 1) else {
        return a;
    };
    let f = (level % LEVELS_PER_STOP) as f32 / LEVELS_PER_STOP as f32;
    [
        lerp8(a[0], b[0], f),
        lerp8(a[1], b[1], f),
        lerp8(a[2], b[2], f),
    ]
}

#[inline]
fn lerp8(a: u8, b: u8, f: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * f).round() as u8
}

/// The badge class for a level: `mirai-grade-0` … `mirai-grade-40`, or `mirai-grade-unknown`.
pub fn grade_class(level: u32) -> String {
    if level == UNKNOWN_LEVEL {
        "mirai-grade-unknown".to_string()
    } else {
        format!("mirai-grade-{}", level.min(LAST_LEVEL))
    }
}

/// Whether black ink reads better than white on this background.
///
/// Rec. 601 luma against the crossover where white text stops being legible. Channels are
/// `0.0..=1.0`; the board pre-composites its blob over the wood before asking.
pub fn is_light(r: f32, g: f32, b: f32) -> bool {
    0.299 * r + 0.587 * g + 0.114 * b > 0.58
}

/// The stylesheet for the list's badges: one rule per level, filled by [`level_rgb`] exactly
/// as the board fills a blob, so the badge and the blob for one move cannot drift apart.
///
/// The numeral's colour is picked here too, by the same luminance rule the board uses for the
/// text on a blob. An unsearched candidate is not a grade, so its badge is not a fill: a faint
/// chip of the list's own ink, which neither theme turns into a slab of grey.
pub fn grade_css() -> String {
    let mut css = String::with_capacity((LAST_LEVEL as usize + 2) * 96);
    for level in 0..=LAST_LEVEL {
        let [r, g, b] = level_rgb(level);
        let light = is_light(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let ink = if light { "#1c1c1c" } else { "#ffffff" };
        css.push_str(&format!(
            ".mirai-grade-{level} {{ background-color: #{r:02x}{g:02x}{b:02x}; color: {ink}; }}\n"
        ));
    }
    css.push_str(
        ".mirai-grade-unknown { background-color: color-mix(in srgb, currentColor 10%, transparent); \
         color: color-mix(in srgb, currentColor 60%, transparent); }\n",
    );
    css
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_core::candidate_grade::{TRUSTED_VISITS, grade};

    /// A searched candidate's badge in the list is filled with exactly the colour of its blob
    /// on the board, wherever its loss falls between two stops. The list used to round to the
    /// nearest stop while the board interpolated, so a move two thirds of the way from green
    /// to yellow was a yellow-green blob beside a yellow badge.
    #[test]
    fn a_badge_is_filled_with_its_blob_colour() {
        let css = grade_css();
        let last = (GRADE_RAMP.len() - 1) as f32;
        for step in 0..=500 {
            let grade = last * step as f32 / 500.0;
            let [r, g, b] = colour(grade, 1, TRUSTED_VISITS);
            let class = grade_class(colour_level(grade, 1, TRUSTED_VISITS));
            let rule = css
                .lines()
                .find(|rule| rule.starts_with(&format!(".{class} ")))
                .unwrap_or_else(|| panic!("no rule for {class}"));
            let fill = format!("background-color: #{r:02x}{g:02x}{b:02x};");
            assert!(
                rule.contains(&fill),
                "grade {grade}: blob {fill}, badge {rule}"
            );
        }
    }

    /// Among searched moves the hue is the loss and nothing else: two candidates that lose the
    /// same amount are the same colour however differently they were searched. Confidence is
    /// the grey floor and the blob's opacity, and it is not allowed back into the hue.
    #[test]
    fn search_depth_does_not_move_the_hue_of_a_searched_move() {
        let g = grade(0.12);
        assert_eq!(
            colour(g, 1, TRUSTED_VISITS),
            colour(g, 2, 10_000),
            "the same loss painted two colours"
        );
    }

    /// The loss reading itself ignores visits; painting does not. A 1-visit last-stop loss is
    /// grey, the same loss at [`TRUSTED_VISITS`] is red.
    #[test]
    fn a_grade_follows_the_loss_whatever_the_search() {
        let last = (GRADE_RAMP.len() - 1) as f32;
        assert_eq!(grade(0.9), last);
        assert_eq!(colour(last, 1, 0), UNKNOWN_RGB);
        assert_eq!(colour(last, 1, TRUSTED_VISITS - 1), UNKNOWN_RGB);
        assert_eq!(colour(last, 1, TRUSTED_VISITS), GRADE_RAMP[5]);
        assert_eq!(colour(0.0, 1, 100_000), GRADE_RAMP[0]);
        assert_eq!(colour_level(last, 1, 1), UNKNOWN_LEVEL);
        assert_eq!(colour_level(last, 1, TRUSTED_VISITS), stop_level(5));
        assert!(!is_known(1, 0));
        assert!(is_known(1, TRUSTED_VISITS));
    }

    /// The pick is the reference, not an estimate: it is cyan from the first report, whatever
    /// search stands behind it, and every other move that thin is grey.
    #[test]
    fn the_pick_is_never_unknown() {
        assert!(is_known(0, 0));
        assert_eq!(colour(0.0, 0, 0), GRADE_RAMP[0]);
        assert_eq!(colour_level(0.0, 0, 1), 0);
        assert_eq!(colour_level(0.0, 1, 1), UNKNOWN_LEVEL);
    }

    /// The ramp hands back its own table at the stops and stays between neighbours in between.
    #[test]
    fn the_ramp_hits_its_stops_and_clamps_outside_them() {
        let paint = |grade: f32| colour(grade, 1, TRUSTED_VISITS);
        for (i, stop) in GRADE_RAMP.iter().enumerate() {
            assert_eq!(paint(i as f32), *stop, "stop {i}");
        }
        assert_eq!(paint(-1.0), GRADE_RAMP[0]);
        assert_eq!(paint(99.0), GRADE_RAMP[GRADE_RAMP.len() - 1]);
        assert_eq!(paint(f32::NAN), GRADE_RAMP[0]);
        let mid = paint(3.5);
        for c in 0..3 {
            let (a, b) = (GRADE_RAMP[3][c], GRADE_RAMP[4][c]);
            assert!(
                mid[c] >= a.min(b) && mid[c] <= a.max(b),
                "channel {c} left the segment"
            );
        }
    }

    /// A mark that sits on one stop — a blunder's loss badge — is filled with that stop's own
    /// hex, the one the win-rate graph ticks the move with.
    #[test]
    fn a_stop_badge_is_the_stop_hex() {
        let css = grade_css();
        for (i, [r, g, b]) in GRADE_RAMP.iter().enumerate() {
            let rule = format!(
                ".{} {{ background-color: #{r:02x}{g:02x}{b:02x};",
                grade_class(stop_level(i as u32))
            );
            assert!(css.contains(&rule), "missing {rule}");
        }
    }

    /// An unsearched candidate is not a grade: its badge borrows the list's ink instead of any
    /// fill, so it cannot read as a searched move's colour or as a slab of grey.
    #[test]
    fn an_unknown_badge_has_no_fill() {
        let css = grade_css();
        let class = grade_class(UNKNOWN_LEVEL);
        let unknown = css
            .lines()
            .find(|rule| rule.starts_with(&format!(".{class} ")))
            .expect("a rule for the unknown badge");
        assert!(!unknown.contains('#'), "{unknown}");
    }
}
