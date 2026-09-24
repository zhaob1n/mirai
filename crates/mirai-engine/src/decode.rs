// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! KataGo response decoding.
//!
//! This is the only place a KataGo JSON response becomes a [`Report`]. `mirai-server`
//! forwards the very same `Report`, so a local and a remote engine produce bit-identical
//! results and the GUI has one code path.
//!
//! All values are Black-perspective: mirai always launches KataGo with
//! `reportAnalysisWinratesAs=BLACK`.

use mirai_core::{Color, Point, Size};
use mirai_proto::types::{
    LCB_SCALE, MoveInfo, RAW_VAR_TIME_SCALE, Report, RootInfo, SCORE_SCALE, STDEV_SCALE,
    UTILITY_SCALE, q_own, q_policy, q16, qs, qu,
};
use serde_json::Value;

/// One classified line of KataGo's stdout.
#[derive(Clone, Debug, PartialEq)]
pub enum RawResponse {
    /// An analysis result for one turn of one query.
    Analysis {
        id: String,
        /// `!isDuringSearch` — the query is finished and will send nothing more.
        terminal: bool,
        /// The query was terminated before any search happened; there is no data.
        no_results: bool,
        body: Value,
    },
    /// The echo of a special action query (`query_version`, `query_models`, `terminate`).
    Action {
        id: String,
        action: String,
        body: Value,
    },
    /// An error tied to one query: only that subscription fails.
    QueryError { id: String, msg: String },
    /// An error with no query attached: the engine itself is broken.
    EngineFault { msg: String },
    /// A warning; the query it belongs to still proceeds.
    Warning { id: Option<String>, msg: String },
}

impl RawResponse {
    /// Classifies one parsed response line.
    ///
    /// `Err` means the line is not something this protocol version defines — the caller
    /// logs it and carries on, as the protocol documentation asks consumers to do.
    pub fn classify(body: Value) -> Result<RawResponse, String> {
        if !body.is_object() {
            return Err("response is not a JSON object".into());
        }
        let id = body.get("id").and_then(Value::as_str).map(str::to_owned);

        if let Some(msg) = message(&body, "error") {
            return Ok(match id {
                Some(id) => RawResponse::QueryError { id, msg },
                None => RawResponse::EngineFault { msg },
            });
        }
        if let Some(msg) = message(&body, "warning") {
            return Ok(RawResponse::Warning { id, msg });
        }
        let Some(id) = id else {
            return Err("response has neither an id nor an error".into());
        };
        if let Some(action) = body.get("action").and_then(Value::as_str) {
            let action = action.to_owned();
            return Ok(RawResponse::Action { id, action, body });
        }
        let no_results = body
            .get("noResults")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let during = body
            .get("isDuringSearch")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(RawResponse::Analysis {
            id,
            terminal: !during || no_results,
            no_results,
            body,
        })
    }
}

/// Turns one analysis response into a [`Report`], quantising every float exactly once.
///
/// `max_candidates` keeps the engine's best `n` by `order`; the rest are never decoded.
pub fn decode_report(size: Size, max_candidates: Option<u8>, v: &Value) -> Result<Report, String> {
    let root_v = v
        .get("rootInfo")
        .ok_or_else(|| "analysis response has no rootInfo".to_string())?;
    let current_player = root_v
        .get("currentPlayer")
        .and_then(Value::as_str)
        .and_then(Color::from_letter)
        .ok_or_else(|| "rootInfo has no usable currentPlayer".to_string())?;

    let root = RootInfo {
        visits: count(root_v, "visits"),
        winrate: q16(num(root_v, "winrate").unwrap_or(0.5)),
        score_lead: qs(num(root_v, "scoreLead").unwrap_or(0.0), SCORE_SCALE),
        score_selfplay: qs(num(root_v, "scoreSelfplay").unwrap_or(0.0), SCORE_SCALE),
        score_stdev: qu(num(root_v, "scoreStdev").unwrap_or(0.0), STDEV_SCALE),
        utility: qs(num(root_v, "utility").unwrap_or(0.0), UTILITY_SCALE),
        current_player,
        raw_winrate: num(root_v, "rawWinrate").map(q16),
        raw_lead: num(root_v, "rawLead").map(|v| qs(v, SCORE_SCALE)),
        raw_var_time_left: num(root_v, "rawVarTimeLeft").map(|v| qu(v, RAW_VAR_TIME_SCALE)),
    };

    let moves = match v.get("moveInfos") {
        Some(Value::Array(list)) => {
            // KataGo emits moveInfos in `order` already, but that is not promised anywhere
            // and the GUI treats moves[0] as the engine's choice. Sorting borrowed JSON
            // values costs one pointer array: sorting decoded moves would memcpy a
            // PV-carrying struct on every swap, and then needed a second `Vec` to strip the
            // sort key off again.
            let mut ordered: Vec<&Value> = list.iter().collect();
            ordered.sort_by_key(|m| order_of(m));
            // After the sort: the cap keeps the engine's best, not KataGo's first.
            if let Some(n) = max_candidates {
                ordered.truncate(usize::from(n));
            }
            let mut out = Vec::with_capacity(ordered.len());
            for m in ordered {
                out.push(decode_move(size, m)?);
            }
            out
        }
        Some(_) => return Err("moveInfos is not an array".into()),
        None => Vec::new(),
    };

    let ownership = match v.get("ownership") {
        Some(list) => Some(quantised(list, "ownership", size.points(), q_own)?),
        None => None,
    };
    let policy = match v.get("policy") {
        Some(list) => Some(quantised(list, "policy", size.points() + 1, q_policy)?),
        None => None,
    };

    Ok(Report {
        turn: v
            .get("turnNumber")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u16::MAX as u64) as u16,
        root,
        moves,
        ownership,
        policy,
    })
}

/// One `moveInfos` entry. Sorting them is the caller's job, via [`order_of`].
fn decode_move(size: Size, m: &Value) -> Result<MoveInfo, String> {
    let mv = point(size, m.get("move"))?;
    let visits = count(m, "visits");

    let pv = match m.get("pv") {
        Some(Value::Array(list)) => list
            .iter()
            .map(|p| point(size, Some(p)))
            .collect::<Result<Vec<Point>, String>>()?,
        _ => Vec::new(),
    };
    let pv_visits = match m.get("pvVisits") {
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| v.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32)
            .collect(),
        _ => Vec::new(),
    };

    Ok(MoveInfo {
        mv,
        visits,
        edge_visits: m
            .get("edgeVisits")
            .and_then(Value::as_u64)
            .map_or(visits, |v| v.min(u32::MAX as u64) as u32),
        winrate: q16(num(m, "winrate").unwrap_or(0.5)),
        prior: q16(num(m, "prior").unwrap_or(0.0)),
        lcb: qs(num(m, "lcb").unwrap_or(0.0), LCB_SCALE),
        utility: qs(num(m, "utility").unwrap_or(0.0), UTILITY_SCALE),
        utility_lcb: qs(num(m, "utilityLcb").unwrap_or(0.0), UTILITY_SCALE),
        score_lead: qs(num(m, "scoreLead").unwrap_or(0.0), SCORE_SCALE),
        score_selfplay: qs(num(m, "scoreSelfplay").unwrap_or(0.0), SCORE_SCALE),
        score_stdev: qu(num(m, "scoreStdev").unwrap_or(0.0), STDEV_SCALE),
        // Clamped for the wire; `order_of` is what ordering actually uses, so two moves
        // colliding at 255 stay in the order KataGo gave them.
        order: order_of(m).min(u8::MAX as u64) as u8,
        play_value: num(m, "playSelectionValue")
            .unwrap_or(0.0)
            .round()
            .clamp(0.0, u32::MAX as f64) as u32,
        pv,
        pv_visits,
    })
}

/// A move's raw `order`. Missing sorts last, which is where KataGo puts unsearched moves.
fn order_of(m: &Value) -> u64 {
    m.get("order").and_then(Value::as_u64).unwrap_or(u64::MAX)
}

fn point(size: Size, v: Option<&Value>) -> Result<Point, String> {
    let s = v
        .and_then(Value::as_str)
        .ok_or_else(|| "moveInfo without a move string".to_string())?;
    size.from_gtp(s)
        .ok_or_else(|| format!("cannot read move {s:?} on a {}x{} board", size.w, size.h))
}

fn message(v: &Value, key: &str) -> Option<String> {
    let msg = v.get(key).and_then(Value::as_str)?;
    Some(match v.get("field").and_then(Value::as_str) {
        Some(field) => format!("{msg} (field {field})"),
        None => msg.to_owned(),
    })
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn count(v: &Value, key: &str) -> u32 {
    v.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

/// Quantises a KataGo float array straight into its wire form.
///
/// The `Vec<f64>` this used to collect first existed only to be length-checked: 2.9 KB for
/// one 19x19 ownership map, allocated and dropped again ten times a second per subscription,
/// and once more per client on the server.
fn quantised<T>(
    v: &Value,
    key: &str,
    expected: usize,
    q: impl Fn(f64) -> T,
) -> Result<Vec<T>, String> {
    let Value::Array(list) = v else {
        return Err(format!("{key} is not an array"));
    };
    if list.len() != expected {
        return Err(format!(
            "{key} has {} entries, expected {expected}",
            list.len()
        ));
    }
    let mut out = Vec::with_capacity(expected);
    for x in list {
        let f = x
            .as_f64()
            .ok_or_else(|| format!("{key} has a non-number"))?;
        out.push(q(f));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_proto::types::{POLICY_ILLEGAL, dq_own, dq_policy, dq16, dqs};

    /// A 5x5 response in the exact shape KataGo emits, trimmed to a readable size.
    fn fixture() -> Value {
        let size = Size::square(5);
        let n = size.points();
        // Ownership: Black owns the top row, White the bottom row, neutral elsewhere.
        let ownership: Vec<f64> = (0..n)
            .map(|i| match i / 5 {
                0 => 0.75,
                4 => -0.5,
                _ => 0.0,
            })
            .collect();
        // Policy: illegal everywhere except C3 and pass.
        let mut policy = vec![-1.0f64; n + 1];
        policy[2 * 5 + 2] = 0.875;
        policy[n] = 0.125;

        serde_json::json!({
            "id": "17",
            "isDuringSearch": true,
            "turnNumber": 3,
            "rootInfo": {
                "currentPlayer": "W",
                "visits": 148,
                "winrate": 0.6125,
                "scoreLead": 3.5,
                "scoreSelfplay": 4.25,
                "scoreStdev": 12.5,
                "utility": 0.4,
                "rawWinrate": 0.58,
                "rawLead": 2.75,
                "rawVarTimeLeft": 41.5,
                "thisHash": "F8FAEDA0E0C89DDC5AA5CCBB5E7B859D",
                "symHash": "1D25038E8FC8C26C456B8DF2DBF70C02"
            },
            "moveInfos": [
                {
                    "move": "B2", "order": 1, "visits": 20, "edgeVisits": 19,
                    "winrate": 0.4, "prior": 0.1, "lcb": 0.35,
                    "utility": -0.25, "utilityLcb": -0.4,
                    "scoreLead": -1.5, "scoreSelfplay": -1.0, "scoreStdev": 11.0,
                    "playSelectionValue": 20.4,
                    "pv": ["B2", "D4"], "pvVisits": [20, 7]
                },
                {
                    "move": "C3", "order": 0, "visits": 120, "edgeVisits": 120,
                    "winrate": 0.6125, "prior": 0.8, "lcb": 0.6,
                    "utility": 0.5, "utilityLcb": 0.45,
                    "scoreLead": 3.5, "scoreSelfplay": 4.25, "scoreStdev": 12.5,
                    "playSelectionValue": 120.0,
                    "pv": ["C3", "B2", "pass"], "pvVisits": [120, 40, 3]
                }
            ],
            "ownership": ownership,
            "policy": policy
        })
    }

    #[test]
    fn decodes_a_report_in_order_with_black_perspective_values() {
        let size = Size::square(5);
        let r = decode_report(size, None, &fixture()).unwrap();

        assert_eq!(r.turn, 3);
        assert_eq!(r.root.current_player, Color::White);
        assert_eq!(r.root.visits, 148);
        // Reported values are Black's; White to play sees the complement.
        assert!((r.root.winrate_f32() - 0.6125).abs() < 1e-4);
        assert!((r.root.winrate_for(Color::White) - 0.3875).abs() < 1e-4);
        assert!((r.root.score_lead_f32() - 3.5).abs() < 0.02);
        assert!((r.root.score_lead_for(Color::White) + 3.5).abs() < 0.02);
        assert!(r.root.raw_winrate.map(dq16).is_some());
        assert!((dqs(r.root.raw_lead.unwrap(), SCORE_SCALE) - 2.75).abs() < 0.02);

        // moveInfos arrived out of order; the report is sorted by `order`.
        assert_eq!(r.moves.len(), 2);
        assert_eq!(r.moves[0].order, 0);
        assert_eq!(r.moves[0].mv, size.from_gtp("C3").unwrap());
        assert_eq!(r.moves[0].visits, 120);
        assert_eq!(r.moves[0].play_value, 120);
        assert_eq!(r.moves[1].mv, size.from_gtp("B2").unwrap());
        assert_eq!(r.moves[1].edge_visits, 19);
        assert_eq!(r.moves[1].play_value, 20);
        assert!((r.moves[1].winrate_f32() - 0.4).abs() < 1e-4);
        assert!((r.moves[1].lcb_f32() - 0.35).abs() < 1e-4);
        assert!((r.moves[1].score_lead_f32() + 1.5).abs() < 0.02);

        // A pass inside a PV must survive as Point::PASS, not as a board point.
        assert_eq!(
            r.moves[0].pv,
            vec![
                size.from_gtp("C3").unwrap(),
                size.from_gtp("B2").unwrap(),
                Point::PASS
            ]
        );
        assert_eq!(r.moves[0].pv_visits, vec![120, 40, 3]);
    }

    /// The fixture lists B2 (order 1) before C3 (order 0). A cap applied in KataGo's array
    /// order would keep B2 and drop the engine's own pick.
    #[test]
    fn the_candidate_cap_keeps_the_engines_best_by_order() {
        let size = Size::square(5);
        let r = decode_report(size, Some(1), &fixture()).unwrap();
        assert_eq!(r.moves.len(), 1);
        assert_eq!(r.moves[0].mv, size.from_gtp("C3").unwrap());
        assert_eq!(r.moves[0].order, 0);
    }

    #[test]
    fn ownership_keeps_katago_row_major_top_left_order() {
        let size = Size::square(5);
        let own = decode_report(size, None, &fixture())
            .unwrap()
            .ownership
            .unwrap();
        assert_eq!(own.len(), size.points());
        // Row 0 is the TOP row (A5..E5) and is Black's in the fixture.
        for x in 0..5 {
            assert!(dq_own(own[size.point(x, 0).index()]) > 0.7);
            assert!(dq_own(own[size.point(x, 4).index()]) < -0.45);
            assert_eq!(own[size.point(x, 2).index()], 0);
        }
    }

    #[test]
    fn policy_has_a_pass_slot_and_marks_illegal_moves() {
        let size = Size::square(5);
        let policy = decode_report(size, None, &fixture())
            .unwrap()
            .policy
            .unwrap();
        assert_eq!(policy.len(), size.points() + 1);
        let c3 = size.from_gtp("C3").unwrap().index();
        assert!((dq_policy(policy[c3]).unwrap() - 0.875).abs() < 1e-4);
        assert!((dq_policy(policy[size.points()]).unwrap() - 0.125).abs() < 1e-4);
        assert_eq!(policy[0], POLICY_ILLEGAL);
        assert_eq!(dq_policy(policy[0]), None);
    }

    #[test]
    fn wrong_length_arrays_are_rejected() {
        let size = Size::square(19);
        // The 5x5 fixture's arrays are the wrong length for a 19x19 board.
        let err = decode_report(size, None, &fixture()).unwrap_err();
        assert!(err.contains("ownership"), "{err}");
    }

    #[test]
    fn classifies_every_response_shape() {
        let during = RawResponse::classify(fixture()).unwrap();
        assert!(matches!(
            during,
            RawResponse::Analysis {
                terminal: false,
                no_results: false,
                ..
            }
        ));

        let final_report = RawResponse::classify(serde_json::json!({
            "id": "17", "isDuringSearch": false, "turnNumber": 3, "rootInfo": {}
        }))
        .unwrap();
        assert!(matches!(
            final_report,
            RawResponse::Analysis {
                terminal: true,
                no_results: false,
                ..
            }
        ));

        // A response with no isDuringSearch field at all is a final one.
        let plain = RawResponse::classify(serde_json::json!({"id": "1", "rootInfo": {}})).unwrap();
        assert!(matches!(
            plain,
            RawResponse::Analysis { terminal: true, .. }
        ));

        let terminated = RawResponse::classify(
            serde_json::json!({"id":"17","isDuringSearch":false,"noResults":true,"turnNumber":2}),
        )
        .unwrap();
        assert!(matches!(
            terminated,
            RawResponse::Analysis {
                terminal: true,
                no_results: true,
                ..
            }
        ));

        assert_eq!(
            RawResponse::classify(serde_json::json!({
                "error": "Must be an integer", "field": "maxVisits", "id": "5"
            }))
            .unwrap(),
            RawResponse::QueryError {
                id: "5".into(),
                msg: "Must be an integer (field maxVisits)".into()
            }
        );
        assert_eq!(
            RawResponse::classify(serde_json::json!({"error": "Could not parse json"})).unwrap(),
            RawResponse::EngineFault {
                msg: "Could not parse json".into()
            }
        );
        assert_eq!(
            RawResponse::classify(serde_json::json!({
                "warning": "Unknown config params: foo", "field": "overrideSettings", "id": "5"
            }))
            .unwrap(),
            RawResponse::Warning {
                id: Some("5".into()),
                msg: "Unknown config params: foo (field overrideSettings)".into()
            }
        );

        let version = RawResponse::classify(serde_json::json!({
            "id": "v0", "action": "query_version", "version": "1.16.4"
        }))
        .unwrap();
        match version {
            RawResponse::Action { id, action, body } => {
                assert_eq!((id.as_str(), action.as_str()), ("v0", "query_version"));
                assert_eq!(body["version"], "1.16.4");
            }
            other => panic!("{other:?}"),
        }

        assert!(RawResponse::classify(serde_json::json!(["not", "an", "object"])).is_err());
        assert!(RawResponse::classify(serde_json::json!({"turnNumber": 1})).is_err());
    }
}
