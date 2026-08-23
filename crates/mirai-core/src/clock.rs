// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Time control and the engine's per-move thinking budget.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TimeControl {
    pub main_s: u32,
    pub byo_periods: u8,
    pub byo_period_s: u32,
    /// Fischer increment added after each move.
    pub increment_s: u32,
}

impl TimeControl {
    /// No clock at all — the caller uses a visit cap instead.
    pub const UNLIMITED: TimeControl = TimeControl {
        main_s: 0,
        byo_periods: 0,
        byo_period_s: 0,
        increment_s: 0,
    };

    #[inline]
    pub const fn is_unlimited(self) -> bool {
        self.main_s == 0 && self.byo_periods == 0
    }
}

/// How long the engine may think for the next move, in seconds.
///
/// `None` means "no time limit" — the caller must cap the search by visits instead.
pub fn think_budget(tc: &TimeControl, remaining_main_s: f32, byo_periods_left: u8) -> Option<f32> {
    if tc.is_unlimited() {
        return None;
    }
    if remaining_main_s > 0.0 {
        // Spend a twentieth of the remaining main time, plus most of the increment, and
        // never more than half of what is left. `max` keeps the clamp range non-empty
        // when almost no main time remains.
        let hi = (remaining_main_s * 0.5).max(0.1);
        Some((remaining_main_s / 20.0 + tc.increment_s as f32 * 0.9).clamp(0.1, hi))
    } else if tc.byo_periods > 0 && byo_periods_left == 0 {
        // Out of periods: about to lose on time, so move immediately.
        Some(0.1)
    } else {
        Some((tc.byo_period_s as f32 * 0.9).max(0.1))
    }
}

/// Formats a clock as `M:SS`, or `H:MM:SS` once past an hour.
pub fn clock_text(seconds: f32) -> String {
    let s = seconds.max(0.0).round() as u32;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_has_no_budget() {
        assert_eq!(think_budget(&TimeControl::UNLIMITED, 0.0, 0), None);
        assert!(TimeControl::UNLIMITED.is_unlimited());
    }

    #[test]
    fn clock_text_switches_to_hours() {
        assert_eq!(clock_text(0.0), "0:00");
        assert_eq!(clock_text(59.4), "0:59");
        assert_eq!(clock_text(600.0), "10:00");
        assert_eq!(clock_text(3661.0), "1:01:01");
        assert_eq!(clock_text(-5.0), "0:00");
    }

    #[test]
    fn main_time_branch_spends_a_twentieth_and_never_over_half() {
        let tc = TimeControl {
            main_s: 600,
            byo_periods: 5,
            byo_period_s: 30,
            increment_s: 0,
        };
        assert_eq!(think_budget(&tc, 600.0, 5), Some(30.0));
        assert_eq!(think_budget(&tc, 40.0, 5), Some(2.0));
        // The floor keeps the budget positive when the clock is nearly out.
        assert_eq!(think_budget(&tc, 1.0, 5), Some(0.1));

        // With a big increment the half-of-remaining ceiling is what bites.
        let fischer = TimeControl {
            main_s: 60,
            byo_periods: 0,
            byo_period_s: 0,
            increment_s: 10,
        };
        assert_eq!(think_budget(&fischer, 1.0, 0), Some(0.5));
    }

    #[test]
    fn increment_is_mostly_spent() {
        let tc = TimeControl {
            main_s: 300,
            byo_periods: 0,
            byo_period_s: 0,
            increment_s: 10,
        };
        assert_eq!(think_budget(&tc, 200.0, 0), Some(200.0 / 20.0 + 9.0));
    }

    #[test]
    fn byoyomi_branch_uses_most_of_a_period() {
        let tc = TimeControl {
            main_s: 60,
            byo_periods: 3,
            byo_period_s: 30,
            increment_s: 0,
        };
        assert_eq!(think_budget(&tc, 0.0, 3), Some(27.0));
        // No periods left: move immediately rather than lose on time.
        assert_eq!(think_budget(&tc, 0.0, 0), Some(0.1));
    }
}
