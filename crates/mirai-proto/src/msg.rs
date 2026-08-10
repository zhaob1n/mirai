// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Control-stream and subscription-stream messages.

use crate::types::{AnalyzeReq, EngineDesc, Report};
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
