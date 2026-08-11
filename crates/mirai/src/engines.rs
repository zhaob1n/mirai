// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! One engine per profile, shared by every window.
//!
//! `window::present` builds a fresh [`AppState`](crate::app::AppState) per window, so without
//! a pool each window starts its own engine: N windows meant N KataGo processes, each holding
//! its own copy of the network in VRAM. Sharing is safe precisely because of INV-4 — requests
//! carry their whole position, there is no engine-side session state, and `LocalEngine`
//! already multiplexes concurrent queries by id — so two windows analysing different
//! positions cannot interfere.
//!
//! Entries are weak. The engine belongs to the windows using it and dies with the last one,
//! which is what keeps closing a window from leaking a KataGo for the rest of the session.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use mirai_engine::{Engine, EngineError};
use tokio::sync::oneshot;

use crate::config::{EngineProfile, ProfileKind};

/// A started engine and, for a remote profile on its first connect, the certificate
/// fingerprint the caller should pin.
pub type Built = (Arc<dyn Engine>, Option<String>);

enum Entry {
    Ready(Weak<dyn Engine>),
    /// A start is in flight; everyone who asked meanwhile waits on one of these.
    Pending(Vec<oneshot::Sender<Result<Built, String>>>),
}

/// The application-wide set of live engines, keyed by profile.
///
/// Lives on the GTK main thread and is only ever touched from it, hence `Rc`/`RefCell`.
#[derive(Default)]
pub struct EnginePool {
    entries: RefCell<HashMap<String, Entry>>,
}

/// Two profiles share an engine only if every field matches.
///
/// Derived `Debug` rather than a hand-written field list: a field added to `ProfileKind`
/// later must not silently widen sharing to profiles that differ in it.
fn key(profile: &EngineProfile) -> String {
    format!("{profile:?}")
}

impl EnginePool {
    /// The engine already running for `profile`, if any.
    ///
    /// Lets a window adopt another window's engine — or re-adopt its own — without going
    /// through the asynchronous path, so re-selecting the active profile neither restarts
    /// KataGo nor blanks the readout.
    pub fn running(&self, profile: &EngineProfile) -> Option<Arc<dyn Engine>> {
        match self.entries.borrow().get(&key(profile)) {
            Some(Entry::Ready(weak)) => weak.upgrade(),
            _ => None,
        }
    }

    /// Hands back the engine for `profile`, starting it or joining a start already in flight.
    pub async fn acquire(
        self: Rc<Self>,
        profile: EngineProfile,
        log_dir: PathBuf,
        runtime: tokio::runtime::Handle,
    ) -> Result<Built, String> {
        let key = key(&profile);

        // Claim the entry, or queue behind whoever already owns it. The borrow ends here:
        // nothing may hold it across the await below, where another window's `acquire`
        // runs on this same thread.
        let waiter = {
            let mut entries = self.entries.borrow_mut();
            match entries.get_mut(&key) {
                Some(Entry::Pending(waiting)) => {
                    let (tx, rx) = oneshot::channel();
                    waiting.push(tx);
                    Some(rx)
                }
                Some(Entry::Ready(weak)) => match weak.upgrade() {
                    Some(engine) => return Ok((engine, None)),
                    None => {
                        entries.insert(key.clone(), Entry::Pending(Vec::new()));
                        None
                    }
                },
                None => {
                    entries.insert(key.clone(), Entry::Pending(Vec::new()));
                    None
                }
            }
        };

        if let Some(rx) = waiter {
            return rx
                .await
                .unwrap_or_else(|_| Err("engine startup was cancelled".to_string()));
        }

        let result = start(profile, log_dir, runtime).await;

        let waiting = match self.entries.borrow_mut().remove(&key) {
            Some(Entry::Pending(waiting)) => waiting,
            // Nobody else may claim a pending entry, so this is unreachable in practice;
            // treating it as "no waiters" keeps the failure mode to a slow start, not a hang.
            _ => Vec::new(),
        };
        if let Ok((engine, _)) = &result {
            self.entries
                .borrow_mut()
                .insert(key, Entry::Ready(Arc::downgrade(engine)));
        }
        for tx in waiting {
            let _ = tx.send(result.clone());
        }
        result
    }
}

/// Runs the blocking start on the tokio runtime and lands the result back on this thread.
async fn start(
    profile: EngineProfile,
    log_dir: PathBuf,
    runtime: tokio::runtime::Handle,
) -> Result<Built, String> {
    let (tx, rx) = oneshot::channel();
    runtime.spawn(async move {
        let _ = tx.send(build(profile, log_dir).await);
    });
    match rx.await {
        Ok(Ok(built)) => Ok(built),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("engine startup was cancelled".to_string()),
    }
}

/// Builds an engine from a profile.
async fn build(profile: EngineProfile, log_dir: PathBuf) -> Result<Built, EngineError> {
    match profile.kind {
        ProfileKind::Local {
            ref katago,
            ref model,
            ref config,
            analysis_threads,
            search_threads,
            ..
        } => {
            // A custom config is the user's file, used as it stands with only the two
            // thread values overridable; otherwise mirai writes one next to the logs.
            let (config, analysis_threads, search_threads) = match config {
                Some(path) => (path.clone(), analysis_threads, search_threads),
                None => {
                    let tuning = profile.kind.tuning();
                    let path = tuning.write_to(&log_dir).map_err(|e| {
                        EngineError::Startup(format!(
                            "could not write the analysis config into {}: {e}",
                            log_dir.display()
                        ))
                    })?;
                    // The generated file already carries them; passing them again would
                    // only make the two sources of truth able to disagree.
                    (path, None, None)
                }
            };

            let mut cfg = mirai_engine::LocalEngineConfig::new(
                profile.name.clone(),
                katago.clone(),
                model.clone(),
                config,
            );
            cfg.log_dir = log_dir;
            cfg.analysis_threads = analysis_threads;
            cfg.search_threads = search_threads;
            let engine = mirai_engine::LocalEngine::spawn(cfg).await?;
            Ok((Arc::new(engine) as Arc<dyn Engine>, None))
        }
        ProfileKind::Remote {
            url,
            token,
            engine,
            cert_sha256,
        } => {
            let remote =
                mirai_engine::RemoteEngine::connect(&url, &token, engine, cert_sha256).await?;
            let fingerprint = remote.fingerprint().to_string();
            Ok((Arc::new(remote) as Arc<dyn Engine>, Some(fingerprint)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_engine::{AnalyzeReq, EngineDesc, Subscription};
    use std::path::PathBuf;

    /// Stands in for a started engine. Nothing in these tests subscribes to it.
    struct Stub;

    impl Engine for Stub {
        fn subscribe(&self, _req: AnalyzeReq) -> Subscription {
            unreachable!("the pool never subscribes")
        }

        fn describe(&self) -> EngineDesc {
            EngineDesc::placeholder("stub")
        }
    }

    fn local(name: &str, threads: Option<u16>) -> EngineProfile {
        EngineProfile {
            name: name.to_string(),
            kind: ProfileKind::Local {
                katago: PathBuf::from("/opt/katago"),
                model: PathBuf::from("/opt/net.bin.gz"),
                config: None,
                analysis_threads: Some(2),
                search_threads: threads,
                nn_max_batch_size: None,
                nn_cache_size_power_of_two: None,
            },
        }
    }

    /// Sharing is keyed on the whole profile: same settings share, any difference does not.
    /// Getting this wrong would hand a window an engine configured for something else.
    #[test]
    fn only_identical_profiles_share_an_engine() {
        assert_eq!(key(&local("a", Some(16))), key(&local("a", Some(16))));
        assert_ne!(key(&local("a", Some(16))), key(&local("a", Some(8))));
        assert_ne!(key(&local("a", Some(16))), key(&local("b", Some(16))));
    }

    /// A dropped engine must not be handed out again: the pool holds weak references so the
    /// last window closing really does take KataGo with it.
    #[test]
    fn a_dropped_engine_is_no_longer_running() {
        let pool = EnginePool::default();
        let profile = local("a", Some(16));
        let engine: Arc<dyn Engine> = Arc::new(Stub);
        pool.entries.borrow_mut().insert(
            key(&profile),
            Entry::Ready(Arc::downgrade(&engine)),
        );
        assert!(pool.running(&profile).is_some());
        drop(engine);
        assert!(pool.running(&profile).is_none());
    }
}
