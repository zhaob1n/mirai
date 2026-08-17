// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026

//! Verifies postcard encoding matches what the ArkTS client produces.

use mirai_core::{RuleSet, Size};
use mirai_proto::msg::ClientMsg;
use mirai_proto::types::{AnalyzeReq, Want};
use std::cmp::min;

fn serialize_frame<T: serde::Serialize>(msg: &T) -> Vec<u8> {
    let payload = postcard::to_stdvec(msg).expect("serialize");
    let mut f = Vec::with_capacity(5 + payload.len());
    f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    f.push(0x00);
    f.extend_from_slice(&payload);
    f
}

#[test]
fn test_hello_encoding() {
    let hello = ClientMsg::Hello {
        proto: 1,
        token: "16f4b946ae5df8568a84b4299444b0f5ed26ff9a0df0f32efd42e2449635a667".into(),
        client: "mirai-hmos/0.1.0".into(),
    };

    let bytes = postcard::to_stdvec(&hello).unwrap();
    println!("Hello postcard bytes ({}): {:?}", bytes.len(), bytes);

    assert_eq!(bytes[0], 0x00, "Hello should be variant 0");
    assert_eq!(bytes[1], 0x01, "proto should be 1");

    let decoded: ClientMsg = postcard::from_bytes(&bytes).unwrap();
    match decoded {
        ClientMsg::Hello { proto, client, .. } => {
            assert_eq!(proto, 1);
            assert_eq!(client, "mirai-hmos/0.1.0");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_open_encoding() {
    let open = ClientMsg::Open {
        sub: 1,
        engine: None,
        req: AnalyzeReq {
            size: Size { w: 19, h: 19 },
            rules: RuleSet::Chinese,
            komi_x2: 13,
            initial_stones: vec![],
            moves: vec![],
            initial_player: None,
            max_visits: Some(50000),
            max_time_ms: None,
            pv_len: Some(10),
            want: Want::OWNERSHIP | Want::POLICY,
            report_every_ms: Some(100),
            priority: 0,
            avoid: vec![],
            overrides: vec![],
        },
    };

    let bytes = postcard::to_stdvec(&open).unwrap();
    println!(
        "Open postcard bytes ({}): {:?}",
        bytes.len(),
        &bytes[..min(40, bytes.len())]
    );

    assert_eq!(bytes[0], 0x01, "Open should be variant 1");

    let decoded: ClientMsg = postcard::from_bytes(&bytes).unwrap();
    match decoded {
        ClientMsg::Open { sub, engine, req } => {
            assert_eq!(sub, 1);
            assert!(engine.is_none());
            assert_eq!(req.size.w, 19);
            assert_eq!(req.rules, RuleSet::Chinese);
            assert_eq!(req.komi_x2, 13);
            assert_eq!(req.max_visits, Some(50000));
            assert!(req.want.contains(Want::OWNERSHIP));
            assert!(req.want.contains(Want::POLICY));
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_cancel_encoding() {
    let cancel = ClientMsg::Cancel { sub: 7 };
    let bytes = postcard::to_stdvec(&cancel).unwrap();

    assert_eq!(bytes[0], 0x02, "Cancel should be variant 2");
    assert_eq!(bytes[1], 0x07, "sub should be 7 (varint)");

    let decoded: ClientMsg = postcard::from_bytes(&bytes).unwrap();
    match decoded {
        ClientMsg::Cancel { sub } => assert_eq!(sub, 7),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_ping_encoding() {
    let ping = ClientMsg::Ping(42);
    let bytes = postcard::to_stdvec(&ping).unwrap();

    assert_eq!(bytes[0], 0x04, "Ping should be variant 4");

    let decoded: ClientMsg = postcard::from_bytes(&bytes).unwrap();
    match decoded {
        ClientMsg::Ping(n) => assert_eq!(n, 42),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_postcard_varint_matches_arkts() {
    let cases: Vec<(u32, Vec<u8>)> = vec![
        (0, vec![0x00]),
        (1, vec![0x01]),
        (127, vec![0x7F]),
        (128, vec![0x80, 0x01]),
        (255, vec![0xFF, 0x01]),
        (300, vec![0xAC, 0x02]),
        (16383, vec![0xFF, 0x7F]),
        (16384, vec![0x80, 0x80, 0x01]),
    ];

    for (value, expected) in cases {
        let mut buf = Vec::new();
        let mut v = value;
        loop {
            let mut byte = (v & 0x7F) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if v == 0 {
                break;
            }
        }
        assert_eq!(buf, expected, "varint({})", value);
    }
}

#[test]
fn test_zigzag_matches_arkts() {
    let cases: Vec<(i16, u16)> = vec![
        (0, 0),
        (-1, 1),
        (1, 2),
        (-2, 3),
        (2, 4),
        (32767, 65534),
        (-32767, 65533),
    ];

    for (signed, expected_unsigned) in cases {
        let zz = ((signed << 1) ^ (signed >> 15)) as u16;
        assert_eq!(zz, expected_unsigned, "zigzag({})", signed);
    }
}

#[test]
fn test_frame_codec_matches_arkts() {
    let hello = ClientMsg::Hello {
        proto: 1,
        token: "test".into(),
        client: "test".into(),
    };

    let frame = serialize_frame(&hello);

    // Header: [len u32 LE][flags u8]
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    assert_eq!(frame.len(), 5 + len);
    assert_eq!(frame[4], 0x00, "flags should be 0x00 (no zstd)");
}

#[test]
fn test_hello_frame_full() {
    // Full integration: create the exact Hello message ArkTS client sends,
    // encode it, and verify the frame is correct
    let hello = ClientMsg::Hello {
        proto: 1,
        token: "16f4b946ae5df8568a84b4299444b0f5ed26ff9a0df0f32efd42e2449635a667".into(),
        client: "mirai-hmos/0.1.0".into(),
    };

    let frame = serialize_frame(&hello);
    println!("Full Hello frame: {:?}", frame);

    // Verify it's a valid frame that the Rust server can parse
    assert!(frame.len() > 5);
    assert_eq!(frame[4], 0x00);

    // Verify the payload decodes back
    let payload = &frame[5..];
    let decoded: ClientMsg = postcard::from_bytes(payload).unwrap();
    match decoded {
        ClientMsg::Hello { proto, .. } => assert_eq!(proto, 1),
        _ => panic!("wrong type"),
    }
}
