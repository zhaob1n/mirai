// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! What each report costs on the wire, measured on a real KataGo analysis stream.
//!
//! Feeds a capture of raw `katago analysis` output through the decoder the engines use,
//! then through the MRP frame codec, and prints the framed bytes per report for each step
//! of the request and wire format. Every frame is decoded straight back and compared, so a
//! row is only printed if it round-trips. How to capture: `docs/dev/TESTING.md` §4.
//!
//! ```text
//! cargo run --release -p mirai-engine --example wire_bench -- /tmp/mrp-capture.jsonl
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use mirai_core::Size;
use mirai_engine::Report;
use mirai_engine::decode::{RawResponse, decode_report};
use mirai_proto::frame::{self, FrameBuf, SubStreamDecoder, SubStreamEncoder};
use mirai_proto::msg::SubMsg;
use serde_json::Value;

#[derive(Parser, Debug)]
#[command(
    name = "wire_bench",
    about = "Framed bytes per report for a captured KataGo analysis stream"
)]
struct Args {
    /// Raw `katago analysis` stdout: one JSON response per line.
    capture: PathBuf,

    /// Board size (square) of the captured position.
    #[arg(long, default_value_t = 19)]
    size: u8,

    /// Candidates kept by the capped rows, as a live request's candidate cap would.
    #[arg(long, default_value_t = 10)]
    cap: u8,

    /// Keep the policy arrays, as with the policy overlay on. Stripped by default.
    #[arg(long)]
    policy: bool,

    /// zstd levels to try for the subscription-stream rows.
    #[arg(long, value_delimiter = ',', default_value = "1,3,6,9")]
    levels: Vec<i32>,
}

/// One analysis response, in capture order.
struct Frame {
    terminal: bool,
    report: Report,
}

struct Row {
    name: String,
    sizes: Vec<usize>,
    encode: Duration,
    decode: Duration,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let size = Size::square(args.size);
    let text = std::fs::read_to_string(&args.capture)
        .with_context(|| format!("reading {}", args.capture.display()))?;

    let mut frames = Vec::new();
    let (mut lines, mut json_bytes, mut parse) = (0usize, 0usize, Duration::ZERO);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let start = Instant::now();
        let value: Value = serde_json::from_str(line).context("a capture line is not JSON")?;
        let Ok(RawResponse::Analysis {
            terminal,
            no_results: false,
            body,
            ..
        }) = RawResponse::classify(value)
        else {
            continue;
        };
        let report = decode_report(size, None, &body).map_err(anyhow::Error::msg)?;
        parse += start.elapsed();
        lines += 1;
        json_bytes += line.len();
        frames.push(Frame { terminal, report });
    }
    if frames.is_empty() {
        bail!("no analysis responses in {}", args.capture.display());
    }
    if !args.policy {
        for f in &mut frames {
            f.report.policy = None;
        }
    }

    let n = frames.len();
    let candidates: usize = frames.iter().map(|f| f.report.moves.len()).sum();
    println!(
        "{} reports; JSON {:.0} B and {:.0} µs to decode per line; {:.1} candidates per report; policy {}",
        n,
        json_bytes as f64 / lines as f64,
        parse.as_secs_f64() * 1e6 / lines as f64,
        candidates as f64 / n as f64,
        if args.policy { "kept" } else { "stripped" },
    );

    let mut rows = vec![standalone("v1 live", &frames)];
    for f in &mut frames {
        for m in &mut f.report.moves {
            m.pv_visits.clear();
        }
    }
    rows.push(standalone("- pv_visits", &frames));
    for f in &mut frames {
        f.report.moves.truncate(usize::from(args.cap));
    }
    rows.push(standalone(&format!("+ cap {}", args.cap), &frames));
    for &level in &args.levels {
        rows.push(sub_stream(
            &format!("+ sub-stream L{level}"),
            &frames,
            level,
        ));
    }

    println!(
        "{:<22} {:>7} {:>7} {:>7} {:>7} {:>9} {:>7} {:>7}",
        "variant", "mean B", "p50", "p95", "first", "total KB", "enc µs", "dec µs"
    );
    for row in &rows {
        print_row(row);
    }
    Ok(())
}

fn message(f: &Frame) -> SubMsg {
    if f.terminal {
        SubMsg::Done(f.report.clone())
    } else {
        SubMsg::Report(f.report.clone())
    }
}

/// Every report as its own frame through the control-stream codec, which is how MRP/1
/// sent reports.
fn standalone(name: &str, frames: &[Frame]) -> Row {
    let (mut wbuf, mut rbuf) = (FrameBuf::new(), FrameBuf::new());
    let mut row = Row::new(name, frames.len());
    for f in frames {
        let msg = message(f);
        let start = Instant::now();
        let wire = frame::encode(&mut wbuf, &msg).expect("encode");
        row.encode += start.elapsed();
        let wire = wire.to_vec();
        let start = Instant::now();
        let back: SubMsg = frame::decode(&mut rbuf, &wire).expect("decode");
        row.decode += start.elapsed();
        assert_eq!(back, msg, "{name}: a frame did not round-trip");
        row.sizes.push(wire.len());
    }
    row
}

/// The whole capture as one subscription stream at `level`.
fn sub_stream(name: &str, frames: &[Frame], level: i32) -> Row {
    let mut enc = SubStreamEncoder::new(level).expect("stream encoder");
    let mut dec = SubStreamDecoder::new().expect("stream decoder");
    let mut row = Row::new(name, frames.len());
    for f in frames {
        let msg = message(f);
        let start = Instant::now();
        let wire = enc.encode(&msg).expect("encode");
        row.encode += start.elapsed();
        let start = Instant::now();
        let back: SubMsg = dec.decode(wire).expect("decode");
        row.decode += start.elapsed();
        assert_eq!(back, msg, "{name}: a frame did not round-trip");
        row.sizes.push(wire.len());
    }
    row
}

impl Row {
    fn new(name: &str, frames: usize) -> Row {
        Row {
            name: name.into(),
            sizes: Vec::with_capacity(frames),
            encode: Duration::ZERO,
            decode: Duration::ZERO,
        }
    }
}

fn print_row(row: &Row) {
    let n = row.sizes.len();
    let mut sorted = row.sizes.clone();
    sorted.sort_unstable();
    let total: usize = sorted.iter().sum();
    println!(
        "{:<22} {:>7.0} {:>7} {:>7} {:>7} {:>9.1} {:>7.1} {:>7.1}",
        row.name,
        total as f64 / n as f64,
        sorted[n / 2],
        sorted[(n * 95 / 100).min(n - 1)],
        row.sizes[0],
        total as f64 / 1024.0,
        row.encode.as_secs_f64() * 1e6 / n as f64,
        row.decode.as_secs_f64() * 1e6 / n as f64,
    );
}
