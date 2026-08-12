// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! MRP/1 — the mirai remote-analysis protocol.
//!
//! * [`types`] — quantised wire values shared by the local and remote engine paths.
//! * [`msg`] — control-stream and subscription-stream messages.
//! * [`frame`] — length-prefixed, optionally zstd-compressed postcard framing over any
//!   `AsyncRead + AsyncWrite`.
//! * [`transport`] — the QUIC client and server that carry those frames.

pub mod frame;
pub mod msg;
pub mod sha256;
pub mod transport;
pub mod types;

pub use frame::{FrameBuf, FrameError, MAX_FRAME, read_msg, write_msg};
pub use msg::{ClientMsg, ErrCode, ServerMsg, SubMsg};
pub use types::{
    AnalyzeReq, AvoidSpec, EngineDesc, MoveInfo, POLICY_ILLEGAL, PROTO_VERSION, Report, RootInfo,
    Want,
};
