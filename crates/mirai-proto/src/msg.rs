// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Control-stream and subscription-stream messages.

use crate::types::{AnalyzeReq, EngineDesc, MoveInfo, Report, RootInfo};
use serde::{Deserialize, Serialize};

/// Sent by the client on the bidirectional control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        proto: u16,
        token: String,
        client: String,
    },
    Open {
        sub: u32,
        engine: Option<String>,
        req: AnalyzeReq,
    },
    Cancel {
        sub: u32,
    },
    ListEngines,
    Ping(u64),
}

/// Sent by the server on the bidirectional control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        proto: u16,
        server: String,
        session: u64,
        engines: Vec<EngineDesc>,
    },
    Engines(Vec<EngineDesc>),
    Opened {
        sub: u32,
    },
    Pong(u64),
    Error {
        sub: Option<u32>,
        code: ErrCode,
        msg: String,
    },
}

/// Carried on the server-opened unidirectional stream belonging to one subscription.
/// The stream is finished after `Done` or `Failed`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SubMsg {
    Report(Report),
    Done(Report),
    Failed(String),
}

/// Borrowed mirror of [`SubMsg`], for the sending end of a subscription stream.
///
/// Variant order and shapes match exactly, so postcard emits byte-identical frames — but a
/// report can be streamed straight out of its `Arc` instead of being cloned ten times a
/// second per subscription. `sub_msg_ref_is_byte_identical_to_sub_msg` pins the two
/// together.
#[derive(Serialize)]
pub enum SubMsgRef<'a> {
    Report(ReportRef<'a>),
    Done(ReportRef<'a>),
    Failed(&'a str),
}

/// Borrowed mirror of [`Report`], field for field, with the ownership map replaced by
/// whatever the stream carries in its place ([`OwnershipDelta`]).
#[derive(Serialize)]
pub struct ReportRef<'a> {
    pub turn: u16,
    pub root: &'a RootInfo,
    pub moves: &'a [MoveInfo],
    pub ownership: Option<&'a [i8]>,
    pub policy: Option<&'a [u16]>,
}

/// Ownership on a subscription stream travels as the change from the stream's previous
/// map (`PROTOCOL.md` §7.11). Between two reports of one search most points move by a step
/// or two; sent as those changes, a live report measured a quarter smaller.
///
/// Each end of a stream keeps one, and both apply one rule, so they cannot disagree: a map
/// goes out as a difference (wrapping `i8`) when the stream has already carried a map of
/// the same length, and whole otherwise. A report without a map changes nothing.
#[derive(Default)]
pub struct OwnershipDelta {
    prev: Vec<i8>,
    wire: Vec<i8>,
}

impl OwnershipDelta {
    /// `r` as the stream carries it.
    pub fn report<'a>(&'a mut self, r: &'a Report) -> ReportRef<'a> {
        let ownership = match r.ownership.as_deref() {
            Some(own) => {
                self.wire.clear();
                if self.prev.len() == own.len() {
                    let diff = own.iter().zip(&self.prev).map(|(&o, &p)| o.wrapping_sub(p));
                    self.wire.extend(diff);
                } else {
                    self.wire.extend_from_slice(own);
                }
                self.prev.clear();
                self.prev.extend_from_slice(own);
                Some(&self.wire[..])
            }
            None => None,
        };
        ReportRef {
            turn: r.turn,
            root: &r.root,
            moves: &r.moves,
            ownership,
            policy: r.policy.as_deref(),
        }
    }

    /// Puts back the ownership map of a message read off the stream.
    pub fn restore(&mut self, msg: &mut SubMsg) {
        let (SubMsg::Report(r) | SubMsg::Done(r)) = msg else {
            return;
        };
        let Some(own) = r.ownership.as_mut() else {
            return;
        };
        if self.prev.len() == own.len() {
            for (o, &p) in own.iter_mut().zip(&self.prev) {
                *o = o.wrapping_add(p);
            }
        }
        self.prev.clear();
        self.prev.extend_from_slice(own);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ErrCode {
    BadVersion,
    Unauthorized,
    NoSuchEngine,
    TooManySubs,
    BadRequest,
    EngineFailed,
    Internal,
}

impl ErrCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrCode::BadVersion => "bad-version",
            ErrCode::Unauthorized => "unauthorized",
            ErrCode::NoSuchEngine => "no-such-engine",
            ErrCode::TooManySubs => "too-many-subs",
            ErrCode::BadRequest => "bad-request",
            ErrCode::EngineFailed => "engine-failed",
            ErrCode::Internal => "internal",
        }
    }
}

impl std::fmt::Display for ErrCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{self, FrameBuf, SUB_STREAM_LEVEL, SubStreamDecoder, SubStreamEncoder};
    use mirai_core::{Color, Point};

    fn sample_report() -> Report {
        let mut r = Report::empty(42, Color::White);
        r.root.visits = 1234;
        r.root.winrate = 40000;
        r.root.score_lead = -97;
        r.root.raw_winrate = Some(31000);
        r.ownership = Some((0..81).map(|i| (i as i8) - 40).collect());
        r.policy = Some((0..82).map(|i| (i * 700) as u16).collect());
        r.moves = (0..3)
            .map(|i| MoveInfo {
                mv: Point(60 + i),
                visits: 900 - i as u32,
                edge_visits: 880 - i as u32,
                winrate: 40000 + i,
                prior: 3000,
                lcb: -120,
                utility: 44,
                utility_lcb: 30,
                score_lead: -97,
                score_selfplay: -101,
                score_stdev: 400,
                order: i as u8,
                play_value: 12345,
                pv: (0..15).map(|k| Point(k * 7 + i)).collect(),
                pv_visits: (0..15).map(|k| 100 - k as u32).collect(),
            })
            .collect();
        r
    }

    /// `SubMsgRef` is only sound while it encodes exactly like `SubMsg`: a reordered or
    /// added field or variant would silently corrupt every stream. The first map a stream
    /// carries goes out whole, so the report is unchanged.
    #[test]
    fn sub_msg_ref_is_byte_identical_to_sub_msg() {
        fn bytes<T: Serialize>(msg: &T) -> Vec<u8> {
            frame::encode(&mut FrameBuf::new(), msg).unwrap().to_vec()
        }
        let r = sample_report();

        let mut own = OwnershipDelta::default();
        assert_eq!(
            bytes(&SubMsgRef::Report(own.report(&r))),
            bytes(&SubMsg::Report(r.clone())),
            "Report variant"
        );
        let mut own = OwnershipDelta::default();
        assert_eq!(
            bytes(&SubMsgRef::Done(own.report(&r))),
            bytes(&SubMsg::Done(r.clone())),
            "Done variant"
        );
        assert_eq!(
            bytes(&SubMsgRef::Failed("boom")),
            bytes(&SubMsg::Failed("boom".into())),
            "Failed variant"
        );
    }

    /// A stream of maps through the real codec, sender and receiver each with their own
    /// state: every map comes back exactly, including values that wrap and a report in the
    /// middle that carries no map at all.
    #[test]
    fn ownership_deltas_reconstruct_across_a_stream() {
        let maps: [Option<Vec<i8>>; 4] = [
            Some(vec![127, -127, 0, 5]),
            Some(vec![-127, 127, 0, 6]),
            None,
            Some(vec![100, -100, -128, 6]),
        ];
        let mut enc = SubStreamEncoder::new(SUB_STREAM_LEVEL).unwrap();
        let mut dec = SubStreamDecoder::new().unwrap();
        let (mut sent, mut read) = (OwnershipDelta::default(), OwnershipDelta::default());

        for (i, map) in maps.into_iter().enumerate() {
            let mut r = Report::empty(i as u16, Color::Black);
            r.ownership = map;
            let frame = enc.encode(&SubMsgRef::Report(sent.report(&r))).unwrap();
            let mut back: SubMsg = dec.decode(frame).unwrap();
            if i > 0 && r.ownership.is_some() {
                assert_ne!(back, SubMsg::Report(r.clone()), "report {i} went out whole");
            }
            read.restore(&mut back);
            assert_eq!(back, SubMsg::Report(r), "report {i}");
        }
    }
}
