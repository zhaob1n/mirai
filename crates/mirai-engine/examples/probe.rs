// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Step 4 verification probe.
//!
//! Drives either a local KataGo process or a remote `mirai-server` through the *same*
//! [`Engine`] trait and prints every streamed report, so the two paths can be compared
//! line for line.
//!
//! ```text
//! cargo run -p mirai-engine --example probe -- \
//!     --katago $KATA --model $MODEL --config $CFG --visits 200
//! cargo run -p mirai-engine --example probe -- \
//!     --remote mirai://127.0.0.1:9678 --token <token>
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use mirai_core::{Color, Point, RuleSet, Size};
use mirai_engine::{
    AnalyzeReq, Engine, LocalEngine, LocalEngineConfig, RemoteEngine, Report, SubEvent, Want,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "probe",
    about = "Analyse one position with a local or remote mirai engine"
)]
struct Args {
    /// Path to the `katago` binary (local engine).
    #[arg(long, requires = "model", requires = "config", conflicts_with = "remote")]
    katago: Option<String>,

    /// Path to the KataGo model (`.bin.gz`).
    #[arg(long)]
    model: Option<String>,

    /// Path to the KataGo analysis config.
    #[arg(long)]
    config: Option<String>,

    /// `mirai://host:port` of a running `mirai-server` (remote engine).
    #[arg(long, requires = "token")]
    remote: Option<String>,

    /// Authentication token for `--remote`.
    #[arg(long)]
    token: Option<String>,

    /// Which of the server's engines to use; defaults to the server's first.
    #[arg(long)]
    engine: Option<String>,

    /// Pinned server certificate fingerprint (SHA-256, lowercase hex).
    #[arg(long)]
    fingerprint: Option<String>,

    /// Visit cap for the search.
    #[arg(long, default_value_t = 200)]
    visits: u32,

    /// Board size (square).
    #[arg(long, default_value_t = 19)]
    size: u8,

    /// Moves to replay before analysing, GTP, comma separated, alternating from Black.
    #[arg(long, default_value = "")]
    moves: String,

    #[arg(long, default_value_t = 7.5)]
    komi: f32,

    /// Ruleset, by KataGo name (`chinese`, `japanese`, `tromp-taylor`, ...).
    #[arg(long, default_value = "chinese")]
    rules: String,

    /// How often KataGo streams an intermediate report.
    #[arg(long, default_value_t = 100)]
    report_every_ms: u16,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode> {
    let args = Args::parse();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let size = Size::new(args.size, args.size)
        .with_context(|| format!("board size {} is out of range", args.size))?;
    let rules = RuleSet::from_katago_name(&args.rules)
        .with_context(|| format!("unknown ruleset {:?}", args.rules))?;
    let moves = parse_moves(size, &args.moves)?;

    let engine = build_engine(&args).await?;

    let desc = engine.describe();
    println!(
        "engine: {} katago {} model {} threads {} human_model {}",
        desc.name, desc.katago_version, desc.model, desc.analysis_threads, desc.has_human_model
    );

    let mut req = AnalyzeReq::new(size, rules, args.komi);
    req.moves = moves;
    req.max_visits = Some(args.visits);
    req.want = Want::OWNERSHIP | Want::PV_VISITS;
    req.report_every_ms = Some(args.report_every_ms);
    let to_play = req.to_play();

    let mut sub = engine.subscribe(req);
    loop {
        // Ctrl-C must drop the subscription AND the engine before the process dies:
        // an abruptly killed client never sends a QUIC CONNECTION_CLOSE, so the server
        // would only notice via the 30 s idle timeout.
        let event = tokio::select! {
            e = sub.next() => e,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("interrupted; closing the subscription");
                drop(sub);
                drop(engine);
                // Give quinn a moment to flush CONNECTION_CLOSE.
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                return Ok(ExitCode::FAILURE);
            }
        };
        let Some(event) = event else {
            bail!("subscription closed without a result");
        };
        match event {
            SubEvent::Pending => {}
            SubEvent::Report(r) => print_report("report", &r, size, to_play),
            SubEvent::Done(r) => {
                print_report("final", &r, size, to_play);
                return Ok(ExitCode::SUCCESS);
            }
            SubEvent::Failed(e) => {
                eprintln!("analysis failed: {e}");
                return Ok(ExitCode::FAILURE);
            }
        }
    }
}

/// Builds whichever engine the arguments selected, behind the shared trait object so the
/// rest of the program cannot tell them apart.
async fn build_engine(args: &Args) -> Result<Arc<dyn Engine>> {
    match (&args.katago, &args.remote) {
        (Some(katago), None) => {
            // clap's `requires` guarantees both are present.
            let model = args.model.as_deref().expect("--model required by --katago");
            let config = args.config.as_deref().expect("--config required by --katago");
            let cfg = LocalEngineConfig::new("local", katago, model, config);
            let engine = LocalEngine::spawn(cfg)
                .await
                .context("could not start the local KataGo engine")?;
            Ok(Arc::new(engine))
        }
        (None, Some(url)) => {
            let token = args.token.as_deref().expect("--token required by --remote");
            let engine = RemoteEngine::connect(
                url,
                token,
                args.engine.clone(),
                args.fingerprint.clone(),
            )
            .await
            .with_context(|| format!("could not connect to {url}"))?;
            tracing::info!(fingerprint = engine.fingerprint(), "server certificate");
            Ok(Arc::new(engine))
        }
        (None, None) => bail!("one of --katago or --remote is required"),
        (Some(_), Some(_)) => bail!("--katago and --remote are mutually exclusive"),
    }
}

/// `"D4,Q16"` → alternating moves starting with Black.
fn parse_moves(size: Size, spec: &str) -> Result<Vec<(Color, Point)>> {
    let mut out = Vec::new();
    for (i, tok) in spec.split(',').map(str::trim).filter(|s| !s.is_empty()).enumerate() {
        let p = size
            .from_gtp(tok)
            .with_context(|| format!("{tok:?} is not a coordinate on a {}x{} board", size.w, size.h))?;
        let color = if i % 2 == 0 { Color::Black } else { Color::White };
        out.push((color, p));
    }
    Ok(out)
}

/// One report block: the root line, then the top five candidates in `order`.
fn print_report(kind: &str, report: &Report, size: Size, to_play: Color) {
    let ownership = report.ownership.as_ref().map_or(0, Vec::len);
    println!("[{kind}] visits={} ownership={ownership}", report.root.visits);
    for m in report.moves.iter().take(5) {
        println!(
            "  {} {:.2} {:+.2} {}",
            size.to_gtp(m.mv),
            m.winrate_for(to_play) * 100.0,
            m.score_lead_for(to_play),
            m.visits
        );
    }
}
