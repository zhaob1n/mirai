// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::RefCell;
use std::rc::Rc;

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::window_shell::MiraiWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MiraiApplication {
        pub runtime: RefCell<Option<tokio::runtime::Runtime>>,
        pub engines: RefCell<Option<Rc<crate::engines::EnginePool>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MiraiApplication {
        const NAME: &'static str = "MiraiApplication";
        type Type = super::MiraiApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for MiraiApplication {
        /// Last resort only. The primary instance releases through
        /// [`ApplicationImpl::shutdown`]; a remote invocation never starts up, so that is
        /// the one case which arrives here instead.
        fn dispose(&self) {
            self.release();
        }
    }

    impl ApplicationImpl for MiraiApplication {
        fn startup(&self) {
            self.parent_startup();
            self.obj().watch_termination_signals();
        }

        /// The single exit path. `gio::Application::quit` is documented to run this before
        /// `run` returns, and GTK runs it when the last window goes, so closing a window,
        /// the harness `quit` step and a termination signal all release identically.
        fn shutdown(&self) {
            // Windows first. Each one flushes its comment, saves the config, deletes its
            // autosave and drops its engine reference, so no engine is still held when the
            // runtime stops.
            for window in self.obj().windows() {
                if let Some(window) = window.downcast_ref::<MiraiWindow>() {
                    window.shutdown();
                }
                window.destroy();
            }
            self.release();
            self.parent_shutdown();
        }
    }
    impl GtkApplicationImpl for MiraiApplication {}
    impl AdwApplicationImpl for MiraiApplication {}

    impl MiraiApplication {
        /// Drops the shared engines and stops the runtime. Idempotent: both are taken.
        ///
        /// The pool goes first. Dropping it aborts any start still in flight, which drops
        /// that start's receiver before the runtime stops, so the engine is dropped on the
        /// runtime side and KataGo's stdin is closed. Reversing the two leaves the child
        /// to `kill_on_drop`.
        fn release(&self) {
            self.engines.borrow_mut().take();
            if let Some(runtime) = self.runtime.borrow_mut().take() {
                // The engine's own grace period plus a margin, so a KataGo leaving through
                // its closed stdin finishes that way instead of being killed on drop.
                runtime.shutdown_timeout(
                    mirai_engine::LOCAL_ENGINE_SHUTDOWN_GRACE
                        + std::time::Duration::from_millis(250),
                );
            }
        }
    }
}

glib::wrapper! {
    pub struct MiraiApplication(ObjectSubclass<imp::MiraiApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl MiraiApplication {
    pub fn new(application_id: &str, flags: gio::ApplicationFlags) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("mirai-rt")
            .build()
            .expect("could not start the tokio runtime");
        let this: Self = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .build();
        this.imp().runtime.replace(Some(runtime));
        this.imp()
            .engines
            .replace(Some(Rc::new(crate::engines::EnginePool::default())));
        this
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.imp()
            .runtime
            .borrow()
            .as_ref()
            .expect("application runtime has shut down")
            .handle()
            .clone()
    }

    pub fn engine_pool(&self) -> Rc<crate::engines::EnginePool> {
        self.imp()
            .engines
            .borrow()
            .as_ref()
            .expect("application engine pool has shut down")
            .clone()
    }

    /// Routes termination requests into the ordinary quit path, so Ctrl-C, a closed
    /// terminal or console, and (on Unix) session logout release exactly like closing the
    /// last window.
    ///
    /// The engine already survives none of these — KataGo leaves when its stdin closes —
    /// but without this the process dies at the default disposition, taking no autosave
    /// deletion, no comment flush and no config save with it.
    fn watch_termination_signals(&self) {
        self.runtime_handle().spawn(async move {
            let mut requests = match TerminationRequests::install() {
                Ok(requests) => requests,
                Err(error) => {
                    tracing::warn!(%error, "could not install termination signal handlers");
                    return;
                }
            };
            let mut asked = false;
            loop {
                requests.recv().await;
                if asked {
                    // Asked twice: the orderly path is wedged, so stop being polite.
                    tracing::warn!("second termination signal, exiting immediately");
                    std::process::exit(130);
                }
                asked = true;
                tracing::info!("termination signal, shutting down");
                // Hop to the GTK thread. `idle_add_once` always queues onto the default
                // main context, where `MainContext::invoke` would sometimes run the closure
                // inline on this tokio worker instead.
                glib::idle_add_once(|| {
                    if let Some(app) = gio::Application::default() {
                        app.quit();
                    }
                });
            }
        });
    }
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

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
        }
    }
}

/// Ctrl-C, Ctrl-Break and a closed console window.
///
/// Only a console process receives any of them, so this matters for a debug build run
/// from a terminal; the release build is a GUI-subsystem program with no console. A
/// logoff or shutdown event is never delivered to an interactive program, so it is not
/// listened for. For a closed console, Tokio parks the handler thread and Windows allows
/// the process five seconds to finish quitting.
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

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.ctrl_c.recv() => {}
            _ = self.ctrl_break.recv() => {}
            _ = self.ctrl_close.recv() => {}
        }
    }
}
