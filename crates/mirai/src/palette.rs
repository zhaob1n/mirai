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
//! **Two channels, whichever is more alarming.** Win-rate loss alone collapses in a decided
//! position — a measured 9x9 endgame reads 0.0 % for every candidate while the score lead spreads
//! over six points — and score loss alone goes quiet in a close endgame, where a single point is
//! the whole game (measured: about 13 % of win rate per point with the root inside 50 % ± 15,
//! against 0.02 % per point once it is decided). So each channel has its own breakpoints,
//! [`POINTS_AT`] and [`WINRATE_AT`], and a move takes the worse of the two readings.
//!
//! **Red is earned by search.** A move with a handful of visits has a loss estimate worth
//! nothing: sweeping a real game at 1 000 and 5 000 root visits moved the score loss of a
//! 1-visit candidate by 3.5 points at the 90th percentile, against 0.97 for a 20-visit one. So
//! the grade is *capped* rather than scaled: with no search behind it a move can show no warmer
//! than the last cool stop, and the cap opens to the full ramp at [`TRUSTED_VISITS`]. Capping
//! leaves a well-searched move's colour alone — scaling it by the visit share dragged legible
//! losses back to cyan, which reads as "the engine likes this" about a move it never examined.
//!
//! How much search a move actually got is the board's *opacity* channel, and the list's Visits
//! column; neither is hue's job.

/// Colour stops, from KataGo's own pick to a move that loses a komi.
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

/// Score-lead loss, in points, that puts a move on each stop.
///
/// The user-visible anchors: everything inside three quarters of a point of the pick stays cool,
/// and a move that throws away a komi is red whatever the win rate says.
pub const POINTS_AT: [f32; GRADE_RAMP.len()] = [0.0, 0.25, 0.75, 2.0, 4.5, 7.5];

/// Win-rate loss, as a fraction, that puts a move on each stop.
///
/// Read against `severity_of_drop` (`widgets/winrate.rs`), which grades a move somebody actually
/// played: a candidate stays cool exactly while playing it would be at most a *Minor* blunder
/// (5 %), and it turns yellow at the *Major* bar (10 %). Warming one band later than the strip is
/// deliberate — the strip measures one move against its own position, this ranks a menu against
/// the best item on it, which is the harshest reference there is. Measured on a human game,
/// aligning the two exactly turned the engine's own second choice warm in 22 % of searched
/// positions, against 11 % here.
pub const WINRATE_AT: [f32; GRADE_RAMP.len()] = [0.0, 0.02, 0.05, 0.10, 0.18, 0.30];

/// Visits at which a candidate may show the whole ramp.
///
/// At twenty visits a score-lead loss is worth about a point (90th percentile, 1 000 against
/// 5 000 root visits on a real game), and no error of a point turns a 7.5-point blunder into an
/// ordinary move.
pub const TRUSTED_VISITS: f32 = 20.0;

/// The hottest grade a move with no search at all may show: index 2, the last cool stop.
const UNSEARCHED_CEILING: f32 = 2.0;

/// How much a candidate loses, as a position along [`GRADE_RAMP`].
///
/// Both losses are relative to the engine's pick and in the side-to-move's perspective, so a
/// candidate that reads *better* than the pick — which happens, the two channels disagree —
/// clamps to the coolest stop rather than going negative.
pub fn grade(winrate_loss: f32, points_loss: f32, visits: u32) -> f32 {
    let t = along(points_loss, &POINTS_AT).max(along(winrate_loss, &WINRATE_AT));
    let last = (GRADE_RAMP.len() - 1) as f32;
    let confidence = (visits as f32 / TRUSTED_VISITS).min(1.0);
    t.min(UNSEARCHED_CEILING + (last - UNSEARCHED_CEILING) * confidence)
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

/// The badge class for a stop: `mirai-grade-1` … `mirai-grade-6`.
pub fn grade_class(stop: u32) -> String {
    format!(
        "mirai-grade-{}",
        (stop as usize).min(GRADE_RAMP.len() - 1) + 1
    )
}

/// The stylesheet for the list's badges, generated from [`GRADE_RAMP`] so the badge and the blob
/// for one move can never drift apart.
///
/// The numeral's colour is picked here too, by the same luminance rule the board uses for the
/// text on a blob.
pub fn grade_css() -> String {
    let mut css = String::with_capacity(GRADE_RAMP.len() * 96);
    for (i, [r, g, b]) in GRADE_RAMP.iter().enumerate() {
        let lum = 0.299 * *r as f32 + 0.587 * *g as f32 + 0.114 * *b as f32;
        let ink = if lum > 148.0 { "#1c1c1c" } else { "#ffffff" };
        css.push_str(&format!(
            ".mirai-grade-{} {{ background-color: #{r:02x}{g:02x}{b:02x}; color: {ink}; }}\n",
            i + 1
        ));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's pick is the coolest stop whatever else is true of it, and a candidate that
    /// reads better than the pick does not wrap around.
    #[test]
    fn the_pick_is_always_the_coolest_stop() {
        assert_eq!(grade(0.0, 0.0, 1), 0.0);
        assert_eq!(grade(0.0, 0.0, 100_000), 0.0);
        assert_eq!(grade(-0.07, -1.4, 500), 0.0);
        assert_eq!(grade_rgb(grade(0.0, 0.0, 4)), GRADE_RAMP[0]);
    }

    /// Each channel reaches the last stop on its own: the decided endgame where every win rate
    /// reads the same, and the close endgame where a point is the game.
    #[test]
    fn either_channel_can_carry_the_grade() {
        let last = (GRADE_RAMP.len() - 1) as f32;
        let deep = 10 * TRUSTED_VISITS as u32;
        assert_eq!(grade(0.0, POINTS_AT[5], deep), last);
        assert_eq!(grade(WINRATE_AT[5], 0.0, deep), last);
        // The worse reading wins, and a mild reading on the other channel cannot cool it down.
        assert_eq!(grade(WINRATE_AT[4], POINTS_AT[1], deep), 4.0);
        assert_eq!(grade(WINRATE_AT[1], POINTS_AT[4], deep), 4.0);
    }

    /// A blunder with two visits behind it is a rumour: it may show green, never red. The same
    /// loss with a real search behind it is the whole ramp.
    #[test]
    fn a_grade_is_only_as_hot_as_the_search_behind_it() {
        let huge = grade(0.9, 40.0, 1);
        assert!(
            huge <= UNSEARCHED_CEILING + 0.2,
            "one visit reached {huge}, past the cool end"
        );
        assert!(grade(0.9, 40.0, TRUSTED_VISITS as u32) >= (GRADE_RAMP.len() - 1) as f32);
        // Small losses are unaffected by the cap: it is a ceiling, not a scale.
        let small = grade(0.0, POINTS_AT[1], 1);
        assert!((small - 1.0).abs() < 1e-6, "a quarter point graded {small}");
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
        assert_eq!(css.lines().count(), GRADE_RAMP.len());
        for (i, [r, g, b]) in GRADE_RAMP.iter().enumerate() {
            let fill = format!(
                ".mirai-grade-{} {{ background-color: #{r:02x}{g:02x}{b:02x};",
                i + 1
            );
            assert!(css.contains(&fill), "missing {fill}");
            assert_eq!(grade_stop(i as f32), i as u32);
            assert_eq!(grade_class(i as u32), format!("mirai-grade-{}", i + 1));
        }
        assert_eq!(grade_stop(1.4), 1);
        assert_eq!(grade_stop(1.6), 2);
        assert_eq!(grade_stop(-3.0), 0);
        assert_eq!(grade_stop(99.0), (GRADE_RAMP.len() - 1) as u32);
        assert_eq!(grade_class(4_000), "mirai-grade-6");
    }
}
