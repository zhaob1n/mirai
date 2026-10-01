// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Local KataGo driver.
//!
//! One `katago analysis` subprocess, three tokio tasks (writer, reader, supervisor) and a
//! map from query id to subscription. Queries are stateless — each one carries its whole
//! position — so there is no engine state to keep in sync and no replay/undo machinery.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use mirai_core::Size;
use mirai_proto::types::{AnalyzeReq, EngineDesc};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, Lines};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch};

use crate::decode::{RawResponse, decode_report};
use crate::query::{action_query, build_query, terminate_query};
use crate::tuning::TuningOverrides;
use crate::{CancelGuard, Engine, EngineError, SubEvent, Subscription};

/// How many stderr lines to keep for diagnosing a failed start.
const STDERR_TAIL: usize = 64;

/// How long a dropped local engine may take to shut down before it is killed.
pub const LOCAL_ENGINE_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Everything needed to start one KataGo process.
#[derive(Clone, Debug)]
pub struct LocalEngineConfig {
    /// Profile name, echoed in [`EngineDesc::name`].
    pub name: String,
    pub katago: PathBuf,
    pub model: PathBuf,
    pub config: PathBuf,
    /// `logDir` override; KataGo writes one log file per run into it.
    pub log_dir: PathBuf,
    /// `numAnalysisThreads` — how many positions are searched at once.
    pub analysis_threads: Option<u16>,
    /// `numSearchThreadsPerAnalysisThread`.
    pub search_threads: Option<u16>,
    /// How long to wait for the startup handshake. The first OpenCL run tunes itself and
    /// can take minutes, hence the generous default.
    pub startup_timeout: Duration,
}

impl LocalEngineConfig {
    pub fn new(
        name: impl Into<String>,
        katago: impl Into<PathBuf>,
        model: impl Into<PathBuf>,
        config: impl Into<PathBuf>,
    ) -> LocalEngineConfig {
        LocalEngineConfig {
            name: name.into(),
            katago: katago.into(),
            model: model.into(),
            config: config.into(),
            log_dir: std::env::temp_dir().join("mirai-katago-logs"),
            analysis_threads: None,
            search_threads: None,
            startup_timeout: Duration::from_secs(180),
        }
    }

    /// Which tuning keys may be forced with `-override-config`.
    ///
    /// A user-supplied analysis.cfg (`custom`) owns every setting but the two
    /// thread counts. A generated config already contains every key; overriding
    /// them again would give one value two sources of truth.
    pub fn apply_config_overrides(&mut self, custom: bool, overrides: TuningOverrides) {
        (self.analysis_threads, self.search_threads) = if custom {
            (overrides.analysis_threads, overrides.search_threads)
        } else {
            (None, None)
        };
    }
}

/// A running KataGo analysis process.
///
/// Dropping it closes KataGo's stdin, which (with `-quit-without-waiting`) makes the
/// process exit promptly; it is killed if it does not.
pub struct LocalEngine {
    inner: Arc<Inner>,
    shutdown_complete: oneshot::Receiver<()>,
}

struct Inner {
    desc: EngineDesc,
    to_engine: mpsc::UnboundedSender<String>,
    /// Query map and the death flag. They share one mutex: `subscribe` checks
    /// `dead` before inserting, and [`Inner::kill`] sets it before draining, so
    /// a death cannot land between the check and the insert.
    subs: Mutex<Subs>,
    next_id: AtomicU64,
    /// Dropped together with the engine; tells the supervisor to stop waiting.
    _shutdown: oneshot::Sender<()>,
}

struct Subs {
    map: HashMap<u64, Entry>,
    dead: bool,
}

struct Entry {
    tx: watch::Sender<SubEvent>,
    size: Size,
    max_candidates: Option<u8>,
}

impl LocalEngine {
    /// Starts KataGo and completes the version/model handshake.
    pub async fn spawn(cfg: LocalEngineConfig) -> Result<LocalEngine, EngineError> {
        let overrides = override_config(&cfg)?;
        if let Err(e) = std::fs::create_dir_all(&cfg.log_dir) {
            tracing::warn!(dir = %cfg.log_dir.display(), %e, "could not create katago log dir");
        }

        tracing::info!(
            katago = %cfg.katago.display(),
            model = %cfg.model.display(),
            config = %cfg.config.display(),
            %overrides,
            "starting katago analysis engine"
        );

        let mut child = Command::new(&cfg.katago)
            .arg("analysis")
            .arg("-model")
            .arg(&cfg.model)
            .arg("-config")
            .arg(&cfg.config)
            .arg("-quit-without-waiting")
            .arg("-override-config")
            .arg(&overrides)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                EngineError::Startup(format!("could not run {}: {e}", cfg.katago.display()))
            })?;

        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(EngineError::Startup("katago pipes are missing".into()));
        };

        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        tokio::spawn(drain_stderr(stderr, Arc::clone(&tail)));

        let (to_engine, from_client) = mpsc::unbounded_channel::<String>();
        tokio::spawn(write_lines(stdin, from_client));

        // The handshake doubles as a readiness probe: KataGo only answers once the model
        // is loaded, which on a cold OpenCL install includes tuning.
        let _ = to_engine.send(action_query("v0", "query_version").to_string());
        let _ = to_engine.send(action_query("m0", "query_models").to_string());

        let mut lines = BufReader::new(stdout).lines();
        let hello = match tokio::time::timeout(cfg.startup_timeout, handshake(&mut lines)).await {
            Ok(Ok(hello)) => hello,
            Ok(Err(e)) => {
                let _ = child.start_kill();
                return Err(startup_error(e, &tail));
            }
            Err(_) => {
                let _ = child.start_kill();
                return Err(startup_error(
                    format!(
                        "katago did not answer within {}s",
                        cfg.startup_timeout.as_secs()
                    ),
                    &tail,
                ));
            }
        };

        let desc = EngineDesc {
            name: cfg.name.clone(),
            katago_version: hello.version,
            model: hello.model.unwrap_or_else(|| file_name(&cfg.model)),
            analysis_threads: cfg
                .analysis_threads
                .or_else(|| config_u16(&cfg.config, "numAnalysisThreads"))
                .unwrap_or(1),
            // Stock KataGo builds cap the board at MAX_LEN = 19.
            max_board: Size::square(19),
            has_human_model: hello.has_human_model,
        };
        tracing::info!(
            version = %desc.katago_version,
            model = %desc.model,
            threads = desc.analysis_threads,
            human = desc.has_human_model,
            "katago ready"
        );

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (shutdown_complete_tx, shutdown_complete) = oneshot::channel();
        let inner = Arc::new(Inner {
            desc,
            to_engine,
            subs: Mutex::new(Subs {
                map: HashMap::new(),
                dead: false,
            }),
            next_id: AtomicU64::new(0),
            _shutdown: shutdown_tx,
        });

        tokio::spawn(read_responses(lines, Arc::downgrade(&inner)));
        tokio::spawn(supervise(
            child,
            Arc::downgrade(&inner),
            shutdown_rx,
            shutdown_complete_tx,
            tail,
        ));

        Ok(LocalEngine {
            inner,
            shutdown_complete,
        })
    }

    /// Closes KataGo's stdin and waits until the child has exited (or been killed after
    /// the normal shutdown grace period).
    pub async fn shutdown(self) {
        let LocalEngine {
            inner,
            shutdown_complete,
        } = self;
        drop(inner);
        let _ = shutdown_complete.await;
    }
}

impl Engine for LocalEngine {
    fn subscribe(&self, req: AnalyzeReq) -> Subscription {
        let inner = &self.inner;
        let (id, rx) = {
            let mut subs = inner.subs();
            if subs.dead {
                return Subscription::failed(EngineError::EngineExited(
                    "katago is no longer running".into(),
                ));
            }
            let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = watch::channel(SubEvent::Pending);
            subs.map.insert(
                id,
                Entry {
                    tx,
                    size: req.size,
                    max_candidates: req.max_candidates,
                },
            );
            (id, rx)
        };

        let line = build_query(&id.to_string(), &req).to_string();
        if inner.to_engine.send(line).is_err() {
            inner.subs().map.remove(&id);
            return Subscription::failed(EngineError::EngineExited(
                "katago stdin is closed".into(),
            ));
        }

        // Weak: a live subscription must not keep the engine process alive.
        let weak = Arc::downgrade(inner);
        Subscription::new(
            rx,
            CancelGuard::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.cancel(id);
                }
            }),
        )
    }

    fn describe(&self) -> EngineDesc {
        self.inner.desc.clone()
    }
}

impl Inner {
    /// The subscription table. Every critical section is one lookup or edit, plus at most one
    /// non-blocking `watch` send, and none spans an `.await`. Each edit leaves the map whole,
    /// so a panic elsewhere while it was held is no reason to stop routing: take it anyway.
    fn subs(&self) -> MutexGuard<'_, Subs> {
        self.subs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks KataGo to stop a query and forgets it. The subscriber is gone, so a later
    /// line is an unknown id: traced, not decoded. KataGo still writes a final line
    /// for a terminated query, including `noResults` when the search never started.
    fn cancel(&self, id: u64) {
        if self.subs().map.remove(&id).is_none() {
            return;
        }
        self.terminate(id);
    }

    fn terminate(&self, id: u64) {
        let key = id.to_string();
        let line = terminate_query(&format!("t{key}"), &key).to_string();
        let _ = self.to_engine.send(line);
    }

    fn handle(&self, line: &str) {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(%e, line, "katago wrote a line that is not json");
                return;
            }
        };
        match RawResponse::classify(value) {
            Ok(RawResponse::Analysis { id, terminal, body }) => self.deliver(&id, terminal, &body),
            Ok(RawResponse::Action { id, action, .. }) => {
                tracing::trace!(id, action, "action acknowledged");
            }
            Ok(RawResponse::Warning { id, msg }) => {
                tracing::warn!(?id, msg, "katago warning");
            }
            Ok(RawResponse::QueryError { id, msg }) => {
                tracing::warn!(id, msg, "katago rejected a query");
                let entry = id
                    .parse::<u64>()
                    .ok()
                    .and_then(|key| self.subs().map.remove(&key));
                if let Some(entry) = entry {
                    entry
                        .tx
                        .send_replace(SubEvent::Failed(EngineError::Query(msg)));
                }
            }
            Ok(RawResponse::EngineFault { msg }) => {
                tracing::error!(msg, "katago reported a fatal error");
                self.kill(EngineError::Query(msg));
            }
            Err(e) => tracing::debug!(%e, line, "ignoring unrecognised katago response"),
        }
    }

    fn deliver(&self, id: &str, terminal: bool, body: &Value) {
        let Ok(key) = id.parse::<u64>() else {
            tracing::trace!(id, "response for a non-analysis id");
            return;
        };
        let (size, cap) = {
            let subs = self.subs();
            let Some(entry) = subs.map.get(&key) else {
                // Tail of a query we already finished or cancelled. KataGo still writes a
                // final line, including `noResults` when the search never started; trace,
                // don't warn — a cancelled search can emit several.
                tracing::trace!(id, "response for an unknown query id");
                return;
            };
            if entry.tx.is_closed() {
                // The subscriber is dropping. `cancel` removes the entry and sends
                // terminate; decoding this line would only burn CPU.
                return;
            }
            (entry.size, entry.max_candidates)
        };

        let event = match decode_report(size, cap, body) {
            Ok(report) => {
                let report = Arc::new(report);
                if terminal {
                    SubEvent::Done(report)
                } else {
                    SubEvent::Report(report)
                }
            }
            Err(e) => {
                tracing::error!(id, %e, "could not decode a katago response");
                // The failure removes the entry, so Drop's cancel cannot send this.
                // Stop KataGo now, as on a consumer cancel (INV-3).
                let entry = self.subs().map.remove(&key);
                if let Some(entry) = entry {
                    entry
                        .tx
                        .send_replace(SubEvent::Failed(EngineError::Protocol(e)));
                    self.terminate(key);
                }
                return;
            }
        };

        if event.is_terminal() {
            let entry = self.subs().map.remove(&key);
            if let Some(entry) = entry {
                entry.tx.send_replace(event);
            }
        } else if let Some(entry) = self.subs().map.get(&key) {
            entry.tx.send_replace(event);
        }
    }

    /// Marks the engine dead and fails every live subscription.
    ///
    /// `dead` is set under the map lock, before the drain, so it excludes `subscribe`'s
    /// check-and-insert. Returns whether any analysis was still live.
    fn kill(&self, err: EngineError) -> bool {
        let entries = {
            let mut subs = self.subs();
            subs.dead = true;
            std::mem::take(&mut subs.map)
        };
        let live = !entries.is_empty();
        for entry in entries.into_values() {
            entry.tx.send_replace(SubEvent::Failed(err.clone()));
        }
        live
    }
}

/// Feeds query lines to KataGo, flushing once per drained batch.
async fn write_lines(stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<String>) {
    let mut out = BufWriter::new(stdin);
    while let Some(mut line) = rx.recv().await {
        loop {
            tracing::trace!(line, "-> katago");
            if out.write_all(line.as_bytes()).await.is_err() || out.write_all(b"\n").await.is_err()
            {
                return;
            }
            match rx.try_recv() {
                Ok(next) => line = next,
                Err(_) => break,
            }
        }
        if out.flush().await.is_err() {
            return;
        }
    }
    // The engine handle is gone: closing stdin asks KataGo to quit.
}

async fn read_responses(mut lines: Lines<BufReader<ChildStdout>>, engine: Weak<Inner>) {
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(%e, "katago stdout failed");
                break;
            }
        };
        let Some(inner) = engine.upgrade() else { break };
        inner.handle(&line);
    }
}

async fn drain_stderr(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "katago", "{line}");
        let mut tail = tail.lock().expect("stderr tail poisoned");
        if tail.len() == STDERR_TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// Watches the process; when it exits, every live subscription fails.
async fn supervise(
    mut child: Child,
    engine: Weak<Inner>,
    shutdown: oneshot::Receiver<()>,
    shutdown_complete: oneshot::Sender<()>,
    tail: Arc<Mutex<VecDeque<String>>>,
) {
    let status = tokio::select! {
        status = child.wait() => status,
        _ = shutdown => {
            // The engine handle was dropped, so stdin is closed and KataGo should be on
            // its way out. Give it a moment, then insist.
            match tokio::time::timeout(LOCAL_ENGINE_SHUTDOWN_GRACE, child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    tracing::debug!("killing katago after its shutdown grace period");
                    let _ = child.kill().await;
                    child.wait().await
                }
            }
        }
    };

    let reason = match status {
        Ok(status) => format!("katago exited with {status}"),
        Err(e) => format!("could not wait for katago: {e}"),
    };
    if let Some(inner) = engine.upgrade() {
        let err = EngineError::EngineExited(with_tail(reason.clone(), &tail));
        if inner.kill(err) {
            tracing::error!(reason, "katago exited with live analyses");
        }
    }
    let _ = shutdown_complete.send(());
}

struct Hello {
    version: String,
    model: Option<String>,
    has_human_model: bool,
}

/// Reads until both handshake queries have been answered.
async fn handshake(lines: &mut Lines<BufReader<ChildStdout>>) -> Result<Hello, String> {
    let mut version: Option<String> = None;
    let mut models: Option<Vec<Value>> = None;

    while version.is_none() || models.is_none() {
        let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| format!("could not read katago stdout: {e}"))?
        else {
            return Err("katago exited during startup".into());
        };
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                tracing::debug!(line, "ignoring non-json startup line");
                continue;
            }
        };
        match RawResponse::classify(value) {
            Ok(RawResponse::Action { id, body, .. }) if id == "v0" => {
                version = Some(
                    body.get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                );
            }
            Ok(RawResponse::Action { id, body, .. }) if id == "m0" => {
                models = Some(match body.get("models") {
                    Some(Value::Array(list)) => list.clone(),
                    _ => Vec::new(),
                });
            }
            // `query_models` is newer than the rest of the protocol; an engine that does
            // not know it is still perfectly usable.
            Ok(RawResponse::QueryError { id, msg }) if id == "m0" => {
                tracing::warn!(msg, "katago does not support query_models");
                models = Some(Vec::new());
            }
            Ok(RawResponse::QueryError { id, msg }) => return Err(format!("query {id}: {msg}")),
            Ok(RawResponse::EngineFault { msg }) => return Err(msg),
            Ok(other) => tracing::debug!(?other, "ignoring response during startup"),
            Err(e) => tracing::debug!(%e, line, "ignoring unrecognised startup response"),
        }
    }

    let models = models.unwrap_or_default();
    let is_human = |m: &Value| {
        m.get("usesHumanSLProfile")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || m.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.contains("human"))
    };
    Ok(Hello {
        version: version.unwrap_or_else(|| "unknown".into()),
        model: models
            .iter()
            .find(|m| !is_human(m))
            .and_then(|m| m.get("name").and_then(Value::as_str))
            .map(str::to_owned),
        has_human_model: models.iter().any(is_human),
    })
}

/// The `-override-config` value. KataGo splits it on commas, so no value may contain one.
fn override_config(cfg: &LocalEngineConfig) -> Result<String, EngineError> {
    let log_dir = cfg.log_dir.display().to_string();
    let mut pairs: Vec<(&str, String)> = vec![
        // Black-perspective values everywhere: stored analysis is then sign-stable.
        ("reportAnalysisWinratesAs", "BLACK".into()),
        ("logDir", log_dir),
        // stderr is ours (we log it); the log file keeps the detail.
        ("logToStderr", "false".into()),
        ("logAllRequests", "false".into()),
        ("logAllResponses", "false".into()),
    ];
    if let Some(n) = cfg.analysis_threads {
        pairs.push(("numAnalysisThreads", n.to_string()));
    }
    if let Some(n) = cfg.search_threads {
        pairs.push(("numSearchThreadsPerAnalysisThread", n.to_string()));
    }
    if let Some((key, value)) = pairs.iter().find(|(_, v)| v.contains(',')) {
        return Err(EngineError::Startup(format!(
            "{key}={value:?} contains a comma, which katago's -override-config cannot express"
        )));
    }
    Ok(pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(","))
}

/// Reads one integer key out of a KataGo config file, for reporting `analysis_threads`.
fn config_u16(path: &Path, key: &str) -> Option<u16> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == key {
            return v.trim().parse().ok();
        }
    }
    None
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn startup_error(msg: String, tail: &Mutex<VecDeque<String>>) -> EngineError {
    EngineError::Startup(with_tail(msg, tail))
}

fn with_tail(msg: String, tail: &Mutex<VecDeque<String>>) -> String {
    let tail = tail.lock().expect("stderr tail poisoned");
    if tail.is_empty() {
        msg
    } else {
        let lines: Vec<&str> = tail.iter().map(String::as_str).collect();
        format!("{msg}\nlast katago output:\n{}", lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LocalEngineConfig {
        let mut cfg =
            LocalEngineConfig::new("local", "/bin/katago", "/nets/m.bin.gz", "/etc/a.cfg");
        cfg.log_dir = PathBuf::from("/var/log/mirai");
        cfg
    }

    #[test]
    fn overrides_always_pin_the_reporting_perspective_and_logging() {
        assert_eq!(
            override_config(&cfg()).unwrap(),
            "reportAnalysisWinratesAs=BLACK,logDir=/var/log/mirai,logToStderr=false,\
             logAllRequests=false,logAllResponses=false"
        );
    }

    #[test]
    fn optional_tuning_is_only_sent_when_set() {
        let mut cfg = cfg();
        cfg.analysis_threads = Some(4);
        cfg.search_threads = Some(8);
        let s = override_config(&cfg).unwrap();
        assert!(s.ends_with("numAnalysisThreads=4,numSearchThreadsPerAnalysisThread=8"));
        // -analysis-threads is a command-line flag KataGo refuses alongside the config key.
        assert!(!s.contains("analysis-threads"));
    }

    #[test]
    fn a_comma_in_a_path_is_reported_rather_than_silently_truncated() {
        let mut cfg = cfg();
        cfg.log_dir = PathBuf::from("/var/log/mirai,2");
        let err = override_config(&cfg).unwrap_err();
        assert!(matches!(err, EngineError::Startup(m) if m.contains("logDir")));
    }

    #[test]
    fn analysis_threads_are_read_out_of_the_config_file() {
        let dir = std::env::temp_dir().join(format!("mirai-local-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("analysis.cfg");
        std::fs::write(
            &path,
            "# numAnalysisThreads = 99\nnumAnalysisThreads = 6\nnumSearchThreadsPerAnalysisThread = 16\n",
        )
        .unwrap();
        assert_eq!(config_u16(&path, "numAnalysisThreads"), Some(6));
        assert_eq!(
            config_u16(&path, "numSearchThreadsPerAnalysisThread"),
            Some(16)
        );
        assert_eq!(config_u16(&path, "nnCacheSizePowerOfTwo"), None);
        std::fs::remove_file(&path).unwrap();
    }

    fn req() -> AnalyzeReq {
        AnalyzeReq::new(Size::square(19), mirai_core::RuleSet::Chinese, 7.5)
    }

    /// No process: `lines` is what subscribe and cancel would write to KataGo.
    fn detached() -> (LocalEngine, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let (_complete_tx, shutdown_complete) = oneshot::channel();
        let engine = LocalEngine {
            inner: Arc::new(Inner {
                desc: EngineDesc::placeholder("test"),
                to_engine: tx,
                subs: Mutex::new(Subs {
                    map: HashMap::new(),
                    dead: false,
                }),
                next_id: AtomicU64::new(0),
                _shutdown: shutdown_tx,
            }),
            shutdown_complete,
        };
        (engine, rx)
    }

    #[test]
    fn a_decode_failure_terminates_the_search() {
        let (engine, mut lines) = detached();
        let sub = engine.subscribe(req());
        let query = lines.try_recv().expect("query");
        assert!(query.contains("\"id\":\"0\""), "{query}");

        // Analysis-shaped, but missing rootInfo, so decode_report fails.
        engine
            .inner
            .handle(r#"{"id":"0","isDuringSearch":true,"moveInfos":[]}"#);

        match sub.current() {
            SubEvent::Failed(EngineError::Protocol(msg)) => {
                assert!(msg.contains("rootInfo"), "{msg}");
            }
            other => panic!("expected a protocol failure, got {other:?}"),
        }
        let term = lines
            .try_recv()
            .expect("decode failure must terminate the search");
        let v: serde_json::Value = serde_json::from_str(&term).unwrap();
        assert_eq!(v["action"], "terminate");
        assert_eq!(v["terminateId"], "0");
        assert!(engine.inner.subs().map.is_empty());

        drop(sub);
        assert!(
            lines.try_recv().is_err(),
            "drop must not terminate a second time"
        );
    }

    #[test]
    fn dropping_a_subscription_forgets_it_before_the_tail_arrives() {
        let (engine, mut lines) = detached();
        let sub = engine.subscribe(req());
        let _query = lines.try_recv().expect("query");
        drop(sub);

        let term = lines.try_recv().expect("drop must terminate");
        let v: serde_json::Value = serde_json::from_str(&term).unwrap();
        assert_eq!(v["action"], "terminate");
        assert_eq!(v["terminateId"], "0");
        assert!(
            engine.inner.subs().map.is_empty(),
            "a cancelled query must not keep its entry until KataGo answers"
        );

        // Would fail decode if the entry were still there. It must not revive the
        // entry or send another terminate.
        engine
            .inner
            .handle(r#"{"id":"0","isDuringSearch":false,"moveInfos":[{"order":0}]}"#);
        assert!(engine.inner.subs().map.is_empty());
        assert!(lines.try_recv().is_err());
    }

    /// A user-supplied file owns every setting but the two thread counts; cache size
    /// and batch size named by the caller must not reach `-override-config`. A
    /// generated file is the only source of its keys, so it gets no thread override.
    #[test]
    fn a_custom_config_is_overridden_only_for_the_two_thread_counts() {
        let overrides = TuningOverrides {
            analysis_threads: Some(2),
            search_threads: Some(8),
            nn_max_batch_size: Some(32),
            nn_cache_size_power_of_two: Some(23),
        };
        let mut custom = cfg();
        custom.apply_config_overrides(true, overrides);
        let line = override_config(&custom).unwrap();
        assert!(
            line.ends_with("numAnalysisThreads=2,numSearchThreadsPerAnalysisThread=8"),
            "{line}"
        );

        let mut generated = cfg();
        generated.analysis_threads = Some(4);
        generated.apply_config_overrides(false, overrides);
        assert_eq!(
            override_config(&generated).unwrap(),
            override_config(&cfg()).unwrap()
        );
    }

    /// Exercises the real subprocess reader and supervisor, not just `Inner::handle`:
    /// a KataGo process dying mid-query must fail its subscriber and refuse new work.
    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_exit_fails_live_and_future_subscriptions() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("mirai-fake-katago-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let script = dir.join("katago");
        std::fs::write(
            &script,
            "#!/bin/sh\nread -r version\nread -r models\n\
             printf '%s\\n' '{\"id\":\"v0\",\"action\":\"query_version\",\"version\":\"fake\"}' \
             '{\"id\":\"m0\",\"action\":\"query_models\",\"models\":[]}'\n\
             read -r query\nexit 17\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut cfg =
            LocalEngineConfig::new("fake", &script, dir.join("model"), dir.join("config"));
        cfg.log_dir = dir.clone();
        cfg.startup_timeout = Duration::from_secs(2);
        let engine = LocalEngine::spawn(cfg).await.unwrap();
        let err = tokio::time::timeout(Duration::from_secs(2), engine.subscribe(req()).finish())
            .await
            .expect("subprocess did not exit")
            .unwrap_err();
        assert!(matches!(err, EngineError::EngineExited(message) if message.contains("17")));
        assert!(matches!(
            engine.subscribe(req()).current(),
            SubEvent::Failed(EngineError::EngineExited(_))
        ));
        engine.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
