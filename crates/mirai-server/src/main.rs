// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `mirai-server` — a headless host that lends KataGo to mirai clients over MRP.
//!
//! One process owns one KataGo `analysis` subprocess per configured `[[engine]]` and
//! multiplexes every connected client onto them; `numAnalysisThreads` is what makes that
//! safe, so a second client never costs a second GPU context.

mod config;
mod session;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use mirai_engine::{Engine, EngineError, LocalEngine, LocalEngineConfig, SubEvent, Subscription};
use mirai_proto::transport;
use tokio::sync::{Semaphore, watch};
use tracing::{error, info, warn};

/// Caps pre-authentication frame buffers as well as authenticated sessions.
/// A silent peer releases its slot at `Budgets::preauth`; a peer whose first
/// frame is rejected gets only a short additional bounded delivery window.
const MAX_SESSIONS: usize = 32;

use config::ServerConfig;
use session::{Host, NamedEngine, Token};

#[derive(Parser, Debug)]
#[command(
    name = "mirai-server",
    version,
    about = "Headless KataGo host speaking the mirai remote-analysis protocol"
)]
struct Args {
    /// Configuration file (default: $XDG_CONFIG_HOME/mirai/server.toml).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Address to bind; overrides `listen` in the configuration file.
    #[arg(long, value_name = "ADDR")]
    listen: Option<String>,

    /// Print a fresh 64-hex-character access token and exit. Needs no configuration file.
    #[arg(long)]
    generate_token: bool,

    /// Load (creating it if necessary) the certificate, print its SHA-256 fingerprint,
    /// and exit.
    #[arg(long)]
    print_fingerprint: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    // Deliberately before anything that needs a config file or a runtime: this is what a
    // fresh install runs first.
    if args.generate_token {
        match generate_token() {
            Ok(token) => {
                println!("{token}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("mirai-server: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    init_tracing();

    let config_path = match args.config.clone() {
        Some(p) => p,
        None => default_config_path(),
    };
    if !config_path.exists() {
        report_missing_config(&config_path);
        return ExitCode::FAILURE;
    }
    let cfg = match ServerConfig::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    info!(config = %config_path.display(), "loaded configuration");

    let listen = match cfg.listen_addr(args.listen.as_deref()) {
        Ok(a) => a,
        Err(e) => {
            error!("{e:#}");
            return ExitCode::FAILURE;
        }
    };

    let (certs, key) =
        match transport::load_or_generate_cert(&cfg.cert, &cfg.key, &cert_hostnames(listen)) {
            Ok(pair) => pair,
            Err(e) => {
                error!(
                    cert = %cfg.cert.display(),
                    key = %cfg.key.display(),
                    "certificate unusable: {e}"
                );
                return ExitCode::FAILURE;
            }
        };
    let fingerprint = transport::fingerprint_of(&certs);

    if args.print_fingerprint {
        println!("{fingerprint}");
        return ExitCode::SUCCESS;
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("could not start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(cfg, listen, certs, key, fingerprint))
}

async fn run(
    cfg: ServerConfig,
    listen: SocketAddr,
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    fingerprint: String,
) -> ExitCode {
    // A longer token could never be presented: the server refuses such a Hello.
    if let Some(i) = cfg
        .tokens
        .iter()
        .position(|t| t.value.len() > session::MAX_HELLO_FIELD)
    {
        error!(
            "[[token]] #{} is longer than {} bytes; no client could present it",
            i + 1,
            session::MAX_HELLO_FIELD
        );
        return ExitCode::FAILURE;
    }
    // Install signal handlers before KataGo's potentially minutes-long startup.
    // A signal during a cold GPU handshake cancels that spawn, shuts down any
    // engines already started, and never binds the listener.
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown_tx.send_replace(true);
    });
    let Started { named, live } = start_engines(&cfg, &mut shutdown_rx).await;
    if *shutdown_rx.borrow() {
        shutdown_engines(live).await;
        return ExitCode::SUCCESS;
    }
    if named.is_empty() {
        error!("no engine started; there is nothing to serve");
        return ExitCode::FAILURE;
    }
    if cfg.tokens.is_empty() {
        warn!("no [[token]] configured — every client will be rejected as unauthorized");
    }

    let endpoint = match transport::server_endpoint(listen, certs, key) {
        Ok(e) => e,
        Err(e) => {
            error!(%listen, "could not bind: {e}");
            shutdown_engines(live).await;
            return ExitCode::FAILURE;
        }
    };

    let host = Arc::new(Host {
        engines: named,
        tokens: cfg
            .tokens
            .into_iter()
            .map(|t| Token::new(t.value, t.name, t.max_subs))
            .collect(),
    });

    info!(
        listen = %listen,
        engines = host.engines.len(),
        tokens = host.tokens.len(),
        "mirai-server listening"
    );
    let shown = mirai_proto::sha256::format_fingerprint(&fingerprint);
    info!(
        sha256 = %shown,
        "certificate fingerprint (compare this with the client before trusting)"
    );

    let sessions = Arc::new(Semaphore::new(MAX_SESSIONS));
    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                info!("termination signal, shutting down");
                break;
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };
                let peer = incoming.remote_address();
                let Ok(permit) = Arc::clone(&sessions).try_acquire_owned() else {
                    warn!(
                        %peer,
                        max_sessions = MAX_SESSIONS,
                        "connection refused: session limit reached"
                    );
                    incoming.refuse();
                    continue;
                };
                let host = Arc::clone(&host);
                tokio::spawn(async move {
                    // Keeping the owned permit in the task releases it on every return
                    // and unwind, including when the pre-authentication deadline closes
                    // a silent peer.
                    let _permit = permit;
                    session::serve(host, incoming, session::Budgets::DEFAULT).await;
                });
            }
        }
    }

    endpoint.close(quinn::VarInt::from_u32(0), b"bye");
    // Sessions notice the close and drop their searches. Don't wait out a stuck one:
    // the control-write budget already bounds that, and a second signal exits now.
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        sessions.acquire_many(MAX_SESSIONS as u32),
    )
    .await;
    drop(host);
    shutdown_engines(live).await;
    info!("endpoint closed");
    ExitCode::SUCCESS
}

struct Started {
    named: Vec<NamedEngine>,
    live: Vec<Arc<LiveEngine>>,
}

/// One KataGo process, replaced if it exits.
///
/// `LocalEngine` marks itself dead and fails live searches, then stays dead. A headless
/// host has nobody to toggle the profile, so the next failed subscribe starts a new
/// process. Restarts back off so a bad binary does not fork in a loop.
struct LiveEngine {
    name: String,
    cfg: LocalEngineConfig,
    current: std::sync::Mutex<Option<Arc<LocalEngine>>>,
    restarting: AtomicBool,
    stop: watch::Sender<bool>,
    weak: std::sync::Weak<LiveEngine>,
}

fn lock_engine(
    mutex: &std::sync::Mutex<Option<Arc<LocalEngine>>>,
) -> std::sync::MutexGuard<'_, Option<Arc<LocalEngine>>> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl LiveEngine {
    fn start(name: String, engine: LocalEngine, cfg: LocalEngineConfig) -> Arc<LiveEngine> {
        Arc::new_cyclic(|weak| LiveEngine {
            name,
            cfg,
            current: std::sync::Mutex::new(Some(Arc::new(engine))),
            restarting: AtomicBool::new(false),
            stop: watch::channel(false).0,
            weak: weak.clone(),
        })
    }

    fn kick(&self) {
        if *self.stop.borrow() || self.restarting.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(this) = self.weak.upgrade() else {
            self.restarting.store(false, Ordering::Release);
            return;
        };
        info!(engine = %self.name, "engine exited; restarting");
        tokio::spawn(async move { this.restart().await });
    }

    async fn restart(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut delay = Duration::from_secs(1);
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                _ = stop.changed() => break,
                _ = tokio::time::sleep(delay) => {}
            }
            if *stop.borrow() {
                break;
            }
            // Dropping a pending spawn kills its child (LocalEngine sets kill_on_drop).
            // Shutdown must not wait out the full first-run GPU startup timeout.
            let started = tokio::select! {
                _ = stop.changed() => break,
                result = LocalEngine::spawn(self.cfg.clone()) => result,
            };
            match started {
                Ok(engine) => {
                    let mut pending = Some(engine);
                    {
                        let mut current = lock_engine(&self.current);
                        if !*self.stop.borrow() {
                            *current = pending.take().map(Arc::new);
                        }
                    }
                    if let Some(engine) = pending {
                        engine.shutdown().await;
                    } else {
                        info!(engine = %self.name, "engine restarted");
                    }
                    break;
                }
                Err(err) => {
                    warn!(engine = %self.name, "engine restart failed, retrying: {err}");
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }
        }
        self.restarting.store(false, Ordering::Release);
    }

    async fn shutdown(self: Arc<Self>) {
        self.stop.send_replace(true);
        let current = lock_engine(&self.current).take();
        if let Some(engine) = current.and_then(|engine| Arc::try_unwrap(engine).ok()) {
            engine.shutdown().await;
        }
    }
}

impl Engine for LiveEngine {
    fn subscribe(&self, req: mirai_proto::types::AnalyzeReq) -> Subscription {
        if *self.stop.borrow() {
            return Subscription::failed(EngineError::EngineExited("shutting down".into()));
        }
        let Some(engine) = lock_engine(&self.current).clone() else {
            self.kick();
            return Subscription::failed(EngineError::EngineExited("katago is restarting".into()));
        };
        let sub = engine.subscribe(req);
        if matches!(
            sub.current(),
            SubEvent::Failed(EngineError::EngineExited(_))
        ) {
            self.kick();
        }
        sub
    }

    fn describe(&self) -> mirai_proto::types::EngineDesc {
        match lock_engine(&self.current).as_ref() {
            Some(engine) => engine.describe(),
            None => mirai_proto::types::EngineDesc::placeholder(self.name.clone()),
        }
    }
}

async fn shutdown_engines(live: Vec<Arc<LiveEngine>>) {
    let mut tasks = tokio::task::JoinSet::new();
    for engine in live {
        tasks.spawn(engine.shutdown());
    }
    while let Some(result) = tasks.join_next().await {
        if let Err(err) = result {
            warn!("engine shutdown task failed: {err}");
        }
    }
}

/// Starts every configured engine. A failure is logged and skipped, never fatal on its
/// own: one broken GPU config should not take a working engine offline with it.
async fn start_engines(cfg: &ServerConfig, shutdown: &mut watch::Receiver<bool>) -> Started {
    let default_log_dir = default_log_dir();
    let mut named = Vec::with_capacity(cfg.engines.len());
    let mut live = Vec::with_capacity(cfg.engines.len());
    for e in &cfg.engines {
        if *shutdown.borrow() {
            break;
        }
        info!(
            engine = %e.name,
            katago = %e.katago.display(),
            model = %e.model.display(),
            "starting engine"
        );
        let local = match e.to_local_config(default_log_dir.as_deref()) {
            Ok(local) => local,
            Err(err) => {
                error!(engine = %e.name, "engine is not usable, continuing without it: {err:#}");
                continue;
            }
        };
        let started = tokio::select! {
            _ = shutdown.changed() => break,
            result = LocalEngine::spawn(local.clone()) => result,
        };
        match started {
            Ok(engine) => {
                let desc = engine.describe();
                info!(
                    engine = %e.name,
                    katago_version = %desc.katago_version,
                    model = %desc.model,
                    analysis_threads = desc.analysis_threads,
                    human_model = desc.has_human_model,
                    "engine ready"
                );
                let running = LiveEngine::start(e.name.clone(), engine, local);
                named.push(NamedEngine {
                    name: e.name.clone(),
                    engine: running.clone(),
                });
                live.push(running);
            }
            Err(err) => {
                error!(engine = %e.name, "engine failed to start, continuing without it: {err}");
            }
        }
    }
    Started { named, live }
}

/// The first termination request starts an orderly shutdown. A second one exits
/// immediately, the same way the desktop application does when quit is wedged.
async fn shutdown_signal() {
    let mut requests = match TerminationRequests::install() {
        Ok(requests) => requests,
        Err(error) => {
            warn!(%error, "could not install termination signal handlers");
            std::future::pending::<()>().await;
            return;
        }
    };
    requests.recv().await;
    tokio::spawn(async move {
        let code = requests.recv().await;
        warn!("second termination signal, exiting immediately");
        std::process::exit(code);
    });
}

/// SIGINT, SIGTERM and SIGHUP.
#[cfg(unix)]
struct TerminationRequests {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl TerminationRequests {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    /// Waits for the next request, and returns the exit status a shell reports for a
    /// process that signal killed: 128 + the signal number.
    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.interrupt.recv() => 128 + 2,
            _ = self.terminate.recv() => 128 + 15,
            _ = self.hangup.recv() => 128 + 1,
        }
    }
}

/// Ctrl-C, Ctrl-Break and a closed console window. For a closed console, Tokio parks the
/// handler thread and Windows allows the process five seconds to finish shutting down.
#[cfg(windows)]
struct TerminationRequests {
    ctrl_c: tokio::signal::windows::CtrlC,
    ctrl_break: tokio::signal::windows::CtrlBreak,
    ctrl_close: tokio::signal::windows::CtrlClose,
}

#[cfg(windows)]
impl TerminationRequests {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
        Ok(Self {
            ctrl_c: ctrl_c()?,
            ctrl_break: ctrl_break()?,
            ctrl_close: ctrl_close()?,
        })
    }

    /// Waits for the next request, and returns the exit status Windows gives a process
    /// that Ctrl-C ended, `STATUS_CONTROL_C_EXIT`.
    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.ctrl_c.recv() => {}
            _ = self.ctrl_break.recv() => {}
            _ = self.ctrl_close.recv() => {}
        }
        0xC000_013A_u32 as i32
    }
}

/// Subject alternative names for a generated certificate. Clients verify by SHA-256
/// fingerprint (TOFU), so these only matter to tools that look at the certificate.
fn cert_hostnames(listen: SocketAddr) -> Vec<String> {
    let mut names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    let ip = listen.ip();
    if !ip.is_unspecified() && !ip.is_loopback() {
        names.push(ip.to_string());
    }
    names
}

/// A 64-hex-character token: 32 bytes from the OS CSPRNG, hex-encoded.
///
/// PROTOCOL §8.2 asks for at least 128 bits. Seeding SplitMix64 from the wall
/// clock and the pid is 64 bits and guessable, so a failure here is reported
/// rather than papered over with that generator.
fn generate_token() -> Result<String, &'static str> {
    let mut buf = [0u8; 32];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut buf)
        .map_err(|_| "the operating system's random number generator failed")?;
    Ok(mirai_proto::sha256::hex(&buf))
}

fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("io.github", "zhaob1n", "mirai")
        .map(|d| d.config_dir().join("server.toml"))
        .unwrap_or_else(|| PathBuf::from("server.toml"))
}

/// KataGo's log directory for an `[[engine]]` without `log_dir`: the one the desktop
/// application uses, in this user's own data directory, never the shared temp directory.
fn default_log_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("io.github", "zhaob1n", "mirai")
        .map(|d| d.data_local_dir().join("katago-logs"))
}

fn report_missing_config(path: &std::path::Path) {
    eprintln!("mirai-server: no configuration file at {}", path.display());
    eprintln!();
    eprintln!("Create it, or point --config at one. A minimal server.toml:");
    eprintln!();
    for line in config::MINIMAL_EXAMPLE.lines() {
        eprintln!("    {line}");
    }
    eprintln!();
    eprintln!("Generate the token with:  mirai-server --generate-token");
    eprintln!("A fully annotated example ships as server.example.toml.");
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("mirai_server=info,mirai_engine=info,warn")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_is_64_lowercase_hex_characters() {
        let t = generate_token().expect("OS random number generator");
        assert_eq!(t.len(), 64);
        assert!(
            t.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "not lowercase hex: {t}"
        );
    }

    #[test]
    fn generated_tokens_differ_between_calls() {
        // Same process, back to back. A generator stuck on one value — or seeded
        // from a constant — would hand every install the same token.
        let a = generate_token().expect("OS random number generator");
        let b = generate_token().expect("OS random number generator");
        assert_ne!(a, b);
    }

    #[test]
    fn cert_names_cover_loopback_and_the_bound_address() {
        let any: SocketAddr = "0.0.0.0:9678".parse().unwrap();
        assert_eq!(cert_hostnames(any), ["localhost", "127.0.0.1", "::1"]);

        let lan: SocketAddr = "192.168.1.10:9678".parse().unwrap();
        assert!(cert_hostnames(lan).contains(&"192.168.1.10".to_string()));
    }

    #[test]
    fn the_cli_matches_the_documented_flags() {
        use clap::CommandFactory;
        Args::command().debug_assert();

        let a = Args::try_parse_from(["mirai-server", "--generate-token"]).unwrap();
        assert!(a.generate_token && a.config.is_none());

        let a = Args::try_parse_from([
            "mirai-server",
            "--config",
            "/tmp/s.toml",
            "--listen",
            "127.0.0.1:9678",
            "--print-fingerprint",
        ])
        .unwrap();
        assert_eq!(
            a.config.as_deref(),
            Some(std::path::Path::new("/tmp/s.toml"))
        );
        assert_eq!(a.listen.as_deref(), Some("127.0.0.1:9678"));
        assert!(a.print_fingerprint);
    }

    /// A process that dies during a query must not leave the headless host
    /// permanently advertising a dead engine. The failed request is not replayed;
    /// a later request uses a newly spawned KataGo.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dead_engine_restarts_for_a_later_request() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mirai-server-restart-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let script = dir.join("katago");
        let marker = dir.join("ran-once");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nread -r version\nread -r models\n\
                 printf '%s\\n' '{{\"id\":\"v0\",\"action\":\"query_version\",\"version\":\"fake\"}}' \
                 '{{\"id\":\"m0\",\"action\":\"query_models\",\"models\":[]}}'\n\
                 if [ ! -e '{}' ]; then\n\
                   touch '{}'\n\
                   read -r query\n\
                   exit 17\n\
                 fi\n\
                 while read -r query; do :; done\n",
                marker.display(),
                marker.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut cfg =
            LocalEngineConfig::new("fake", &script, dir.join("model"), dir.join("config"));
        cfg.log_dir = dir.clone();
        cfg.startup_timeout = Duration::from_secs(2);
        let first = LocalEngine::spawn(cfg.clone()).await.unwrap();
        let engine = LiveEngine::start("fake".into(), first, cfg);
        let req = || {
            mirai_proto::types::AnalyzeReq::new(
                mirai_core::Size::square(19),
                mirai_core::RuleSet::Chinese,
                7.5,
            )
        };
        let failure =
            tokio::time::timeout(Duration::from_secs(2), engine.subscribe(req()).finish())
                .await
                .expect("first KataGo did not exit")
                .unwrap_err();
        assert!(matches!(failure, EngineError::EngineExited(_)), "{failure}");
        assert!(matches!(
            engine.subscribe(req()).current(),
            SubEvent::Failed(EngineError::EngineExited(_))
        ));

        let mut recovered = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let sub = engine.subscribe(req());
            if matches!(sub.current(), SubEvent::Pending) {
                recovered = true;
                break;
            }
        }
        assert!(recovered, "a later request still sees the dead process");
        engine.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
