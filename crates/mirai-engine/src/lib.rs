// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Engine drivers: one trait, two implementations.
//!
//! [`LocalEngine`] drives a KataGo `analysis` subprocess; [`RemoteEngine`] drives a
//! `mirai-server` over MRP/1. Both produce bit-identical [`Report`]s, so the GUI has a
//! single code path.

pub mod calibrate;
pub mod decode;
pub mod local;
pub mod query;
pub mod remote;
pub mod tuning;

use std::sync::Arc;

use tokio::sync::watch;

pub use calibrate::{
    CalibrationConfig, CalibrationProgress, CalibrationResult, CalibrationSample, calibrate,
};
pub use local::{LOCAL_ENGINE_SHUTDOWN_GRACE, LocalEngine, LocalEngineConfig};
pub use mirai_proto::types::{AnalyzeReq, AvoidSpec, EngineDesc, MoveInfo, Report, RootInfo, Want};
pub use remote::{RemoteEngine, TofuStore};
pub use tuning::EngineTuning;

/// Why an analysis stopped, or why an engine could not be used at all.
#[derive(Clone, Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine failed to start: {0}")]
    Startup(String),
    #[error("engine process exited: {0}")]
    EngineExited(String),
    #[error("query rejected: {0}")]
    Query(String),
    #[error("disconnected from remote engine: {0}")]
    Disconnected(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Other(String),
}

/// One state transition of a subscription.
///
/// A subscription is a `watch` channel, so a consumer that falls behind sees only the
/// newest report — superseded partial reports collapse automatically, which is exactly
/// what a 10 Hz analysis stream wants.
#[derive(Clone, Debug)]
pub enum SubEvent {
    /// No result yet.
    Pending,
    /// An intermediate result (`isDuringSearch == true`).
    Report(Arc<Report>),
    /// The final result. Terminal.
    Done(Arc<Report>),
    /// The analysis will not produce (further) results. Terminal.
    Failed(EngineError),
}

impl SubEvent {
    /// The report carried by this event, if any.
    pub fn report(&self) -> Option<&Arc<Report>> {
        match self {
            SubEvent::Report(r) | SubEvent::Done(r) => Some(r),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, SubEvent::Done(_) | SubEvent::Failed(_))
    }
}

/// Cancels the underlying query when the [`Subscription`] is dropped.
pub struct CancelGuard(Option<Box<dyn FnOnce() + Send + Sync>>);

impl CancelGuard {
    pub fn new(f: impl FnOnce() + Send + Sync + 'static) -> CancelGuard {
        CancelGuard(Some(Box::new(f)))
    }

    /// A guard that does nothing — for already-terminal subscriptions.
    pub fn noop() -> CancelGuard {
        CancelGuard(None)
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// A live analysis. Dropping it terminates the query in the engine.
pub struct Subscription {
    rx: watch::Receiver<SubEvent>,
    _cancel: CancelGuard,
}

impl Subscription {
    pub fn new(rx: watch::Receiver<SubEvent>, cancel: CancelGuard) -> Subscription {
        Subscription {
            rx,
            _cancel: cancel,
        }
    }

    /// A subscription that has already failed — for errors detected before dispatch.
    pub fn failed(err: EngineError) -> Subscription {
        let (tx, rx) = watch::channel(SubEvent::Pending);
        // A seeded value is already seen by `rx`; send the failure so `next()` observes it.
        tx.send_replace(SubEvent::Failed(err));
        // Keep the sender alive so subsequent `changed()` calls block instead of erroring.
        Subscription {
            rx,
            _cancel: CancelGuard::new(move || drop(tx)),
        }
    }

    /// The most recent event, without waiting.
    pub fn current(&self) -> SubEvent {
        self.rx.borrow().clone()
    }

    /// Waits for the next event. Returns `None` once the engine side is gone.
    pub async fn next(&mut self) -> Option<SubEvent> {
        self.rx.changed().await.ok()?;
        Some(self.rx.borrow_and_update().clone())
    }

    /// Drives the subscription to its terminal event.
    pub async fn finish(mut self) -> Result<Arc<Report>, EngineError> {
        loop {
            match self.current() {
                SubEvent::Done(r) => return Ok(r),
                SubEvent::Failed(e) => return Err(e),
                _ => {}
            }
            match self.next().await {
                Some(SubEvent::Done(r)) => return Ok(r),
                Some(SubEvent::Failed(e)) => return Err(e),
                Some(_) => {}
                None => {
                    return Err(EngineError::Other(
                        "subscription closed without a result".into(),
                    ));
                }
            }
        }
    }
}

/// A source of analysis. Implementations are cheap to clone behind an `Arc` and safe to
/// use from any thread.
pub trait Engine: Send + Sync + 'static {
    /// Starts an analysis. Synchronous by design: the caller gets a handle immediately and
    /// the work happens on the engine's own tasks.
    fn subscribe(&self, req: AnalyzeReq) -> Subscription;

    fn describe(&self) -> EngineDesc;
}

impl<T: Engine + ?Sized> Engine for Arc<T> {
    fn subscribe(&self, req: AnalyzeReq) -> Subscription {
        (**self).subscribe(req)
    }
    fn describe(&self) -> EngineDesc {
        (**self).describe()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn failed_subscription_is_immediately_observable() {
        let mut sub = Subscription::failed(EngineError::Other("dispatch failed".into()));

        assert!(matches!(
            &sub.current(),
            SubEvent::Failed(EngineError::Other(message)) if message == "dispatch failed"
        ));

        let event = tokio::time::timeout(Duration::from_secs(1), sub.next())
            .await
            .expect("failed event was not delivered")
            .expect("failed subscription closed");
        assert!(matches!(
            event,
            SubEvent::Failed(EngineError::Other(message)) if message == "dispatch failed"
        ));

        assert!(matches!(
            sub.finish().await,
            Err(EngineError::Other(message)) if message == "dispatch failed"
        ));
    }
}
