// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The application layer shared by mirai's frontends.
//!
//! `mirai-core` owns the rules, the tree and SGF; `mirai-engine` owns the engines and the
//! MRP session. This crate owns the part above them that is neither: which position to
//! analyse, what a request should say, what the user's record is called, when it is dirty,
//! and how a connection is trusted.
//!
//! Nothing here draws, allocates a widget, or touches a file. Frontends supply those.
//!
//! * [`analysis`] — position to [`AnalyzeReq`](mirai_engine::AnalyzeReq), and search speed.
//! * [`batch`] — planning and running a whole-game sweep with bounded concurrency.
//! * [`game`] — the record the user is editing: cursor, navigation, variations, comments.
//! * [`session`] — connecting, trust-on-first-use, and observable connection state.

pub mod analysis;
pub mod batch;
pub mod fox;
pub mod game;
pub mod play;
pub mod session;

pub use analysis::{PV_LEN, SpeedMeter, analysis_of, dead_from_ownership, request_for_node};
pub use batch::{Analysed, Blunder, Flow, Planned, blunders, in_flight, plan_mainline, sweep};
pub use game::GameSession;
pub use play::{GameSetup, Play, PlayState, Strength};
pub use session::{Peer, RemoteConnector, Session, SessionConfig, SessionState};
