// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! End-to-end session behaviour of [`RemoteEngine`], driven by a scripted MRP/2 server over
//! the real QUIC transport.
//!
//! `remote.rs`'s own unit tests cover what can be checked without a peer: the backoff
//! schedule, engine selection, the TOFU store, and failing to reach an address. These cover
//! the rules in `docs/dev/PROTOCOL.md` §8 that only appear once two endpoints are talking,
//! and that a client cannot get right by accident:
//!
//! * §8.1/5 — messages before `Welcome` are ignored, not fatal.
//! * §8.1 — `Welcome.proto != 1` is unusable and no `Open` follows.
//! * §8.2, §8.6 — `Unauthorized` is `Failed`, never a healthy session.
//! * §8.3 — `Opened` and the subscription stream are independent, in either order.
//! * §8.3 — a clean EOF with no `Done`/`Failed` fails the subscription.
//! * §9.1 — `Error { Some(sub) }` fails that subscription only; the connection survives.
//! * §8.4 — cancelling sends `STOP_SENDING` *and* `Cancel`.
//! * §8.6 — connection loss fails every live subscription and replays nothing.
#![cfg(feature = "remote")]

use std::time::Duration;

use mirai_core::{RuleSet, Size};
use mirai_engine::{AnalyzeReq, Engine, EngineError, RemoteEngine, SubEvent};
use mirai_proto::msg::{ErrCode, ServerMsg};

mod support;

use support::{Script, Seen, TestServer};

/// Every await in these tests is bounded, so a broken rule fails instead of hanging the run.
const PATIENCE: Duration = Duration::from_secs(10);

fn a_request() -> AnalyzeReq {
    AnalyzeReq::new(Size { w: 19, h: 19 }, RuleSet::Chinese, 7.5)
}

async fn connect(server: &TestServer) -> Result<RemoteEngine, EngineError> {
    tokio::time::timeout(
        PATIENCE,
        RemoteEngine::connect(&server.url, "tok", None, Some(server.fingerprint.clone())),
    )
    .await
    .expect("connect hung")
}

/// Drives a subscription until it produces a terminal event.
async fn finish(sub: mirai_engine::Subscription) -> Result<u32, EngineError> {
    match tokio::time::timeout(PATIENCE, sub.finish()).await {
        Ok(Ok(report)) => Ok(report.root.visits),
        Ok(Err(e)) => Err(e),
        Err(_) => panic!("subscription never reached a terminal event"),
    }
}

#[tokio::test]
async fn a_hello_is_answered_and_the_first_engine_is_resolved() {
    let mut server = TestServer::start(Script::offering(&["default", "big"])).await;
    let engine = connect(&server).await.expect("handshake failed");

    match server.next().await {
        Seen::Control(mirai_proto::msg::ClientMsg::Hello { proto, token, .. }) => {
            assert_eq!(proto, mirai_proto::types::PROTO_VERSION);
            assert_eq!(token, "tok", "the token is sent verbatim");
        }
        other => panic!("expected Hello, saw {other:?}"),
    }
    assert_eq!(engine.describe().name, "default");
    assert!(engine.connected());
    assert_eq!(engine.fingerprint(), server.fingerprint);
}

/// §8.1 rule 5: a client that failed on unexpected pre-`Welcome` frames could not talk to a
/// server that ever sends anything unsolicited.
#[tokio::test]
async fn frames_before_welcome_are_ignored_rather_than_fatal() {
    let script = Script {
        preamble: vec![
            ServerMsg::Pong(42),
            ServerMsg::Engines(vec![mirai_engine::EngineDesc::placeholder("noise")]),
        ],
        ..Script::offering(&["default"])
    };
    let mut server = TestServer::start(script).await;

    let engine = connect(&server)
        .await
        .expect("noise before Welcome was fatal");
    assert_eq!(engine.describe().name, "default");
    assert!(matches!(server.next().await, Seen::Control(_)));
}

/// §8.1: a peer that offered `mirai/2` without implementing it must not be analysed against.
#[tokio::test]
async fn a_welcome_with_the_wrong_protocol_version_is_refused() {
    let script = Script {
        proto: mirai_proto::types::PROTO_VERSION + 1,
        ..Script::offering(&["default"])
    };
    let server = TestServer::start(script).await;

    let err = connect(&server)
        .await
        .expect_err("a future version was accepted");
    assert!(matches!(err, EngineError::Protocol(_)), "{err}");
    let msg = err.to_string();
    assert!(msg.contains("MRP/"), "{msg}");
}

/// §8.2 and §8.6: a rejected token is not a transient fault, so it must surface as an error
/// from `connect` rather than a session that looks healthy.
#[tokio::test]
async fn a_rejected_token_fails_the_connection() {
    let server = TestServer::start(Script::rejecting(ErrCode::Unauthorized, "no")).await;

    let err = connect(&server)
        .await
        .expect_err("an unauthorized client connected");
    let msg = err.to_string();
    assert!(msg.contains("unauthorized"), "{msg}");
}

/// §8.3: "`Opened` and the subscription stream are independent; neither side may require one
/// before the other." This is the ordering a threaded server produces under load.
#[tokio::test]
async fn a_subscription_stream_arriving_before_opened_is_still_paired() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let sub = engine.subscribe(a_request());
    let (id, _req) = server.next_open().await;

    // Stream first, `Opened` second — the reverse of the documented diagram.
    server.open_stream(id);
    server.report(id, 5);
    server.control(ServerMsg::Opened { sub: id });
    server.done(id, 11);

    assert_eq!(finish(sub).await.expect("subscription failed"), 11);
}

/// §8.3: "Treat a subscription stream that reaches clean EOF without `Done`/`Failed` as
/// failed — never as success, never waiting indefinitely."
#[tokio::test]
async fn a_stream_that_ends_without_a_result_fails_the_subscription() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let sub = engine.subscribe(a_request());
    let (id, _req) = server.next_open().await;
    server.control(ServerMsg::Opened { sub: id });
    server.open_stream(id);
    server.report(id, 3);
    server.finish_without_result(id);

    let err = finish(sub)
        .await
        .expect_err("a truncated stream counted as success");
    assert!(
        matches!(err, EngineError::Protocol(_) | EngineError::Disconnected(_)),
        "{err}"
    );
}

/// §9.1: `SubMsg::Failed` on the subscription stream — the search itself failed — is
/// terminal for that subscription and carries the server's reason to the user.
#[tokio::test]
async fn a_failed_search_is_terminal_and_keeps_its_reason() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let sub = engine.subscribe(a_request());
    let (id, _req) = server.next_open().await;
    server.control(ServerMsg::Opened { sub: id });
    server.open_stream(id);
    server.fail(id, "katago exited");

    let err = finish(sub)
        .await
        .expect_err("a failed search reported success");
    assert!(err.to_string().contains("katago exited"), "{err}");
    assert!(
        engine.connected(),
        "one failed search killed the connection"
    );
}

/// §9.1: `Error { Some(sub) }` is scoped to that subscription. The connection, and every
/// other subscription on it, must survive.
#[tokio::test]
async fn a_per_subscription_error_spares_the_connection() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let doomed = engine.subscribe(a_request());
    let (first, _) = server.next_open().await;
    let survivor = engine.subscribe(a_request());
    let (second, _) = server.next_open().await;
    assert_ne!(first, second, "ids must be unique among live subscriptions");

    server.control(ServerMsg::Error {
        sub: Some(first),
        code: ErrCode::EngineFailed,
        msg: "search died".into(),
    });
    server.control(ServerMsg::Opened { sub: second });
    server.open_stream(second);
    server.done(second, 9);

    let err = finish(doomed)
        .await
        .expect_err("the named subscription survived");
    assert!(err.to_string().contains("search died"), "{err}");
    assert_eq!(finish(survivor).await.expect("bystander was failed"), 9);
    assert!(
        engine.connected(),
        "a scoped error tore down the connection"
    );
}

/// §8.4 (INV-3): cancellation is propagated on both channels, `STOP_SENDING` first, so
/// reports already in the receive window are discarded instead of delivered.
#[tokio::test]
async fn cancelling_stops_the_stream_and_tells_the_server() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let sub = engine.subscribe(a_request());
    let (id, _req) = server.next_open().await;
    server.control(ServerMsg::Opened { sub: id });
    server.open_stream(id);
    server.report(id, 4);

    // Observe the first report so the stream is definitely live, then cancel by dropping.
    let mut sub = sub;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if matches!(sub.current(), SubEvent::Report(r) if r.root.visits == 4) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no report arrived");
        let _ = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
    }
    drop(sub);

    // Both signals must arrive; the order between channels is not observable from here, but
    // each one is required. Writing to the stopped stream is how the server learns.
    let mut saw_stop = false;
    let mut saw_cancel = false;
    while !(saw_stop && saw_cancel) {
        server.report(id, 5);
        match server.next().await {
            Seen::Stopped { sub: s, code } => {
                assert_eq!(s, id);
                assert_eq!(code, 1, "STOP_SENDING must carry application code 1");
                saw_stop = true;
            }
            Seen::Control(mirai_proto::msg::ClientMsg::Cancel { sub: s }) => {
                assert_eq!(s, id);
                saw_cancel = true;
            }
            Seen::Control(_) => {}
            Seen::Gone => panic!("the connection died instead of cancelling one subscription"),
        }
    }
}

/// §8.6: "On connection loss both endpoints treat every live subscription on it as failed"
/// and subscriptions are "never replayed, resumed or reissued after a reconnect".
#[tokio::test]
async fn losing_the_connection_fails_every_live_subscription() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let first = engine.subscribe(a_request());
    let (a, _) = server.next_open().await;
    let second = engine.subscribe(a_request());
    let (b, _) = server.next_open().await;
    server.control(ServerMsg::Opened { sub: a });
    server.control(ServerMsg::Opened { sub: b });

    server.kill();

    for sub in [first, second] {
        let err = finish(sub)
            .await
            .expect_err("a live subscription survived the outage");
        assert!(matches!(err, EngineError::Disconnected(_)), "{err}");
    }
    assert!(
        !engine.connected(),
        "the engine still claims to be connected"
    );

    // Nothing is reissued: the only thing a reconnect may produce is a fresh `Hello`, never
    // an `Open` for work the user is no longer waiting for.
    let mut sub = engine.subscribe(a_request());
    let event = tokio::time::timeout(PATIENCE, sub.next())
        .await
        .expect("a request made while down must fail immediately")
        .expect("subscription closed");
    assert!(
        matches!(event, SubEvent::Failed(EngineError::Disconnected(_))),
        "{event:?}"
    );
}
