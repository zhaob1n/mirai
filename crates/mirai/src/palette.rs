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
//! Records saved before MRAI v2 have no utility on the tree. [`grade_means`] is the old
//! two-channel reading — worse of win-rate and score-lead loss — used only as that fallback.
//!
//! How much search a move actually got is the board's *opacity* channel, and the list's Visits
//! column. Below [`TRUSTED_VISITS`] the loss is not a colour at all — grey, not green and not
//! red — because a 1-visit mean is a rumour. The engine's pick is never unknown: it is the
//! reference every other loss is measured against, and it is always the coolest stop.

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

/// Score-lead loss, in points, that puts a move on each stop — [`grade_means`] only.
///
/// The user-visible anchors: everything inside three quarters of a point of the pick stays cool,
/// and a move that throws away a komi is red whatever the win rate says. Fitted on real games
/// (`docs/dev/TESTING.md` §4) and now reached only by an MRAI v1 record, whose candidates have
/// no utility; [`UTILITY_AT`] is the live table.
pub const POINTS_AT: [f32; GRADE_RAMP.len()] = [0.0, 0.25, 0.75, 2.0, 4.5, 7.5];

/// Win-rate loss, as a fraction, that puts a move on each stop — [`grade_means`] only.
///
/// Read against `severity_of_drop` (`widgets/winrate.rs`), which grades a move somebody actually
/// played: a candidate stays cool exactly while playing it would be at most a *Minor* blunder
/// (5 %), and it turns yellow at the *Major* bar (10 %). Warming one band later than the strip is
/// deliberate — the strip measures one move against its own position, this ranks a menu against
/// the best item on it, which is the harshest reference there is. Measured on a human game,
/// aligning the two exactly turned the engine's own second choice warm in 22 % of searched
/// positions, against 11 % here.
pub const WINRATE_AT: [f32; GRADE_RAMP.len()] = [0.0, 0.02, 0.05, 0.10, 0.18, 0.30];

/// Utility loss that puts a move on each stop.
///
/// Anchored on KataGo's own utility scale, not on a win-rate table: `0.04` is analysis
/// `wideRootNoise` (the per-visit noise the root explores with), `0.20` is
/// `fpuReductionMax` (a gap as large as not having looked), `0.50` is half a win on the
/// win/loss term (`winLossUtilityFactor = 1`). Score is already inside utility, so a
/// decided endgame where every win rate reads 0.0 % still moves along this table.
///
/// Those constants also land where the two fitted tables above already were. Swept over
/// 42 positions at 5 000 visits (`examples/sweep.rs`, `dutil` column), a candidate losing
/// 0.25 points measures 0.034 utility, 0.75 points 0.09, 2 points and 10 % win rate both
/// 0.20 — mint, green and yellow, the same stops [`POINTS_AT`] and [`WINRATE_AT`] give them.
/// A breakpoint moved here should be re-measured that way, not reasoned about.
pub const UTILITY_AT: [f32; GRADE_RAMP.len()] = [0.0, 0.04, 0.10, 0.20, 0.35, 0.50];

/// Visits at which a candidate's reading is trusted, and the only search threshold there is.
///
/// One number does three things, and they are the same statement: below it a candidate is
/// grey instead of a loss colour, the board omits its figures (`widgets/board.rs` —
/// `LABEL_MIN_VISITS`), and its blob is drawn faded (`blob_alpha`). So a blob earns its
/// colour, its numbers and its full opacity at the same visit, and fading only ever happens
/// to grey — an estimate is either worth reading or it is not, and there is no third state to
/// tune. The engine's pick is exempt: it is the reference, not a rumour.
///
/// Ten is where the numbers become worth printing at all. It used to be two constants, with
/// opacity reaching full at twenty — that second one dated from the hue *cap* that
/// `analysis/uncapped-grade` removed, and once grey said "unknown" outright, the extra fade
/// window covered 4 % of a twenty-move list and said nothing the Visits column did not.
pub const TRUSTED_VISITS: u32 = 10;

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

/// How much a candidate loses in KataGo `utility`, as a position along [`GRADE_RAMP`].
///
/// `loss` is pick minus candidate, side-to-move. A candidate that reads *better* than the
/// pick clamps to the coolest stop rather than going negative.
pub fn grade(utility_loss: f32) -> f32 {
    along(utility_loss, &UTILITY_AT)
}

/// The pre-v2 reading: worse of win-rate loss and score-lead loss against the pick.
///
/// Kept for tree nodes whose [`mirai_core::Candidate`] has no utility (MRAI v1).
pub fn grade_means(winrate_loss: f32, points_loss: f32) -> f32 {
    along(points_loss, &POINTS_AT).max(along(winrate_loss, &WINRATE_AT))
}

/// True when a candidate's loss is worth a colour at all.
///
/// `rank` is the move's place in KataGo's `order`, so rank 0 is the pick: the reference every
/// other loss is measured against rather than an estimate of its own, cyan from the first
/// report even before it has [`TRUSTED_VISITS`] behind it. Every other move has to earn its
/// hue with search. The rule lives here and not in the two callers so that a third one cannot
/// paint the pick grey.
#[inline]
pub fn is_known(rank: usize, visits: u32) -> bool {
    rank == 0 || visits >= TRUSTED_VISITS
}

/// RGB the board and the list share: unknown grey below [`TRUSTED_VISITS`], otherwise the
/// loss ramp at `grade`'s level.
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

/// Where `loss` falls on a breakpoint table, in stops.
///
/// Anything at or below the first breakpoint — including a negative loss, and NaN — is stop 0.
fn along(loss: f32, at: &[f32; GRADE_RAMP.len()]) -> f32 {
    if loss.is_nan() || loss <= at[0] {
        return 0.0;
    }
    for i in 1..at.len() {
        if loss < at[i] {
            return (i - 1) as f32 + (loss - at[i - 1]) / (at[i] - at[i - 1]);
        }
    }
    (at.len() - 1) as f32
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

    /// The engine's pick is the coolest stop whatever else is true of it, and a candidate that
    /// reads better than the pick does not wrap around.
    #[test]
    fn the_pick_is_always_the_coolest_stop() {
        assert_eq!(grade(0.0), 0.0);
        assert_eq!(grade(-0.07), 0.0);
        assert_eq!(colour(grade(0.0), 1, TRUSTED_VISITS), GRADE_RAMP[0]);
    }

    /// The utility table hits its own stops, and a better reading than the pick does not
    /// wrap around.
    #[test]
    fn a_utility_loss_walks_the_ramp() {
        let last = (GRADE_RAMP.len() - 1) as f32;
        assert_eq!(grade(UTILITY_AT[5]), last);
        assert_eq!(grade(UTILITY_AT[4]), 4.0);
        assert_eq!(grade(UTILITY_AT[1]), 1.0);
        assert_eq!(grade(UTILITY_AT[3]), 3.0);
        // wideRootNoise sits on the first step; FPU sits on yellow.
        assert!((grade(0.04) - 1.0).abs() < 1e-6);
        assert!((grade(0.20) - 3.0).abs() < 1e-6);
    }

    /// The v1 fallback still lets either mean channel carry the grade.
    #[test]
    fn either_mean_channel_can_carry_the_fallback() {
        let last = (GRADE_RAMP.len() - 1) as f32;
        assert_eq!(grade_means(0.0, POINTS_AT[5]), last);
        assert_eq!(grade_means(WINRATE_AT[5], 0.0), last);
        assert_eq!(grade_means(WINRATE_AT[4], POINTS_AT[1]), 4.0);
        assert_eq!(grade_means(WINRATE_AT[1], POINTS_AT[4]), 4.0);
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
