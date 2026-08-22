// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Plays KataGo against itself and writes the record, analysis and all.
//!
//! This is where `crates/mirai-core/tests/data/katago-selfplay.sgf` comes from. It has to be
//! produced the way mirai saves a record, or the test only proves the test agrees with
//! itself, so this drives the *same* `request_for_node` → engine → `analysis_of` →
//! `sgf::write` path the GUI does. That is also why the generator lives in this crate rather
//! than in `mirai-engine`: those two functions are the client layer's, and reimplementing
//! them here would be a second convention beside the real one.
//!
//! It cannot replace the other fixture beside it. `lizzieyzy-autoGame1.sgf` is KataGo
//! self-play as well, but *another program* serialised it, which is the whole point of
//! keeping it — see `docs/dev/TESTING.md` §4.
//!
//! ```text
//! cargo run -p mirai-client --example selfplay -- \
//!     --model ~/.katago/models/<net>.bin.gz --moves 25 --visits 1000 \
//!     --out crates/mirai-core/tests/data/katago-selfplay.sgf
//! ```
//!
//! KataGo's search is not deterministic across thread schedules, so regenerating the
//! fixture yields a *different* game. That is fine — the test asserts the record's
//! structure and that every move is legal, not which moves were chosen — but the exact
//! numbers in it are then stale, so update them from what the run prints.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;
use mirai_client::{analysis_of, request_for_node};
use mirai_core::{GameInfo, GameTree, RuleSet, Size, sgf};
use mirai_engine::{Engine, EngineTuning, LocalEngine, LocalEngineConfig, Want};

#[derive(Parser, Debug)]
#[command(about = "Play KataGo against itself and write the record as SGF")]
struct Args {
    /// Where to write the record.
    #[arg(long, short)]
    out: PathBuf,

    /// Path to the KataGo model (`.bin.gz`).
    #[arg(long)]
    model: PathBuf,

    /// The `katago` binary; resolved on `PATH` when it is a bare name.
    #[arg(long, default_value = "katago")]
    katago: PathBuf,

    /// Analysis config; mirai's own default tuning writes one when absent.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Visit cap per position.
    #[arg(long, default_value_t = 1000)]
    visits: u32,

    /// How many moves to play.
    #[arg(long, default_value_t = 25)]
    moves: usize,

    /// Board size (square).
    #[arg(long, default_value_t = 19)]
    size: u8,

    #[arg(long, default_value_t = 7.5)]
    komi: f32,

    /// Ruleset, by KataGo name (`chinese`, `japanese`, `tromp-taylor`, ...).
    #[arg(long, default_value = "chinese")]
    rules: String,

    /// Candidates stored per node, as `AnalysisSettings::stored_suggestion_limit` would.
    #[arg(long, default_value_t = 8)]
    candidates: usize,

    /// Continuations to fan out at the final position instead of playing just one, so the
    /// record ends in a real branch point.
    #[arg(long, default_value_t = 3)]
    variations: usize,
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

    let size = Size::new(args.size, args.size)
        .with_context(|| format!("board size {} is out of range", args.size))?;
    let rules = RuleSet::from_katago_name(&args.rules)
        .with_context(|| format!("unknown ruleset {:?}", args.rules))?;

    let dir = std::env::temp_dir().join("mirai-selfplay");
    let config = match &args.config {
        Some(path) => path.clone(),
        None => EngineTuning::default()
            .write_to(&dir)
            .context("could not write an analysis config")?,
    };
    println!("config: {}", config.display());

    let mut cfg = LocalEngineConfig::new("selfplay", &args.katago, &args.model, &config);
    cfg.log_dir = dir;
    let engine = LocalEngine::spawn(cfg)
        .await
        .context("could not start KataGo")?;
    let desc = engine.describe();
    println!(
        "engine: katago {} model {} threads {}",
        desc.katago_version, desc.model, desc.analysis_threads
    );

    let mut info = GameInfo::new(size, rules);
    info.komi = args.komi;
    info.players[0].name = "KataGo".into();
    info.players[1].name = "KataGo".into();
    let mut tree = GameTree::new(info);

    let want = Want::OWNERSHIP | Want::PV_VISITS;
    let mut id = tree.root();
    for turn in 0..=args.moves {
        let req = request_for_node(&mut tree, id, want, args.visits);
        let to_play = tree.position(id).to_play;
        let report = engine
            .subscribe(req)
            .finish()
            .await
            .with_context(|| format!("turn {turn}"))?;
        if report.moves.is_empty() {
            bail!("turn {turn}: the engine returned no candidates");
        }
        tree.set_analysis(id, Some(analysis_of(&report, args.candidates)));

        // The last position fans out instead of playing one move, so the record ends the
        // way a reviewed game does: several engine continuations off one node.
        let fan = if turn == args.moves {
            args.variations.max(1)
        } else {
            1
        };
        let mut first = None;
        for cand in report.moves.iter().take(fan) {
            let child = tree
                .add_variation(id, to_play, cand.mv)
                .with_context(|| format!("turn {turn}: {} is illegal", size.to_gtp(cand.mv)))?;
            first = first.or(Some(child));
        }
        println!(
            "turn {turn}: {to_play:?} {} at {} visits, {} candidates",
            size.to_gtp(report.moves[0].mv),
            report.root.visits,
            report.moves.len()
        );
        id = first.expect("the candidate list was checked to be non-empty");
    }

    let text = sgf::write(&tree, true);
    std::fs::write(&args.out, &text)
        .with_context(|| format!("could not write {}", args.out.display()))?;
    println!(
        "wrote {}: {} bytes, {} nodes, main line {}",
        args.out.display(),
        text.len(),
        tree.len(),
        tree.main_line().len()
    );

    Ok(ExitCode::SUCCESS)
}
