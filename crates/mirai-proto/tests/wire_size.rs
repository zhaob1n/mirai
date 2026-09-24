// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The performance claim of MRP/2 is that a live analysis report costs a couple of
//! kilobytes on the wire instead of the ~45 KB of equivalent KataGo JSON. That is a claim,
//! so it is measured here rather than asserted in prose.

use mirai_core::{Color, Point, RuleSet, Size};
use mirai_proto::frame::{FrameBuf, encode};
use mirai_proto::msg::SubMsg;
use mirai_proto::types::*;

fn sample_report(size: Size, candidates: usize, pv_len: usize) -> Report {
    let n = size.points();
    let mut moves = Vec::with_capacity(candidates);
    for i in 0..candidates {
        let base = 60 + i * 7;
        moves.push(MoveInfo {
            mv: Point((base % n) as u16),
            visits: (200_000 / (i as u32 + 1)).max(3),
            edge_visits: (199_000 / (i as u32 + 1)).max(3),
            winrate: q16(0.52 - i as f64 * 0.004),
            prior: q16(0.18 / (i as f64 + 1.0)),
            lcb: qs(0.51 - i as f64 * 0.004, LCB_SCALE),
            utility: qs(0.09 - i as f64 * 0.01, UTILITY_SCALE),
            utility_lcb: qs(0.07 - i as f64 * 0.01, UTILITY_SCALE),
            score_lead: qs(1.5 - i as f64 * 0.3, SCORE_SCALE),
            score_selfplay: qs(1.7 - i as f64 * 0.3, SCORE_SCALE),
            score_stdev: qu(14.25, STDEV_SCALE),
            order: i as u8,
            play_value: 200_000 / (i as u32 + 1),
            pv: (0..pv_len)
                .map(|k| Point(((base + k * 23) % n) as u16))
                .collect(),
            pv_visits: (0..pv_len)
                .map(|k| (200_000 / (i as u32 + 1)) >> k.min(17))
                .collect(),
        });
    }

    Report {
        turn: 42,
        root: RootInfo {
            visits: 1_000_000,
            winrate: q16(0.5231),
            score_lead: qs(1.52, SCORE_SCALE),
            score_selfplay: qs(1.74, SCORE_SCALE),
            score_stdev: qu(14.25, STDEV_SCALE),
            utility: qs(0.0912, UTILITY_SCALE),
            current_player: Color::Black,
            raw_winrate: None,
            raw_lead: None,
            raw_var_time_left: None,
        },
        moves,
        ownership: Some(
            (0..n)
                .map(|i| q_own(((i % 41) as f64 - 20.0) / 20.0))
                .collect(),
        ),
        policy: None,
    }
}

#[test]
fn a_full_live_report_frames_under_4_kb() {
    let size = Size::square(19);
    let report = sample_report(size, 50, 15);
    assert_eq!(report.ownership.as_ref().unwrap().len(), 361);

    let mut buf = FrameBuf::new();
    let n = encode(&mut buf, &SubMsg::Report(report.clone()))
        .unwrap()
        .len();
    let framed = buf.frame().to_vec();
    println!("framed SubMsg::Report = {n} bytes (50 candidates, PV 15, pv_visits, 361 ownership)");
    assert!(
        n < 4096,
        "a live report must stay under 4096 framed bytes, got {n}"
    );

    // And it survives the round trip unchanged.
    let mut rbuf = FrameBuf::new();
    let back: SubMsg = mirai_proto::frame::decode(&mut rbuf, &framed).unwrap();
    assert_eq!(back, SubMsg::Report(report));
}

#[test]
fn dequantisation_error_stays_inside_the_documented_tolerances() {
    // Winrate: <= 1e-4 absolute. Score lead: <= 0.02 points. Ownership: <= 0.005.
    let mut worst_wr = 0.0f64;
    let mut worst_lead = 0.0f64;
    let mut worst_own = 0.0f64;

    for i in 0..=10_000 {
        let wr = i as f64 / 10_000.0;
        worst_wr = worst_wr.max((dq16(q16(wr)) as f64 - wr).abs());
    }
    for i in -40_000..=40_000i32 {
        let lead = i as f64 / 40.0; // -1000.0 ..= 1000.0 in 0.025 steps
        let back = dqs(qs(lead, SCORE_SCALE), SCORE_SCALE) as f64;
        worst_lead = worst_lead.max((back - lead).abs());
    }
    for i in -1000..=1000i32 {
        let v = i as f64 / 1000.0;
        worst_own = worst_own.max((dq_own(q_own(v)) as f64 - v).abs());
    }

    println!(
        "worst winrate err {worst_wr:.3e}, lead err {worst_lead:.5}, ownership err {worst_own:.5}"
    );
    assert!(worst_wr <= 1e-4, "winrate error {worst_wr}");
    assert!(worst_lead <= 0.02, "score lead error {worst_lead}");
    assert!(worst_own <= 0.005, "ownership error {worst_own}");
}

#[test]
fn an_open_request_is_tiny() {
    let mut req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
    let size = req.size;
    for i in 0..200u16 {
        let c = if i % 2 == 0 {
            Color::Black
        } else {
            Color::White
        };
        req.moves
            .push((c, size.point((i % 19) as u8, (i / 19) as u8)));
    }
    req.want = Want::OWNERSHIP | Want::PV_VISITS;
    req.max_visits = Some(1_000_000);
    req.report_every_ms = Some(100);

    let msg = mirai_proto::msg::ClientMsg::Open {
        sub: 7,
        engine: None,
        req,
    };
    let mut buf = FrameBuf::new();
    let n = encode(&mut buf, &msg).unwrap().len();
    println!("framed ClientMsg::Open with 200 moves = {n} bytes");
    assert!(n < 1024, "a 200-move Open should stay under 1 KB, got {n}");
}
