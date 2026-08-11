// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! KataGo analysis-engine query construction.
//!
//! One [`AnalyzeReq`] is always exactly one position: `analyzeTurns` is never emitted, so
//! every query produces exactly one stream of reports for one turn and a `watch` channel
//! can never drop a result that matters.
//!
//! Field names follow `KataGo/docs/Analysis_Engine.md`. Anything not requested is left
//! out of the query entirely so that KataGo's own config defaults apply.

use mirai_core::{Color, Point, Size};
use mirai_proto::types::{AnalyzeReq, Want};
use serde_json::{Map, Value, json};

/// Builds the query line for one position.
///
/// `id` is the string KataGo echoes back on every response for this query; the local
/// driver uses the decimal form of its subscription id.
pub fn build_query(id: &str, req: &AnalyzeReq) -> Value {
    let mut q = Map::new();
    q.insert("id".into(), Value::String(id.to_owned()));
    q.insert("boardXSize".into(), json!(req.size.w));
    q.insert("boardYSize".into(), json!(req.size.h));
    q.insert("rules".into(), json!(req.rules.katago_name()));
    q.insert("komi".into(), json!(req.komi()));
    q.insert("moves".into(), placements(req.size, &req.moves));

    if !req.initial_stones.is_empty() {
        q.insert(
            "initialStones".into(),
            placements(req.size, &req.initial_stones),
        );
    }
    if let Some(c) = req.initial_player {
        q.insert("initialPlayer".into(), json!(c.katago()));
    }
    if let Some(v) = req.max_visits {
        q.insert("maxVisits".into(), json!(v));
    }
    if let Some(n) = req.pv_len {
        q.insert("analysisPVLen".into(), json!(n));
    }

    q.insert(
        "includeOwnership".into(),
        json!(req.want.contains(Want::OWNERSHIP)),
    );
    q.insert("includePolicy".into(), json!(req.want.contains(Want::POLICY)));
    q.insert(
        "includePVVisits".into(),
        json!(req.want.contains(Want::PV_VISITS)),
    );
    // `Want::MOVES_OWNERSHIP` is reserved: `MoveInfo` has no field for the result, so asking
    // for it would cost KataGo an ownership map per candidate that the decoder then throws
    // away. Always off until a protocol version adds somewhere to put it.
    q.insert("includeMovesOwnership".into(), json!(false));

    if let Some(ms) = req.report_every_ms {
        q.insert(
            "reportDuringSearchEvery".into(),
            json!(ms as f64 / 1000.0),
        );
    }
    q.insert("priority".into(), json!(req.priority));

    let mut avoid = Vec::new();
    let mut allow = Vec::new();
    for spec in &req.avoid {
        if spec.moves.is_empty() {
            continue;
        }
        let entry = json!({
            "player": spec.player.katago(),
            "moves": gtp_list(req.size, &spec.moves),
            "untilDepth": spec.until_depth,
        });
        if spec.allow { allow.push(entry) } else { avoid.push(entry) }
    }
    if !avoid.is_empty() {
        q.insert("avoidMoves".into(), Value::Array(avoid));
    }
    if !allow.is_empty() {
        q.insert("allowMoves".into(), Value::Array(allow));
    }

    // Per-query search parameter overrides. KataGo stringifies every value here anyway
    // (`analysis.cpp` `overrideSettings`), so a time budget goes out as a JSON number and
    // caller-supplied overrides as strings.
    let mut settings = Map::new();
    if let Some(ms) = req.max_time_ms {
        settings.insert("maxTime".into(), json!(ms as f64 / 1000.0));
    }
    for (k, v) in &req.overrides {
        settings.insert(k.clone(), Value::String(v.clone()));
    }
    if !settings.is_empty() {
        q.insert("overrideSettings".into(), Value::Object(settings));
    }

    Value::Object(q)
}

/// `{"id":…,"action":…}` — the shape of `query_version`, `query_models` and `clear_cache`.
pub fn action_query(id: &str, action: &str) -> Value {
    json!({ "id": id, "action": action })
}

/// Asks KataGo to stop the query submitted under `terminate_id` and report what it has.
pub fn terminate_query(id: &str, terminate_id: &str) -> Value {
    json!({ "id": id, "action": "terminate", "terminateId": terminate_id })
}

fn placements(size: Size, list: &[(Color, Point)]) -> Value {
    Value::Array(
        list.iter()
            .map(|&(c, p)| json!([c.katago(), size.to_gtp(p).as_str()]))
            .collect(),
    )
}

fn gtp_list(size: Size, points: &[Point]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|&p| Value::String(size.to_gtp(p).as_str().to_owned()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_core::{Color, RuleSet};
    use mirai_proto::types::AvoidSpec;

    fn req_19() -> AnalyzeReq {
        AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5)
    }

    #[test]
    fn empty_board_query_has_only_the_required_keys() {
        let q = build_query("7", &req_19());
        assert_eq!(
            q,
            json!({
                "id": "7",
                "boardXSize": 19,
                "boardYSize": 19,
                "rules": "chinese",
                "komi": 7.5,
                "moves": [],
                "includeOwnership": false,
                "includePolicy": false,
                "includePVVisits": false,
                "includeMovesOwnership": false,
                "priority": 0,
            })
        );
    }

    #[test]
    fn full_query_matches_katago_field_names() {
        let size = Size::square(19);
        let mut req = req_19();
        req.moves = vec![
            (Color::Black, size.from_gtp("Q4").unwrap()),
            (Color::White, size.from_gtp("D16").unwrap()),
            (Color::Black, Point::PASS),
        ];
        req.initial_stones = vec![(Color::Black, size.from_gtp("Q16").unwrap())];
        req.initial_player = Some(Color::White);
        req.max_visits = Some(1500);
        req.max_time_ms = Some(2500);
        req.pv_len = Some(12);
        req.want = Want::OWNERSHIP | Want::PV_VISITS;
        req.report_every_ms = Some(100);
        req.priority = -3;
        req.avoid = vec![
            AvoidSpec {
                player: Color::White,
                moves: vec![size.from_gtp("A1").unwrap(), Point::PASS],
                until_depth: 4,
                allow: false,
            },
            AvoidSpec {
                player: Color::Black,
                moves: vec![size.from_gtp("C3").unwrap()],
                until_depth: 1,
                allow: true,
            },
            // Empty specs would make KataGo reject the whole query.
            AvoidSpec {
                player: Color::Black,
                moves: vec![],
                until_depth: 9,
                allow: false,
            },
        ];
        req.overrides = vec![("wideRootNoise".into(), "0.0".into())];

        assert_eq!(
            build_query("42", &req),
            json!({
                "id": "42",
                "boardXSize": 19,
                "boardYSize": 19,
                "rules": "chinese",
                "komi": 7.5,
                "moves": [["B", "Q4"], ["W", "D16"], ["B", "pass"]],
                "initialStones": [["B", "Q16"]],
                "initialPlayer": "W",
                "maxVisits": 1500,
                "analysisPVLen": 12,
                "includeOwnership": true,
                "includePolicy": false,
                "includePVVisits": true,
                "includeMovesOwnership": false,
                "reportDuringSearchEvery": 0.1,
                "priority": -3,
                "avoidMoves": [{ "player": "W", "moves": ["A1", "pass"], "untilDepth": 4 }],
                "allowMoves": [{ "player": "B", "moves": ["C3"], "untilDepth": 1 }],
                "overrideSettings": { "maxTime": 2.5, "wideRootNoise": "0.0" },
            })
        );
    }

    #[test]
    fn never_emits_analyze_turns() {
        let mut req = req_19();
        req.moves = vec![(Color::Black, Size::square(19).from_gtp("Q4").unwrap())];
        let q = build_query("1", &req);
        assert!(q.get("analyzeTurns").is_none());
        assert!(q.get("priorities").is_none());
    }

    #[test]
    fn rectangular_board_and_half_komi_survive() {
        let size = Size::new(13, 9).unwrap();
        let mut req = AnalyzeReq::new(size, RuleSet::Japanese, 6.5);
        req.moves = vec![(Color::Black, size.from_gtp("A9").unwrap())];
        let q = build_query("0", &req);
        assert_eq!(q["boardXSize"], json!(13));
        assert_eq!(q["boardYSize"], json!(9));
        assert_eq!(q["komi"], json!(6.5));
        assert_eq!(q["rules"], json!("japanese"));
        // A9 is the top-left corner of a 9-high board, i.e. Point(0).
        assert_eq!(q["moves"], json!([["B", "A9"]]));
    }

    #[test]
    fn action_and_terminate_queries() {
        assert_eq!(
            action_query("v0", "query_version"),
            json!({"id": "v0", "action": "query_version"})
        );
        assert_eq!(
            terminate_query("t9", "9"),
            json!({"id": "t9", "action": "terminate", "terminateId": "9"})
        );
    }
}
