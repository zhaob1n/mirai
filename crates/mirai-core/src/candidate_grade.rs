// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Candidate loss severity, independent of frontend colour and rendering choices.
//!
//! Loss is relative to the engine's first candidate, from the side-to-move perspective.
//! Prefer mean KataGo utility; records saved before MRAI v2 have no utility and use the
//! worse of win-rate and score-lead loss instead. A grade lies in `0.0..=5.0`.

/// Score-lead loss in points at each severity stop, for [`grade_means`] only.
///
/// Inside three quarters of a point stays in the lower bands; losing a komi reaches the
/// last stop. These anchors were fitted on real games before utility was stored.
pub const POINTS_AT: [f32; 6] = [0.0, 0.25, 0.75, 2.0, 4.5, 7.5];

/// Win-rate loss as a fraction at each severity stop, for [`grade_means`] only.
///
/// Five percent reaches stop two and ten percent reaches stop three. This menu-relative
/// grading deliberately warms one band later than a played-move blunder classification.
pub const WINRATE_AT: [f32; 6] = [0.0, 0.02, 0.05, 0.10, 0.18, 0.30];

/// Mean KataGo utility loss at each severity stop.
///
/// `0.04` is KataGo's `wideRootNoise`, `0.20` its `fpuReductionMax`, and `0.50` is half
/// a win on the win/loss term (`winLossUtilityFactor = 1`). Score is already blended
/// into utility, so even a decided endgame can move along this table.
///
/// Measurements over 42 positions at 5 000 visits put losses of 0.25, 0.75 and 2 points
/// near utility losses of 0.034, 0.09 and 0.20, respectively. Re-measure before changing
/// a breakpoint; see the desktop's `docs/dev/CANDIDATE_COLOUR.md`.
pub const UTILITY_AT: [f32; 6] = [0.0, 0.04, 0.10, 0.20, 0.35, 0.50];

/// Visits at which a candidate's estimate is trusted.
///
/// Frontends choose whether the engine's first candidate is exempt, and how to depict
/// unknown estimates. The desktop uses [`is_known`]; other frontends can apply this floor
/// uniformly, including to the reference candidate.
pub const TRUSTED_VISITS: u32 = 10;

/// Mean utility loss as a severity in `0.0..=5.0`.
///
/// Loss is pick minus candidate, side-to-move. Better readings clamp to zero.
pub fn grade(utility_loss: f32) -> f32 {
    along(utility_loss, &UTILITY_AT)
}

/// The pre-v2 fallback: worse of win-rate loss and score-lead loss against the pick.
///
/// Used for tree nodes whose [`crate::Candidate`] has no utility (MRAI v1).
pub fn grade_means(winrate_loss: f32, points_loss: f32) -> f32 {
    along(points_loss, &POINTS_AT).max(along(winrate_loss, &WINRATE_AT))
}

/// Desktop trust rule: the engine's pick is known even before reaching the visit floor.
///
/// `rank` is the move's place in KataGo's `order`; rank zero is the reference every other
/// loss is measured against. Other moves must earn their loss reading with search.
#[inline]
pub fn is_known(rank: usize, visits: u32) -> bool {
    rank == 0 || visits >= TRUSTED_VISITS
}

/// Position along a breakpoint table. Negative losses and NaN clamp to stop zero.
fn along(loss: f32, at: &[f32; 6]) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pick_and_better_readings_have_no_loss() {
        assert_eq!(grade(0.0), 0.0);
        assert_eq!(grade(-0.07), 0.0);
        assert_eq!(grade(f32::NAN), 0.0);
    }

    #[test]
    fn a_utility_loss_walks_the_severity_stops() {
        for (stop, loss) in UTILITY_AT.iter().enumerate() {
            assert_eq!(grade(*loss), stop as f32);
        }
        assert!((grade(0.15) - 2.5).abs() < 1e-6);
        assert_eq!(grade(0.9), 5.0);
    }

    #[test]
    fn either_mean_channel_can_carry_the_fallback() {
        assert_eq!(grade_means(0.0, POINTS_AT[5]), 5.0);
        assert_eq!(grade_means(WINRATE_AT[5], 0.0), 5.0);
        assert_eq!(grade_means(WINRATE_AT[4], POINTS_AT[1]), 4.0);
        assert_eq!(grade_means(WINRATE_AT[1], POINTS_AT[4]), 4.0);
    }

    #[test]
    fn the_desktop_pick_is_exempt_from_the_trust_floor() {
        assert!(is_known(0, 0));
        assert!(!is_known(1, TRUSTED_VISITS - 1));
        assert!(is_known(1, TRUSTED_VISITS));
    }
}
