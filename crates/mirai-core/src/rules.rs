// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Rulesets, mirrored from KataGo's `cpp/game/rules.cpp` so that local legality checks
//! never disagree with the engine.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Ko {
    Simple,
    Positional,
    Situational,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Scoring {
    Area,
    Territory,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Tax {
    None,
    Seki,
    All,
}

/// White's compensation for handicap stones.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Whb {
    Zero,
    N,
    NMinusOne,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Rules {
    pub ko: Ko,
    pub scoring: Scoring,
    pub tax: Tax,
    pub multi_stone_suicide: bool,
    pub has_button: bool,
    pub whb: Whb,
    pub friendly_pass_ok: bool,
}

/// The named rulesets mirai exposes. `katago_name` is what goes into a query's `rules` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum RuleSet {
    TrompTaylor,
    #[default]
    Chinese,
    ChineseOgs,
    Japanese,
    Korean,
    StoneScoring,
    Aga,
    AgaButton,
    NewZealand,
}

impl RuleSet {
    pub const ALL: [RuleSet; 9] = [
        RuleSet::TrompTaylor,
        RuleSet::Chinese,
        RuleSet::ChineseOgs,
        RuleSet::Japanese,
        RuleSet::Korean,
        RuleSet::StoneScoring,
        RuleSet::Aga,
        RuleSet::AgaButton,
        RuleSet::NewZealand,
    ];

    /// The shorthand string KataGo's analysis engine accepts in `rules`.
    pub const fn katago_name(self) -> &'static str {
        match self {
            RuleSet::TrompTaylor => "tromp-taylor",
            RuleSet::Chinese => "chinese",
            RuleSet::ChineseOgs => "chinese-ogs",
            RuleSet::Japanese => "japanese",
            RuleSet::Korean => "korean",
            RuleSet::StoneScoring => "stone-scoring",
            RuleSet::Aga => "aga",
            RuleSet::AgaButton => "aga-button",
            RuleSet::NewZealand => "new-zealand",
        }
    }

    /// Human-readable label for the UI.
    pub const fn label(self) -> &'static str {
        match self {
            RuleSet::TrompTaylor => "Tromp-Taylor",
            RuleSet::Chinese => "Chinese",
            RuleSet::ChineseOgs => "Chinese (OGS)",
            RuleSet::Japanese => "Japanese",
            RuleSet::Korean => "Korean",
            RuleSet::StoneScoring => "Stone scoring",
            RuleSet::Aga => "AGA",
            RuleSet::AgaButton => "AGA (button)",
            RuleSet::NewZealand => "New Zealand",
        }
    }

    pub fn from_katago_name(s: &str) -> Option<RuleSet> {
        let s = s.trim().to_ascii_lowercase();
        RuleSet::ALL.into_iter().find(|r| r.katago_name() == s)
    }

    pub const fn rules(self) -> Rules {
        use Ko::*;
        use Scoring::*;
        macro_rules! r {
            ($ko:expr, $sc:expr, $tax:expr, $sui:expr, $btn:expr, $whb:expr, $pass:expr) => {
                Rules {
                    ko: $ko,
                    scoring: $sc,
                    tax: $tax,
                    multi_stone_suicide: $sui,
                    has_button: $btn,
                    whb: $whb,
                    friendly_pass_ok: $pass,
                }
            };
        }
        match self {
            RuleSet::TrompTaylor => r!(Positional, Area, Tax::None, true, false, Whb::Zero, false),
            RuleSet::Chinese => r!(Simple, Area, Tax::None, false, false, Whb::N, true),
            RuleSet::ChineseOgs => r!(Positional, Area, Tax::None, false, false, Whb::N, true),
            RuleSet::Japanese | RuleSet::Korean => {
                r!(Simple, Territory, Tax::Seki, false, false, Whb::Zero, false)
            }
            RuleSet::StoneScoring => r!(Simple, Area, Tax::All, false, false, Whb::Zero, true),
            RuleSet::Aga => r!(
                Situational,
                Area,
                Tax::None,
                false,
                false,
                Whb::NMinusOne,
                true
            ),
            RuleSet::AgaButton => {
                r!(
                    Situational,
                    Area,
                    Tax::None,
                    false,
                    true,
                    Whb::NMinusOne,
                    true
                )
            }
            RuleSet::NewZealand => r!(Situational, Area, Tax::None, true, false, Whb::Zero, true),
        }
    }

    pub const fn default_komi(self) -> f32 {
        match self {
            RuleSet::Japanese | RuleSet::Korean => 6.5,
            RuleSet::AgaButton | RuleSet::NewZealand => 7.0,
            _ => 7.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn katago_names_round_trip() {
        for r in RuleSet::ALL {
            assert_eq!(RuleSet::from_katago_name(r.katago_name()), Some(r));
        }
        assert_eq!(RuleSet::from_katago_name("nonsense"), None);
    }

    #[test]
    fn ruleset_tuples_match_katago() {
        let tt = RuleSet::TrompTaylor.rules();
        assert_eq!(tt.ko, Ko::Positional);
        assert!(tt.multi_stone_suicide);
        assert_eq!(tt.whb, Whb::Zero);

        let ch = RuleSet::Chinese.rules();
        assert_eq!(ch.ko, Ko::Simple);
        assert!(!ch.multi_stone_suicide);
        assert_eq!(ch.whb, Whb::N);
        assert_eq!(RuleSet::Chinese.default_komi(), 7.5);

        let jp = RuleSet::Japanese.rules();
        assert_eq!(jp.scoring, Scoring::Territory);
        assert_eq!(jp.tax, Tax::Seki);
        assert_eq!(RuleSet::Japanese.default_komi(), 6.5);

        let ab = RuleSet::AgaButton.rules();
        assert!(ab.has_button);
        assert_eq!(ab.whb, Whb::NMinusOne);
        assert_eq!(RuleSet::AgaButton.default_komi(), 7.0);

        assert_eq!(RuleSet::StoneScoring.rules().tax, Tax::All);
        assert!(RuleSet::NewZealand.rules().multi_stone_suicide);
    }
}
