// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The performance claim of MRP is that a live analysis report costs well under a
//! kilobyte on the wire, where KataGo's own JSON for it runs to tens of kilobytes, and that
//! a subscription stream carries much less than that once it has seen the reports before.
//! Real-search figures come from `wire_bench` (`docs/dev/TESTING.md` §4); these tests pin
//! the codec on synthetic reports so that a regression shows up in `cargo test`.

use mirai_core::{Color, Point, RuleSet, Size};
use mirai_proto::frame::{FrameBuf, SUB_STREAM_LEVEL, SubStreamDecoder, SubStreamEncoder, encode};
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

/// The first frame of a stream has nothing to be compressed against, so this is the
/// worst case a report pays.
#[test]
fn a_full_live_report_frames_under_4_kb() {
    let size = Size::square(19);
    let report = sample_report(size, 50, 15);
    assert_eq!(report.ownership.as_ref().unwrap().len(), 361);

    let mut enc = SubStreamEncoder::new(SUB_STREAM_LEVEL).unwrap();
    let msg = SubMsg::Report(report);
    let framed = enc.encode(&msg).unwrap().to_vec();
    let n = framed.len();
    println!("framed SubMsg::Report = {n} bytes (50 candidates, PV 15, pv_visits, 361 ownership)");
    assert!(
        n < 4096,
        "a live report must stay under 4096 framed bytes, got {n}"
    );

    // And it survives the round trip unchanged.
    let back: SubMsg = SubStreamDecoder::new().unwrap().decode(&framed).unwrap();
    assert_eq!(back, msg);
}

/// One zstd stream per subscription: each report is compressed against those before it.
/// Consecutive reports of one search differ in a few numbers, so after the first frame a
/// stream should carry little more than those numbers.
#[test]
fn a_report_stream_compresses_each_report_against_the_last() {
    let size = Size::square(19);
    let base = sample_report(size, 10, 15);
    let mut enc = SubStreamEncoder::new(SUB_STREAM_LEVEL).unwrap();
    let mut dec = SubStreamDecoder::new().unwrap();

    let mut sizes = Vec::new();
    for k in 0..20u32 {
        let mut r = base.clone();
        r.root.visits = 1000 * (k + 1);
        for (i, m) in r.moves.iter_mut().enumerate() {
            m.visits += 100 * k;
            m.winrate = q16(0.52 - 0.004 * i as f64 + 0.0005 * k as f64);
            m.play_value = m.visits;
        }
        for (j, o) in r.ownership.as_mut().unwrap().iter_mut().enumerate() {
            if (j + k as usize).is_multiple_of(7) {
                *o = o.saturating_add(1);
            }
        }
        let msg = SubMsg::Report(r);
        let frame = enc.encode(&msg).unwrap();
        sizes.push(frame.len());
        // Decoded as it arrives: nothing may wait for the next frame.
        let back: SubMsg = dec.decode(frame).unwrap();
        assert_eq!(back, msg);
    }

    let later = sizes[1..].iter().sum::<usize>() as f64 / (sizes.len() - 1) as f64;
    println!(
        "stream of 20 reports: first frame {} bytes, then {later:.0} on average",
        sizes[0]
    );
    assert!(
        later <= 0.5 * sizes[0] as f64,
        "later frames average {later:.0} bytes against a first frame of {}",
        sizes[0]
    );
}

#[test]
fn dequantisation_error_stays_inside_the_documented_tolerances() {
    // PROTOCOL §7.2: max round-trip error is half a quantum. A sweep whose step is a
    // multiple of the quantum (lead stepped by 1/40 against a 1/32 grid) never sees it.
    fn half_quantum(x: f64, decode: impl Fn(f64) -> f64) -> f64 {
        (decode(x) - x).abs().max((decode(-x) - (-x)).abs())
    }
    let mut worst_wr = 0.0f64;
    for i in 0..=10_000 {
        let wr = i as f64 / 10_000.0;
        worst_wr = worst_wr.max((dq16(q16(wr)) as f64 - wr).abs());
    }
    let lead = half_quantum(0.5 / SCORE_SCALE, |x| {
        dqs(qs(x, SCORE_SCALE), SCORE_SCALE) as f64
    });
    let lcb = half_quantum(0.5 / LCB_SCALE, |x| dqs(qs(x, LCB_SCALE), LCB_SCALE) as f64);
    let utility = half_quantum(0.5 / UTILITY_SCALE, |x| {
        dqs(qs(x, UTILITY_SCALE), UTILITY_SCALE) as f64
    });
    let stdev = {
        let x = 0.5 / STDEV_SCALE;
        (dqu(qu(x, STDEV_SCALE), STDEV_SCALE) as f64 - x).abs()
    };
    let own = {
        let x = 0.5 / 127.0;
        (dq_own(q_own(x)) as f64 - x).abs()
    };
    let raw_var = {
        let x = 0.5 / RAW_VAR_TIME_SCALE;
        (dqu(qu(x, RAW_VAR_TIME_SCALE), RAW_VAR_TIME_SCALE) as f64 - x).abs()
    };
    // The whole documented range still round-trips: a larger scale would saturate
    // the i16/i8 before a big lead or a settled point, which a half-quantum probe
    // near zero cannot see.
    let mut worst_lead_range = 0.0f64;
    for i in -1000..=1000 {
        let v = i as f64;
        worst_lead_range =
            worst_lead_range.max((dqs(qs(v, SCORE_SCALE), SCORE_SCALE) as f64 - v).abs());
    }
    let mut worst_own_range = 0.0f64;
    for i in -200..=200 {
        let v = i as f64 / 200.0;
        worst_own_range = worst_own_range.max((dq_own(q_own(v)) as f64 - v).abs());
    }

    println!("worst winrate err {worst_wr:.3e}, lead err {lead:.6}, ownership err {own:.5}");
    assert!(
        worst_lead_range <= 0.02,
        "score lead over ±1000: {worst_lead_range}"
    );
    assert!(
        worst_own_range <= 0.005,
        "ownership over ±1: {worst_own_range}"
    );
    assert!(worst_wr <= 1e-4, "winrate error {worst_wr}");
    assert!(
        (lead - 0.015625).abs() < 1e-9,
        "score lead half-quantum is 0.015625, measured {lead}"
    );
    assert!(lead <= 0.02, "score lead error {lead}");
    assert!((own - 0.5 / 127.0).abs() < 1e-6, "ownership error {own}");
    assert!(own <= 0.005, "ownership error {own}");
    // Literals, not the constants: these are the normative figures in PROTOCOL §7.2,
    // so a changed scale must fail here and send someone to the specification.
    assert!((lcb - 3.0517578125e-5).abs() < 1e-12, "lcb error {lcb}");
    assert!(
        (utility - 6.103515625e-5).abs() < 1e-12,
        "utility error {utility}"
    );
    assert!((stdev - 0.015625).abs() < 1e-9, "score stdev error {stdev}");
    // q16 and q_policy spell their scale separately in the encoder and the decoder, so a
    // round trip cannot see one side changed alone: a half-quantum probe sits on a
    // rounding tie. Exact codes can. Each encoder gets one point per direction a wrong
    // scale would round (65534 or 65536 for q16, 65533 or 65535 for q_policy), and each
    // decoder the full-scale code, which is exactly 1 only at the right divisor.
    assert_eq!(q16(0.5), 32768);
    assert_eq!(q16(0.75), 49151);
    assert_eq!(dq16(65535), 1.0);
    assert_eq!(q_policy(0.5), 32767);
    assert_eq!(q_policy(0.25), 16384);
    assert_eq!(dq_policy(65534), Some(1.0));
    assert_ne!(
        q_policy(1.0),
        POLICY_ILLEGAL,
        "q_policy must not use the illegal sentinel for a legal probability"
    );
    assert!(dq_policy(q_policy(1.0)).is_some());
    assert!(
        (raw_var - 0.125).abs() < 1e-9,
        "raw_var_time_left error {raw_var}"
    );
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
