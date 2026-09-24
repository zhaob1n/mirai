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
//! * §8.1 — a `Welcome.proto` other than this client's is unusable and no `Open` follows.
//! * §8.2, §8.6 — `Unauthorized` is `Failed`, never a healthy session.
//! * §8.3 — `Opened` and the subscription stream are independent, in either order.
//! * §8.3 — a clean EOF with no `Done`/`Failed` fails the subscription.
//! * §9.1 — `Error { Some(sub) }` fails that subscription only; the connection survives.
//! * §8.4 — cancelling sends `STOP_SENDING` *and* `Cancel`.
//! * §7.11 — ownership sent as changes arrives as the map itself, report after report.
//! * §7.1 — a report that does not fit the requested board fails its subscription.
//! * §8.6 — connection loss fails every live subscription and replays nothing.
#![cfg(feature = "remote")]

use std::time::Duration;

use mirai_core::{Point, RuleSet, Size};
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

/// §7.11: a subscription stream carries each ownership map as the change from the one
/// before it. What a subscriber sees must be the map itself, every time.
#[tokio::test]
async fn ownership_arrives_whole_although_the_stream_sends_changes() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let mut sub = engine.subscribe(a_request());
    let (id, _req) = server.next_open().await;
    server.control(ServerMsg::Opened { sub: id });
    server.open_stream(id);

    for k in 0..3u32 {
        let map: Vec<i8> = (0..361u32)
            .map(|i| (((i * 7 + k * 50) % 255) as i16 - 127) as i8)
            .collect();
        server.report_with_ownership(id, k + 1, map.clone());
        // One at a time: the subscription keeps only the newest report.
        let deadline = tokio::time::Instant::now() + PATIENCE;
        let got = loop {
            if let SubEvent::Report(r) = sub.current()
                && r.root.visits == k + 1
            {
                break r;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "report {k} never arrived"
            );
            let _ = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
        };
        assert_eq!(got.ownership.as_deref(), Some(&map[..]), "report {k}");
    }
}

/// §7.1: a server can put any `u16` in a report, and the GUI labels and indexes by it. A
/// report that does not fit the requested board is never delivered: its subscription fails
/// and is cancelled as §8.4 cancels, with `STOP_SENDING` and a `Cancel` that lets the server
/// stop the search and free its slot, while the connection and a well-formed search carry on.
#[tokio::test]
async fn a_report_that_does_not_fit_the_board_fails_its_subscription() {
    let mut server = TestServer::start(Script::offering(&["default"])).await;
    let engine = connect(&server).await.expect("handshake failed");
    let _hello = server.next().await;

    let open = async |server: &mut TestServer| {
        let sub = engine.subscribe(a_request());
        let (id, _req) = server.next_open().await;
        server.control(ServerMsg::Opened { sub: id });
        server.open_stream(id);
        (sub, id)
    };

    let (sub, id) = open(&mut server).await;
    server.report_with_move(id, 1, Point(360), vec![Point(360), Point::PASS]);
    server.done(id, 2);
    assert_eq!(finish(sub).await.expect("a fitting report was refused"), 2);

    for bad in 0..3 {
        let (sub, id) = open(&mut server).await;
        match bad {
            0 => server.report_with_move(id, 1, Point(361), vec![Point(361)]),
            1 => server.report_with_move(id, 1, Point(0), vec![Point(0), Point(u16::MAX - 1)]),
            _ => server.report_with_ownership(id, 1, vec![0; 360]),
        }
        let err = finish(sub)
            .await
            .expect_err("a report off the board was delivered");
        assert!(matches!(err, EngineError::Protocol(_)), "case {bad}: {err}");
        // A write only fails once `STOP_SENDING` has arrived, so keep reporting until it has.
        let (mut saw_stop, mut saw_cancel) = (false, false);
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while !(saw_stop && saw_cancel) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "case {bad}: stopped {saw_stop}, cancelled {saw_cancel}"
            );
            if !saw_stop {
                server.report(id, 3);
            }
            match tokio::time::timeout(Duration::from_millis(200), server.next()).await {
                Ok(Seen::Stopped { sub: s, code }) => {
                    assert_eq!((s, code), (id, 1), "case {bad}");
                    saw_stop = true;
                }
                Ok(Seen::Control(mirai_proto::msg::ClientMsg::Cancel { sub: s })) => {
                    assert_eq!(s, id, "case {bad}");
                    saw_cancel = true;
                }
                Ok(Seen::Control(_)) | Err(_) => {}
                Ok(Seen::Gone) => panic!("case {bad}: the connection died"),
            }
        }
    }
    assert!(engine.connected());
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
