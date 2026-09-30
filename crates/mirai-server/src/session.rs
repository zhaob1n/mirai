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
use tokio::sync::{mpsc, oneshot};
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

/// How long a peer can make a session wait. The server runs with [`Budgets::DEFAULT`];
/// tests shrink them rather than sit through them.
#[derive(Clone, Copy, Debug)]
pub struct Budgets {
    /// How long a peer may hold a session slot before it has authenticated.
    ///
    /// The accept loop takes the slot before the handshake. A QUIC keep-alive resets the
    /// idle timer, so a silent peer would otherwise hold the slot until the process
    /// exits, and 32 of them refuse every real client. One clock covers the handshake,
    /// opening the control stream and the first Hello frame. A rejection decided on that
    /// frame is delivered under `reject`, so a peer that sends a bad `Hello` and then
    /// stops reading cannot hold the slot by ignoring the `Error`. After a successful
    /// `Hello` the slot is a session and this bound no longer applies.
    pub preauth: Duration,
    /// How long [`reject`] waits for the peer to acknowledge the `Error` frame.
    ///
    /// Past this the connection is closed and the session slot released, whether or not
    /// the frame was acknowledged. A peer that is not reading must not pin the slot
    /// until the process exits.
    pub reject: Duration,
    /// How long one authenticated control write may block on flow control.
    ///
    /// A peer that stops reading fills the reply queue, and the control reader then
    /// waits on it too, so this budget also bounds how long its `Cancel` can go unread.
    /// When it expires the writer closes the connection, which drops every subscription
    /// of the session.
    pub control_write: Duration,
}

impl Budgets {
    pub const DEFAULT: Budgets = Budgets {
        preauth: Duration::from_secs(10),
        reject: Duration::from_secs(2),
        control_write: Duration::from_secs(10),
    };
}

/// Floor for `report_every_ms`. The desktop interval slider starts at 20 ms;
/// a faster interval only multiplies report traffic on a shared engine.
const MIN_REPORT_EVERY: u16 = 20;

/// Visit cap used when the request names no time bound. This is the analysis
/// slider's upper end. A time-bounded search may ask for more visits; its clock
/// is what stops it.
const MAX_UNBOUNDED_VISITS: u32 = 10_000_000;

/// Longest `max_time_ms` forwarded to KataGo. The desktop clock's longest think
/// budget is half of a 10-hour main time, which is under six hours.
const MAX_TIME_MS: u32 = 6 * 60 * 60 * 1000;

/// Close reason when [`Budgets::preauth`] expires. There may be no control
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
    /// Permits shared by every connection that presented this token. `max_subs`
    /// is a property of the token, not of one connection: N connections must not
    /// multiply it.
    slots: Arc<tokio::sync::Semaphore>,
}

impl Token {
    pub fn new(value: impl Into<String>, name: impl Into<String>, max_subs: u32) -> Token {
        Token {
            value: value.into(),
            name: name.into(),
            max_subs,
            slots: Arc::new(tokio::sync::Semaphore::new(max_subs as usize)),
        }
    }
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

/// Clamps what one search may cost the shared engine.
///
/// Priority was already clamped; visits, time and the report interval were not, so one
/// authenticated client could ask KataGo for a report every millisecond or a search that
/// never ends. Values the reference client sends are left alone: its interval slider
/// starts at 20 ms, its analysis cap is 10 million visits, and a timed game carries a
/// `max_time_ms` under six hours. A search with neither a visit cap nor a time cap is
/// given the visit cap. Clamping, not refusing, matches priority.
fn clamp_search(req: &mut AnalyzeReq) {
    req.priority = req
        .priority
        .clamp(*PRIORITY_RANGE.start(), *PRIORITY_RANGE.end());
    if req.report_every_ms.is_some_and(|ms| ms < MIN_REPORT_EVERY) {
        req.report_every_ms = Some(MIN_REPORT_EVERY);
    }
    if req.max_time_ms.is_some_and(|ms| ms > MAX_TIME_MS) {
        req.max_time_ms = Some(MAX_TIME_MS);
    }
    let visits_unbounded = match req.max_visits {
        Some(visits) => visits > MAX_UNBOUNDED_VISITS,
        None => true,
    };
    if visits_unbounded && !req.max_time_ms.is_some_and(|ms| ms > 0) {
        req.max_visits = Some(MAX_UNBOUNDED_VISITS);
    }
}

/// Serves one incoming connection.
///
/// `budgets.preauth` bounds only the unauthenticated prefix. Once `Hello` has been read
/// the session runs until the peer leaves. On expiry the connection is closed
/// and this returns, so the caller's session permit is released. After the
/// handshake that close is application code 1; before 1-RTT keys exist it is a
/// transport `APPLICATION_ERROR`.
pub async fn serve(host: Arc<Host>, incoming: Incoming, budgets: Budgets) {
    let peer = incoming.remote_address();
    // One clock for the handshake, the control stream and the first Hello. A
    // fresh timeout on each step would let a slow peer consume the bound three
    // times, and a QUIC keep-alive would otherwise renew the slot forever.
    let deadline = tokio::time::Instant::now() + budgets.preauth;
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
    let outcome = session_loop(&host, &conn, session, deadline, budgets).await;
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
    budgets: Budgets,
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
            reject(
                &mut tx,
                &mut wbuf,
                conn,
                budgets.reject,
                ErrCode::BadRequest,
                &msg,
            )
            .await;
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
            budgets.reject,
            ErrCode::BadRequest,
            "expected Hello as the first control message",
        )
        .await;
        anyhow::bail!("first control message was not Hello");
    };

    if token.len() > MAX_HELLO_FIELD || client.len() > MAX_HELLO_FIELD {
        let msg = format!("Hello.token and Hello.client are limited to {MAX_HELLO_FIELD} bytes");
        reject(
            &mut tx,
            &mut wbuf,
            conn,
            budgets.reject,
            ErrCode::BadRequest,
            &msg,
        )
        .await;
        anyhow::bail!("{msg}");
    }

    if proto != PROTO_VERSION {
        reject(
            &mut tx,
            &mut wbuf,
            conn,
            budgets.reject,
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
            budgets.reject,
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
    let slots = Arc::clone(&auth.slots);
    info!(session, client = ?client, token = token_name, max_subs, "authenticated");

    // Writes run on their own task, so a write blocked on flow control does not stop the
    // reader from applying `Cancel`. The queue absorbs a burst of replies; once it is
    // full the reader waits for the writer, which gives up after `budgets.control_write`.
    // A full queue alone says nothing: the reader can fill it from one packet before
    // the writer is ever scheduled.
    let (out_tx, out_rx) = mpsc::channel(32);
    let writer = tokio::spawn(control_writer(
        conn.clone(),
        tx,
        out_rx,
        budgets.control_write,
    ));
    let outcome = async {
        enqueue(
            &out_tx,
            ServerMsg::Welcome {
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
        // Permits this connection has taken and not yet released. The semaphore itself is
        // the token's, shared with every other connection, so it cannot tell a cancelled
        // query of *this* connection from a live query of another.
        let mut holding = 0usize;
        let (release_tx, mut release_rx) = mpsc::unbounded_channel();

        loop {
            let msg = match frame::read_msg::<_, ClientMsg>(&mut rx, &mut rbuf).await {
                Ok(m) => m,
                Err(FrameError::Eof) => return Ok(()),
                Err(e) => return Err(e.into()),
            };

            match msg {
                ClientMsg::Hello { .. } => {
                    enqueue_error(&out_tx, None, ErrCode::BadRequest, "already greeted").await?;
                }
                ClientMsg::Ping(n) => enqueue(&out_tx, ServerMsg::Pong(n)).await?,
                ClientMsg::ListEngines => {
                    enqueue(&out_tx, ServerMsg::Engines(host.describe())).await?;
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
                    // Drained after the reap, so a search that ended on its own is not
                    // mistaken below for a cancelled one still stopping.
                    while release_rx.try_recv().is_ok() {
                        holding = holding.saturating_sub(1);
                    }

                    if subs.contains_key(&sub) {
                        enqueue_error(
                            &out_tx,
                            Some(sub),
                            ErrCode::BadRequest,
                            "subscription id is already in use",
                        )
                        .await?;
                        continue;
                    }
                    let slot = match Arc::clone(&slots).try_acquire_owned() {
                        Ok(slot) => Some(slot),
                        // A cancelled query of this connection still holds its permit. Stopping
                        // takes the pump a scheduler turn, and a client that cancels and reopens
                        // at its limit must not lose the race to it. Another connection's live
                        // query is not that race: refuse it at once.
                        Err(_) if holding > subs.len() => {
                            tokio::time::timeout(CANCEL_GRACE, Arc::clone(&slots).acquire_owned())
                                .await
                                .ok()
                                .and_then(Result::ok)
                        }
                        Err(_) => None,
                    };
                    let Some(slot) = slot else {
                        enqueue_error(
                            &out_tx,
                            Some(sub),
                            ErrCode::TooManySubs,
                            &format!(
                                "token {token_name:?} allows {max_subs} concurrent subscriptions"
                            ),
                        )
                        .await?;
                        continue;
                    };
                    let Some(named) = host.resolve_engine(engine.as_deref()) else {
                        let wanted = engine.as_deref().unwrap_or("<default>");
                        enqueue_error(
                            &out_tx,
                            Some(sub),
                            ErrCode::NoSuchEngine,
                            &format!("no engine named {wanted:?}"),
                        )
                        .await?;
                        continue;
                    };

                    if let Some(msg) = request_error(&req) {
                        enqueue_error(&out_tx, Some(sub), ErrCode::BadRequest, &msg).await?;
                        continue;
                    }

                    clamp_search(&mut req);
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
                    enqueue(&out_tx, ServerMsg::Opened { sub }).await?;
                    holding += 1;
                    let conn = conn.clone();
                    let slot = PumpSlot {
                        permit: Some(slot),
                        released: release_tx.clone(),
                    };
                    tokio::spawn(async move {
                        pump(conn, session, sub, subscription, cancel_rx).await;
                        // Only now is the query gone: `pump` has dropped the subscription.
                        drop(slot);
                    });
                }
            }
        }
    }
    .await;
    // A write still blocked on flow control must not outlive the session: aborting it
    // drops the send stream, and `serve` closes the connection.
    writer.abort();
    outcome
}

/// A pump's share of its token's quota. Dropped when the pump ends — or unwinds — it
/// returns the permit first and then tells its session, so the session's count of the
/// permits it holds never stays high.
struct PumpSlot {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    released: mpsc::UnboundedSender<()>,
}

impl Drop for PumpSlot {
    fn drop(&mut self) {
        drop(self.permit.take());
        let _ = self.released.send(());
    }
}

/// Streams one subscription's events down its own unidirectional stream.
///
/// Returning drops `subscription`, which terminates the KataGo query — so every exit path
/// here, including the connection simply going away, cleans up the engine side.
///
/// `cancel` is raced against every await, not only the wait for the next event: opening the
/// stream waits on the peer's stream limit and a write waits on its flow control, and a
/// client that stops reading would otherwise keep a cancelled query running. Once the stream
/// is open the peer's `STOP_SENDING` is raced too (§8.4): a search without a report interval
/// writes nothing until it ends, so no failed write would reveal it.
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
    // Not a borrow of `stream`: `stream_events` needs it mutably.
    let stopped = stream.stopped();
    let dropped = tokio::select! {
        biased;
        _ = cancel => true,
        _ = stopped => true,
        () = stream_events(&mut stream, session, sub, subscription) => false,
    };
    if dropped {
        // The losing branch, and the subscription it owned, is already dropped. Reset
        // rather than finish: buffered stale reports must never arrive.
        let _ = stream.reset(VarInt::from_u32(CODE_CANCELLED));
        info!(session, sub, "subscription dropped");
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
            warn!(session, sub, error = %e, "could not start the stream's compressor");
            return;
        }
    };
    let mut own = OwnershipDelta::default();
    // `current` borrows without marking the value seen, so `next` would hand a report
    // already published back to be sent again.
    let mut event = subscription.latest();
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

/// Queues a control reply, waiting while the queue is full. An error means the writer
/// has gone: the connection closed or a write outlasted [`Budgets::control_write`].
async fn enqueue(tx: &mpsc::Sender<ServerMsg>, msg: ServerMsg) -> anyhow::Result<()> {
    tx.send(msg)
        .await
        .map_err(|_| anyhow::anyhow!("the control stream writer has stopped"))
}

async fn enqueue_error(
    tx: &mpsc::Sender<ServerMsg>,
    sub: Option<u32>,
    code: ErrCode,
    msg: &str,
) -> anyhow::Result<()> {
    warn!(?sub, %code, msg, "rejecting");
    enqueue(
        tx,
        ServerMsg::Error {
            sub,
            code,
            msg: msg.to_string(),
        },
    )
    .await
}

/// Writes control messages until the peer, the connection, or the budget gives up.
///
/// A stuck write must not be the only thing keeping the connection open: on timeout the
/// connection is closed, which fails the reader's next read and drops every subscription.
async fn control_writer(
    conn: Connection,
    mut tx: SendStream,
    mut rx: mpsc::Receiver<ServerMsg>,
    budget: Duration,
) {
    let mut buf = FrameBuf::new();
    loop {
        let msg = tokio::select! {
            biased;
            _ = conn.closed() => return,
            msg = rx.recv() => match msg {
                Some(msg) => msg,
                None => return,
            },
        };
        if write_control(&conn, &mut tx, &mut buf, &msg, budget)
            .await
            .is_err()
        {
            conn.close(VarInt::from_u32(0), b"bye");
            return;
        }
    }
}

async fn write_control(
    conn: &Connection,
    tx: &mut SendStream,
    buf: &mut FrameBuf,
    msg: &ServerMsg,
    budget: Duration,
) -> anyhow::Result<()> {
    let write = frame::write_msg(tx, buf, msg);
    tokio::pin!(write);
    tokio::select! {
        biased;
        _ = conn.closed() => anyhow::bail!("connection closed"),
        result = &mut write => result.map_err(anyhow::Error::from),
        _ = tokio::time::sleep(budget) => {
            anyhow::bail!("control write timed out")
        }
    }
}

/// Reports a fatal handshake problem, then closes the connection with application code 1.
///
/// The write and the wait for its acknowledgement are bounded by `budget`.
/// Peers that acknowledge in time receive the `Error`; others still release the
/// session slot when the budget expires.
async fn reject(
    tx: &mut SendStream,
    buf: &mut FrameBuf,
    conn: &Connection,
    budget: Duration,
    code: ErrCode,
    msg: &str,
) {
    warn!(%code, msg, "rejecting");
    let deliver = async {
        let _ = frame::write_msg(
            tx,
            buf,
            &ServerMsg::Error {
                sub: None,
                code,
                msg: msg.to_string(),
            },
        )
        .await;
        // Flush before the close: a reset would discard the error the client needs to see.
        let _ = stream_flush(tx).await;
    };
    tokio::pin!(deliver);
    tokio::select! {
        biased;
        _ = conn.closed() => {}
        _ = tokio::time::sleep(budget) => {
            warn!("pre-authentication rejection was not acknowledged");
        }
        _ = &mut deliver => {}
    }
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

    /// Budgets a test can wait out. A peer here is a local task that answers at once,
    /// so a stalled control write is a real stall well within a second.
    const TEST_BUDGETS: Budgets = Budgets {
        preauth: Duration::from_secs(2),
        control_write: Duration::from_secs(1),
        ..Budgets::DEFAULT
    };

    fn host(tokens: &[(&str, u32)]) -> Host {
        Host {
            engines: Vec::new(),
            tokens: tokens
                .iter()
                .map(|(v, max)| Token::new(*v, *v, *max))
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
                    tokens: vec![Token::new("secret", "test", 4)],
                }),
                incoming,
                TEST_BUDGETS,
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), fp)
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
                Budgets {
                    preauth: deadline,
                    ..TEST_BUDGETS
                },
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), fp)
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
                    tokens: vec![Token::new("secret", "test", 1)],
                }),
                incoming,
                TEST_BUDGETS,
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), fp)
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
    /// nobody reads blocks on flow control within a few reports. Like KataGo, a request
    /// without a report interval publishes nothing. Dropping a subscription sends on
    /// `dropped`, then waits for `gate` to open: a query that is slow to die.
    struct Flood {
        dropped: tokio::sync::mpsc::UnboundedSender<()>,
        gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    }

    impl Engine for Flood {
        fn subscribe(&self, req: AnalyzeReq) -> Subscription {
            let (tx, rx) = tokio::sync::watch::channel(SubEvent::Pending);
            if req.report_every_ms.is_none() {
                return self.guarded(rx, Some(tx));
            }
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
            self.guarded(rx, None)
        }

        fn describe(&self) -> EngineDesc {
            EngineDesc::placeholder("flood")
        }
    }

    impl Flood {
        /// `quiet` is the sender of a search that publishes nothing; the guard keeps it
        /// alive so the subscription stays open until it is dropped.
        fn guarded(
            &self,
            rx: tokio::sync::watch::Receiver<SubEvent>,
            quiet: Option<tokio::sync::watch::Sender<SubEvent>>,
        ) -> Subscription {
            let dropped = self.dropped.clone();
            let gate = Arc::clone(&self.gate);
            Subscription::new(
                rx,
                mirai_engine::CancelGuard::new(move || {
                    drop(quiet);
                    let _ = dropped.send(());
                    let (open, cv) = &*gate;
                    let mut open = open.lock().unwrap();
                    while !*open {
                        open = cv.wait(open).unwrap();
                    }
                }),
            )
        }
    }

    struct Client {
        tx: SendStream,
        rx: quinn::RecvStream,
        buf: FrameBuf,
        dropped: tokio::sync::mpsc::UnboundedReceiver<()>,
        gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        conn: Connection,
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
                tokens: vec![Token::new("secret", "test", max_subs)],
            });
            let accepting = endpoint.clone();
            tokio::spawn(async move {
                let incoming = accepting.accept().await.expect("incoming");
                serve(host, incoming, TEST_BUDGETS).await;
            });

            let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), fp)
                .await
                .expect("handshake");
            let (tx, rx) = conn.open_bi().await.expect("control stream");
            let mut client = Client {
                tx,
                rx,
                buf: FrameBuf::new(),
                dropped,
                gate,
                conn,
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

        /// Opens a reporting search as `sub` and returns the answer. The subscription stream
        /// is never read.
        async fn open(&mut self, sub: u32) -> ServerMsg {
            let mut req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
            req.report_every_ms = Some(MIN_REPORT_EVERY);
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

    /// §8.4 lets a client stop a subscription with `STOP_SENDING` alone. A search without
    /// a report interval writes nothing until it ends, so no failed write reveals the stop.
    #[tokio::test]
    async fn stop_sending_alone_drops_a_search_that_is_not_reporting() {
        let mut client = Client::connect(1, true).await;
        let req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        client
            .send(ClientMsg::Open {
                sub: 1,
                engine: None,
                req,
            })
            .await;
        assert!(matches!(client.recv().await, ServerMsg::Opened { sub: 1 }));
        let mut stream = tokio::time::timeout(Duration::from_secs(2), client.conn.accept_uni())
            .await
            .expect("no subscription stream")
            .expect("accept uni");
        stream.stop(VarInt::from_u32(CODE_CANCELLED)).expect("stop");
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
                tokens: vec![Token::new("secret", "test", 1)],
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            session_loop(&host, &conn, 1, deadline, TEST_BUDGETS).await
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), fp)
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

    /// A report published before the stream opens is still unseen. Sending what `current`
    /// borrowed and then waiting on `next` puts that report on the wire twice.
    #[tokio::test]
    async fn a_report_already_published_is_sent_once() {
        use std::sync::Mutex;

        use mirai_engine::CancelGuard;
        use mirai_proto::frame::SubStreamDecoder;
        use mirai_proto::msg::SubMsg;
        use mirai_proto::types::Report;

        struct Holding {
            tx: Mutex<Option<tokio::sync::watch::Sender<SubEvent>>>,
        }

        impl Engine for Holding {
            fn subscribe(&self, _req: AnalyzeReq) -> Subscription {
                let (tx, rx) = tokio::sync::watch::channel(SubEvent::Pending);
                let mut report = Report::empty(0, Color::Black);
                report.root.visits = 1;
                tx.send(SubEvent::Report(Arc::new(report))).unwrap();
                *self.tx.lock().unwrap() = Some(tx);
                Subscription::new(rx, CancelGuard::noop())
            }

            fn describe(&self) -> EngineDesc {
                EngineDesc::placeholder("holding")
            }
        }

        let holding = Arc::new(Holding {
            tx: Mutex::new(None),
        });
        let (endpoint, fp, dir) = test_endpoint("once");
        let addr = endpoint.local_addr().expect("bound");
        let accepting = endpoint.clone();
        let engine: Arc<dyn Engine> = holding.clone();
        let served = tokio::spawn(async move {
            let incoming = accepting.accept().await.expect("incoming");
            serve(
                Arc::new(Host {
                    engines: vec![NamedEngine {
                        name: "holding".into(),
                        engine,
                    }],
                    tokens: vec![Token::new("secret", "test", 4)],
                }),
                incoming,
                TEST_BUDGETS,
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), fp)
            .await
            .expect("handshake");
        let (mut tx, mut rx) = conn.open_bi().await.expect("control stream");
        let mut buf = FrameBuf::new();
        let hello = ClientMsg::Hello {
            proto: PROTO_VERSION,
            token: "secret".into(),
            client: "test".into(),
        };
        frame::write_msg(&mut tx, &mut buf, &hello)
            .await
            .expect("write Hello");
        let welcome: ServerMsg =
            tokio::time::timeout(Duration::from_secs(2), frame::read_msg(&mut rx, &mut buf))
                .await
                .expect("no Welcome")
                .expect("read Welcome");
        assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");

        let open = ClientMsg::Open {
            sub: 1,
            engine: None,
            req: AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5),
        };
        frame::write_msg(&mut tx, &mut buf, &open)
            .await
            .expect("write Open");
        let opened: ServerMsg =
            tokio::time::timeout(Duration::from_secs(2), frame::read_msg(&mut rx, &mut buf))
                .await
                .expect("no Opened")
                .expect("read Opened");
        assert!(matches!(opened, ServerMsg::Opened { sub: 1 }), "{opened:?}");

        let mut uni = tokio::time::timeout(Duration::from_secs(2), conn.accept_uni())
            .await
            .expect("no subscription stream")
            .expect("accept uni");
        let mut preamble = [0u8; 4];
        uni.read_exact(&mut preamble).await.expect("preamble");
        assert_eq!(u32::from_le_bytes(preamble), 1);

        let mut dec = SubStreamDecoder::new().expect("decoder");
        let first: SubMsg = tokio::time::timeout(Duration::from_secs(2), dec.read(&mut uni))
            .await
            .expect("the published report was not sent")
            .expect("read report");
        assert!(
            matches!(&first, SubMsg::Report(r) if r.root.visits == 1),
            "first message was not the published report: {first:?}"
        );

        // The duplicate, if `next` hands the same unseen value back, is written in the
        // same turn as the first report. Nothing newer is published during this wait.
        let duplicate =
            tokio::time::timeout(Duration::from_millis(200), dec.read::<_, SubMsg>(&mut uni)).await;
        if let Ok(msg) = duplicate {
            panic!("a report already present was sent twice: {msg:?}");
        }

        let sender = holding.tx.lock().unwrap().take().expect("sender");
        let mut newer = Report::empty(0, Color::Black);
        newer.root.visits = 2;
        sender
            .send(SubEvent::Report(Arc::new(newer)))
            .expect("publish");
        let second: SubMsg = tokio::time::timeout(Duration::from_secs(2), dec.read(&mut uni))
            .await
            .expect("a newer report was not sent")
            .expect("read newer");
        assert!(
            matches!(&second, SubMsg::Report(r) if r.root.visits == 2),
            "the report after the snapshot was dropped: {second:?}"
        );

        conn.close(VarInt::from_u32(0), b"bye");
        tokio::time::timeout(Duration::from_secs(2), served)
            .await
            .expect("serve did not finish")
            .expect("serve task");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_cost_is_clamped_without_refusing_the_request() {
        let mut req = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        req.max_visits = Some(u32::MAX);
        req.max_time_ms = None;
        req.report_every_ms = Some(1);
        req.priority = 100;
        clamp_search(&mut req);
        assert_eq!(req.max_visits, Some(MAX_UNBOUNDED_VISITS));
        assert_eq!(req.report_every_ms, Some(MIN_REPORT_EVERY));
        assert_eq!(req.priority, *PRIORITY_RANGE.end());
        assert!(request_error(&req).is_none(), "clamping is not a refusal");

        req.max_visits = None;
        clamp_search(&mut req);
        assert_eq!(req.max_visits, Some(MAX_UNBOUNDED_VISITS));

        // A timed search may ask for unlimited visits: the clock is the bound.
        // The desktop client does this for play.
        let mut timed = AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5);
        timed.max_visits = Some(u32::MAX);
        timed.max_time_ms = Some(60_000);
        timed.report_every_ms = Some(200);
        timed.priority = 8;
        clamp_search(&mut timed);
        assert_eq!(timed.max_visits, Some(u32::MAX));
        assert_eq!(timed.max_time_ms, Some(60_000));
        assert_eq!(timed.report_every_ms, Some(200));
        assert_eq!(timed.priority, 8);

        timed.max_time_ms = Some(u32::MAX);
        clamp_search(&mut timed);
        assert_eq!(timed.max_time_ms, Some(MAX_TIME_MS));

        // Zero milliseconds cannot be relied on to bound a search.
        timed.max_time_ms = Some(0);
        clamp_search(&mut timed);
        assert_eq!(timed.max_visits, Some(MAX_UNBOUNDED_VISITS));
    }

    /// A bad `Hello` whose `Error` is never read must still release the session slot.
    /// Quinn ACKs received packets even when the application does not read them; the
    /// bound also protects against a peer withholding that ACK while sending other traffic.
    #[tokio::test]
    async fn a_rejection_that_is_not_read_releases_its_slot() {
        let sessions = Arc::new(tokio::sync::Semaphore::new(1));
        let (endpoint, fp, dir) = test_endpoint("reject-unread");
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
                    tokens: vec![Token::new("secret", "test", 1)],
                }),
                incoming,
                TEST_BUDGETS,
            )
            .await;
        });

        let (conn, _) = mirai_proto::transport::connect(&format!("mirai://{addr}"), fp)
            .await
            .expect("handshake");
        let (mut tx, _rx) = conn.open_bi().await.expect("control stream");
        frame::write_msg(
            &mut tx,
            &mut FrameBuf::new(),
            &ClientMsg::Hello {
                proto: PROTO_VERSION,
                token: "wrong".into(),
                client: "test".into(),
            },
        )
        .await
        .expect("write Hello");
        // Do not read. Dropping the recv half would reset the stream and the server
        // would see that instead of a peer that simply is not reading.
        tokio::time::timeout(Duration::from_secs(4), conn.closed())
            .await
            .expect("an unread rejection held the connection");
        tokio::time::timeout(Duration::from_secs(2), served)
            .await
            .expect("serve did not return after the rejection budget")
            .expect("serve task");
        assert_eq!(
            sessions.available_permits(),
            1,
            "an unread rejection still holds a session slot"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Filling the control-stream window must not pin the search. The reader used to
    /// block in the write for ever, so `Cancel` was never seen and a keep-alive kept the
    /// connection open. The write budget now ends the session.
    #[tokio::test]
    async fn a_peer_that_stops_reading_the_control_stream_drops_its_searches() {
        let mut client = Client::connect(1, true).await;
        assert!(matches!(client.open(1).await, ServerMsg::Opened { sub: 1 }));
        // More Pongs than a 16 KiB stream window holds. The client keeps writing; it
        // does not read, so the server's next control write blocks on flow control.
        for n in 0..3_000u64 {
            if frame::write_msg(&mut client.tx, &mut client.buf, &ClientMsg::Ping(n))
                .await
                .is_err()
            {
                break;
            }
        }
        tokio::time::timeout(
            TEST_BUDGETS.control_write + Duration::from_secs(2),
            client.dropped.recv(),
        )
        .await
        .expect("the search outlived the control write budget")
        .expect("engine gone");
    }

    /// A peer that pipelines requests and reads the replies is not a peer that stopped
    /// reading. Two hundred `Ping`s in one write used to overflow the reply queue before
    /// the writer ran, and the session was closed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pipelined_burst_is_answered_in_full() {
        let mut client = Client::connect(1, true).await;
        let mut wire = Vec::new();
        for n in 0..200u64 {
            wire.extend_from_slice(frame::encode(&mut client.buf, &ClientMsg::Ping(n)).unwrap());
        }
        tokio::io::AsyncWriteExt::write_all(&mut client.tx, &wire)
            .await
            .expect("write");
        for n in 0..200u64 {
            assert!(matches!(client.recv().await, ServerMsg::Pong(m) if m == n));
        }
    }

    /// `max_subs` is the token's, not the connection's. A second connection must not
    /// get another copy of the limit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_token_shares_its_subscription_limit_across_connections() {
        let (endpoint, fp, dir) = test_endpoint("quota");
        let addr = endpoint.local_addr().expect("bound");
        let (dropped_tx, mut dropped) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let host = Arc::new(Host {
            engines: vec![NamedEngine {
                name: "flood".into(),
                engine: Arc::new(Flood {
                    dropped: dropped_tx,
                    gate: Arc::clone(&gate),
                }),
            }],
            tokens: vec![Token::new("secret", "shared", 1)],
        });
        let accepting = endpoint.clone();
        let accept_host = Arc::clone(&host);
        tokio::spawn(async move {
            for _ in 0..2 {
                let incoming = accepting.accept().await.expect("incoming");
                let host = Arc::clone(&accept_host);
                tokio::spawn(async move {
                    serve(host, incoming, TEST_BUDGETS).await;
                });
            }
        });

        async fn greet(
            addr: std::net::SocketAddr,
            fp: &str,
        ) -> (Connection, SendStream, quinn::RecvStream, FrameBuf) {
            let (conn, _) =
                mirai_proto::transport::connect(&format!("mirai://{addr}"), fp.to_string())
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
            let welcome: ServerMsg =
                tokio::time::timeout(Duration::from_secs(2), frame::read_msg(&mut rx, &mut buf))
                    .await
                    .expect("no Welcome")
                    .expect("read Welcome");
            assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");
            (conn, tx, rx, buf)
        }

        let (_conn_a, mut tx_a, mut rx_a, mut buf_a) = greet(addr, &fp).await;
        let (_conn_b, mut tx_b, mut rx_b, mut buf_b) = greet(addr, &fp).await;
        let open = |sub| ClientMsg::Open {
            sub,
            engine: None,
            req: AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5),
        };
        frame::write_msg(&mut tx_a, &mut buf_a, &open(1))
            .await
            .expect("open");
        let opened: ServerMsg = tokio::time::timeout(
            Duration::from_secs(2),
            frame::read_msg(&mut rx_a, &mut buf_a),
        )
        .await
        .expect("no Opened")
        .expect("read Opened");
        assert!(matches!(opened, ServerMsg::Opened { sub: 1 }), "{opened:?}");

        frame::write_msg(&mut tx_b, &mut buf_b, &open(1))
            .await
            .expect("open");
        let refused: ServerMsg = tokio::time::timeout(
            Duration::from_secs(2),
            frame::read_msg(&mut rx_b, &mut buf_b),
        )
        .await
        .expect("no answer")
        .expect("read answer");
        assert!(
            matches!(
                refused,
                ServerMsg::Error {
                    sub: Some(1),
                    code: ErrCode::TooManySubs,
                    ..
                }
            ),
            "a second connection was given its own limit: {refused:?}"
        );

        frame::write_msg(&mut tx_a, &mut buf_a, &ClientMsg::Cancel { sub: 1 })
            .await
            .expect("cancel");
        tokio::time::timeout(Duration::from_secs(1), dropped.recv())
            .await
            .expect("the first subscription was not dropped")
            .expect("engine gone");
        {
            let (open, cv) = &*gate;
            *open.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            cv.notify_all();
        }
        let mut moved = false;
        for sub in 2..20u32 {
            frame::write_msg(&mut tx_b, &mut buf_b, &open(sub))
                .await
                .expect("reopen");
            let again: ServerMsg = tokio::time::timeout(
                Duration::from_secs(2),
                frame::read_msg(&mut rx_b, &mut buf_b),
            )
            .await
            .expect("the shared slot never came back")
            .expect("read");
            match again {
                ServerMsg::Opened { .. } => {
                    moved = true;
                    break;
                }
                ServerMsg::Error {
                    code: ErrCode::TooManySubs,
                    ..
                } => tokio::time::sleep(Duration::from_millis(20)).await,
                other => panic!("the token's slot did not move to the other connection: {other:?}"),
            }
        }
        assert!(
            moved,
            "the token's slot did not move to the other connection"
        );
        let _ = std::fs::remove_dir_all(dir);
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
