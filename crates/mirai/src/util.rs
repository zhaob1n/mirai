// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Small shared helpers.

use mirai_core::{NodeAnalysis, Point, Size};
use mirai_engine::Report;

/// Converts a wire [`Report`] into the dequantised, Black-perspective [`NodeAnalysis`]
/// that the tree stores and the SGF writer persists.
///
/// `max_candidates` is an already-resolved limit — see
/// [`crate::config::AnalysisSettings::stored_suggestion_limit`], which never yields zero.
pub fn analysis_of(report: &Report, max_candidates: usize) -> NodeAnalysis {
    mirai_client::analysis_of(report, max_candidates)
}

/// `true` when a newly arrived evaluation should replace what the node already stores.
///
/// Live analysis streams from a handful of visits up. Writing every snapshot would
/// clobber a finished whole-game sweep with a noisier one and make blunder bars flicker
/// off as the user sits on a move.
pub fn replaces_stored_analysis(existing_visits: Option<u32>, new_visits: u32) -> bool {
    existing_visits.is_none_or(|v| v < new_visits)
}

/// SI-abbreviated visit count: `947`, `1.2k`, `34k`, `1.1m`.
pub fn si_visits(v: u32) -> String {
    match v {
        0..=999 => v.to_string(),
        1_000..=9_999 => format!("{:.1}k", v as f32 / 1000.0),
        10_000..=999_999 => format!("{}k", v / 1000),
        _ => format!("{:.1}m", v as f32 / 1_000_000.0),
    }
}

/// Search speed as `840/s`, `1.2k/s` — [`si_visits`] per second.
pub fn visits_per_second(v: f32) -> String {
    // A float-to-integer cast saturates, so a nonsense rate cannot wrap round.
    format!("{}/s", si_visits(v.max(0.0).round() as u32))
}

/// `56.3` — a win rate as a one-decimal percentage.
pub fn pct1(v: f32) -> String {
    format!("{:.1}", v * 100.0)
}

/// `+3.4` / `-0.8` — a signed score lead with one decimal.
pub fn signed1(v: f32) -> String {
    format!("{v:+.1}")
}

/// Formats a clock as `M:SS` or `H:MM:SS`.
pub fn clock_text(seconds: f32) -> String {
    let s = seconds.max(0.0).round() as u32;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The GTP name of a point, for labels and lists.
pub fn gtp(size: Size, p: Point) -> String {
    size.to_gtp(p).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn si_visits_matches_the_documented_shape() {
        assert_eq!(si_visits(947), "947");
        assert_eq!(si_visits(1234), "1.2k");
        assert_eq!(si_visits(34_000), "34k");
        assert_eq!(si_visits(1_100_000), "1.1m");
        assert_eq!(si_visits(999), "999");
        assert_eq!(si_visits(1000), "1.0k");
    }

    #[test]
    fn visits_per_second_abbreviates_like_a_visit_count() {
        assert_eq!(visits_per_second(0.0), "0/s");
        assert_eq!(visits_per_second(839.6), "840/s");
        assert_eq!(visits_per_second(1240.0), "1.2k/s");
        assert_eq!(visits_per_second(-3.0), "0/s");
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
    fn a_shallower_search_does_not_replace_stored_analysis() {
        assert!(replaces_stored_analysis(None, 8));
        assert!(replaces_stored_analysis(Some(100), 101));
        assert!(!replaces_stored_analysis(Some(100), 100));
        assert!(!replaces_stored_analysis(Some(100), 8));
    }
}
