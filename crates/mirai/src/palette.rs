// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The one candidate colour table the board and the candidate list share.
//!
//! A candidate's colour is its **rank** in KataGo's own ordering — `moveInfos[i].order`, which
//! KataGo sorts by `playSelectionValue`, so rank 1 is the move it would actually play. That is not
//! always the most-visited move: with `useLcbForSelection` (on by default in the analysis engine)
//! a move holding at least `minVisitPropForLCB = 0.15` of the top move's weight can win on its
//! utility lower confidence bound and take rank 1 with fewer visits
//! (`cpp/search/searchresults.cpp`, `getPlaySelectionValues`). How much search a move actually
//! received is a separate question, and the board answers it with opacity, not hue.
//!
//! **Chroma is the quality channel.** The moves worth reading are vivid — blue, cyan, teal,
//! green — and the tail dries out into dusty amber, ochre and brick. Red as a *signal* is not
//! spent here on purpose: on a 20-suggestion board most of what is drawn is the tail of the list,
//! and a saturated red there would shout *blunder* about moves the engine merely never spent time
//! on, while red already means blunder in the win-rate graph and last move on the board.
//!
//! Every stop stays dark enough for white numerals, so a badge or a blob never switches its text
//! colour halfway down the list.
//!
//! Grading a candidate by *how much it loses* — so that a well-searched mistake can earn a real
//! red — is the obvious next step and is deliberately not here yet: win-rate loss alone collapses
//! in decided positions (a measured 9×9 endgame read 0.0 % for every candidate while the score
//! lead spread over six points), so it needs the score channel and a confidence rule that were
//! not settled.

/// Rank 1 through 8-or-worse, as RGB.
pub const RANK_RAMP: [[u8; 3]; 8] = [
    [0x2E, 0x93, 0xFF],
    [0x0F, 0xB5, 0xDE],
    [0x12, 0xC7, 0x9E],
    [0x3F, 0xC2, 0x4A],
    [0x86, 0xA0, 0x2F],
    [0xA8, 0x8A, 0x2C],
    [0xA9, 0x67, 0x3A],
    [0x9E, 0x51, 0x47],
];

/// The colour for a candidate KataGo ranked `order` (0-based), clamped to the last stop.
///
/// Everything past the eighth choice shares that stop: the exact order down there is noise, and
/// the badge in the list still carries the number.
#[inline]
pub fn rank_rgb(order: u32) -> [u8; 3] {
    RANK_RAMP[(order as usize).min(RANK_RAMP.len() - 1)]
}

/// The badge class for a 0-based rank: `mirai-rank-1` … `mirai-rank-8`.
pub fn rank_class(order: u32) -> String {
    format!(
        "mirai-rank-{}",
        (order as usize).min(RANK_RAMP.len() - 1) + 1
    )
}

/// The stylesheet for the list's rank badges, generated from [`RANK_RAMP`] so the badge and the
/// blob for one move can never drift apart.
///
/// The numeral's colour is picked here too, by the same luminance rule the board uses for the text
/// on a blob.
pub fn rank_css() -> String {
    let mut css = String::with_capacity(RANK_RAMP.len() * 96);
    for (i, [r, g, b]) in RANK_RAMP.iter().enumerate() {
        let lum = 0.299 * *r as f32 + 0.587 * *g as f32 + 0.114 * *b as f32;
        let ink = if lum > 148.0 { "#1c1c1c" } else { "#ffffff" };
        css.push_str(&format!(
            ".mirai-rank-{} {{ background-color: #{r:02x}{g:02x}{b:02x}; color: {ink}; }}\n",
            i + 1
        ));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 50-suggestion board hands out ranks far past the table; indexing it must clamp rather
    /// than panic, and the last stop has to stay the last stop.
    #[test]
    fn ranks_past_the_table_share_its_last_stop() {
        assert_eq!(rank_rgb(0), RANK_RAMP[0]);
        assert_eq!(rank_rgb(7), RANK_RAMP[7]);
        assert_eq!(rank_rgb(8), RANK_RAMP[7]);
        assert_eq!(rank_rgb(4_000), RANK_RAMP[7]);
        assert_eq!(rank_class(0), "mirai-rank-1");
        assert_eq!(rank_class(4_000), "mirai-rank-8");
    }

    /// The badge stylesheet and the blob colours come from one table: a rule per stop, each
    /// naming that stop's own hex.
    #[test]
    fn every_stop_gets_a_badge_rule() {
        let css = rank_css();
        assert_eq!(css.lines().count(), RANK_RAMP.len());
        for (i, [r, g, b]) in RANK_RAMP.iter().enumerate() {
            let fill = format!(
                ".mirai-rank-{} {{ background-color: #{r:02x}{g:02x}{b:02x};",
                i + 1
            );
            assert!(css.contains(&fill), "missing {fill}");
        }
    }
}
