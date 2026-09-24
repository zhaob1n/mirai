// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Connecting to a `mirai-server`, and the trust decision in front of it.
//!
//! [`mirai_engine::RemoteEngine`] already owns the MRP/2 session: handshake, reconnection,
//! subscription lifecycle. What it deliberately does not own is *policy* — it will happily
//! connect with no certificate pin and hand back whatever fingerprint it saw, because only
//! the application knows whether a human has looked at that fingerprint and agreed to it.
//!
//! [`Session`] is that policy. On a first connection to an unpinned server it reaches
//! [`SessionState::AwaitingTrust`] and **refuses to dispatch analysis** until [`Session::trust`]
//! is called. That is the whole trust-on-first-use flow, expressed once for every frontend:
//! a GUI shows the fingerprint in a dialog, a phone shows it in a sheet, and neither can
//! forget to gate on it, because the engine is not reachable until they do.
//!
//! There are no locks here. The state lives in one task; everything else is a channel.

use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;

use mirai_engine::remote::RemoteStatus;
use mirai_engine::{AnalyzeReq, Engine, EngineDesc, EngineError, RemoteEngine, Subscription};
use tokio::sync::{mpsc, watch};

/// Where to connect and what to trust.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionConfig {
    /// `mirai://host:port`.
    pub url: String,
    pub token: String,
    /// Engine name, or `None` for the server's first.
    pub engine: Option<String>,
    /// The fingerprint already agreed for this server. `None` means this is a first
    /// connection and the user must be asked.
    pub pin: Option<String>,
}

/// Who we are talking to, once the transport says hello.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// The server certificate's SHA-256, lowercase hex — the value to pin.
    pub fingerprint: String,
    /// The server's self-identification, e.g. `mirai-server/0.1.0`.
    pub server: String,
    /// The engine this session resolved to.
    pub engine: String,
}

/// What a frontend renders, and the only thing it needs to know about the link.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SessionState {
    #[default]
    Offline,
    Connecting,
    /// Connected, but this server's certificate has never been agreed to. No analysis will
    /// be dispatched until [`Session::trust`] is called or the session is dropped.
    AwaitingTrust(Peer),
    Connected(Peer),
    /// The link dropped and is being retried with capped exponential backoff.
    Reconnecting {
        attempt: u32,
    },
    /// An error retrying cannot fix: a rejected token, a protocol mismatch, or a
    /// certificate that no longer matches the pin.
    Failed(String),
}

impl SessionState {
    /// The peer, if the link is up — trusted or not.
    pub fn peer(&self) -> Option<&Peer> {
        match self {
            SessionState::AwaitingTrust(p) | SessionState::Connected(p) => Some(p),
            _ => None,
        }
    }

    /// True only when analysis may be dispatched.
    pub fn is_usable(&self) -> bool {
        matches!(self, SessionState::Connected(_))
    }
}

/// A live link, as handed back by a [`Connector`].
pub struct Link {
    pub engine: Arc<dyn Engine>,
    pub peer: Peer,
    /// The transport's own view, which the session folds into [`SessionState`].
    pub status: watch::Receiver<RemoteStatus>,
}

/// How a session gets a link. The real implementation is [`RemoteConnector`]; tests
/// substitute their own, which is why the trust rules below are checkable without a server.
pub trait Connector: Send + Sync + 'static {
    fn connect(
        &self,
        config: SessionConfig,
    ) -> Pin<Box<dyn Future<Output = Result<Link, EngineError>> + Send>>;
}

/// Connects with [`RemoteEngine`] over MRP/2.
pub struct RemoteConnector;

impl Connector for RemoteConnector {
    fn connect(
        &self,
        config: SessionConfig,
    ) -> Pin<Box<dyn Future<Output = Result<Link, EngineError>> + Send>> {
        Box::pin(async move {
            let engine = RemoteEngine::connect(
                &config.url,
                &config.token,
                config.engine.clone(),
                config.pin.clone(),
            )
            .await?;
            let peer = Peer {
                fingerprint: engine.fingerprint().to_string(),
                server: engine.server().to_string(),
                engine: engine.describe().name,
            };
            let status = engine.subscribe_status();
            Ok(Link {
                engine: Arc::new(engine),
                peer,
                status,
            })
        })
    }
}

enum Cmd {
    Connect(SessionConfig),
    Trust,
    Disconnect,
}

/// The connection, its trust decision, and the engine behind both.
pub struct Session {
    cmd: mpsc::UnboundedSender<Cmd>,
    state: watch::Receiver<SessionState>,
    /// `Some` only while the link is up *and* trusted. Reading it is how dispatch is gated.
    engine: watch::Receiver<Option<Arc<dyn Engine>>>,
}

impl Session {
    /// Spawns the session task. Nothing connects until [`Session::connect`] is called.
    pub fn new(connector: impl Connector) -> Session {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = watch::channel(SessionState::Offline);
        let (engine_tx, engine_rx) = watch::channel(None);
        tokio::spawn(run(Box::new(connector), cmd_rx, state_tx, engine_tx));
        Session {
            cmd: cmd_tx,
            state: state_rx,
            engine: engine_rx,
        }
    }

    /// Connects, or reconnects with different settings. Replaces any current link.
    pub fn connect(&self, config: SessionConfig) {
        let _ = self.cmd.send(Cmd::Connect(config));
    }

    /// Accepts the fingerprint currently shown in [`SessionState::AwaitingTrust`].
    ///
    /// Callers persist [`Peer::fingerprint`] themselves — this crate does not touch files —
    /// and pass it back as [`SessionConfig::pin`] next time.
    pub fn trust(&self) {
        let _ = self.cmd.send(Cmd::Trust);
    }

    pub fn disconnect(&self) {
        let _ = self.cmd.send(Cmd::Disconnect);
    }

    pub fn state(&self) -> SessionState {
        self.state.borrow().clone()
    }

    /// For a frontend to await changes on.
    pub fn watch(&self) -> watch::Receiver<SessionState> {
        self.state.clone()
    }

    /// Starts an analysis, or returns an already-failed subscription explaining why not.
    ///
    /// Untrusted is a refusal, not an error to retry: nothing is sent to a server whose
    /// certificate the user has not accepted.
    pub fn subscribe(&self, req: AnalyzeReq) -> Subscription {
        match self.engine.borrow().clone() {
            Some(engine) => engine.subscribe(req),
            None => Subscription::failed(match self.state() {
                SessionState::AwaitingTrust(peer) => EngineError::Disconnected(format!(
                    "waiting for you to accept the certificate {}",
                    short(&peer.fingerprint)
                )),
                SessionState::Failed(why) => EngineError::Disconnected(why),
                SessionState::Reconnecting { attempt } => {
                    EngineError::Disconnected(format!("reconnecting (attempt {attempt})"))
                }
                _ => EngineError::Disconnected("not connected".into()),
            }),
        }
    }
}

/// A session is an analysis source, so anything that takes an [`Engine`] — the whole-game
/// sweep, play mode — can be handed one directly and inherits the trust gate for free.
impl Engine for Session {
    fn subscribe(&self, req: AnalyzeReq) -> Subscription {
        Session::subscribe(self, req)
    }

    /// The resolved engine while the link is up and trusted, otherwise a nameless
    /// placeholder: callers use this for display, never to decide whether to dispatch.
    fn describe(&self) -> EngineDesc {
        match self.engine.borrow().as_ref() {
            Some(engine) => engine.describe(),
            None => EngineDesc::placeholder(""),
        }
    }
}

/// First and last four hex characters, which is what a user compares.
fn short(fingerprint: &str) -> String {
    if fingerprint.len() <= 12 {
        return fingerprint.to_string();
    }
    format!(
        "{}…{}",
        &fingerprint[..8],
        &fingerprint[fingerprint.len() - 8..]
    )
}

/// Folds the transport's status into the session's, preserving the trust decision.
fn fold(status: &RemoteStatus, peer: &Peer, trusted: bool) -> SessionState {
    match status {
        RemoteStatus::Connected if trusted => SessionState::Connected(peer.clone()),
        RemoteStatus::Connected => SessionState::AwaitingTrust(peer.clone()),
        RemoteStatus::Reconnecting { attempt } => SessionState::Reconnecting { attempt: *attempt },
        RemoteStatus::Failed(why) => SessionState::Failed(why.clone()),
    }
}

async fn run(
    connector: Box<dyn Connector>,
    mut cmd: mpsc::UnboundedReceiver<Cmd>,
    state: watch::Sender<SessionState>,
    engine: watch::Sender<Option<Arc<dyn Engine>>>,
) {
    let mut link: Option<Link> = None;
    let mut trusted = false;

    loop {
        // Only wait on a status change when there is a link to watch; otherwise this task
        // must not spin.
        let changed = async {
            match link.as_mut() {
                Some(l) => l.status.changed().await.is_ok(),
                None => pending().await,
            }
        };

        tokio::select! {
            command = cmd.recv() => match command {
                None => return,
                Some(Cmd::Connect(config)) => {
                    // Dropping the old link is what closes the old connection.
                    link = None;
                    trusted = false;
                    let _ = engine.send(None);
                    let _ = state.send(SessionState::Connecting);

                    match connector.connect(config.clone()).await {
                        Ok(fresh) => {
                            // A configured pin means the user already agreed to this
                            // certificate; the transport enforced it during the handshake.
                            trusted = config.pin.is_some();
                            if trusted {
                                let _ = engine.send(Some(fresh.engine.clone()));
                            }
                            let _ = state.send(fold(&fresh.status.borrow(), &fresh.peer, trusted));
                            link = Some(fresh);
                        }
                        Err(e) => {
                            let _ = state.send(SessionState::Failed(e.to_string()));
                        }
                    }
                }
                Some(Cmd::Trust) => {
                    if let Some(l) = link.as_ref()
                        && !trusted
                    {
                        trusted = true;
                        let _ = engine.send(Some(l.engine.clone()));
                        let _ = state.send(fold(&l.status.borrow(), &l.peer, true));
                    }
                }
                Some(Cmd::Disconnect) => {
                    link = None;
                    trusted = false;
                    let _ = engine.send(None);
                    let _ = state.send(SessionState::Offline);
                }
            },
            alive = changed => {
                let Some(l) = link.as_ref() else { continue };
                if !alive {
                    // The engine handle went away without us dropping it.
                    let _ = engine.send(None);
                    let _ = state.send(SessionState::Offline);
                    link = None;
                    trusted = false;
                    continue;
                }
                let next = fold(&l.status.borrow(), &l.peer, trusted);
                // A dropped link must not keep dispatching: withdraw the engine while the
                // transport is retrying, so requests fail fast instead of queueing.
                match &next {
                    SessionState::Connected(_) if trusted => {
                        let _ = engine.send(Some(l.engine.clone()));
                    }
                    SessionState::Reconnecting { .. } | SessionState::Failed(_) => {
                        let _ = engine.send(None);
                    }
                    _ => {}
                }
                let _ = state.send(next);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use mirai_core::{RuleSet, Size};
    use mirai_engine::SubEvent;

    use super::*;

    /// An engine that records what it was asked and answers nothing.
    struct Recorder {
        asked: mpsc::UnboundedSender<AnalyzeReq>,
    }

    impl Engine for Recorder {
        fn subscribe(&self, req: AnalyzeReq) -> Subscription {
            let _ = self.asked.send(req);
            let (tx, rx) = watch::channel(SubEvent::Pending);
            Subscription::new(rx, mirai_engine::CancelGuard::new(move || drop(tx)))
        }

        fn describe(&self) -> EngineDesc {
            EngineDesc::placeholder("fake")
        }
    }

    struct Fake {
        engine: Arc<Recorder>,
        status: watch::Sender<RemoteStatus>,
        /// Set to fail every connection attempt.
        refuse: AtomicBool,
    }

    impl Connector for Arc<Fake> {
        fn connect(
            &self,
            _config: SessionConfig,
        ) -> Pin<Box<dyn Future<Output = Result<Link, EngineError>> + Send>> {
            let this = self.clone();
            Box::pin(async move {
                if this.refuse.load(Ordering::Relaxed) {
                    return Err(EngineError::Disconnected("unauthorized".into()));
                }
                Ok(Link {
                    engine: this.engine.clone(),
                    peer: Peer {
                        fingerprint: "a".repeat(64),
                        server: "mirai-test/1".into(),
                        engine: "fake".into(),
                    },
                    status: this.status.subscribe(),
                })
            })
        }
    }

    /// The fake backend plus the receiving end of what its engine was asked.
    fn fake() -> (Arc<Fake>, mpsc::UnboundedReceiver<AnalyzeReq>) {
        let (status, _) = watch::channel(RemoteStatus::Connected);
        let (asked, seen) = mpsc::unbounded_channel();
        (
            Arc::new(Fake {
                engine: Arc::new(Recorder { asked }),
                status,
                refuse: AtomicBool::new(false),
            }),
            seen,
        )
    }

    /// How many requests reached the engine since the last call.
    fn dispatched(seen: &mut mpsc::UnboundedReceiver<AnalyzeReq>) -> usize {
        let mut n = 0;
        while seen.try_recv().is_ok() {
            n += 1;
        }
        n
    }

    fn a_request() -> AnalyzeReq {
        AnalyzeReq::new(Size::square(19), RuleSet::Chinese, 7.5)
    }

    /// Waits for the session to reach a state the predicate accepts.
    async fn until(session: &Session, what: impl Fn(&SessionState) -> bool) -> SessionState {
        let mut rx = session.watch();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let now = rx.borrow_and_update();
                if what(&now) {
                    return now.clone();
                }
            }
            tokio::time::timeout_at(deadline, rx.changed())
                .await
                .expect("session never reached the expected state")
                .expect("session task ended");
        }
    }

    #[tokio::test]
    async fn an_unpinned_connection_waits_for_trust_before_analysing() {
        let (backend, mut seen) = fake();
        let session = Session::new(backend.clone());

        session.connect(SessionConfig {
            url: "mirai://test:9678".into(),
            token: "tok".into(),
            engine: None,
            pin: None,
        });

        let state = until(&session, |s| matches!(s, SessionState::AwaitingTrust(_))).await;
        let peer = state.peer().expect("a peer to show the user");
        assert_eq!(peer.fingerprint, "a".repeat(64));
        assert!(
            !state.is_usable(),
            "an unverified server must not be usable"
        );

        // A request now is refused locally: nothing reaches the engine.
        let mut sub = session.subscribe(a_request());
        let event = sub.next().await.expect("a refusal is delivered");
        assert!(matches!(event, SubEvent::Failed(_)), "{event:?}");
        assert_eq!(
            dispatched(&mut seen),
            0,
            "a request was dispatched to an untrusted server"
        );

        session.trust();
        let state = until(&session, SessionState::is_usable).await;
        assert_eq!(state.peer().map(|p| p.engine.as_str()), Some("fake"));

        let _sub = session.subscribe(a_request());
        assert_eq!(dispatched(&mut seen), 1, "a trusted session must dispatch");
    }

    /// A pin the user agreed to earlier needs no second confirmation: the transport already
    /// refused anything else during the handshake.
    #[tokio::test]
    async fn a_pinned_connection_is_usable_immediately() {
        let (backend, mut seen) = fake();
        let session = Session::new(backend.clone());

        session.connect(SessionConfig {
            url: "mirai://test:9678".into(),
            token: "tok".into(),
            engine: None,
            pin: Some("a".repeat(64)),
        });

        until(&session, SessionState::is_usable).await;
        let _sub = session.subscribe(a_request());
        assert_eq!(dispatched(&mut seen), 1);
    }

    #[tokio::test]
    async fn a_failed_handshake_is_reported_and_dispatches_nothing() {
        let (backend, mut seen) = fake();
        backend.refuse.store(true, Ordering::Relaxed);
        let session = Session::new(backend.clone());

        session.connect(SessionConfig {
            url: "mirai://test:9678".into(),
            token: "wrong".into(),
            engine: None,
            pin: Some("a".repeat(64)),
        });

        let state = until(&session, |s| matches!(s, SessionState::Failed(_))).await;
        match state {
            SessionState::Failed(why) => assert!(why.contains("unauthorized"), "{why}"),
            other => panic!("{other:?}"),
        }
        let mut sub = session.subscribe(a_request());
        assert!(matches!(
            sub.next().await.expect("refusal"),
            SubEvent::Failed(_)
        ));
        assert_eq!(dispatched(&mut seen), 0);
    }

    /// While the transport retries, requests must fail fast rather than queue — and the
    /// trust decision must survive the outage.
    #[tokio::test]
    async fn a_reconnecting_session_withdraws_the_engine_but_keeps_the_trust() {
        let (backend, mut seen) = fake();
        let session = Session::new(backend.clone());
        session.connect(SessionConfig {
            url: "mirai://test:9678".into(),
            token: "tok".into(),
            engine: None,
            pin: Some("a".repeat(64)),
        });
        until(&session, SessionState::is_usable).await;

        backend
            .status
            .send(RemoteStatus::Reconnecting { attempt: 2 })
            .expect("status");
        until(&session, |s| {
            matches!(s, SessionState::Reconnecting { attempt: 2 })
        })
        .await;

        let mut sub = session.subscribe(a_request());
        assert!(matches!(
            sub.next().await.expect("refusal"),
            SubEvent::Failed(_)
        ));
        assert_eq!(
            dispatched(&mut seen),
            0,
            "a request was queued during an outage"
        );

        backend
            .status
            .send(RemoteStatus::Connected)
            .expect("status");
        until(&session, SessionState::is_usable).await;
        let _sub = session.subscribe(a_request());
        assert_eq!(
            dispatched(&mut seen),
            1,
            "trust had to be given again after a reconnect"
        );
    }

    #[tokio::test]
    async fn disconnecting_goes_offline_and_stops_dispatching() {
        let (backend, mut seen) = fake();
        let session = Session::new(backend.clone());
        session.connect(SessionConfig {
            url: "mirai://test:9678".into(),
            token: "tok".into(),
            engine: None,
            pin: Some("a".repeat(64)),
        });
        until(&session, SessionState::is_usable).await;

        session.disconnect();
        until(&session, |s| matches!(s, SessionState::Offline)).await;

        let mut sub = session.subscribe(a_request());
        assert!(matches!(
            sub.next().await.expect("refusal"),
            SubEvent::Failed(_)
        ));
        assert_eq!(dispatched(&mut seen), 0);
    }
}
