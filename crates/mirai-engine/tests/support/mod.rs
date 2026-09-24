// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! A scripted MRP/2 server for driving [`RemoteEngine`] through the session rules in
//! `docs/dev/PROTOCOL.md` §8.
//!
//! It speaks the real protocol over the real transport — same QUIC endpoint helper, same
//! frame codec, same self-signed certificate path as `mirai-server` — but it is scripted
//! rather than engine-backed, so a test can produce sequences a correct server would never
//! produce: a subscription stream before `Opened`, a clean EOF with no result, an error
//! naming one subscription while the connection stays up.
//!
//! Everything is bounded by `next_*` awaits on channels, so a rule that stops holding makes
//! a test hang rather than pass; run with `--test-threads` as you like, each server binds an
//! ephemeral port of its own.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};

use mirai_proto::frame::{self, FrameBuf, SUB_STREAM_LEVEL, SubStreamEncoder};
use mirai_proto::msg::{ClientMsg, ErrCode, OwnershipDelta, ServerMsg, SubMsg, SubMsgRef};
use mirai_proto::transport;
use mirai_proto::types::{AnalyzeReq, EngineDesc, PROTO_VERSION, Report};
use quinn::VarInt;
use tokio::sync::mpsc;

/// What the server does during the handshake, and what it claims afterwards.
pub struct Script {
    pub proto: u16,
    pub engines: Vec<EngineDesc>,
    /// Frames sent before `Welcome`. §8.1 rule 5: the client must ignore these.
    pub preamble: Vec<ServerMsg>,
    /// Reply with this instead of `Welcome`, then close with application code 1, as §8.6
    /// requires of a server rejecting a handshake.
    pub reject: Option<(ErrCode, String)>,
}

impl Default for Script {
    fn default() -> Script {
        Script {
            proto: PROTO_VERSION,
            engines: vec![EngineDesc::placeholder("default")],
            preamble: Vec::new(),
            reject: None,
        }
    }
}

impl Script {
    /// A well-behaved server offering `names`, first one first.
    pub fn offering(names: &[&str]) -> Script {
        Script {
            engines: names.iter().map(|n| EngineDesc::placeholder(*n)).collect(),
            ..Script::default()
        }
    }

    pub fn rejecting(code: ErrCode, msg: &str) -> Script {
        Script {
            reject: Some((code, msg.to_string())),
            ..Script::default()
        }
    }
}

/// Something the server observed. Tests await these, never sleep.
#[derive(Debug)]
pub enum Seen {
    /// A control-stream message from the client.
    Control(ClientMsg),
    /// The client sent `STOP_SENDING` for a subscription stream, with this application code.
    Stopped { sub: u32, code: u64 },
    /// The control stream reached EOF, or the connection went away.
    Gone,
}

enum Cmd {
    Control(ServerMsg),
    OpenStream(u32),
    Sub(u32, SubMsg),
    Finish(u32),
    Close(u32, Vec<u8>),
}

pub struct TestServer {
    pub url: String,
    pub fingerprint: String,
    seen: mpsc::UnboundedReceiver<Seen>,
    cmd: mpsc::UnboundedSender<Cmd>,
    /// Kept alive so the OS does not hand the port to another test mid-run.
    _endpoint: quinn::Endpoint,
}

impl TestServer {
    /// Binds an ephemeral port and serves exactly one connection.
    pub async fn start(script: Script) -> TestServer {
        static NTH: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mirai-engine-test-{}-{}",
            std::process::id(),
            NTH.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (certs, key) = transport::load_or_generate_cert(
            &dir.join("cert.pem"),
            &dir.join("key.pem"),
            &["localhost".to_string()],
        )
        .expect("self-signed certificate");
        let fingerprint = transport::fingerprint_of(&certs);

        let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let endpoint = transport::server_endpoint(listen, certs, key).expect("server endpoint");
        let port = endpoint.local_addr().expect("bound address").port();

        let (seen_tx, seen_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(serve(endpoint.clone(), script, seen_tx, cmd_rx));

        TestServer {
            url: format!("mirai://127.0.0.1:{port}"),
            fingerprint,
            seen: seen_rx,
            cmd: cmd_tx,
            _endpoint: endpoint,
        }
    }

    /// The next thing the server observed. Panics rather than hanging forever.
    pub async fn next(&mut self) -> Seen {
        match tokio::time::timeout(std::time::Duration::from_secs(10), self.seen.recv()).await {
            Ok(Some(seen)) => seen,
            Ok(None) => panic!("the server task ended"),
            Err(_) => panic!("the client never did anything the server could observe"),
        }
    }

    /// The next control message, asserting it is an `Open`, and returning its parts.
    pub async fn next_open(&mut self) -> (u32, AnalyzeReq) {
        match self.next().await {
            Seen::Control(ClientMsg::Open { sub, req, .. }) => (sub, req),
            other => panic!("expected Open, saw {other:?}"),
        }
    }

    pub fn control(&self, msg: ServerMsg) {
        let _ = self.cmd.send(Cmd::Control(msg));
    }

    /// Opens the subscription's unidirectional stream and writes its 4-byte LE id preamble.
    pub fn open_stream(&self, sub: u32) {
        let _ = self.cmd.send(Cmd::OpenStream(sub));
    }

    pub fn report(&self, sub: u32, visits: u32) {
        let _ = self
            .cmd
            .send(Cmd::Sub(sub, SubMsg::Report(report_of(visits))));
    }

    /// An intermediate report of `visits` carrying `ownership`, which the stream sends as
    /// the change from its previous map, as a real server does.
    pub fn report_with_ownership(&self, sub: u32, visits: u32, ownership: Vec<i8>) {
        let mut report = report_of(visits);
        report.ownership = Some(ownership);
        let _ = self.cmd.send(Cmd::Sub(sub, SubMsg::Report(report)));
    }

    pub fn done(&self, sub: u32, visits: u32) {
        let _ = self
            .cmd
            .send(Cmd::Sub(sub, SubMsg::Done(report_of(visits))));
        let _ = self.cmd.send(Cmd::Finish(sub));
    }

    pub fn fail(&self, sub: u32, why: &str) {
        let _ = self
            .cmd
            .send(Cmd::Sub(sub, SubMsg::Failed(why.to_string())));
        let _ = self.cmd.send(Cmd::Finish(sub));
    }

    /// Finishes the stream cleanly without ever sending `Done` or `Failed` — §8.3 requires
    /// the client to treat this as a failure.
    pub fn finish_without_result(&self, sub: u32) {
        let _ = self.cmd.send(Cmd::Finish(sub));
    }

    /// Closes the QUIC connection under the client, as a crashed or restarted server would.
    pub fn kill(&self) {
        let _ = self.cmd.send(Cmd::Close(7, b"gone".to_vec()));
    }
}

/// A report distinguishable by its visit count, which is all these tests compare.
fn report_of(visits: u32) -> Report {
    let mut report = Report::empty(0, mirai_core::Color::Black);
    report.root.visits = visits;
    report
}

async fn serve(
    endpoint: quinn::Endpoint,
    script: Script,
    seen: mpsc::UnboundedSender<Seen>,
    mut cmd: mpsc::UnboundedReceiver<Cmd>,
) {
    let Some(incoming) = endpoint.accept().await else {
        return;
    };
    let Ok(conn) = incoming.await else {
        return;
    };
    let Ok((mut tx, mut rx)) = conn.accept_bi().await else {
        return;
    };

    let mut rbuf = FrameBuf::new();
    let mut wbuf = FrameBuf::new();

    // §8.1 rule 2: the first frame is `Hello`.
    match frame::read_msg::<_, ClientMsg>(&mut rx, &mut rbuf).await {
        Ok(hello) => {
            let _ = seen.send(Seen::Control(hello));
        }
        Err(_) => return,
    }

    for msg in &script.preamble {
        let _ = frame::write_msg(&mut tx, &mut wbuf, msg).await;
    }

    if let Some((code, msg)) = script.reject {
        let _ = frame::write_msg(
            &mut tx,
            &mut wbuf,
            &ServerMsg::Error {
                sub: None,
                code,
                msg,
            },
        )
        .await;
        // §8.6: flush and finish before closing, or the explanation is discarded.
        let _ = tx.finish();
        conn.closed().await;
        conn.close(VarInt::from_u32(1), code.as_str().as_bytes());
        return;
    }

    let _ = frame::write_msg(
        &mut tx,
        &mut wbuf,
        &ServerMsg::Welcome {
            proto: script.proto,
            server: "mirai-test/1".to_string(),
            session: 1,
            engines: script.engines.clone(),
        },
    )
    .await;

    // The control reader gets its own task: `read_msg` is not cancel-safe, so it must not
    // share a `select!` with the command loop.
    let reader = tokio::spawn({
        let seen = seen.clone();
        async move {
            loop {
                match frame::read_msg::<_, ClientMsg>(&mut rx, &mut rbuf).await {
                    Ok(msg) => {
                        if seen.send(Seen::Control(msg)).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = seen.send(Seen::Gone);
                        return;
                    }
                }
            }
        }
    });

    let mut streams: HashMap<u32, SubStream> = HashMap::new();

    while let Some(command) = cmd.recv().await {
        match command {
            Cmd::Control(msg) => {
                let _ = frame::write_msg(&mut tx, &mut wbuf, &msg).await;
            }
            Cmd::OpenStream(sub) => {
                let Ok(mut stream) = conn.open_uni().await else {
                    break;
                };
                // The 4-byte LE subscription id prefix, then `SubMsg` frames.
                // quinn's own `write_all`, so this module needs no tokio io features.
                if stream.write_all(&sub.to_le_bytes()).await.is_err() {
                    break;
                }
                let enc = SubStreamEncoder::new(SUB_STREAM_LEVEL).expect("stream encoder");
                let own = OwnershipDelta::default();
                streams.insert(sub, SubStream { stream, enc, own });
            }
            Cmd::Sub(sub, msg) => {
                let Some(s) = streams.get_mut(&sub) else {
                    continue;
                };
                if let Err(quinn::WriteError::Stopped(code)) = frame_sub(s, &msg).await {
                    let _ = seen.send(Seen::Stopped {
                        sub,
                        code: code.into_inner(),
                    });
                    streams.remove(&sub);
                }
            }
            Cmd::Finish(sub) => {
                if let Some(SubStream { mut stream, .. }) = streams.remove(&sub) {
                    let _ = stream.finish();
                }
            }
            Cmd::Close(code, reason) => {
                conn.close(VarInt::from_u32(code), &reason);
                break;
            }
        }
    }

    reader.abort();
}

/// One subscription stream's sending end. Each stream is its own zstd stream and carries
/// its own ownership deltas, so each has its own encoder and delta state.
struct SubStream {
    stream: quinn::SendStream,
    enc: SubStreamEncoder,
    own: OwnershipDelta,
}

/// Writes one `SubMsg`, surfacing `STOP_SENDING` as a `WriteError` the caller can report.
async fn frame_sub(s: &mut SubStream, msg: &SubMsg) -> Result<(), quinn::WriteError> {
    let wire = match msg {
        SubMsg::Report(r) => SubMsgRef::Report(s.own.report(r)),
        SubMsg::Done(r) => SubMsgRef::Done(s.own.report(r)),
        SubMsg::Failed(why) => SubMsgRef::Failed(why),
    };
    let frame = s.enc.encode(&wire).expect("encode SubMsg");
    s.stream.write_all(frame).await
}
