// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! One QUIC connection: the control stream, and one task per live subscription.
//!
//! Stream topology is MRP/2's (see `mirai_proto::transport`): the client opens a single
//! bidirectional control stream, and the server opens one unidirectional stream per
//! subscription, prefixed with the 4-byte LE subscription id.
//!
//! Engines are shared across every session — `numAnalysisThreads` is what makes that
//! work, so a second client never costs a second KataGo process.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mirai_core::Size;
use mirai_engine::{Engine, SubEvent, Subscription};
use mirai_proto::frame::{self, FrameBuf, FrameError, SUB_STREAM_LEVEL, SubStreamEncoder};
use mirai_proto::msg::{ClientMsg, ErrCode, OwnershipDelta, ServerMsg, SubMsgRef};
use mirai_proto::types::{AnalyzeReq, EngineDesc, PROTO_VERSION};
use quinn::{Connection, SendStream, VarInt};
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// Reported in `Welcome.server`.
pub const SERVER_NAME: &str = concat!("mirai-server/", env!("CARGO_PKG_VERSION"));

/// QUIC application error code for a rejected or torn-down connection.
const CODE_REJECTED: u32 = 1;
/// QUIC stream reset code for a cancelled subscription. The client sees the reset and
/// discards whatever it had buffered — that is the point of a stream per subscription.
const CODE_CANCELLED: u32 = 1;

/// Requests are clamped into this band so one client cannot starve the others.
const PRIORITY_RANGE: std::ops::RangeInclusive<i8> = -8..=8;

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

pub struct NamedEngine {
    pub name: String,
    pub engine: Arc<dyn Engine>,
}

pub struct Token {
    pub value: String,
    pub name: String,
    pub max_subs: u32,
}

/// Everything a session needs, shared by every connection.
pub struct Host {
    pub engines: Vec<NamedEngine>,
    pub tokens: Vec<Token>,
}

impl Host {
    /// Constant-time token lookup: every configured token is compared, so neither the
    /// match position nor an early mismatch is observable in the timing.
    pub fn authenticate(&self, presented: &str) -> Option<&Token> {
        let mut hit: Option<&Token> = None;
        for t in &self.tokens {
            if bool::from(t.value.as_bytes().ct_eq(presented.as_bytes())) {
                hit = Some(t);
            }
        }
        hit
    }

    /// `None` resolves to the first configured engine, as `Open` specifies.
    pub fn resolve_engine(&self, name: Option<&str>) -> Option<&NamedEngine> {
        match name {
            None => self.engines.first(),
            Some(n) => self.engines.iter().find(|e| e.name == n),
        }
    }

    pub fn describe(&self) -> Vec<EngineDesc> {
        self.engines.iter().map(|e| e.engine.describe()).collect()
    }
}

fn request_geometry_error(req: &AnalyzeReq) -> Option<&'static str> {
    if Size::new(req.size.w, req.size.h).is_none() {
        return Some("board size is outside 2..=19");
    }

    if req
        .moves
        .iter()
        .chain(&req.initial_stones)
        .any(|&(_, point)| !point.is_pass() && !req.size.contains(point))
    {
        return Some("a move or initial stone is off the board");
    }

    if req
        .avoid
        .iter()
        .flat_map(|spec| &spec.moves)
        .any(|&point| !point.is_pass() && !req.size.contains(point))
    {
        return Some("an avoid move is off the board");
    }

    None
}

pub async fn serve(host: Arc<Host>, conn: Connection) {
    let peer = conn.remote_address();
    let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    info!(session, %peer, "connection open");
    let outcome = session_loop(&host, &conn, session).await;
    match outcome {
        Ok(()) => info!(session, %peer, "connection closed"),
        Err(e) => info!(session, %peer, error = %e, "connection closed"),
    }
    conn.close(VarInt::from_u32(0), b"bye");
}

async fn session_loop(host: &Host, conn: &Connection, session: u64) -> anyhow::Result<()> {
    let (mut tx, mut rx) = conn.accept_bi().await?;
    let mut wbuf = FrameBuf::new();
    let mut rbuf = FrameBuf::new();

    let ClientMsg::Hello {
        proto,
        token,
        client,
    } = frame::read_msg::<_, ClientMsg>(&mut rx, &mut rbuf).await?
    else {
        reject(
            &mut tx,
            &mut wbuf,
            conn,
            ErrCode::BadRequest,
            "expected Hello as the first control message",
        )
        .await;
        anyhow::bail!("first control message was not Hello");
    };

    if proto != PROTO_VERSION {
        reject(
            &mut tx,
            &mut wbuf,
            conn,
            ErrCode::BadVersion,
            &format!("server speaks MRP/{PROTO_VERSION}, client offered MRP/{proto}"),
        )
        .await;
        anyhow::bail!("protocol version mismatch: client offered {proto}");
    }

    let Some(auth) = host.authenticate(&token) else {
        reject(
            &mut tx,
            &mut wbuf,
            conn,
            ErrCode::Unauthorized,
            "unknown token",
        )
        .await;
        anyhow::bail!("unauthorized client {client:?}");
    };
    let token_name = if auth.name.is_empty() {
        "<unnamed>"
    } else {
        auth.name.as_str()
    };
    let max_subs = auth.max_subs;
    info!(session, %client, token = token_name, max_subs, "authenticated");

    frame::write_msg(
        &mut tx,
        &mut wbuf,
        &ServerMsg::Welcome {
            proto: PROTO_VERSION,
            server: SERVER_NAME.to_string(),
            session,
            engines: host.describe(),
        },
    )
    .await?;

    // Dropping this map cancels every subscription of this connection: each entry is the
    // sending half of its pump task's cancel channel.
    let mut subs: HashMap<u32, oneshot::Sender<()>> = HashMap::new();

    loop {
        let msg = match frame::read_msg::<_, ClientMsg>(&mut rx, &mut rbuf).await {
            Ok(m) => m,
            Err(FrameError::Eof) => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        match msg {
            ClientMsg::Hello { .. } => {
                error_msg(
                    &mut tx,
                    &mut wbuf,
                    None,
                    ErrCode::BadRequest,
                    "already greeted",
                )
                .await?;
            }
            ClientMsg::Ping(n) => {
                frame::write_msg(&mut tx, &mut wbuf, &ServerMsg::Pong(n)).await?;
            }
            ClientMsg::ListEngines => {
                frame::write_msg(&mut tx, &mut wbuf, &ServerMsg::Engines(host.describe())).await?;
            }
            ClientMsg::Cancel { sub } => {
                if subs.remove(&sub).is_some() {
                    info!(session, sub, "cancel subscription");
                } else {
                    debug!(session, sub, "cancel for an unknown subscription");
                }
            }
            ClientMsg::Open {
                sub,
                engine,
                mut req,
            } => {
                // Finished pumps drop their cancel receiver; reap them before counting.
                subs.retain(|_, cancel| !cancel.is_closed());

                if subs.contains_key(&sub) {
                    error_msg(
                        &mut tx,
                        &mut wbuf,
                        Some(sub),
                        ErrCode::BadRequest,
                        "subscription id is already in use",
                    )
                    .await?;
                    continue;
                }
                if subs.len() as u32 >= max_subs {
                    error_msg(
                        &mut tx,
                        &mut wbuf,
                        Some(sub),
                        ErrCode::TooManySubs,
                        &format!("token {token_name:?} allows {max_subs} concurrent subscriptions"),
                    )
                    .await?;
                    continue;
                }
                let Some(named) = host.resolve_engine(engine.as_deref()) else {
                    let wanted = engine.as_deref().unwrap_or("<default>");
                    error_msg(
                        &mut tx,
                        &mut wbuf,
                        Some(sub),
                        ErrCode::NoSuchEngine,
                        &format!("no engine named {wanted:?}"),
                    )
                    .await?;
                    continue;
                };

                if let Some(msg) = request_geometry_error(&req) {
                    error_msg(&mut tx, &mut wbuf, Some(sub), ErrCode::BadRequest, msg).await?;
                    continue;
                }

                req.priority = req
                    .priority
                    .clamp(*PRIORITY_RANGE.start(), *PRIORITY_RANGE.end());
                info!(
                    session,
                    sub,
                    engine = %named.name,
                    moves = req.moves.len(),
                    max_visits = ?req.max_visits,
                    max_candidates = ?req.max_candidates,
                    priority = req.priority,
                    "open subscription"
                );

                let subscription = named.engine.subscribe(req);
                let (cancel_tx, cancel_rx) = oneshot::channel();
                subs.insert(sub, cancel_tx);
                frame::write_msg(&mut tx, &mut wbuf, &ServerMsg::Opened { sub }).await?;
                tokio::spawn(pump(conn.clone(), session, sub, subscription, cancel_rx));
            }
        }
    }
}

/// Streams one subscription's events down its own unidirectional stream.
///
/// Returning drops `subscription`, which terminates the KataGo query — so every exit path
/// here, including the connection simply going away, cleans up the engine side.
async fn pump(
    conn: Connection,
    session: u64,
    sub: u32,
    mut subscription: Subscription,
    mut cancel: oneshot::Receiver<()>,
) {
    let mut stream = match conn.open_uni().await {
        Ok(s) => s,
        Err(e) => {
            warn!(session, sub, error = %e, "could not open the subscription stream");
            return;
        }
    };
    // Stream preamble: the raw 4-byte LE sub id, before any frame.
    if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut stream, &sub.to_le_bytes()).await {
        warn!(session, sub, error = %e, "could not write the subscription preamble");
        return;
    }

    let mut enc = match SubStreamEncoder::new(SUB_STREAM_LEVEL) {
        Ok(enc) => enc,
        Err(e) => {
            // A stream that ends without `Done` or `Failed` fails on the client.
            warn!(session, sub, error = %e, "could not start the stream's compressor");
            let _ = stream.finish();
            return;
        }
    };
    let mut own = OwnershipDelta::default();
    let mut event = subscription.current();
    loop {
        match &event {
            SubEvent::Pending => {}
            SubEvent::Report(r) => {
                let msg = SubMsgRef::Report(own.report(r));
                if let Err(e) = send(&mut stream, &mut enc, msg).await {
                    debug!(session, sub, error = %e, "subscription stream closed early");
                    info!(session, sub, "subscription dropped");
                    return;
                }
            }
            SubEvent::Done(r) => {
                let _ = send(&mut stream, &mut enc, SubMsgRef::Done(own.report(r))).await;
                let _ = stream.finish();
                info!(session, sub, visits = r.root.visits, "subscription done");
                return;
            }
            SubEvent::Failed(err) => {
                let text = err.to_string();
                let _ = send(&mut stream, &mut enc, SubMsgRef::Failed(&text)).await;
                let _ = stream.finish();
                info!(session, sub, error = %text, "subscription failed");
                return;
            }
        }

        tokio::select! {
            biased;
            _ = &mut cancel => {
                // Reset rather than finish: buffered stale reports must never arrive.
                let _ = stream.reset(VarInt::from_u32(CODE_CANCELLED));
                info!(session, sub, "subscription dropped");
                return;
            }
            next = subscription.next() => match next {
                Some(e) => event = e,
                None => {
                    let _ = send(
                        &mut stream,
                        &mut enc,
                        SubMsgRef::Failed("engine closed the subscription"),
                    )
                    .await;
                    let _ = stream.finish();
                    info!(session, sub, "subscription dropped");
                    return;
                }
            },
        }
    }
}

async fn send(
    stream: &mut SendStream,
    enc: &mut SubStreamEncoder,
    msg: SubMsgRef<'_>,
) -> Result<(), FrameError> {
    enc.write(stream, &msg).await
}

async fn error_msg(
    tx: &mut SendStream,
    buf: &mut FrameBuf,
    sub: Option<u32>,
    code: ErrCode,
    msg: &str,
) -> Result<(), FrameError> {
    warn!(?sub, %code, msg, "rejecting");
    frame::write_msg(
        tx,
        buf,
        &ServerMsg::Error {
            sub,
            code,
            msg: msg.to_string(),
        },
    )
    .await
}

/// Reports a fatal handshake problem, then closes the connection with application code 1.
async fn reject(
    tx: &mut SendStream,
    buf: &mut FrameBuf,
    conn: &Connection,
    code: ErrCode,
    msg: &str,
) {
    let _ = error_msg(tx, buf, None, code, msg).await;
    // Flush before the close: a reset would discard the error the client needs to see.
    let _ = stream_flush(tx).await;
    conn.close(VarInt::from_u32(CODE_REJECTED), code.as_str().as_bytes());
}

async fn stream_flush(tx: &mut SendStream) -> std::io::Result<()> {
    tokio::io::AsyncWriteExt::flush(tx).await?;
    let _ = tx.finish();
    tx.stopped().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_core::{Color, Point, RuleSet};
    use mirai_proto::types::AvoidSpec;

    fn host(tokens: &[(&str, u32)]) -> Host {
        Host {
            engines: Vec::new(),
            tokens: tokens
                .iter()
                .map(|(v, max)| Token {
                    value: (*v).to_string(),
                    name: (*v).to_string(),
                    max_subs: *max,
                })
                .collect(),
        }
    }

    #[test]
    fn authentication_accepts_only_an_exact_token() {
        let h = host(&[("alpha", 2), ("bravobravo", 7)]);
        assert_eq!(h.authenticate("alpha").map(|t| t.max_subs), Some(2));
        assert_eq!(h.authenticate("bravobravo").map(|t| t.max_subs), Some(7));
        assert!(h.authenticate("alph").is_none(), "prefix must not match");
        assert!(
            h.authenticate("alphaX").is_none(),
            "extension must not match"
        );
        assert!(h.authenticate("Alpha").is_none(), "case matters");
        assert!(h.authenticate("").is_none());
    }

    #[test]
    fn a_host_with_no_tokens_rejects_everyone() {
        let h = host(&[]);
        assert!(h.authenticate("").is_none());
        assert!(h.authenticate("anything").is_none());
    }

    #[test]
    fn engine_resolution_defaults_to_the_first_configured_engine() {
        // No engines: even the default must not resolve.
        let h = host(&[]);
        assert!(h.resolve_engine(None).is_none());
        assert!(h.resolve_engine(Some("default")).is_none());
    }

    #[test]
    fn hostile_request_geometry_is_rejected_before_subscribing() {
        let mut hostile = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        hostile.size = Size { w: 0, h: 0 };
        let mut wbuf = FrameBuf::new();
        let bytes = frame::encode(
            &mut wbuf,
            &ClientMsg::Open {
                sub: 7,
                engine: None,
                req: hostile,
            },
        )
        .unwrap()
        .to_vec();
        let mut rbuf = FrameBuf::new();
        let decoded: ClientMsg = frame::decode(&mut rbuf, &bytes).unwrap();
        let ClientMsg::Open { req, .. } = decoded else {
            panic!("decoded a different message");
        };
        assert_eq!(
            request_geometry_error(&req),
            Some("board size is outside 2..=19")
        );

        let mut req = AnalyzeReq::new(Size::square(9), RuleSet::Chinese, 7.5);
        req.moves.push((Color::Black, Point(81)));
        assert_eq!(
            request_geometry_error(&req),
            Some("a move or initial stone is off the board")
        );

        req.moves.clear();
        req.avoid.push(AvoidSpec {
            player: Color::White,
            moves: vec![Point(81)],
            until_depth: 1,
            allow: false,
        });
        assert_eq!(
            request_geometry_error(&req),
            Some("an avoid move is off the board")
        );

        req.avoid[0].moves[0] = Point::PASS;
        req.initial_stones.push((Color::Black, Point::PASS));
        assert_eq!(request_geometry_error(&req), None);
    }

    #[test]
    fn priority_is_clamped_into_the_served_band() {
        let clamp = |p: i8| p.clamp(*PRIORITY_RANGE.start(), *PRIORITY_RANGE.end());
        assert_eq!(clamp(127), 8);
        assert_eq!(clamp(-128), -8);
        assert_eq!(clamp(4), 4);
        assert_eq!(clamp(0), 0);
    }
}
