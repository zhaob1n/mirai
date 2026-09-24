// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `mirai-server` — a headless host that lends KataGo to mirai clients over MRP/2.
//!
//! One process owns one KataGo `analysis` subprocess per configured `[[engine]]` and
//! multiplexes every connected client onto them; `numAnalysisThreads` is what makes that
//! safe, so a second client never costs a second GPU context.

mod config;
mod session;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use mirai_engine::{Engine, LocalEngine};
use mirai_proto::transport;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

/// Caps pre-authentication frame buffers as well as authenticated sessions.
const MAX_SESSIONS: usize = 32;

use config::ServerConfig;
use session::{Host, NamedEngine, Token};

#[derive(Parser, Debug)]
#[command(
    name = "mirai-server",
    version,
    about = "Headless KataGo host speaking the mirai remote-analysis protocol"
)]
struct Args {
    /// Configuration file (default: $XDG_CONFIG_HOME/mirai/server.toml).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Address to bind; overrides `listen` in the configuration file.
    #[arg(long, value_name = "ADDR")]
    listen: Option<String>,

    /// Print a fresh 64-hex-character access token and exit. Needs no configuration file.
    #[arg(long)]
    generate_token: bool,

    /// Load (creating it if necessary) the certificate, print its SHA-256 fingerprint,
    /// and exit.
    #[arg(long)]
    print_fingerprint: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    // Deliberately before anything that needs a config file or a runtime: this is what a
    // fresh install runs first.
    if args.generate_token {
        match generate_token() {
            Ok(token) => {
                println!("{token}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("mirai-server: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    init_tracing();

    let config_path = match args.config.clone() {
        Some(p) => p,
        None => default_config_path(),
    };
    if !config_path.exists() {
        report_missing_config(&config_path);
        return ExitCode::FAILURE;
    }
    let cfg = match ServerConfig::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    info!(config = %config_path.display(), "loaded configuration");

    let listen = match cfg.listen_addr(args.listen.as_deref()) {
        Ok(a) => a,
        Err(e) => {
            error!("{e:#}");
            return ExitCode::FAILURE;
        }
    };

    let (certs, key) =
        match transport::load_or_generate_cert(&cfg.cert, &cfg.key, &cert_hostnames(listen)) {
            Ok(pair) => pair,
            Err(e) => {
                error!(
                    cert = %cfg.cert.display(),
                    key = %cfg.key.display(),
                    "certificate unusable: {e}"
                );
                return ExitCode::FAILURE;
            }
        };
    let fingerprint = transport::fingerprint_of(&certs);

    if args.print_fingerprint {
        println!("{fingerprint}");
        return ExitCode::SUCCESS;
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("could not start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(cfg, listen, certs, key, fingerprint))
}

async fn run(
    cfg: ServerConfig,
    listen: SocketAddr,
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    fingerprint: String,
) -> ExitCode {
    let engines = start_engines(&cfg).await;
    if engines.is_empty() {
        error!("no engine started; there is nothing to serve");
        return ExitCode::FAILURE;
    }
    if cfg.tokens.is_empty() {
        warn!("no [[token]] configured — every client will be rejected as unauthorized");
    }

    let endpoint = match transport::server_endpoint(listen, certs, key) {
        Ok(e) => e,
        Err(e) => {
            error!(%listen, "could not bind: {e}");
            return ExitCode::FAILURE;
        }
    };

    let host = Arc::new(Host {
        engines,
        tokens: cfg
            .tokens
            .into_iter()
            .map(|t| Token {
                value: t.value,
                name: t.name,
                max_subs: t.max_subs,
            })
            .collect(),
    });

    info!(
        listen = %listen,
        engines = host.engines.len(),
        tokens = host.tokens.len(),
        "mirai-server listening"
    );
    info!(sha256 = %fingerprint, "certificate fingerprint (pin this in the client)");

    let sessions = Arc::new(Semaphore::new(MAX_SESSIONS));
    while let Some(incoming) = endpoint.accept().await {
        let peer = incoming.remote_address();
        let Ok(permit) = Arc::clone(&sessions).try_acquire_owned() else {
            warn!(
                %peer,
                max_sessions = MAX_SESSIONS,
                "connection refused: session limit reached"
            );
            incoming.refuse();
            continue;
        };
        let host = Arc::clone(&host);
        tokio::spawn(async move {
            // Keeping the owned permit in the task releases it on every return and unwind.
            let _permit = permit;
            match incoming.await {
                Ok(conn) => session::serve(host, conn).await,
                Err(e) => warn!("handshake failed: {e}"),
            }
        });
    }

    info!("endpoint closed");
    ExitCode::SUCCESS
}

/// Starts every configured engine. A failure is logged and skipped, never fatal on its
/// own: one broken GPU config should not take a working engine offline with it.
async fn start_engines(cfg: &ServerConfig) -> Vec<NamedEngine> {
    let mut out = Vec::with_capacity(cfg.engines.len());
    for e in &cfg.engines {
        info!(
            engine = %e.name,
            katago = %e.katago.display(),
            model = %e.model.display(),
            "starting engine"
        );
        let local = match e.to_local_config() {
            Ok(local) => local,
            Err(err) => {
                error!(engine = %e.name, "engine is not usable, continuing without it: {err:#}");
                continue;
            }
        };
        match LocalEngine::spawn(local).await {
            Ok(engine) => {
                let desc = engine.describe();
                info!(
                    engine = %e.name,
                    katago_version = %desc.katago_version,
                    model = %desc.model,
                    analysis_threads = desc.analysis_threads,
                    human_model = desc.has_human_model,
                    "engine ready"
                );
                out.push(NamedEngine {
                    name: e.name.clone(),
                    engine: Arc::new(engine),
                });
            }
            Err(err) => {
                error!(engine = %e.name, "engine failed to start, continuing without it: {err}");
            }
        }
    }
    out
}

/// Subject alternative names for a generated certificate. Clients verify by SHA-256
/// fingerprint (TOFU), so these only matter to tools that look at the certificate.
fn cert_hostnames(listen: SocketAddr) -> Vec<String> {
    let mut names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    let ip = listen.ip();
    if !ip.is_unspecified() && !ip.is_loopback() {
        names.push(ip.to_string());
    }
    names
}

/// A 64-hex-character token: 32 bytes from the OS CSPRNG, hex-encoded.
///
/// PROTOCOL §8.2 asks for at least 128 bits. Seeding SplitMix64 from the wall
/// clock and the pid is 64 bits and guessable, so a failure here is reported
/// rather than papered over with that generator.
fn generate_token() -> Result<String, &'static str> {
    let mut buf = [0u8; 32];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut buf)
        .map_err(|_| "the operating system's random number generator failed")?;
    Ok(mirai_proto::sha256::hex(&buf))
}

fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("io.github", "mirai", "mirai")
        .map(|d| d.config_dir().join("server.toml"))
        .unwrap_or_else(|| PathBuf::from("server.toml"))
}

fn report_missing_config(path: &std::path::Path) {
    eprintln!("mirai-server: no configuration file at {}", path.display());
    eprintln!();
    eprintln!("Create it, or point --config at one. A minimal server.toml:");
    eprintln!();
    for line in config::MINIMAL_EXAMPLE.lines() {
        eprintln!("    {line}");
    }
    eprintln!();
    eprintln!("Generate the token with:  mirai-server --generate-token");
    eprintln!("A fully annotated example ships as server.example.toml.");
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("mirai_server=info,mirai_engine=info,warn")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_is_64_lowercase_hex_characters() {
        let t = generate_token().expect("OS random number generator");
        assert_eq!(t.len(), 64);
        assert!(
            t.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "not lowercase hex: {t}"
        );
    }

    #[test]
    fn generated_tokens_differ_between_calls() {
        // Same process, back to back. A generator stuck on one value — or seeded
        // from a constant — would hand every install the same token.
        let a = generate_token().expect("OS random number generator");
        let b = generate_token().expect("OS random number generator");
        assert_ne!(a, b);
    }

    #[test]
    fn cert_names_cover_loopback_and_the_bound_address() {
        let any: SocketAddr = "0.0.0.0:9678".parse().unwrap();
        assert_eq!(cert_hostnames(any), ["localhost", "127.0.0.1", "::1"]);

        let lan: SocketAddr = "192.168.1.10:9678".parse().unwrap();
        assert!(cert_hostnames(lan).contains(&"192.168.1.10".to_string()));
    }

    #[test]
    fn the_cli_matches_the_documented_flags() {
        use clap::CommandFactory;
        Args::command().debug_assert();

        let a = Args::try_parse_from(["mirai-server", "--generate-token"]).unwrap();
        assert!(a.generate_token && a.config.is_none());

        let a = Args::try_parse_from([
            "mirai-server",
            "--config",
            "/tmp/s.toml",
            "--listen",
            "127.0.0.1:9678",
            "--print-fingerprint",
        ])
        .unwrap();
        assert_eq!(
            a.config.as_deref(),
            Some(std::path::Path::new("/tmp/s.toml"))
        );
        assert_eq!(a.listen.as_deref(), Some("127.0.0.1:9678"));
        assert!(a.print_fingerprint);
    }
}
