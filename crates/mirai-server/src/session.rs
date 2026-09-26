// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! One QUIC connection: the control stream, and one task per live subscription.
//!
//! Stream topology is MRP's (see `mirai_proto::transport`): the client opens a single
//! bidirectional control stream, and the server opens one unidirectional stream per
//! subscription, prefixed with the 4-byte LE subscription id.
//!
//! Engines are shared across every session — `numAnalysisThreads` is what makes that
//! work, so a second client never costs a second KataGo process.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use mirai_core::Size;
use mirai_engine::{Engine, SubEvent, Subscription};
use mirai_proto::frame::{self, FrameBuf, FrameError, SUB_STREAM_LEVEL, SubStreamEncoder};
use mirai_proto::msg::{ClientMsg, ErrCode, OwnershipDelta, ServerMsg, SubMsgRef};
use mirai_proto::types::{AnalyzeReq, EngineDesc, PROTO_VERSION};
use quinn::{Connection, Incoming, SendStream, VarInt};
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

/// How long an `Open` at the subscription limit waits for a cancelled subscription to stop.
/// Stopping one takes a scheduler turn; this only has to cover a loaded host.
const CANCEL_GRACE: Duration = Duration::from_secs(1);

/// How long a peer may hold a session slot before it has sent `Hello`.
///
/// The accept loop takes the slot before the handshake. A QUIC keep-alive
/// resets the idle timer, so a silent peer would otherwise hold the slot until
/// the process exits, and 32 of them refuse every real client. One clock covers
/// the handshake, opening the control stream and the first Hello frame. After
/// Hello the slot is a session and this bound no longer applies.
pub const PREAUTH_DEADLINE: Duration = Duration::from_secs(10);

/// Close reason when [`PREAUTH_DEADLINE`] expires. There may be no control
/// stream to write an `Error` frame on, so the close itself is the signal.
const PREAUTH_CLOSE_REASON: &[u8] = b"pre-authentication deadline";

/// Longest `Hello.token` and `Hello.client` the server accepts, in bytes.
///
/// Both arrive before authentication, and `client` goes into the log. A generated token is
/// 64 characters and the reference client string is `mirai/<version>`; four times the token
/// leaves room for any hand-made one while keeping every pre-authentication log line short.
pub const MAX_HELLO_FIELD: usize = 256;

/// Largest first control frame, in bytes of payload and again of decompressed plaintext.
///
/// A `Hello` at [`MAX_HELLO_FIELD`] is about 520 bytes of postcard, and compressing a
/// payload that small adds only a few bytes. Anything larger is not a `Hello` a real client
/// sends, so the unauthenticated peer never gets to make the server allocate `MAX_FRAME`,
/// neither for the frame nor for what a few hundred bytes of zstd inflate to.
const MAX_HELLO_FRAME: usize = 1024;

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

/// Most `moves` plus `initial_stones` one request may carry.
///
/// Boards are at most 19×19 and real games end within a few hundred moves, so over ten
/// full boards of moves is far past any real game. Without it a few kilobytes of zstd
/// inflate to millions of moves, which the engine turns into one JSON line of tens of
/// megabytes on KataGo's stdin.
const MAX_REQUEST_STONES: usize = 4096;

/// Most `AvoidSpec` entries one request may carry. The reference client sends none; a
/// restriction per player per depth band fits many times over.
const MAX_AVOID_SPECS: usize = 64;

/// Most points across all of a request's `AvoidSpec` lists. One list per player covering
/// every point of a 19×19 board, pass included, is 2 × 362.
const MAX_AVOID_POINTS: usize = 1024;

/// The `overrides` keys forwarded to KataGo: what the reference client sends for play
/// (`mirai-client` — `play.rs`, `ai_request`). PROTOCOL §7.3 treats overrides as
/// untrusted: a server may ignore any key and must not reject an unknown one, so every
/// other key is dropped: an arbitrary `overrideSettings` entry (`maxVisits`,
/// `numSearchThreads`, …) can change what a shared search costs.
const FORWARDED_OVERRIDES: &[&str] = &["wideRootNoise", "humanSLProfile"];

/// Longest forwarded override value. A human profile is a name like `preaz_5k`.
const MAX_OVERRIDE_VALUE: usize = 64;

fn request_error(req: &AnalyzeReq) -> Option<String> {
    if Size::new(req.size.w, req.size.h).is_none() {
        return Some("board size is outside 2..=19".into());
    }

    if req.moves.len() + req.initial_stones.len() > MAX_REQUEST_STONES {
        return Some(format!(
            "more than {MAX_REQUEST_STONES} moves and initial stones"
        ));
    }
    if req
        .moves
        .iter()
        .chain(&req.initial_stones)
        .any(|&(_, point)| !point.is_pass() && !req.size.contains(point))
    {
        return Some("a move or initial stone is off the board".into());
    }

    if req.avoid.len() > MAX_AVOID_SPECS
        || req.avoid.iter().map(|spec| spec.moves.len()).sum::<usize>() > MAX_AVOID_POINTS
    {
        return Some(format!(
            "more than {MAX_AVOID_SPECS} avoid lists or {MAX_AVOID_POINTS} avoid moves"
        ));
    }
    if req
        .avoid
        .iter()
        .flat_map(|spec| &spec.moves)
        .any(|&point| !point.is_pass() && !req.size.contains(point))
    {
        return Some("an avoid move is off the board".into());
    }

    None
}

/// Drops every override the server does not forward ([`FORWARDED_OVERRIDES`]).
fn keep_forwarded_overrides(overrides: &mut Vec<(String, String)>) {
    overrides.retain(|(key, value)| {
        FORWARDED_OVERRIDES.contains(&key.as_str()) && value.len() <= MAX_OVERRIDE_VALUE
    });
}

/// Serves one incoming connection.
///
/// `preauth` bounds only the unauthenticated prefix. Once `Hello` has been read
/// the session runs until the peer leaves. On expiry the connection is closed
/// and this returns, so the caller's session permit is released. After the
/// handshake that close is application code 1; before 1-RTT keys exist it is a
/// transport `APPLICATION_ERROR`.
pub async fn serve(host: Arc<Host>, incoming: Incoming, preauth: Duration) {
    let peer = incoming.remote_address();
    // One clock for the handshake, the control stream and the first Hello. A
    // fresh timeout on each step would let a slow peer consume the bound three
    // times, and a QUIC keep-alive would otherwise renew the slot forever.
    let deadline = tokio::time::Instant::now() + preauth;
    let conn = match tokio::time::timeout_at(deadline, incoming.into_future()).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => {
            warn!(%peer, "handshake failed: {e}");
            return;
        }
        Err(_) => {
            // No 1-RTT keys yet, so close_preauth cannot be delivered: quinn
            // encodes an application close in the handshake spaces as a
            // transport APPLICATION_ERROR with an empty reason. Dropping the
            // Connecting is that close. Returning releases the permit.
            warn!(%peer, "pre-authentication deadline exceeded during handshake");
            return;
        }
    };

    let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    info!(session, %peer, "connection open");
    let outcome = session_loop(&host, &conn, session, deadline).await;
    match outcome {
        Ok(()) => info!(session, %peer, "connection closed"),
        Err(e) => info!(session, %peer, error = %e, "connection closed"),
    }
    // A pre-authentication close has already been sent; quinn ignores a second one.
    conn.close(VarInt::from_u32(0), b"bye");
}

fn close_preauth(conn: &Connection) {
    conn.close(VarInt::from_u32(CODE_REJECTED), PREAUTH_CLOSE_REASON);
}

async fn session_loop(
    host: &Host,
    conn: &Connection,
    session: u64,
    preauth_deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    let (mut tx, mut rx) = match tokio::time::timeout_at(preauth_deadline, conn.accept_bi()).await {
        Ok(Ok(streams)) => streams,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            close_preauth(conn);
            anyhow::bail!("pre-authentication deadline exceeded waiting for the control stream");
        }
    };
    let mut wbuf = FrameBuf::new();
    let mut rbuf = FrameBuf::new();

    let first = match tokio::time::timeout_at(
        preauth_deadline,
        frame::read_msg_within::<_, ClientMsg>(&mut rx, &mut rbuf, MAX_HELLO_FRAME),
    )
    .await
    {
        Ok(Ok(msg)) => msg,
        Ok(Err(FrameError::TooLarge(_))) => {
            let msg = format!("the first frame exceeds {MAX_HELLO_FRAME} bytes");
            reject(&mut tx, &mut wbuf, conn, ErrCode::BadRequest, &msg).await;
            anyhow::bail!("{msg}");
        }
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            close_preauth(conn);
            anyhow::bail!("pre-authentication deadline exceeded waiting for Hello");
        }
    };
    let ClientMsg::Hello {
        proto,
        token,
        client,
    } = first
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

    if token.len() > MAX_HELLO_FIELD || client.len() > MAX_HELLO_FIELD {
        let msg = format!("Hello.token and Hello.client are limited to {MAX_HELLO_FIELD} bytes");
        reject(&mut tx, &mut wbuf, conn, ErrCode::BadRequest, &msg).await;
        anyhow::bail!("{msg}");
    }

    if proto != PROTO_VERSION {
        reject(
            &mut tx,
            &mut wbuf,
            conn,
            ErrCode::BadVersion,
            &format!("server speaks {PROTO_VERSION}, client offered {proto}"),
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
        // Bounded above and Debug-escaped: this reaches the log unauthenticated.
        anyhow::bail!("unauthorized client {client:?}");
    };
    let token_name = if auth.name.is_empty() {
        "<unnamed>"
    } else {
        auth.name.as_str()
    };
    let max_subs = auth.max_subs;
    info!(session, client = ?client, token = token_name, max_subs, "authenticated");

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
    // `max_subs` counts live KataGo queries, not map entries. `Cancel` removes the entry at
    // once, but the query lives until its pump drops the `Subscription`; each pump holds a
    // permit until then, so a client cannot cancel-and-reopen past its limit.
    let slots = Arc::new(tokio::sync::Semaphore::new(max_subs as usize));

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
                // Finished pumps drop their cancel receiver; reap them before checking ids.
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
                let held = max_subs as usize - slots.available_permits();
                let slot = match Arc::clone(&slots).try_acquire_owned() {
                    Ok(slot) => Some(slot),
                    // Some slots are held by cancelled subscriptions still stopping. That
                    // takes their pump a scheduler turn, and a client that cancels and
                    // reopens at its limit must not lose the race to it.
                    Err(_) if held > subs.len() => {
                        tokio::time::timeout(CANCEL_GRACE, Arc::clone(&slots).acquire_owned())
                            .await
                            .ok()
                            .and_then(Result::ok)
                    }
                    Err(_) => None,
                };
                let Some(slot) = slot else {
                    error_msg(
                        &mut tx,
                        &mut wbuf,
                        Some(sub),
                        ErrCode::TooManySubs,
                        &format!("token {token_name:?} allows {max_subs} concurrent subscriptions"),
                    )
                    .await?;
                    continue;
                };
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

                if let Some(msg) = request_error(&req) {
                    error_msg(&mut tx, &mut wbuf, Some(sub), ErrCode::BadRequest, &msg).await?;
                    continue;
                }

                req.priority = req
                    .priority
                    .clamp(*PRIORITY_RANGE.start(), *PRIORITY_RANGE.end());
                keep_forwarded_overrides(&mut req.overrides);
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
                let conn = conn.clone();
                tokio::spawn(async move {
                    pump(conn, session, sub, subscription, cancel_rx).await;
                    // Only now is the query gone: `pump` has dropped the subscription.
                    drop(slot);
                });
            }
        }
    }
}

/// Streams one subscription's events down its own unidirectional stream.
///
/// Returning drops `subscription`, which terminates the KataGo query — so every exit path
/// here, including the connection simply going away, cleans up the engine side.
///
/// `cancel` is raced against every await, not only the wait for the next event: opening the
/// stream waits on the peer's stream limit and a write waits on its flow control, and a
/// client that stops reading would otherwise keep a cancelled query running.
async fn pump(
    conn: Connection,
    session: u64,
    sub: u32,
    subscription: Subscription,
    mut cancel: oneshot::Receiver<()>,
) {
    let mut stream = tokio::select! {
        biased;
        _ = &mut cancel => {
            info!(session, sub, "subscription dropped");
            return;
        }
        opened = conn.open_uni() => match opened {
            Ok(s) => s,
            Err(e) => {
                warn!(session, sub, error = %e, "could not open the subscription stream");
                return;
            }
        },
    };
    tokio::select! {
        biased;
        _ = cancel => {
            // The losing branch, and the subscription it owned, is already dropped. Reset
            // rather than finish: buffered stale reports must never arrive.
            let _ = stream.reset(VarInt::from_u32(CODE_CANCELLED));
            info!(session, sub, "subscription dropped");
        }
        () = stream_events(&mut stream, session, sub, subscription) => {}
    }
}

/// The body of [`pump`] once its stream is open: runs until the subscription is terminal
/// or the stream fails.
async fn stream_events(
    stream: &mut SendStream,
    session: u64,
    sub: u32,
    mut subscription: Subscription,
) {
    // Stream preamble: the raw 4-byte LE sub id, before any frame.
    if let Err(e) = tokio::io::AsyncWriteExt::write_all(stream, &sub.to_le_bytes()).await {
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
                if let Err(e) = send(stream, &mut enc, msg).await {
                    debug!(session, sub, error = %e, "subscription stream closed early");
                    info!(session, sub, "subscription dropped");
                    return;
                }
            }
            SubEvent::Done(r) => {
                let _ = send(stream, &mut enc, SubMsgRef::Done(own.report(r))).await;
                let _ = stream.finish();
                info!(session, sub, visits = r.root.visits, "subscription done");
                return;
            }
            SubEvent::Failed(err) => {
                let text = err.to_string();
                let _ = send(stream, &mut enc, SubMsgRef::Failed(&text)).await;
                let _ = stream.finish();
                info!(session, sub, error = %text, "subscription failed");
                return;
            }
        }

        match subscription.next().await {
            Some(e) => event = e,
            None => {
                let _ = send(
                    stream,
                    &mut enc,
                    SubMsgRef::Failed("engine closed the subscription"),
                )
                .await;
                let _ = stream.finish();
                info!(session, sub, "subscription dropped");
                return;
            }
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
    use mirai_proto::types::{AvoidSpec, ProtoVersion};

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
            request_error(&req).as_deref(),
            Some("board size is outside 2..=19")
        );

        let mut req = AnalyzeReq::new(Size::square(9), RuleSet::Chinese, 7.5);
        req.moves.push((Color::Black, Point(81)));
        assert_eq!(
            request_error(&req).as_deref(),
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
            request_error(&req).as_deref(),
            Some("an avoid move is off the board")
        );

        req.avoid[0].moves[0] = Point::PASS;
        req.initial_stones.push((Color::Black, Point::PASS));
        assert_eq!(request_error(&req), None);
    }

    #[test]
    fn request_sizes_are_bounded_at_their_limits() {
        let pass = (Color::Black, Point::PASS);
        let mut req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        // Moves and set-up stones share one budget.
        req.moves = vec![pass; MAX_REQUEST_STONES - 1];
        req.initial_stones = vec![pass];
        assert_eq!(request_error(&req), None);
        req.initial_stones.push(pass);
        assert!(request_error(&req).is_some());

        let mut req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        let spec = |n: usize| AvoidSpec {
            player: Color::White,
            moves: vec![Point::PASS; n],
            until_depth: 1,
            allow: false,
        };
        req.avoid = vec![spec(1); MAX_AVOID_SPECS];
        assert_eq!(request_error(&req), None);
        req.avoid.push(spec(1));
        assert!(request_error(&req).is_some());

        // Points are counted across every list, not per list.
        req.avoid = vec![spec(MAX_AVOID_POINTS / 2), spec(MAX_AVOID_POINTS / 2)];
        assert_eq!(request_error(&req), None);
        req.avoid[1].moves.push(Point::PASS);
        assert!(request_error(&req).is_some());
    }

    /// An oversized `Open` is refused with `BadRequest` naming its subscription, never
    /// reaches the engine, and leaves the connection usable; a well-formed one that follows
    /// reaches the engine with only the forwarded overrides.
    #[tokio::test]
    async fn an_oversized_request_is_refused_before_the_engine_sees_it() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct Recorder(Mutex<Vec<AnalyzeReq>>);
        impl Engine for Recorder {
            fn subscribe(&self, req: AnalyzeReq) -> Subscription {
                self.0.lock().unwrap().push(req);
                Subscription::failed(mirai_engine::EngineError::Other("recorded".into()))
            }
            fn describe(&self) -> EngineDesc {
                EngineDesc::placeholder("recorder")
            }
        }

        let recorder = Arc::new(Recorder::default());
        let (endpoint, fp, dir) = test_endpoint("oversized");
        let addr = endpoint.local_addr().expect("bound");
        let accepting = endpoint.clone();
        let engine: Arc<dyn Engine> = recorder.clone();
        let served = tokio::spawn(async move {
            let incoming = accepting.accept().await.expect("incoming");
            serve(
                Arc::new(Host {
                    engines: vec![NamedEngine {
                        name: "recorder".into(),
                        engine,
                    }],
                    tokens: vec![Token {
                        value: "secret".into(),
                        name: "test".into(),
                        max_subs: 4,
                    }],
                }),
                incoming,
                Duration::from_secs(2),
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), Some(fp))
            .await
            .expect("handshake");
        let (mut tx, mut rx) = conn.open_bi().await.expect("control stream");
        let mut buf = FrameBuf::new();
        let mut next = async |tx: &mut SendStream, buf: &mut FrameBuf, msg: ClientMsg| {
            frame::write_msg(tx, buf, &msg).await.expect("write");
            tokio::time::timeout(Duration::from_secs(2), frame::read_msg(&mut rx, buf))
                .await
                .expect("no answer")
                .expect("read")
        };

        let hello = ClientMsg::Hello {
            proto: PROTO_VERSION,
            token: "secret".into(),
            client: "test".into(),
        };
        let welcome: ServerMsg = next(&mut tx, &mut buf, hello).await;
        assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");

        let mut req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        req.moves = vec![(Color::Black, Point::PASS); MAX_REQUEST_STONES + 1];
        let open = ClientMsg::Open {
            sub: 1,
            engine: None,
            req,
        };
        let refused: ServerMsg = next(&mut tx, &mut buf, open).await;
        assert!(
            matches!(
                refused,
                ServerMsg::Error {
                    sub: Some(1),
                    code: ErrCode::BadRequest,
                    ..
                }
            ),
            "an oversized request was not refused: {refused:?}"
        );
        assert!(recorder.0.lock().unwrap().is_empty());

        let mut req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        req.overrides = vec![
            ("maxVisits".into(), "1000000000".into()),
            ("wideRootNoise".into(), "0.0".into()),
            ("humanSLProfile".into(), "x".repeat(MAX_OVERRIDE_VALUE + 1)),
        ];
        let open = ClientMsg::Open {
            sub: 2,
            engine: None,
            req,
        };
        let opened: ServerMsg = next(&mut tx, &mut buf, open).await;
        assert!(matches!(opened, ServerMsg::Opened { sub: 2 }), "{opened:?}");
        assert_eq!(
            recorder.0.lock().unwrap()[0].overrides,
            [("wideRootNoise".to_string(), "0.0".to_string())]
        );

        conn.close(VarInt::from_u32(0), b"bye");
        tokio::time::timeout(Duration::from_secs(2), served)
            .await
            .expect("serve did not finish after the client left")
            .expect("serve task");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn priority_is_clamped_into_the_served_band() {
        let clamp = |p: i8| p.clamp(*PRIORITY_RANGE.start(), *PRIORITY_RANGE.end());
        assert_eq!(clamp(127), 8);
        assert_eq!(clamp(-128), -8);
        assert_eq!(clamp(4), 4);
        assert_eq!(clamp(0), 0);
    }

    /// A client that finishes the handshake and then stays silent must not hold a
    /// session slot. QUIC keep-alives would otherwise keep the connection alive past
    /// the idle timeout; the close has to be ours, and the slot has to come back.
    #[tokio::test]
    async fn a_client_that_never_opens_the_control_stream_releases_its_slot() {
        silent_client_is_closed(false).await;
    }

    /// Opening the control stream and then sending nothing is the same hold: the
    /// first Hello read is under the same deadline as waiting for the stream.
    #[tokio::test]
    async fn a_client_that_never_sends_hello_releases_its_slot() {
        silent_client_is_closed(true).await;
    }

    async fn silent_client_is_closed(open_control_stream: bool) {
        // Far below the 30 s idle timeout and the 5 s keep-alive, so a close here
        // is the pre-authentication deadline and not QUIC giving up.
        let deadline = Duration::from_millis(500);
        let sessions = Arc::new(tokio::sync::Semaphore::new(1));
        let (endpoint, fp, dir) = test_endpoint("silent");
        let addr = endpoint.local_addr().expect("bound");
        let accepting = endpoint.clone();
        let slots = Arc::clone(&sessions);

        let served = tokio::spawn(async move {
            let incoming = tokio::time::timeout(Duration::from_secs(2), accepting.accept())
                .await
                .expect("the client never connected")
                .expect("endpoint closed");
            let permit = slots.try_acquire_owned().expect("session slot");
            let _permit = permit;
            serve(
                Arc::new(Host {
                    engines: Vec::new(),
                    tokens: Vec::new(),
                }),
                incoming,
                deadline,
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), Some(fp))
            .await
            .expect("handshake");
        // Hold the stream open and write nothing. Dropping it would reset the stream,
        // and the server would then see EOF instead of a silent peer.
        let held = if open_control_stream {
            Some(conn.open_bi().await.expect("control stream"))
        } else {
            None
        };

        let closed = tokio::time::timeout(Duration::from_secs(2), conn.closed())
            .await
            .expect("silent client was not closed within the deadline");
        match closed {
            quinn::ConnectionError::ApplicationClosed(close) => {
                assert_eq!(close.error_code, VarInt::from_u32(1));
                assert_eq!(close.reason.as_ref(), b"pre-authentication deadline");
            }
            other => panic!("expected an application close, got {other}"),
        }
        drop(held);

        tokio::time::timeout(Duration::from_secs(1), served)
            .await
            .expect("serve did not return after closing")
            .expect("serve task");
        assert_eq!(
            sessions.available_permits(),
            1,
            "the silent client still holds a session slot"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A Hello that arrives inside the deadline is authenticated, not closed as late.
    /// Otherwise the checks above would pass for a server that refuses every connection.
    #[tokio::test]
    async fn a_prompt_hello_is_not_closed_as_late() {
        let (endpoint, fp, dir) = test_endpoint("prompt");
        let addr = endpoint.local_addr().expect("bound");
        let accepting = endpoint.clone();
        let served = tokio::spawn(async move {
            let incoming = accepting.accept().await.expect("incoming");
            serve(
                Arc::new(Host {
                    engines: Vec::new(),
                    tokens: vec![Token {
                        value: "secret".into(),
                        name: "test".into(),
                        max_subs: 1,
                    }],
                }),
                incoming,
                Duration::from_secs(2),
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), Some(fp))
            .await
            .expect("handshake");
        let (mut tx, mut rx) = conn.open_bi().await.expect("control stream");
        let mut buf = FrameBuf::new();
        frame::write_msg(
            &mut tx,
            &mut buf,
            &ClientMsg::Hello {
                proto: PROTO_VERSION,
                token: "secret".into(),
                client: "test".into(),
            },
        )
        .await
        .expect("write Hello");
        let msg: ServerMsg =
            tokio::time::timeout(Duration::from_secs(2), frame::read_msg(&mut rx, &mut buf))
                .await
                .expect("a prompt Hello was not answered")
                .expect("read Welcome");
        assert!(
            matches!(msg, ServerMsg::Welcome { .. }),
            "a prompt Hello was not welcomed: {msg:?}"
        );
        assert!(conn.close_reason().is_none(), "a prompt Hello was closed");
        conn.close(VarInt::from_u32(0), b"bye");
        tokio::time::timeout(Duration::from_secs(2), served)
            .await
            .expect("serve did not finish after the client left")
            .expect("serve task");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Publishes incompressible reports until its subscription is dropped, so a stream
    /// nobody reads blocks on flow control within a few reports. Dropping a subscription
    /// sends on `dropped`, then waits for `gate` to open: a query that is slow to die.
    struct Flood {
        dropped: tokio::sync::mpsc::UnboundedSender<()>,
        gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    }

    impl Engine for Flood {
        fn subscribe(&self, _req: AnalyzeReq) -> Subscription {
            let (tx, rx) = tokio::sync::watch::channel(SubEvent::Pending);
            tokio::spawn(async move {
                let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
                loop {
                    let mut report = mirai_proto::types::Report::empty(0, Color::Black);
                    let policy = (0..362).map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        seed as u16
                    });
                    report.policy = Some(policy.collect());
                    if tx.send(SubEvent::Report(Arc::new(report))).is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            let dropped = self.dropped.clone();
            let gate = Arc::clone(&self.gate);
            Subscription::new(
                rx,
                mirai_engine::CancelGuard::new(move || {
                    let _ = dropped.send(());
                    let (open, cv) = &*gate;
                    let mut open = open.lock().unwrap();
                    while !*open {
                        open = cv.wait(open).unwrap();
                    }
                }),
            )
        }

        fn describe(&self) -> EngineDesc {
            EngineDesc::placeholder("flood")
        }
    }

    struct Client {
        tx: SendStream,
        rx: quinn::RecvStream,
        buf: FrameBuf,
        dropped: tokio::sync::mpsc::UnboundedReceiver<()>,
        gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        _conn: Connection,
        _endpoint: quinn::Endpoint,
        dir: std::path::PathBuf,
    }

    impl Client {
        /// A greeted session on a [`Flood`] engine whose drop gate starts `open`.
        async fn connect(max_subs: u32, open: bool) -> Client {
            let (endpoint, fp, dir) = test_endpoint("flood");
            let addr = endpoint.local_addr().expect("bound");
            let (dropped_tx, dropped) = tokio::sync::mpsc::unbounded_channel();
            let gate = Arc::new((std::sync::Mutex::new(open), std::sync::Condvar::new()));
            let host = Arc::new(Host {
                engines: vec![NamedEngine {
                    name: "flood".into(),
                    engine: Arc::new(Flood {
                        dropped: dropped_tx,
                        gate: Arc::clone(&gate),
                    }),
                }],
                tokens: vec![Token {
                    value: "secret".into(),
                    name: "test".into(),
                    max_subs,
                }],
            });
            let accepting = endpoint.clone();
            tokio::spawn(async move {
                let incoming = accepting.accept().await.expect("incoming");
                serve(host, incoming, Duration::from_secs(2)).await;
            });

            let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), Some(fp))
                .await
                .expect("handshake");
            let (tx, rx) = conn.open_bi().await.expect("control stream");
            let mut client = Client {
                tx,
                rx,
                buf: FrameBuf::new(),
                dropped,
                gate,
                _conn: conn,
                _endpoint: endpoint,
                dir,
            };
            client
                .send(ClientMsg::Hello {
                    proto: PROTO_VERSION,
                    token: "secret".into(),
                    client: "test".into(),
                })
                .await;
            assert!(matches!(client.recv().await, ServerMsg::Welcome { .. }));
            client
        }

        async fn send(&mut self, msg: ClientMsg) {
            frame::write_msg(&mut self.tx, &mut self.buf, &msg)
                .await
                .expect("write");
        }

        async fn recv(&mut self) -> ServerMsg {
            tokio::time::timeout(
                Duration::from_secs(2),
                frame::read_msg(&mut self.rx, &mut self.buf),
            )
            .await
            .expect("the server did not answer")
            .expect("read")
        }

        /// Opens `sub` and returns the answer. The subscription stream is never read.
        async fn open(&mut self, sub: u32) -> ServerMsg {
            let req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
            self.send(ClientMsg::Open {
                sub,
                engine: None,
                req,
            })
            .await;
            self.recv().await
        }

        async fn dropped(&mut self) {
            tokio::time::timeout(Duration::from_secs(1), self.dropped.recv())
                .await
                .expect("the cancelled subscription was not dropped")
                .expect("engine gone");
        }

        fn open_gate(&self) {
            let (open, cv) = &*self.gate;
            *open.lock().unwrap() = true;
            cv.notify_all();
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            self.open_gate();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A client that stops reading leaves the pump blocked on flow control. `Cancel` must
    /// still drop the subscription, or the KataGo query keeps running.
    #[tokio::test]
    async fn cancel_drops_a_subscription_whose_stream_is_not_read() {
        let mut client = Client::connect(4, true).await;
        assert!(matches!(client.open(1).await, ServerMsg::Opened { sub: 1 }));
        // Far longer than the flood needs to fill a 16 KiB window.
        tokio::time::sleep(Duration::from_millis(300)).await;
        client.send(ClientMsg::Cancel { sub: 1 }).await;
        client.dropped().await;
    }

    /// Stepping through a game at the limit is `Cancel` then `Open`, back to back. The
    /// cancelled query has not stopped when the `Open` is read; it must still be served.
    #[tokio::test]
    async fn cancel_then_reopen_at_the_limit_is_served() {
        let mut client = Client::connect(1, true).await;
        for sub in 1..20 {
            client.send(ClientMsg::Cancel { sub: sub - 1 }).await;
            let answer = client.open(sub).await;
            assert!(
                matches!(answer, ServerMsg::Opened { .. }),
                "reopen {sub} was refused: {answer:?}"
            );
        }
    }

    /// `max_subs` counts queries, not ids: a cancelled subscription holds its slot until it
    /// has actually been dropped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancelled_subscription_holds_its_slot_until_dropped() {
        let mut client = Client::connect(1, false).await;
        assert!(matches!(client.open(1).await, ServerMsg::Opened { sub: 1 }));
        client.send(ClientMsg::Cancel { sub: 1 }).await;
        client.dropped().await;
        let refused = client.open(2).await;
        assert!(
            matches!(
                refused,
                ServerMsg::Error {
                    sub: Some(2),
                    code: ErrCode::TooManySubs,
                    ..
                }
            ),
            "a query still being dropped did not count: {refused:?}"
        );

        client.open_gate();
        for sub in 3..100 {
            match client.open(sub).await {
                ServerMsg::Opened { .. } => return,
                ServerMsg::Error {
                    code: ErrCode::TooManySubs,
                    ..
                } => tokio::time::sleep(Duration::from_millis(10)).await,
                other => panic!("unexpected answer {other:?}"),
            }
        }
        panic!("the slot never came back after the subscription was dropped");
    }

    /// Runs the server side of one connection on `first`, the raw bytes the client puts on
    /// its control stream, and returns what the client read back and how the session ended.
    async fn first_frame_exchange(first: Vec<u8>) -> (ServerMsg, String) {
        let (endpoint, fp, dir) = test_endpoint("hello");
        let addr = endpoint.local_addr().expect("bound");
        let accepting = endpoint.clone();
        let served = tokio::spawn(async move {
            let conn = accepting
                .accept()
                .await
                .expect("incoming")
                .await
                .expect("handshake");
            let host = Host {
                engines: Vec::new(),
                tokens: vec![Token {
                    value: "secret".into(),
                    name: "test".into(),
                    max_subs: 1,
                }],
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            session_loop(&host, &conn, 1, deadline).await
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), Some(fp))
            .await
            .expect("handshake");
        let (mut tx, mut rx) = conn.open_bi().await.expect("control stream");
        tokio::io::AsyncWriteExt::write_all(&mut tx, &first)
            .await
            .expect("write the first frame");
        let reply: ServerMsg = tokio::time::timeout(
            Duration::from_secs(2),
            frame::read_msg(&mut rx, &mut FrameBuf::new()),
        )
        .await
        .expect("the server did not answer")
        .expect("read the answer");
        let outcome = tokio::time::timeout(Duration::from_secs(2), served)
            .await
            .expect("the session did not end")
            .expect("serve task");
        let _ = std::fs::remove_dir_all(dir);
        (
            reply,
            outcome.expect_err("the session was accepted").to_string(),
        )
    }

    fn hello_frame(token: &str, client: &str) -> Vec<u8> {
        let mut buf = FrameBuf::new();
        frame::encode(
            &mut buf,
            &ClientMsg::Hello {
                proto: PROTO_VERSION,
                token: token.into(),
                client: client.into(),
            },
        )
        .expect("encode Hello")
        .to_vec()
    }

    fn assert_bad_request(reply: &ServerMsg) {
        assert!(
            matches!(
                reply,
                ServerMsg::Error {
                    sub: None,
                    code: ErrCode::BadRequest,
                    ..
                }
            ),
            "expected a BadRequest, got {reply:?}"
        );
    }

    /// A few hundred bytes of zstd inflate to a megabyte `client`. The first frame's bound
    /// covers the plaintext, so inflating stops at the bound and the frame is refused as too
    /// large before anything is deserialised or the token is looked at.
    #[tokio::test]
    async fn a_compressed_oversized_hello_is_refused_before_inflating() {
        for token in ["wrong", "secret"] {
            let client = "A".repeat(1 << 20);
            let first = hello_frame(token, &client);
            assert!(first.len() - 5 <= MAX_HELLO_FRAME, "{} bytes", first.len());
            let (reply, error) = first_frame_exchange(first).await;
            assert_bad_request(&reply);
            assert_eq!(
                error,
                format!("the first frame exceeds {MAX_HELLO_FRAME} bytes")
            );
        }
    }

    /// Fields over [`MAX_HELLO_FIELD`] in a frame within the bound are refused too, and the
    /// oversized `client` stays out of the log whatever the token.
    #[tokio::test]
    async fn an_oversized_hello_field_is_refused_and_kept_out_of_the_log() {
        for token in ["wrong", "secret"] {
            let client = "A".repeat(MAX_HELLO_FIELD + 1);
            let first = hello_frame(token, &client);
            assert!(first.len() - 5 <= MAX_HELLO_FRAME, "{} bytes", first.len());
            let (reply, error) = first_frame_exchange(first).await;
            assert_bad_request(&reply);
            assert!(error.len() < 200, "{} bytes of error text", error.len());
        }
    }

    /// The bound is checked on the header, before any body arrives.
    #[tokio::test]
    async fn a_first_frame_over_the_hello_bound_is_refused_unread() {
        let mut header = (mirai_proto::frame::MAX_FRAME as u32)
            .to_le_bytes()
            .to_vec();
        header.push(0);
        let (reply, error) = first_frame_exchange(header).await;
        assert_bad_request(&reply);
        assert!(error.contains(&MAX_HELLO_FRAME.to_string()), "{error}");
    }

    /// A `client` within the bound is still escaped: a newline in it cannot forge a log line.
    #[tokio::test]
    async fn an_unauthorized_client_name_is_escaped() {
        let (reply, error) =
            first_frame_exchange(hello_frame("wrong", "x\nINFO forged\u{1b}[2J")).await;
        assert!(
            matches!(
                reply,
                ServerMsg::Error {
                    code: ErrCode::Unauthorized,
                    ..
                }
            ),
            "{reply:?}"
        );
        assert!(
            !error.contains('\n') && !error.contains('\u{1b}'),
            "{error:?}"
        );
    }

    /// ALPN is `mirai`, not a version, so a peer speaking `0.2.0` completes TLS and is
    /// refused with `BadVersion` naming both versions.
    #[tokio::test]
    async fn a_mismatched_protocol_version_is_bad_version_not_a_tls_failure() {
        let mut buf = FrameBuf::new();
        let first = frame::encode(
            &mut buf,
            &ClientMsg::Hello {
                proto: ProtoVersion::new(0, 2, 0),
                token: "secret".into(),
                client: "test".into(),
            },
        )
        .expect("encode Hello")
        .to_vec();
        let (reply, error) = first_frame_exchange(first).await;
        match reply {
            ServerMsg::Error {
                sub: None,
                code: ErrCode::BadVersion,
                msg,
            } => {
                assert!(
                    msg.contains("0.1.0") && msg.contains("0.2.0"),
                    "BadVersion did not name both versions: {msg}"
                );
            }
            other => panic!("expected BadVersion after the TLS handshake, got {other:?}"),
        }
        assert!(
            error.contains("0.2.0"),
            "the session ended without naming the offered version: {error}"
        );
    }

    fn test_endpoint(tag: &str) -> (quinn::Endpoint, String, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};

        static NTH: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mirai-server-preauth-{}-{}-{tag}",
            std::process::id(),
            NTH.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (certs, key) = mirai_proto::transport::load_or_generate_cert(
            &dir.join("cert.pem"),
            &dir.join("key.pem"),
            &["localhost".into()],
        )
        .expect("certificate");
        let fp = mirai_proto::transport::fingerprint_of(&certs);
        let listen = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
        let endpoint =
            mirai_proto::transport::server_endpoint(listen, certs, key).expect("endpoint");
        (endpoint, fp, dir)
    }
}
