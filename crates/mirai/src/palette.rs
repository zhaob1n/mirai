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
//! column. Below [`UNKNOWN_VISITS`] the loss is not a colour at all — grey, not green and not
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

/// Visits at which a candidate blob is drawn at full opacity.
///
/// Twenty is where a score-lead reading is worth about a point (90th percentile, 1 000 against
/// 5 000 root visits). Hue uses [`UNKNOWN_VISITS`], not this: opacity can still fade a
/// well-coloured blob, but a 1-visit mean is not a colour.
pub const TRUSTED_VISITS: f32 = 20.0;

/// Below this many visits a candidate is grey, not a loss colour.
///
/// The same line the board uses to drop the numbers: if the estimate is not worth printing, it
/// is not worth painting as a blunder or as a good move. The engine's pick is exempt — it is
/// the reference, not a rumour.
pub const UNKNOWN_VISITS: u32 = 10;

/// Warm grey for an unsearched candidate, off the loss ramp so it cannot be read as cyan or as
/// a blunder tick.
pub const UNKNOWN_RGB: [u8; 3] = [0x9E, 0x98, 0x8C];

/// Sentinel [`colour_stop`] returns for an unsearched candidate. Not an index into
/// [`GRADE_RAMP`].
pub const UNKNOWN_STOP: u32 = u32::MAX;

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
/// report even before it has [`UNKNOWN_VISITS`] behind it. Every other move has to earn its
/// hue with search. The rule lives here and not in the two callers so that a third one cannot
/// paint the pick grey.
#[inline]
pub fn is_known(rank: usize, visits: u32) -> bool {
    rank == 0 || visits >= UNKNOWN_VISITS
}

/// RGB the board and the list share: unknown grey below [`UNKNOWN_VISITS`], otherwise the
/// loss ramp at `grade`.
pub fn colour(grade: f32, rank: usize, visits: u32) -> [u8; 3] {
    if is_known(rank, visits) {
        grade_rgb(grade)
    } else {
        UNKNOWN_RGB
    }
}

/// Badge stop for the same reading as [`colour`]. [`UNKNOWN_STOP`] when unsearched.
pub fn colour_stop(grade: f32, rank: usize, visits: u32) -> u32 {
    if is_known(rank, visits) {
        grade_stop(grade)
    } else {
        UNKNOWN_STOP
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

/// The colour for a grade, interpolated between the two stops it falls between.
pub fn grade_rgb(grade: f32) -> [u8; 3] {
    let last = GRADE_RAMP.len() - 1;
    if grade.is_nan() || grade <= 0.0 {
        return GRADE_RAMP[0];
    }
    if grade >= last as f32 {
        return GRADE_RAMP[last];
    }
    let i = grade as usize;
    let f = grade - i as f32;
    let (a, b) = (GRADE_RAMP[i], GRADE_RAMP[i + 1]);
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

/// The nearest stop to a grade, for the list's badge.
///
/// The board interpolates; a 20-pixel pill cannot show the difference, and CSS cannot carry a
/// per-row colour without a style provider per widget.
pub fn grade_stop(grade: f32) -> u32 {
    let last = (GRADE_RAMP.len() - 1) as u32;
    if grade.is_nan() || grade <= 0.0 {
        return 0;
    }
    ((grade + 0.5) as u32).min(last)
}

/// The badge class for a stop: `mirai-grade-1` … `mirai-grade-6`, or `mirai-grade-unknown`.
pub fn grade_class(stop: u32) -> String {
    if stop == UNKNOWN_STOP {
        "mirai-grade-unknown".to_string()
    } else {
        format!(
            "mirai-grade-{}",
            (stop as usize).min(GRADE_RAMP.len() - 1) + 1
        )
    }
}

/// Whether black ink reads better than white on this background.
///
/// Rec. 601 luma against the crossover where white text stops being legible. Channels are
/// `0.0..=1.0`; the board pre-composites its blob over the wood before asking.
pub fn is_light(r: f32, g: f32, b: f32) -> bool {
    0.299 * r + 0.587 * g + 0.114 * b > 0.58
}

/// The stylesheet for the list's badges, generated from [`GRADE_RAMP`] so the badge and the blob
/// for one move can never drift apart.
///
/// The numeral's colour is picked here too, by the same luminance rule the board uses for the
/// text on a blob.
pub fn grade_css() -> String {
    let mut css = String::with_capacity((GRADE_RAMP.len() + 1) * 96);
    for (i, rgb) in GRADE_RAMP.iter().enumerate() {
        push_grade_rule(&mut css, &format!("{}", i + 1), *rgb);
    }
    push_grade_rule(&mut css, "unknown", UNKNOWN_RGB);
    css
}

fn push_grade_rule(css: &mut String, name: &str, [r, g, b]: [u8; 3]) {
    let light = is_light(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let ink = if light { "#1c1c1c" } else { "#ffffff" };
    css.push_str(&format!(
        ".mirai-grade-{name} {{ background-color: #{r:02x}{g:02x}{b:02x}; color: {ink}; }}\n"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's pick is the coolest stop whatever else is true of it, and a candidate that
    /// reads better than the pick does not wrap around.
    #[test]
    fn the_pick_is_always_the_coolest_stop() {
        assert_eq!(grade(0.0), 0.0);
        assert_eq!(grade(-0.07), 0.0);
        assert_eq!(grade_rgb(grade(0.0)), GRADE_RAMP[0]);
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
            colour(g, 1, UNKNOWN_VISITS),
            colour(g, 2, 10_000),
            "the same loss painted two colours"
        );
    }

    /// The loss reading itself ignores visits; painting does not. A 1-visit last-stop loss is
    /// grey, the same loss at [`UNKNOWN_VISITS`] is red.
    #[test]
    fn a_grade_follows_the_loss_whatever_the_search() {
        let last = (GRADE_RAMP.len() - 1) as f32;
        assert_eq!(grade(0.9), last);
        assert_eq!(colour(last, 1, 0), UNKNOWN_RGB);
        assert_eq!(colour(last, 1, UNKNOWN_VISITS - 1), UNKNOWN_RGB);
        assert_eq!(colour(last, 1, UNKNOWN_VISITS), GRADE_RAMP[5]);
        assert_eq!(colour(0.0, 1, 100_000), GRADE_RAMP[0]);
        assert_eq!(colour_stop(last, 1, 1), UNKNOWN_STOP);
        assert_eq!(colour_stop(last, 1, UNKNOWN_VISITS), 5);
        assert!(!is_known(1, 0));
        assert!(is_known(1, UNKNOWN_VISITS));
    }

    /// The pick is the reference, not an estimate: it is cyan from the first report, whatever
    /// search stands behind it, and every other move that thin is grey.
    #[test]
    fn the_pick_is_never_unknown() {
        assert!(is_known(0, 0));
        assert_eq!(colour(0.0, 0, 0), GRADE_RAMP[0]);
        assert_eq!(colour_stop(0.0, 0, 1), 0);
        assert_eq!(colour_stop(0.0, 1, 1), UNKNOWN_STOP);
    }

    /// The ramp hands back its own table at the stops and stays between neighbours in between.
    #[test]
    fn the_ramp_hits_its_stops_and_clamps_outside_them() {
        for (i, stop) in GRADE_RAMP.iter().enumerate() {
            assert_eq!(grade_rgb(i as f32), *stop, "stop {i}");
        }
        assert_eq!(grade_rgb(-1.0), GRADE_RAMP[0]);
        assert_eq!(grade_rgb(99.0), GRADE_RAMP[GRADE_RAMP.len() - 1]);
        assert_eq!(grade_rgb(f32::NAN), GRADE_RAMP[0]);
        let mid = grade_rgb(3.5);
        for c in 0..3 {
            let (a, b) = (GRADE_RAMP[3][c], GRADE_RAMP[4][c]);
            assert!(
                mid[c] >= a.min(b) && mid[c] <= a.max(b),
                "channel {c} left the segment"
            );
        }
    }

    /// The badge stylesheet and the blob colours come from one table: a rule per stop, each
    /// naming that stop's own hex, and a grade lands on the nearest one.
    #[test]
    fn badges_cover_every_stop_and_snap_to_the_nearest() {
        let css = grade_css();
        assert_eq!(css.lines().count(), GRADE_RAMP.len() + 1);
        for (i, [r, g, b]) in GRADE_RAMP.iter().enumerate() {
            let fill = format!(
                ".mirai-grade-{} {{ background-color: #{r:02x}{g:02x}{b:02x};",
                i + 1
            );
            assert!(css.contains(&fill), "missing {fill}");
            assert_eq!(grade_stop(i as f32), i as u32);
            assert_eq!(grade_class(i as u32), format!("mirai-grade-{}", i + 1));
        }
        let [r, g, b] = UNKNOWN_RGB;
        let unknown = format!(".mirai-grade-unknown {{ background-color: #{r:02x}{g:02x}{b:02x};");
        assert!(css.contains(&unknown), "missing {unknown}");
        assert_eq!(grade_class(UNKNOWN_STOP), "mirai-grade-unknown");
        assert_eq!(grade_stop(1.4), 1);
        assert_eq!(grade_stop(1.6), 2);
        assert_eq!(grade_stop(-3.0), 0);
        assert_eq!(grade_stop(99.0), (GRADE_RAMP.len() - 1) as u32);
        assert_eq!(grade_class(4_000), "mirai-grade-6");
    }
}
