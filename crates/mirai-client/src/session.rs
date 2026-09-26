// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Connecting to a `mirai-server`, and the trust decision in front of the token.
//!
//! [`mirai_engine::RemoteEngine`] owns the MRP session once a pin exists: handshake,
//! reconnection, subscription lifecycle. It will not connect without that pin, because
//! the connection is where `Hello { token }` is sent.
//!
//! [`Session`] is the policy in front of that. A first connection probes — TLS only,
//! no stream, no token — and reaches [`SessionState::AwaitingTrust`] with the
//! fingerprint and no engine. [`Session::trust`] then connects with the fingerprint
//! that was shown. A click while the probe is still running, or after a newer connect
//! has replaced the one on screen, is ignored: it must not authorise a certificate
//! the user has not seen. The engine is not reachable until they do.
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
    /// Probed, not connected. No engine is published and no token has been sent.
    /// [`Session::trust`] connects with the fingerprint shown here.
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
///
/// [`Connector::connect`] is the token-bearing path and takes the pin as a `String`, so
/// a session cannot ask for `Hello` without one. [`Connector::probe`] must not send it.
pub trait Connector: Send + Sync + 'static {
    fn probe(
        &self,
        config: &SessionConfig,
    ) -> Pin<Box<dyn Future<Output = Result<String, EngineError>> + Send>>;

    fn connect(
        &self,
        config: SessionConfig,
        pin: String,
    ) -> Pin<Box<dyn Future<Output = Result<Link, EngineError>> + Send>>;
}

/// Connects with [`RemoteEngine`] over MRP.
pub struct RemoteConnector;

impl Connector for RemoteConnector {
    fn probe(
        &self,
        config: &SessionConfig,
    ) -> Pin<Box<dyn Future<Output = Result<String, EngineError>> + Send>> {
        let url = config.url.clone();
        Box::pin(async move { RemoteEngine::probe_fingerprint(&url).await })
    }

    fn connect(
        &self,
        config: SessionConfig,
        pin: String,
    ) -> Pin<Box<dyn Future<Output = Result<Link, EngineError>> + Send>> {
        Box::pin(async move {
            let engine =
                RemoteEngine::connect(&config.url, &config.token, config.engine.clone(), pin)
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
    /// The fingerprint the caller displayed. Anything else is ignored.
    Trust(String),
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

    /// Accepts `fingerprint` only if it is the one currently shown in
    /// [`SessionState::AwaitingTrust`], and connects with that pin.
    ///
    /// A click while the probe is still running, or one meant for a fingerprint a
    /// newer [`Session::connect`] has already replaced, is ignored: the argument is
    /// what the user saw, and it must not authorise a different certificate. Callers
    /// persist [`Peer::fingerprint`] themselves — this crate does not touch files —
    /// and pass it back as [`SessionConfig::pin`] next time.
    pub fn trust(&self, fingerprint: impl Into<String>) {
        let _ = self.cmd.send(Cmd::Trust(fingerprint.into()));
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

/// Folds the transport's status into the session's. A link only exists after a pinned
/// connect, so `Connected` here is already trusted.
fn fold(status: &RemoteStatus, peer: &Peer) -> SessionState {
    match status {
        RemoteStatus::Connected => SessionState::Connected(peer.clone()),
        RemoteStatus::Reconnecting { attempt } => SessionState::Reconnecting { attempt: *attempt },
        RemoteStatus::Failed(why) => SessionState::Failed(why.clone()),
    }
}

/// The fingerprint on screen, and the settings to connect with once it is accepted.
struct Shown {
    config: SessionConfig,
    fingerprint: String,
}

enum Inflight {
    Probe {
        config: SessionConfig,
        fut: Pin<Box<dyn Future<Output = Result<String, EngineError>> + Send>>,
    },
    Connect {
        fut: Pin<Box<dyn Future<Output = Result<Link, EngineError>> + Send>>,
    },
}

enum Attempt {
    Probe(Result<String, EngineError>),
    Connect(Result<Link, EngineError>),
}

async fn run(
    connector: Box<dyn Connector>,
    mut cmd: mpsc::UnboundedReceiver<Cmd>,
    state: watch::Sender<SessionState>,
    engine: watch::Sender<Option<Arc<dyn Engine>>>,
) {
    let mut link: Option<Link> = None;
    let mut shown: Option<Shown> = None;
    let mut inflight: Option<Inflight> = None;

    loop {
        // Only wait on a status change when there is a link to watch; otherwise this task
        // must not spin. The probe or connect future is polled beside commands so
        // Disconnect, Trust and a newer Connect are not stuck behind DNS or QUIC.
        let changed = async {
            match link.as_mut() {
                Some(l) => l.status.changed().await.is_ok(),
                None => pending().await,
            }
        };
        let attempt_done = async {
            match inflight.as_mut() {
                Some(Inflight::Probe { fut, .. }) => Attempt::Probe(fut.as_mut().await),
                Some(Inflight::Connect { fut }) => Attempt::Connect(fut.as_mut().await),
                None => pending().await,
            }
        };

        tokio::select! {
            // A command already queued wins over a handshake that finished in the same
            // poll. Otherwise a superseded attempt becomes usable for one turn of the loop.
            biased;
            command = cmd.recv() => match command {
                None => return,
                Some(Cmd::Connect(config)) => {
                    // Dropping the old link closes it. Dropping the future cancels the
                    // probe or the handshake; its result is never observed.
                    link = None;
                    shown = None;
                    drop(inflight.take());
                    let _ = engine.send(None);
                    let _ = state.send(SessionState::Connecting);
                    inflight = Some(match config.pin.clone() {
                        Some(pin) => Inflight::Connect {
                            fut: connector.connect(config, pin),
                        },
                        None => Inflight::Probe {
                            fut: connector.probe(&config),
                            config,
                        },
                    });
                }
                Some(Cmd::Trust(fingerprint)) => {
                    // Only the fingerprint in AwaitingTrust, and only if it is the one
                    // the caller displayed. A click during the probe, or one meant for
                    // a certificate a newer Connect has already replaced, must not
                    // authorise the handshake that finishes next.
                    let Some(pending) = shown.take() else {
                        continue;
                    };
                    if pending.fingerprint != fingerprint {
                        shown = Some(pending);
                        continue;
                    }
                    let pin = pending.fingerprint;
                    let mut config = pending.config;
                    config.pin = Some(pin.clone());
                    let _ = state.send(SessionState::Connecting);
                    inflight = Some(Inflight::Connect {
                        fut: connector.connect(config, pin),
                    });
                }
                Some(Cmd::Disconnect) => {
                    inflight = None;
                    shown = None;
                    link = None;
                    let _ = engine.send(None);
                    let _ = state.send(SessionState::Offline);
                }
            },
            result = attempt_done => {
                match (inflight.take(), result) {
                    (Some(Inflight::Probe { config, .. }), Attempt::Probe(Ok(fingerprint))) => {
                        let peer = Peer {
                            fingerprint: fingerprint.clone(),
                            server: String::new(),
                            engine: config.engine.clone().unwrap_or_default(),
                        };
                        shown = Some(Shown { config, fingerprint });
                        let _ = state.send(SessionState::AwaitingTrust(peer));
                    }
                    (Some(Inflight::Probe { .. }), Attempt::Probe(Err(e))) => {
                        let _ = state.send(SessionState::Failed(e.to_string()));
                    }
                    (Some(Inflight::Connect { .. }), Attempt::Connect(Ok(fresh))) => {
                        let _ = engine.send(Some(fresh.engine.clone()));
                        let _ = state.send(fold(&fresh.status.borrow(), &fresh.peer));
                        link = Some(fresh);
                    }
                    (Some(Inflight::Connect { .. }), Attempt::Connect(Err(e))) => {
                        let _ = state.send(SessionState::Failed(e.to_string()));
                    }
                    // A command in this same poll already dropped the attempt.
                    _ => {}
                }
            },
            alive = changed => {
                let Some(l) = link.as_ref() else { continue };
                if !alive {
                    // The engine handle went away without us dropping it.
                    let _ = engine.send(None);
                    let _ = state.send(SessionState::Offline);
                    link = None;
                    shown = None;
                    continue;
                }
                let next = fold(&l.status.borrow(), &l.peer);
                // A dropped link must not keep dispatching: withdraw the engine while the
                // transport is retrying, so requests fail fast instead of queueing.
                match &next {
                    SessionState::Connected(_) => {
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
    use std::sync::Mutex;
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
        /// Set to fail every token-bearing connection attempt. A probe is not auth.
        refuse: AtomicBool,
        /// Keyed by `SessionConfig::engine`. `None` waits forever; `Some(d)` sleeps.
        /// Absent means the attempt completes immediately, which is what the other tests use.
        delays: Mutex<std::collections::HashMap<String, Option<std::time::Duration>>>,
        /// Engine names whose connect future returned a link. A dropped attempt never appears.
        finished: Mutex<Vec<String>>,
        /// Pins passed to the token-bearing connect. A probe must not appear here.
        connects: Mutex<Vec<String>>,
        /// URLs that were probed.
        probes: Mutex<Vec<String>>,
    }

    impl Fake {
        async fn wait(&self, engine: Option<&str>) {
            let delay =
                engine.and_then(|name| self.delays.lock().expect("delays").get(name).copied());
            match delay {
                Some(None) => pending::<()>().await,
                Some(Some(d)) => tokio::time::sleep(d).await,
                None => {}
            }
        }
    }

    struct Named {
        name: String,
        inner: Arc<Recorder>,
    }

    impl Engine for Named {
        fn subscribe(&self, req: AnalyzeReq) -> Subscription {
            self.inner.subscribe(req)
        }

        fn describe(&self) -> EngineDesc {
            EngineDesc::placeholder(&self.name)
        }
    }

    impl Connector for Arc<Fake> {
        fn probe(
            &self,
            config: &SessionConfig,
        ) -> Pin<Box<dyn Future<Output = Result<String, EngineError>> + Send>> {
            let this = self.clone();
            let config = config.clone();
            Box::pin(async move {
                this.wait(config.engine.as_deref()).await;
                this.probes.lock().expect("probes").push(config.url);
                Ok(probed_fingerprint(config.engine.as_deref()))
            })
        }

        fn connect(
            &self,
            config: SessionConfig,
            pin: String,
        ) -> Pin<Box<dyn Future<Output = Result<Link, EngineError>> + Send>> {
            let this = self.clone();
            Box::pin(async move {
                this.wait(config.engine.as_deref()).await;
                if this.refuse.load(Ordering::Relaxed) {
                    return Err(EngineError::Disconnected("unauthorized".into()));
                }
                this.connects.lock().expect("connects").push(pin.clone());
                let name = config.engine.clone().unwrap_or_else(|| "fake".into());
                this.finished.lock().expect("finished").push(name.clone());
                let engine: Arc<dyn Engine> = match &config.engine {
                    Some(name) => Arc::new(Named {
                        name: name.clone(),
                        inner: this.engine.clone(),
                    }),
                    None => this.engine.clone(),
                };
                Ok(Link {
                    engine,
                    peer: Peer {
                        fingerprint: pin,
                        server: "mirai-test/1".into(),
                        engine: name,
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
                delays: Mutex::new(std::collections::HashMap::new()),
                finished: Mutex::new(Vec::new()),
                connects: Mutex::new(Vec::new()),
                probes: Mutex::new(Vec::new()),
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

        session.trust("a".repeat(64));
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

    fn config(engine: Option<&str>) -> SessionConfig {
        SessionConfig {
            url: "mirai://test:9678".into(),
            token: "tok".into(),
            engine: engine.map(str::to_string),
            pin: Some("a".repeat(64)),
        }
    }

    /// A connect that never finishes must not swallow Disconnect.
    #[tokio::test]
    async fn disconnect_during_connect_goes_offline_without_waiting() {
        let (backend, _) = fake();
        backend
            .delays
            .lock()
            .expect("delays")
            .insert("hang".into(), None);
        let session = Session::new(backend);
        session.connect(config(Some("hang")));
        until(&session, |s| matches!(s, SessionState::Connecting)).await;

        session.disconnect();
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            until(&session, |s| matches!(s, SessionState::Offline)),
        )
        .await
        .expect("disconnect did not cancel the in-flight connect");
    }

    /// Connect B must replace A. A's engine is never published, even after A would have finished.
    #[tokio::test]
    async fn a_newer_connect_drops_the_one_still_in_flight() {
        let (backend, _) = fake();
        {
            let mut delays = backend.delays.lock().expect("delays");
            delays.insert("A".into(), Some(std::time::Duration::from_millis(400)));
            delays.insert("B".into(), Some(std::time::Duration::from_millis(20)));
        }
        let session = Session::new(backend.clone());
        let mut rx = session.watch();
        session.connect(config(Some("A")));
        session.connect(config(Some("B")));

        let mut published = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let state = rx.borrow_and_update().clone();
            if let SessionState::Connected(peer) = &state {
                published.push(peer.engine.clone());
                if peer.engine == "B" {
                    break;
                }
            }
            tokio::time::timeout_at(deadline, rx.changed())
                .await
                .expect("B never became the connected engine")
                .expect("session task ended");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            matches!(session.state(), SessionState::Connected(peer) if peer.engine == "B"),
            "{:?}",
            session.state()
        );
        assert!(
            !published.iter().any(|name| name == "A"),
            "a superseded connect was published: {published:?}"
        );
        assert_eq!(
            backend.finished.lock().expect("finished").as_slice(),
            ["B".to_string()]
        );
        assert_eq!(session.describe().name, "B");
    }

    fn unpinned(engine: &str) -> SessionConfig {
        SessionConfig {
            pin: None,
            ..config(Some(engine))
        }
    }

    /// Distinct per engine name, so a Trust for A cannot match B by accident.
    fn probed_fingerprint(engine: Option<&str>) -> String {
        match engine {
            None => "a".repeat(64),
            Some(name) => name.chars().cycle().take(64).collect(),
        }
    }

    /// Trust during Connecting has no fingerprint to accept. The handshake that
    /// then finishes must still wait.
    #[tokio::test]
    async fn trust_during_connect_does_not_accept_an_unseen_certificate() {
        let (backend, mut seen) = fake();
        backend
            .delays
            .lock()
            .expect("delays")
            .insert("slow".into(), Some(std::time::Duration::from_millis(80)));
        let session = Session::new(backend);
        session.connect(unpinned("slow"));
        until(&session, |s| matches!(s, SessionState::Connecting)).await;
        session.trust("not-shown");

        let state = until(&session, |s| matches!(s, SessionState::AwaitingTrust(_))).await;
        assert!(!state.is_usable());
        assert_eq!(session.describe().name, "");
        let mut sub = session.subscribe(a_request());
        assert!(matches!(
            sub.next().await.expect("refusal"),
            SubEvent::Failed(_)
        ));
        assert_eq!(dispatched(&mut seen), 0);
    }

    /// A Trust meant for the fingerprint on screen must not authorise the Connect
    /// that replaced it.
    #[tokio::test]
    async fn trust_after_a_superseding_connect_waits_for_the_new_fingerprint() {
        let (backend, mut seen) = fake();
        backend
            .delays
            .lock()
            .expect("delays")
            .insert("B".into(), Some(std::time::Duration::from_millis(80)));
        let session = Session::new(backend);
        session.connect(unpinned("A"));
        until(&session, |s| matches!(s, SessionState::AwaitingTrust(_))).await;
        session.connect(unpinned("B"));
        session.trust("A".repeat(64));

        let state = until(
            &session,
            |s| matches!(s, SessionState::AwaitingTrust(peer) if peer.engine == "B"),
        )
        .await;
        assert!(!state.is_usable());
        assert_eq!(session.describe().name, "");
        let mut sub = session.subscribe(a_request());
        assert!(matches!(
            sub.next().await.expect("refusal"),
            SubEvent::Failed(_)
        ));
        assert_eq!(dispatched(&mut seen), 0);
    }

    /// A's Trust, delayed until B's probe has already finished, must not open a
    /// token-bearing connection to B.
    #[tokio::test]
    async fn a_stale_trust_does_not_authorise_the_fingerprint_now_shown() {
        let (backend, mut seen) = fake();
        let session = Session::new(backend.clone());
        session.connect(unpinned("A"));
        let shown = until(
            &session,
            |s| matches!(s, SessionState::AwaitingTrust(peer) if peer.engine == "A"),
        )
        .await;
        let accepted = shown.peer().expect("A").fingerprint.clone();
        assert_eq!(accepted, probed_fingerprint(Some("A")));

        session.connect(unpinned("B"));
        until(
            &session,
            |s| matches!(s, SessionState::AwaitingTrust(peer) if peer.engine == "B"),
        )
        .await;
        session.trust(accepted);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            matches!(
                session.state(),
                SessionState::AwaitingTrust(peer) if peer.engine == "B"
            ),
            "a stale Trust changed the session: {:?}",
            session.state()
        );
        assert!(
            backend.connects.lock().expect("connects").is_empty(),
            "a stale Trust opened a token-bearing connection"
        );
        assert!(!session.state().is_usable());
        let mut sub = session.subscribe(a_request());
        assert!(matches!(
            sub.next().await.expect("refusal"),
            SubEvent::Failed(_)
        ));
        assert_eq!(dispatched(&mut seen), 0);
    }

    /// Connect without a pin probes and waits. The token-bearing connect happens only
    /// after Trust, and only with the fingerprint that was shown.
    #[tokio::test]
    async fn an_unpinned_connect_probes_and_trust_connects_with_that_pin() {
        let (backend, mut seen) = fake();
        let session = Session::new(backend.clone());
        session.connect(SessionConfig {
            url: "mirai://test:9678".into(),
            token: "tok".into(),
            engine: None,
            pin: None,
        });

        let state = until(&session, |s| matches!(s, SessionState::AwaitingTrust(_))).await;
        assert_eq!(
            state.peer().map(|p| p.fingerprint.as_str()),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(
            session.describe().name,
            "",
            "an engine was published before trust"
        );
        assert!(
            backend.connects.lock().expect("connects").is_empty(),
            "a token-bearing connect ran before trust"
        );
        assert_eq!(backend.probes.lock().expect("probes").len(), 1);

        session.trust("a".repeat(64));
        until(&session, SessionState::is_usable).await;
        assert_eq!(
            backend.connects.lock().expect("connects").as_slice(),
            ["a".repeat(64)]
        );
        let _sub = session.subscribe(a_request());
        assert_eq!(dispatched(&mut seen), 1);
    }

    /// Disconnect during the probe drops it. Nothing is pinned and nothing connects.
    #[tokio::test]
    async fn disconnect_during_probe_goes_offline() {
        let (backend, _) = fake();
        backend
            .delays
            .lock()
            .expect("delays")
            .insert("hang".into(), None);
        let session = Session::new(backend.clone());
        session.connect(unpinned("hang"));
        until(&session, |s| matches!(s, SessionState::Connecting)).await;

        session.disconnect();
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            until(&session, |s| matches!(s, SessionState::Offline)),
        )
        .await
        .expect("disconnect did not cancel the in-flight probe");
        assert!(
            backend.connects.lock().expect("connects").is_empty(),
            "disconnect during a probe still connected"
        );
    }
}
