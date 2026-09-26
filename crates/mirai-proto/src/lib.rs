// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! MRP — the mirai Remote Protocol.
//!
//! * [`types`] — quantised wire values shared by the local and remote engine paths.
//! * [`msg`] — control-stream and subscription-stream messages.
//! * [`frame`] — length-prefixed, optionally zstd-compressed postcard framing over any
//!   `AsyncRead + AsyncWrite`.
//! * [`endpoint`] — ALPN, default port and `mirai://` URL parsing.
//! * [`atomic`] — crash-safe replacement of a durable file. Not behind `quinn-transport`:
//!   `mirai-engine` writes a KataGo config through it with default features off.
//! * [`transport`] — the Quinn QUIC client and server that carry those frames; behind the
//!   default `quinn-transport` feature, so a peer that drives a platform QUIC stack can
//!   depend on the codec alone.

pub mod atomic;
pub mod endpoint;
pub mod frame;
pub mod msg;
pub mod sha256;
#[cfg(feature = "quinn-transport")]
pub mod transport;
pub mod types;

pub use endpoint::{ALPN, AddressError, DEFAULT_PORT, URL_SCHEME, parse_url};
pub use frame::{FrameBuf, FrameError, MAX_FRAME, read_msg, write_msg};
pub use msg::{ClientMsg, ErrCode, ServerMsg, SubMsg};
pub use types::{
    AnalyzeReq, AvoidSpec, EngineDesc, MoveInfo, POLICY_ILLEGAL, PROTO_VERSION, ProtoVersion,
    Report, RootInfo, Want,
};
