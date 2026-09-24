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
//!
//! The start is owned by the pool, not by whichever window asked first.
//! [`AppState::activate_profile`](crate::app::AppState::activate_profile) aborts the previous
//! activation when the user picks another profile; that drop must only drop a waiter. If the
//! first acquirer were the one awaiting `start`, aborting it would leave the entry `Pending`
//! forever and every later acquire of that profile would wait on a oneshot nobody sends. A
//! coordinator finishes the start, installs the engine or clears the entry, and delivers the
//! result to whoever is still waiting. A result that arrives with no waiter left drops the
//! engine `Arc`; the weak entry is already dead, and the next acquire starts again. That
//! restart is acceptable: a start with no owner must not leak a KataGo for the rest of the
//! session.
//!
//! The pool keeps the coordinator's handle and aborts it on drop. Quit runs
//! [`MiraiApplication`](crate::application_shell::MiraiApplication)'s `release`, which drops
//! the pool and only then the tokio runtime. If the coordinator were still holding `start`'s
//! receiver, the runtime task would deliver the engine into a future nobody polls, and
//! dropping the runtime would SIGKILL KataGo instead of closing its stdin.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use gtk::glib;
use mirai_engine::{Engine, EngineError};
use tokio::sync::oneshot;

use crate::config::{EngineProfile, ProfileKind};

/// A started engine and, for a remote profile on its first connect, the certificate
/// fingerprint the caller should pin.
pub type Built = (Arc<dyn Engine>, Option<String>);

enum Entry {
    Ready(Weak<dyn Engine>),
    /// A start is in flight. The pool's coordinator, not any one waiter, sends these.
    Pending(InFlight),
}

struct InFlight {
    waiters: Vec<oneshot::Sender<Result<Built, String>>>,
    /// Taken by [`EnginePool::finish`] so dropping the entry does not abort a coordinator
    /// that is already delivering its result. [`Drop`] aborts whatever is still here.
    coordinator: Option<glib::JoinHandle<()>>,
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
    ///
    /// Dropping this future drops only this caller's waiter. The start keeps running: another
    /// window may still be waiting, and a `Pending` entry whose starter was aborted would make
    /// the profile unusable until the process exits.
    pub async fn acquire(
        self: Rc<Self>,
        profile: EngineProfile,
        log_dir: PathBuf,
        runtime: tokio::runtime::Handle,
    ) -> Result<Built, String> {
        let starting = profile.clone();
        self.acquire_with(profile, move || start(starting, log_dir, runtime))
            .await
    }

    /// [`Self::acquire`] with the start itself injected, so a test can pause one.
    async fn acquire_with<F, Fut>(
        self: Rc<Self>,
        profile: EngineProfile,
        start: F,
    ) -> Result<Built, String>
    where
        F: FnOnce() -> Fut + 'static,
        Fut: Future<Output = Result<Built, String>> + 'static,
    {
        let key = key(&profile);
        let (launch, rx) = {
            let mut entries = self.entries.borrow_mut();
            if let Some(Entry::Ready(weak)) = entries.get(&key)
                && let Some(engine) = weak.upgrade()
            {
                return Ok((engine, None));
            }
            let (tx, rx) = oneshot::channel();
            let launch = match entries.get_mut(&key) {
                Some(Entry::Pending(pending)) => {
                    pending.waiters.push(tx);
                    false
                }
                // Missing, or a Ready whose last window has already dropped the engine.
                _ => {
                    entries.insert(
                        key.clone(),
                        Entry::Pending(InFlight {
                            waiters: vec![tx],
                            coordinator: None,
                        }),
                    );
                    true
                }
            };
            (launch, rx)
        };
        if launch {
            // Weak, not `Rc`: the coordinator must not keep the pool alive, or quit cannot
            // drop it — and abort this task — before the runtime goes.
            let pool = Rc::downgrade(&self);
            let finished = key.clone();
            let handle = glib::spawn_future_local(async move {
                let result = start().await;
                if let Some(pool) = pool.upgrade() {
                    pool.finish(&finished, result);
                }
            });
            if let Some(Entry::Pending(pending)) = self.entries.borrow_mut().get_mut(&key) {
                pending.coordinator = Some(handle);
            } else {
                handle.abort();
            }
        }

        rx.await
            .unwrap_or_else(|_| Err("engine startup was cancelled".to_string()))
    }

    /// Installs a finished start and wakes every waiter registered for it.
    ///
    /// Runs from the coordinator, so it still runs when every waiter has been dropped. The
    /// `Arc` in `result` is the only strong reference until a waiter receives its clone; if
    /// every `send` fails, that `Arc` drops here and the `Ready` weak is already dead. The
    /// next acquire starts again. Acceptable: nobody was left to own the engine.
    fn finish(&self, key: &str, result: Result<Built, String>) {
        let waiting = {
            let mut entries = self.entries.borrow_mut();
            match entries.remove(key) {
                Some(Entry::Pending(pending)) => {
                    // Detach. Aborting here would destroy the task that is running this
                    // function. Drop of the handle does not destroy an attached source.
                    drop(pending.coordinator);
                    pending.waiters
                }
                Some(other) => {
                    entries.insert(key.to_string(), other);
                    return;
                }
                None => return,
            }
        };
        if let Ok((engine, _)) = &result {
            self.entries
                .borrow_mut()
                .insert(key.to_string(), Entry::Ready(Arc::downgrade(engine)));
        }
        for tx in waiting {
            let _ = tx.send(result.clone());
        }
    }
}

impl Drop for EnginePool {
    fn drop(&mut self) {
        // Before the runtime. `release` drops the pool, then shuts the runtime down.
        // Aborting the coordinator drops `start`'s receiver, so the runtime task's send
        // fails and the engine drops on that side. LocalEngine then closes stdin; a
        // runtime drop would otherwise SIGKILL the child.
        for (_, entry) in std::mem::take(self.entries.get_mut()) {
            if let Entry::Pending(pending) = entry
                && let Some(handle) = pending.coordinator
            {
                handle.abort();
            }
        }
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
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::time::Duration;

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
        pool.entries
            .borrow_mut()
            .insert(key(&profile), Entry::Ready(Arc::downgrade(&engine)));
        assert!(pool.running(&profile).is_some());
        drop(engine);
        assert!(pool.running(&profile).is_none());
    }

    fn on_main<T>(f: impl Future<Output = T>) -> T {
        glib::MainContext::new().block_on(f)
    }

    /// A start the test can hold open. Panics if a second one runs: one profile, one start.
    fn paused(
        starts: &Rc<Cell<u32>>,
        release: &Rc<RefCell<Option<oneshot::Receiver<()>>>>,
    ) -> impl FnOnce() -> Pin<Box<dyn Future<Output = Result<Built, String>>>> + use<> {
        let starts = Rc::clone(starts);
        let release = Rc::clone(release);
        move || {
            let starts = Rc::clone(&starts);
            let release = Rc::clone(&release);
            Box::pin(async move {
                starts.set(starts.get() + 1);
                let rx = release
                    .borrow_mut()
                    .take()
                    .expect("a second start ran for one profile");
                rx.await.expect("the test dropped the start gate");
                Ok((Arc::new(Stub) as Arc<dyn Engine>, None))
            })
        }
    }

    async fn until_started(starts: &Cell<u32>) {
        for _ in 0..50 {
            if starts.get() > 0 {
                return;
            }
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        panic!("start never began");
    }

    /// Aborting the first acquire — what `activate_profile` does on a new selection — must
    /// not leave the entry pending, or the next acquire waits forever.
    #[test]
    fn dropping_a_waiter_mid_start_does_not_strand_the_profile() {
        on_main(async {
            let pool = Rc::new(EnginePool::default());
            let starts = Rc::new(Cell::new(0u32));
            let (gate_tx, gate_rx) = oneshot::channel();
            let release = Rc::new(RefCell::new(Some(gate_rx)));
            let profile = local("a", Some(16));

            let first = glib::spawn_future_local({
                let pool = Rc::clone(&pool);
                let starts = Rc::clone(&starts);
                let release = Rc::clone(&release);
                let profile = profile.clone();
                async move {
                    let _ = pool.acquire_with(profile, paused(&starts, &release)).await;
                }
            });
            until_started(&starts).await;
            first.abort();

            let second = glib::spawn_future_local({
                let pool = Rc::clone(&pool);
                let starts = Rc::clone(&starts);
                let release = Rc::clone(&release);
                let profile = profile.clone();
                async move { pool.acquire_with(profile, paused(&starts, &release)).await }
            });
            // Register on the in-flight start before it is allowed to finish.
            glib::timeout_future(Duration::from_millis(1)).await;
            assert_eq!(
                starts.get(),
                1,
                "dropping a waiter must not start a second engine"
            );
            gate_tx.send(()).expect("the start is still waiting");

            let joined = glib::future_with_timeout(Duration::from_secs(2), second)
                .await
                .expect("a later acquire hung: the dropped waiter left the profile Pending");
            let built = joined.expect("acquire task").expect("start");
            assert_eq!(starts.get(), 1);
            let running = pool
                .running(&profile)
                .expect("the finished start is installed");
            assert!(Arc::ptr_eq(&built.0, &running));
        });
    }

    /// Two windows asking while a profile is starting must share that start.
    #[test]
    fn two_concurrent_acquires_share_one_start() {
        on_main(async {
            let pool = Rc::new(EnginePool::default());
            let starts = Rc::new(Cell::new(0u32));
            let (gate_tx, gate_rx) = oneshot::channel();
            let release = Rc::new(RefCell::new(Some(gate_rx)));
            let profile = local("a", Some(16));

            let first = glib::spawn_future_local({
                let pool = Rc::clone(&pool);
                let starts = Rc::clone(&starts);
                let release = Rc::clone(&release);
                let profile = profile.clone();
                async move { pool.acquire_with(profile, paused(&starts, &release)).await }
            });
            let second = glib::spawn_future_local({
                let pool = Rc::clone(&pool);
                let starts = Rc::clone(&starts);
                let release = Rc::clone(&release);
                let profile = profile.clone();
                async move { pool.acquire_with(profile, paused(&starts, &release)).await }
            });

            until_started(&starts).await;
            assert_eq!(starts.get(), 1, "two waiters started two engines");
            gate_tx.send(()).expect("the start is still waiting");

            let first = glib::future_with_timeout(Duration::from_secs(2), first)
                .await
                .expect("first acquire hung")
                .expect("acquire task")
                .expect("start");
            let second = glib::future_with_timeout(Duration::from_secs(2), second)
                .await
                .expect("second acquire hung")
                .expect("acquire task")
                .expect("start");
            assert!(Arc::ptr_eq(&first.0, &second.0));
            assert_eq!(starts.get(), 1);
        });
    }

    /// Quit drops the pool, then the runtime. The runtime task must see its send fail,
    /// which is what drops the engine on that side so KataGo's stdin is closed.
    #[test]
    fn dropping_the_pool_mid_start_closes_the_start_receiver() {
        on_main(async {
            let pool = Rc::new(EnginePool::default());
            let (gate_tx, gate_rx) = oneshot::channel::<Result<Built, String>>();
            let (started_tx, started_rx) = oneshot::channel::<()>();
            let gate_rx = Rc::new(RefCell::new(Some(gate_rx)));
            let started_tx = Rc::new(RefCell::new(Some(started_tx)));

            let waiter = glib::spawn_future_local({
                let pool = Rc::clone(&pool);
                let gate_rx = Rc::clone(&gate_rx);
                let started_tx = Rc::clone(&started_tx);
                async move {
                    let _ = pool
                        .acquire_with(local("a", Some(16)), move || {
                            let gate_rx = Rc::clone(&gate_rx);
                            let started_tx = Rc::clone(&started_tx);
                            async move {
                                let _ = started_tx
                                    .borrow_mut()
                                    .take()
                                    .expect("start ran twice")
                                    .send(());
                                let rx = gate_rx
                                    .borrow_mut()
                                    .take()
                                    .expect("start receiver already taken");
                                rx.await
                                    .unwrap_or_else(|_| Err("start receiver dropped".to_string()))
                            }
                        })
                        .await;
                }
            });
            started_rx.await.expect("start never began");
            // Closing the window aborts the acquire; release then drops the last pool Rc.
            // `abort` only destroys the source. The future, and the pool `Rc` it holds, drops
            // with the handle.
            waiter.abort();
            drop(waiter);
            drop(pool);
            assert!(
                gate_tx
                    .send(Ok((Arc::new(Stub) as Arc<dyn Engine>, None)))
                    .is_err(),
                "the start receiver was still alive after the pool dropped"
            );
        });
    }
}
