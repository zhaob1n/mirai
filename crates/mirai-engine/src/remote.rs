// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Remote MRP engine client — the network mirror of [`LocalEngine`](crate::local::LocalEngine).
//!
//! One background task owns the whole connection: the QUIC handle, the bidirectional
//! control stream, and the live subscription table. Everything the outside world does goes
//! through an unbounded command channel, so [`Engine::subscribe`] stays synchronous and
//! never blocks the GUI thread.
//!
//! Stream topology is [`mirai_proto::transport`]'s: `ClientMsg`/`ServerMsg` on the control
//! stream, and one server-opened unidirectional stream per subscription whose first four
//! bytes are the little-endian subscription id. Cancelling a subscription both sends
//! `ClientMsg::Cancel` and `stop_sending`s that stream, so reports the user no longer cares
//! about are discarded in the network stack instead of being decoded and delivered.
//!
//! On connection loss every live subscription fails with [`EngineError::Disconnected`] and
//! the task reconnects with capped exponential backoff. Subscriptions are never silently
//! replayed: the GUI re-requests analysis for whatever node the user is actually looking at
//! once [`RemoteStatus::Connected`] returns.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use mirai_core::Size;
use mirai_proto::frame::{self, FrameBuf, FrameError, SubStreamDecoder};
use mirai_proto::msg::{ClientMsg, OwnershipDelta, ServerMsg, SubMsg};
use mirai_proto::transport::{self, TransportError};
use mirai_proto::types::{PROTO_VERSION, Report};
use quinn::{Connection, RecvStream, SendStream, VarInt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use crate::{AnalyzeReq, CancelGuard, Engine, EngineDesc, EngineError, SubEvent, Subscription};

/// Identifies this client build to the server.
const CLIENT_NAME: &str = concat!("mirai/", env!("CARGO_PKG_VERSION"));

/// QUIC application code sent with `stop_sending` on a cancelled subscription stream.
const CANCEL_CODE: u32 = 1;

/// QUIC application code for an orderly client-side close.
const BYE_CODE: u32 = 0;

/// Longest backoff between reconnection attempts.
const MAX_BACKOFF_MS: u64 = 8_000;

// ---------------------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------------------

/// Connection state of a [`RemoteEngine`], observable through
/// [`RemoteEngine::subscribe_status`].
///
/// `Failed` marks an error that reconnecting cannot fix on its own — a rejected token, a
/// protocol version mismatch, or a certificate that no longer matches the pin. The task
/// keeps retrying at the maximum backoff anyway (the server may be fixed and restarted),
/// but the status stays `Failed` so the UI can say something truthful instead of showing an
/// endless "reconnecting" spinner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteStatus {
    Connected,
    Reconnecting { attempt: u32 },
    Failed(String),
}

/// Trust-on-first-use certificate pins, `mirai://host:port` → lowercase hex SHA-256.
///
/// The GUI owns one of these, seeded from the config file, and writes back whatever a first
/// connection observed after the user has confirmed the fingerprint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TofuStore {
    pins: HashMap<String, String>,
}

impl TofuStore {
    pub fn new() -> TofuStore {
        TofuStore::default()
    }

    /// The pinned fingerprint for `url`, if this store has seen it before.
    pub fn get(&self, url: &str) -> Option<&str> {
        self.pins.get(url).map(String::as_str)
    }

    pub fn insert(&mut self, url: impl Into<String>, fingerprint: impl Into<String>) {
        self.pins.insert(url.into(), fingerprint.into());
    }
}

/// A `mirai-server` reached over MRP, behaving exactly like a local engine.
pub struct RemoteEngine {
    peer: Arc<Peer>,
    desc: EngineDesc,
    fingerprint: String,
    /// The server's own identification from `Welcome`, e.g. `mirai-server/0.1.0`. Kept
    /// because a client shows it next to the engine name.
    server: String,
    next_sub: AtomicU32,
    cmd: mpsc::UnboundedSender<Cmd>,
    status: watch::Receiver<RemoteStatus>,
    /// Dropped with the handle; that is what tells the background task to stop.
    _shutdown: oneshot::Sender<()>,
}

impl std::fmt::Debug for RemoteEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEngine")
            .field("engine", &self.desc.name)
            .field("fingerprint", &self.fingerprint)
            .field("status", &*self.status.borrow())
            .finish()
    }
}

impl RemoteEngine {
    /// Connects to `url` (`mirai://host:port`), authenticates, and picks an engine.
    ///
    /// `pin` is the fingerprint the user has already accepted. A mismatch is a hard
    /// failure. There is no unpinned mode: call [`probe_fingerprint`] and ask the user
    /// before this, or the token would be sent to whoever answered.
    pub async fn connect(
        url: &str,
        token: &str,
        engine: Option<String>,
        pin: String,
    ) -> Result<RemoteEngine, EngineError> {
        let session = handshake(url, token, pin).await?;
        let desc = pick_engine(&session.engines, engine.as_deref())?;
        let fingerprint = session.fingerprint.clone();
        let server = session.server.clone();

        tracing::info!(
            url,
            server = %session.server,
            engine = %desc.name,
            fingerprint = %fingerprint,
            "connected to remote engine"
        );

        // Reconnects pin the fingerprint we already accepted, and always name the engine we
        // resolved, so a server that gains engines later cannot silently switch us to a
        // different one.
        let peer = Arc::new(Peer {
            url: url.to_string(),
            token: token.to_string(),
            engine: Some(desc.name.clone()),
            pin: fingerprint.clone(),
        });

        let (status_tx, status_rx) = watch::channel(RemoteStatus::Connected);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        tokio::spawn(run(peer.clone(), session, cmd_rx, shutdown_rx, status_tx));

        Ok(RemoteEngine {
            peer,
            desc,
            fingerprint,
            server,
            next_sub: AtomicU32::new(1),
            cmd: cmd_tx,
            status: status_rx,
            _shutdown: shutdown_tx,
        })
    }

    /// TLS handshake only. Returns the leaf fingerprint and closes with application
    /// code 0. No stream is opened and the token is not sent.
    pub async fn probe_fingerprint(url: &str) -> Result<String, EngineError> {
        transport::probe(url)
            .await
            .map_err(|e| EngineError::Disconnected(e.to_string()))
    }

    /// The server certificate's SHA-256, lowercase hex — the value to pin.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// What the server called itself in `Welcome`, e.g. `mirai-server/0.1.0`.
    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn connected(&self) -> bool {
        *self.status.borrow() == RemoteStatus::Connected
    }

    pub fn subscribe_status(&self) -> watch::Receiver<RemoteStatus> {
        self.status.clone()
    }
}

impl Engine for RemoteEngine {
    fn subscribe(&self, req: AnalyzeReq) -> Subscription {
        // Requests made while the link is down fail immediately rather than queueing: the
        // GUI re-requests when the banner clears, and a stale queued query would only waste
        // the server's search threads.
        match self.status.borrow().clone() {
            RemoteStatus::Connected => {}
            RemoteStatus::Reconnecting { attempt } => {
                return Subscription::failed(EngineError::Disconnected(format!(
                    "reconnecting to {} (attempt {attempt})",
                    self.peer.url
                )));
            }
            RemoteStatus::Failed(why) => {
                return Subscription::failed(EngineError::Disconnected(why));
            }
        }

        let sub = self.next_sub.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(SubEvent::Pending);
        if self
            .cmd
            .send(Cmd::Open {
                sub,
                tx,
                req: Box::new(req),
            })
            .is_err()
        {
            return Subscription::failed(EngineError::Disconnected(
                "the remote engine connection task has stopped".into(),
            ));
        }

        let cmd = self.cmd.clone();
        Subscription::new(
            rx,
            CancelGuard::new(move || {
                let _ = cmd.send(Cmd::Cancel { sub });
            }),
        )
    }

    fn describe(&self) -> EngineDesc {
        self.desc.clone()
    }
}

// ---------------------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------------------

/// Everything needed to re-establish the session after a drop.
#[derive(Debug)]
struct Peer {
    url: String,
    token: String,
    engine: Option<String>,
    pin: String,
}

/// A connected, authenticated session: the control stream is live and `Welcome` is in.
struct Session {
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    buf: FrameBuf,
    fingerprint: String,
    engines: Vec<EngineDesc>,
    server: String,
}

/// Connects with `pin`, sends `Hello`, and waits for `Welcome`.
///
/// Errors are classified by variant, and the classification is load-bearing:
/// [`EngineError::Protocol`] is something reconnecting will not fix (bad token, bad version,
/// fingerprint mismatch), [`EngineError::Disconnected`] is worth retrying. A pin mismatch
/// fails inside the TLS handshake, before `Hello`.
async fn handshake(url: &str, token: &str, pin: String) -> Result<Session, EngineError> {
    let (conn, fingerprint) = match transport::connect(url, pin).await {
        Ok(v) => v,
        Err(e @ TransportError::FingerprintMismatch { .. }) => {
            return Err(EngineError::Protocol(e.to_string()));
        }
        Err(e) => return Err(EngineError::Disconnected(e.to_string())),
    };

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| EngineError::Disconnected(format!("opening the control stream: {e}")))?;
    let mut buf = FrameBuf::new();

    let hello = ClientMsg::Hello {
        proto: PROTO_VERSION,
        token: token.to_string(),
        client: CLIENT_NAME.to_string(),
    };
    frame::write_msg(&mut send, &mut buf, &hello)
        .await
        .map_err(|e| EngineError::Disconnected(format!("sending hello: {e}")))?;

    loop {
        let msg: ServerMsg = frame::read_msg(&mut recv, &mut buf)
            .await
            .map_err(|e| EngineError::Disconnected(format!("waiting for welcome: {e}")))?;
        match msg {
            ServerMsg::Welcome {
                proto,
                server,
                engines,
                ..
            } => {
                if proto != PROTO_VERSION {
                    return Err(EngineError::Protocol(format!(
                        "the server speaks {proto}, this client speaks {PROTO_VERSION}"
                    )));
                }
                return Ok(Session {
                    conn,
                    send,
                    recv,
                    buf,
                    fingerprint,
                    engines,
                    server,
                });
            }
            // `code` renders as e.g. `unauthorized`, so the reason survives into the toast.
            ServerMsg::Error { code, msg, .. } => {
                return Err(EngineError::Protocol(format!("{code}: {msg}")));
            }
            other => tracing::debug!(?other, "ignoring message received before welcome"),
        }
    }
}

fn pick_engine(engines: &[EngineDesc], want: Option<&str>) -> Result<EngineDesc, EngineError> {
    match want {
        Some(name) => engines
            .iter()
            .find(|e| e.name == name)
            .cloned()
            .ok_or_else(|| {
                EngineError::Protocol(format!(
                    "the server has no engine named {name:?}; it offers {}",
                    engine_names(engines)
                ))
            }),
        None => engines
            .first()
            .cloned()
            .ok_or_else(|| EngineError::Protocol("the server has no engines running".into())),
    }
}

fn engine_names(engines: &[EngineDesc]) -> String {
    if engines.is_empty() {
        return "none".into();
    }
    engines
        .iter()
        .map(|e| e.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------------------
// Connection task
// ---------------------------------------------------------------------------------------

/// Work handed to the connection task.
enum Cmd {
    Open {
        sub: u32,
        tx: watch::Sender<SubEvent>,
        /// Boxed: `AnalyzeReq` carries the whole move list and dwarfs the other variant.
        req: Box<AnalyzeReq>,
    },
    Cancel {
        sub: u32,
    },
}

/// Work reported *to* the connection task by its per-connection reader tasks.
enum Int {
    Server(ServerMsg),
    /// A subscription stream showed up; `stop` fires `stop_sending` on it.
    Stream {
        sub: u32,
        stop: oneshot::Sender<()>,
    },
    Sub {
        sub: u32,
        msg: SubMsg,
    },
    /// A subscription stream ended without a terminal message.
    SubEnd {
        sub: u32,
    },
    Lost(String),
}

struct SubState {
    tx: watch::Sender<SubEvent>,
    stop: Option<oneshot::Sender<()>>,
    /// The requested board: every point a report names must lie on it.
    size: Size,
}

enum Outcome {
    /// The engine handle was dropped.
    Shutdown,
    Lost(String),
}

async fn run(
    peer: Arc<Peer>,
    first: Session,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    mut shutdown: oneshot::Receiver<()>,
    status: watch::Sender<RemoteStatus>,
) {
    let mut subs: HashMap<u32, SubState> = HashMap::new();
    let mut session = Some(first);

    loop {
        let s = match session.take() {
            Some(s) => s,
            None => match reconnect(&peer, &status, &mut cmds, &mut shutdown).await {
                Some(s) => s,
                None => break,
            },
        };

        match serve(s, &peer, &mut subs, &mut cmds, &mut shutdown).await {
            Outcome::Shutdown => break,
            Outcome::Lost(why) => {
                tracing::warn!(url = %peer.url, why, "remote engine connection lost");
                fail_all(&mut subs, &why);
            }
        }
    }

    fail_all(&mut subs, "the remote engine was closed");
}

/// Fails every live subscription and forgets them. Never replayed on reconnect.
fn fail_all(subs: &mut HashMap<u32, SubState>, why: &str) {
    for (_, st) in subs.drain() {
        st.tx
            .send_replace(SubEvent::Failed(EngineError::Disconnected(why.to_string())));
    }
}

/// Runs one connection until it breaks or the handle is dropped.
async fn serve(
    session: Session,
    peer: &Peer,
    subs: &mut HashMap<u32, SubState>,
    cmds: &mut mpsc::UnboundedReceiver<Cmd>,
    shutdown: &mut oneshot::Receiver<()>,
) -> Outcome {
    let Session {
        conn,
        mut send,
        recv,
        mut buf,
        ..
    } = session;

    let (int_tx, mut int_rx) = mpsc::unbounded_channel();
    let mut tasks = JoinSet::new();
    tasks.spawn(control_reader(recv, int_tx.clone()));
    tasks.spawn(uni_acceptor(conn.clone(), int_tx.clone()));
    // Only the tasks hold senders now, so `int_rx` closing means both have stopped.
    drop(int_tx);

    let outcome = loop {
        tokio::select! {
            _ = &mut *shutdown => break Outcome::Shutdown,

            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break Outcome::Shutdown };
                if let Err(why) = handle_cmd(cmd, peer, subs, &mut send, &mut buf).await {
                    break Outcome::Lost(why);
                }
            }

            int = int_rx.recv() => {
                let Some(int) = int else {
                    break Outcome::Lost("the connection readers stopped".into());
                };
                match handle_int(int, subs) {
                    Routed::Carry => {}
                    // The same path as a cancel from the consumer (INV-3), so the server
                    // stops the search and frees its slot instead of running on unseen.
                    Routed::Reject(sub) => {
                        if let Err(why) =
                            handle_cmd(Cmd::Cancel { sub }, peer, subs, &mut send, &mut buf).await
                        {
                            break Outcome::Lost(why);
                        }
                    }
                    Routed::Lost(why) => break Outcome::Lost(why),
                }
            }
        }
    };

    tasks.abort_all();
    conn.close(VarInt::from_u32(BYE_CODE), b"bye");
    outcome
}

/// Applies one command. `Err` means the control stream died.
async fn handle_cmd(
    cmd: Cmd,
    peer: &Peer,
    subs: &mut HashMap<u32, SubState>,
    send: &mut SendStream,
    buf: &mut FrameBuf,
) -> Result<(), String> {
    match cmd {
        Cmd::Open { sub, tx, req } => {
            subs.insert(
                sub,
                SubState {
                    tx,
                    stop: None,
                    size: req.size,
                },
            );
            let msg = ClientMsg::Open {
                sub,
                engine: peer.engine.clone(),
                req: *req,
            };
            write_ctrl(send, buf, &msg).await?;
        }
        Cmd::Cancel { sub } => {
            // Unknown id: already terminal, nothing to cancel.
            let Some(st) = subs.remove(&sub) else {
                return Ok(());
            };
            // `stop_sending` first: reports already buffered in the QUIC receive window are
            // dropped by the stack instead of being decoded and delivered.
            if let Some(stop) = st.stop {
                let _ = stop.send(());
            }
            write_ctrl(send, buf, &ClientMsg::Cancel { sub }).await?;
        }
    }
    Ok(())
}

async fn write_ctrl(
    send: &mut SendStream,
    buf: &mut FrameBuf,
    msg: &ClientMsg,
) -> Result<(), String> {
    frame::write_msg(send, buf, msg)
        .await
        .map_err(|e| format!("control stream: {e}"))
}

/// What the connection loop does after [`handle_int`].
enum Routed {
    Carry,
    /// The subscription has been failed; cancel it as the consumer would.
    Reject(u32),
    /// The connection is gone.
    Lost(String),
}

/// Routes one reader-task event.
fn handle_int(int: Int, subs: &mut HashMap<u32, SubState>) -> Routed {
    match int {
        Int::Server(msg) => match msg {
            ServerMsg::Error {
                sub: Some(sub),
                code,
                msg,
            } => {
                if let Some(st) = subs.remove(&sub) {
                    st.tx
                        .send_replace(SubEvent::Failed(EngineError::Query(format!(
                            "{code}: {msg}"
                        ))));
                }
            }
            ServerMsg::Error {
                sub: None,
                code,
                msg,
            } => tracing::warn!(%code, msg, "remote engine reported an error"),
            other => tracing::trace!(?other, "control message"),
        },

        Int::Stream { sub, stop } => match subs.get_mut(&sub) {
            Some(st) => st.stop = Some(stop),
            // Cancelled before its stream arrived — shut it down on sight.
            None => {
                let _ = stop.send(());
            }
        },

        Int::Sub { sub, msg } => {
            let unfit = match (&msg, subs.get(&sub)) {
                (SubMsg::Report(r) | SubMsg::Done(r), Some(st)) => misfit(r, st.size),
                _ => None,
            };
            if let Some(why) = unfit {
                // Consumers label and index by these points and arrays, so a report that
                // does not fit the board is never delivered: the subscription fails, then is
                // cancelled (§8.4). `Cancel` is idempotent, so a `Done` is covered as well.
                if let Some(st) = subs.get(&sub) {
                    st.tx
                        .send_replace(SubEvent::Failed(EngineError::Protocol(format!(
                            "the server sent a report that {why}"
                        ))));
                }
                return Routed::Reject(sub);
            }
            match msg {
                SubMsg::Report(r) => {
                    if let Some(st) = subs.get(&sub) {
                        st.tx.send_replace(SubEvent::Report(Arc::new(r)));
                    }
                }
                SubMsg::Done(r) => {
                    if let Some(st) = subs.remove(&sub) {
                        st.tx.send_replace(SubEvent::Done(Arc::new(r)));
                    }
                }
                SubMsg::Failed(why) => {
                    if let Some(st) = subs.remove(&sub) {
                        st.tx
                            .send_replace(SubEvent::Failed(EngineError::Query(why)));
                    }
                }
            }
        }

        Int::SubEnd { sub } => {
            if let Some(st) = subs.remove(&sub) {
                st.tx
                    .send_replace(SubEvent::Failed(EngineError::Disconnected(
                        "the subscription stream ended without a result".into(),
                    )));
            }
        }

        Int::Lost(why) => return Routed::Lost(why),
    }
    Routed::Carry
}

/// Why `report` cannot describe a `size` board (PROTOCOL.md §7.1), if it cannot.
fn misfit(report: &Report, size: Size) -> Option<String> {
    let points = size.points();
    if let Some(p) = report
        .moves
        .iter()
        .flat_map(|m| std::iter::once(&m.mv).chain(&m.pv))
        .find(|p| !p.is_pass() && !size.contains(**p))
    {
        return Some(format!(
            "names point {} on a {}x{} board",
            p.0, size.w, size.h
        ));
    }
    if let Some(own) = &report.ownership
        && own.len() != points
    {
        return Some(format!(
            "has {} ownership entries for {points} points",
            own.len()
        ));
    }
    if let Some(policy) = &report.policy
        && policy.len() != points + 1
    {
        return Some(format!(
            "has {} policy entries for {points} points and pass",
            policy.len()
        ));
    }
    None
}

/// Reads `ServerMsg`s off the control stream for as long as it lives.
async fn control_reader(mut recv: RecvStream, int: mpsc::UnboundedSender<Int>) {
    let mut buf = FrameBuf::new();
    loop {
        match frame::read_msg::<_, ServerMsg>(&mut recv, &mut buf).await {
            Ok(msg) => {
                if int.send(Int::Server(msg)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = int.send(Int::Lost(format!("control stream: {e}")));
                return;
            }
        }
    }
}

/// Accepts server-opened subscription streams and reads each one in its own task.
///
/// The child tasks live in a local [`JoinSet`], so aborting this task tears them all down
/// with it when the connection is replaced.
async fn uni_acceptor(conn: Connection, int: mpsc::UnboundedSender<Int>) {
    let mut readers = JoinSet::new();
    loop {
        tokio::select! {
            accepted = conn.accept_uni() => match accepted {
                Ok(recv) => { readers.spawn(sub_reader(recv, int.clone())); }
                Err(e) => {
                    let _ = int.send(Int::Lost(e.to_string()));
                    return;
                }
            },
            // Reap finished readers so the set does not grow over a long session.
            _ = readers.join_next(), if !readers.is_empty() => {}
        }
    }
}

/// Reads the 4-byte LE subscription preamble, then framed [`SubMsg`]s.
async fn sub_reader(mut recv: RecvStream, int: mpsc::UnboundedSender<Int>) {
    let mut preamble = [0u8; 4];
    if let Err(e) = recv.read_exact(&mut preamble).await {
        tracing::debug!(error = %e, "subscription stream without a usable preamble");
        return;
    }
    let sub = u32::from_le_bytes(preamble);

    let (stop_tx, mut stop_rx) = oneshot::channel();
    if int.send(Int::Stream { sub, stop: stop_tx }).is_err() {
        return;
    }

    let mut dec = match SubStreamDecoder::new() {
        Ok(dec) => dec,
        Err(e) => {
            tracing::debug!(sub, error = %e, "could not start the stream's decompressor");
            let _ = int.send(Int::SubEnd { sub });
            return;
        }
    };
    let mut own = OwnershipDelta::default();
    loop {
        // `read` is not cancel-safe, which is fine: the only thing that cancels it is a
        // cancellation, after which the stream is abandoned anyway.
        let stopped = tokio::select! {
            _ = &mut stop_rx => true,
            read = dec.read::<_, SubMsg>(&mut recv) => match read {
                Ok(mut msg) => {
                    own.restore(&mut msg);
                    if int.send(Int::Sub { sub, msg }).is_err() {
                        return;
                    }
                    false
                }
                Err(FrameError::Eof) => {
                    let _ = int.send(Int::SubEnd { sub });
                    return;
                }
                Err(e) => {
                    tracing::debug!(sub, error = %e, "subscription stream failed");
                    let _ = int.send(Int::SubEnd { sub });
                    return;
                }
            },
        };
        if stopped {
            let _ = recv.stop(VarInt::from_u32(CANCEL_CODE));
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------
// Reconnection
// ---------------------------------------------------------------------------------------

/// 0.5 s, 1 s, 2 s, 4 s, then 8 s forever.
fn backoff(attempt: u32) -> Duration {
    let steps = attempt.saturating_sub(1).min(4);
    Duration::from_millis((500u64 << steps).min(MAX_BACKOFF_MS))
}

/// Retries the handshake until it succeeds. `None` means the handle was dropped.
async fn reconnect(
    peer: &Peer,
    status: &watch::Sender<RemoteStatus>,
    cmds: &mut mpsc::UnboundedReceiver<Cmd>,
    shutdown: &mut oneshot::Receiver<()>,
) -> Option<Session> {
    let mut attempt: u32 = 1;
    loop {
        // A fatal diagnosis outranks the attempt counter; do not paper over it.
        if !matches!(*status.borrow(), RemoteStatus::Failed(_)) {
            status.send_replace(RemoteStatus::Reconnecting { attempt });
        }

        if !idle(backoff(attempt), cmds, shutdown).await {
            return None;
        }

        let attempting = handshake(&peer.url, &peer.token, peer.pin.clone());
        let result = tokio::select! {
            _ = &mut *shutdown => return None,
            r = attempting => r,
        };

        match result {
            Ok(session) => {
                if let Some(name) = &peer.engine
                    && !session.engines.iter().any(|e| &e.name == name)
                {
                    tracing::warn!(engine = %name, "the server no longer offers this engine");
                }
                tracing::info!(url = %peer.url, attempt, "reconnected to remote engine");
                status.send_replace(RemoteStatus::Connected);
                return Some(session);
            }
            Err(err) => {
                tracing::warn!(url = %peer.url, attempt, error = %err, "reconnect failed");
                if matches!(err, EngineError::Protocol(_)) {
                    status.send_replace(RemoteStatus::Failed(err.to_string()));
                }
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Waits out the backoff while still answering commands. `false` means the handle was
/// dropped.
async fn idle(
    delay: Duration,
    cmds: &mut mpsc::UnboundedReceiver<Cmd>,
    shutdown: &mut oneshot::Receiver<()>,
) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        tokio::select! {
            _ = &mut *shutdown => return false,
            _ = tokio::time::sleep_until(deadline) => return true,
            cmd = cmds.recv() => match cmd {
                // Racing `subscribe`: the status said connected a moment ago. Fail it now
                // rather than letting it hang until the link comes back.
                Some(Cmd::Open { tx, .. }) => {
                    tx.send_replace(SubEvent::Failed(EngineError::Disconnected(
                        "not connected to the remote engine".into(),
                    )));
                }
                Some(Cmd::Cancel { .. }) => {}
                None => return false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tofu_store_round_trips_pins() {
        let mut store = TofuStore::new();
        assert_eq!(store.get("mirai://box:9678"), None);

        store.insert("mirai://box:9678", "a".repeat(64));
        assert_eq!(store.get("mirai://box:9678"), Some("a".repeat(64).as_str()));
        // Distinct servers must not share a pin.
        assert_eq!(store.get("mirai://other:9678"), None);

        // A re-pin (the user accepted a new certificate) replaces the old value.
        store.insert("mirai://box:9678", "b".repeat(64));
        assert_eq!(store.get("mirai://box:9678"), Some("b".repeat(64).as_str()));
    }

    #[test]
    fn backoff_is_capped_at_eight_seconds() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(2));
        assert_eq!(backoff(4), Duration::from_secs(4));
        assert_eq!(backoff(5), Duration::from_secs(8));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(8));
    }

    #[test]
    fn engine_selection_reports_what_is_available() {
        let engines = vec![
            EngineDesc::placeholder("default"),
            EngineDesc::placeholder("big"),
        ];
        assert_eq!(pick_engine(&engines, None).unwrap().name, "default");
        assert_eq!(pick_engine(&engines, Some("big")).unwrap().name, "big");

        let err = pick_engine(&engines, Some("nope")).unwrap_err();
        assert!(matches!(err, EngineError::Protocol(_)), "{err}");
        let msg = err.to_string();
        assert!(
            msg.contains("nope") && msg.contains("default, big"),
            "{msg}"
        );

        let err = pick_engine(&[], None).unwrap_err();
        assert!(matches!(err, EngineError::Protocol(_)), "{err}");
    }

    /// An unresolvable host fails before any packet is sent, so this is fast and needs no
    /// server: what it defends is that `connect` reports the failure as an `EngineError`
    /// instead of panicking or hanging.
    #[tokio::test]
    async fn connect_reports_an_unusable_address() {
        let err = RemoteEngine::connect(
            "mirai://no.such.host.invalid:9678",
            "tok",
            None,
            "a".repeat(64),
        )
        .await
        .expect_err("connecting to an unresolvable host must fail");
        assert!(matches!(err, EngineError::Disconnected(_)), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("no.such.host.invalid"), "{msg}");
    }

    /// Nothing listens on UDP port 1. QUIC has no instant "connection refused", so this is
    /// bounded by a timeout: within it, `connect` must either report a connection error or
    /// still be waiting — never panic and never return `Ok`.
    #[tokio::test]
    async fn connect_to_a_dead_port_never_succeeds() {
        let attempt = RemoteEngine::connect("mirai://127.0.0.1:1", "tok", None, "a".repeat(64));
        match tokio::time::timeout(Duration::from_secs(2), attempt).await {
            Ok(Ok(_)) => panic!("connected to a port with no server on it"),
            Ok(Err(err)) => {
                assert!(matches!(err, EngineError::Disconnected(_)), "{err}");
                let msg = err.to_string();
                assert!(
                    msg.contains("connection error") || msg.contains("i/o error"),
                    "{msg}"
                );
            }
            Err(_elapsed) => {} // still handshaking; the QUIC idle timeout will end it
        }
    }
}
