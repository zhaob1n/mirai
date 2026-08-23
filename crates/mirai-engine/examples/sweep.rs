// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Grades every n-th position of a record and prints one CSV row per candidate.
//!
//! The board colours a candidate by how much the move loses, and the breakpoints of that
//! ramp (`crates/mirai/src/palette.rs`) have to be fitted to real games rather than
//! guessed. This dumps the raw material: how far apart the engine's own top choices are in
//! win rate, in score lead *and* in the utility the ramp is actually keyed on, how much a
//! tail move with a handful of visits differs from the pick, and what a decided position
//! looks like once the win rate has saturated but the score lead has not.
//!
//! ```text
//! cargo run -p mirai-engine --example sweep -- \
//!     --katago ~/.local/bin/katago --model ~/.katago/models/model.bin.gz \
//!     --visits 1000 --every 5 game.sgf > sweep.csv
//! ```
//!
//! Losses are relative to `moves[0]`, KataGo's own pick, and are in the mover's
//! perspective, exactly as the GUI reads them. `dutil` is the mean-utility loss, `dlcb` the
//! `utilityLcb` loss the live path paints with, and their difference is the extra confidence
//! radius this candidate carries over the pick — the whole reason the two disagree.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;
use mirai_core::{Color, Point, sgf};
use mirai_engine::{
    AnalyzeReq, Engine, EngineTuning, LocalEngine, LocalEngineConfig, SubEvent, Want,
};

#[derive(Parser, Debug)]
#[command(about = "Dump per-candidate win-rate and score-lead losses for a whole record")]
struct Args {
    /// The record to sweep; its first game tree, main line only.
    sgf: PathBuf,

    /// Path to the `katago` binary.
    #[arg(long)]
    katago: PathBuf,

    /// Path to the KataGo model (`.bin.gz`).
    #[arg(long)]
    model: PathBuf,

    /// Analysis config; one is generated from the built-in tuning when absent.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Visit cap per position.
    #[arg(long, default_value_t = 1000)]
    visits: u32,

    /// Analyse every n-th position, counting the empty board as the first.
    #[arg(long, default_value_t = 5)]
    every: usize,

    /// Stop after this move number.
    #[arg(long)]
    until: Option<usize>,

    /// Label for the `game` column; defaults to the file stem.
    #[arg(long)]
    label: Option<String>,
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

    let bytes = std::fs::read(&args.sgf)
        .with_context(|| format!("could not read {}", args.sgf.display()))?;
    let mut trees = sgf::parse(&bytes).context("could not parse the record")?;
    if trees.is_empty() {
        bail!("no game tree in {}", args.sgf.display());
    }
    let tree = &mut trees[0];
    let label = args.label.clone().unwrap_or_else(|| {
        args.sgf
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    let config = match &args.config {
        Some(p) => p.clone(),
        None => EngineTuning::default()
            .write_to(&std::env::temp_dir().join("mirai-sweep"))
            .context("could not write an analysis config")?,
    };
    let mut cfg = LocalEngineConfig::new("sweep", &args.katago, &args.model, &config);
    cfg.log_dir = std::env::temp_dir().join("mirai-sweep");
    let engine = LocalEngine::spawn(cfg)
        .await
        .context("could not start KataGo")?;

    let info = tree.info.clone();
    let root = tree.root();
    let mut initial: Vec<(Color, Point)> = Vec::new();
    for &p in &tree.node(root).setup.add_black {
        initial.push((Color::Black, p));
    }
    for &p in &tree.node(root).setup.add_white {
        initial.push((Color::White, p));
    }
    let root_player = tree.position(root).to_play;
    let line = tree.main_line();

    println!(
        "game,move,to_play,root_visits,root_win,rank,gtp,visits,win,pts,dwin,dpts,dutil,dlcb,played"
    );
    let mut moves: Vec<(Color, Point)> = Vec::new();
    for (i, &id) in line.iter().enumerate() {
        if let Some((c, p)) = tree.node(id).mv {
            moves.push((c, p));
        }
        if args.until.is_some_and(|u| i > u) {
            break;
        }
        if i % args.every != 0 {
            continue;
        }
        let played = line
            .get(i + 1)
            .and_then(|&n| tree.node(n).mv)
            .map(|(_, p)| p);

        let mut req = AnalyzeReq::new(info.size, info.rules, info.komi);
        req.initial_stones = initial.clone();
        req.moves = moves.clone();
        if moves.is_empty() {
            req.initial_player = Some(root_player);
        }
        req.max_visits = Some(args.visits);
        req.want = Want::empty();
        let to_play = req.to_play();

        let mut sub = engine.subscribe(req);
        let report = loop {
            match sub.next().await {
                Some(SubEvent::Done(r)) => break r,
                Some(SubEvent::Failed(e)) => bail!("analysis failed at move {i}: {e}"),
                Some(_) => {}
                None => bail!("subscription closed at move {i}"),
            }
        };

        let Some(best) = report.moves.first() else {
            eprintln!("move {i}: no candidates");
            continue;
        };
        let (bw, bp) = (best.winrate_for(to_play), best.score_lead_for(to_play));
        let (bu, blcb) = (best.utility_for(to_play), best.utility_lcb_for(to_play));
        let root_win = report.root.winrate_f32();
        let root_win = if to_play == Color::Black {
            root_win
        } else {
            1.0 - root_win
        };
        for (rank, m) in report.moves.iter().enumerate() {
            let (w, p) = (m.winrate_for(to_play), m.score_lead_for(to_play));
            println!(
                "{label},{i},{to_play:?},{},{root_win:.4},{rank},{},{},{w:.4},{p:.2},{:.4},{:.2},{:.4},{:.4},{}",
                report.root.visits,
                info.size.to_gtp(m.mv),
                m.visits,
                bw - w,
                bp - p,
                bu - m.utility_for(to_play),
                blcb - m.utility_lcb_for(to_play),
                u8::from(played == Some(m.mv)),
            );
        }
        eprintln!("move {i}: {} candidates", report.moves.len());
    }

    Ok(ExitCode::SUCCESS)
}
