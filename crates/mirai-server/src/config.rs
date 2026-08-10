// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `server.toml` — what the headless host serves, and who may ask for it.
//!
//! Every path in the file may be relative; relative paths resolve against the directory
//! holding the config file, never the process's working directory, so a config is
//! self-contained and can be moved with its `cert.pem`/`key.pem`.

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use mirai_engine::LocalEngineConfig;
use serde::Deserialize;

pub const DEFAULT_LISTEN: &str = "0.0.0.0:9678";

/// The minimal file a first-time user has to write. Shown verbatim when the config is
/// missing, so the error is actionable without opening documentation.
pub const MINIMAL_EXAMPLE: &str = r#"listen = "0.0.0.0:9678"
cert   = "cert.pem"          # generated on first start if missing
key    = "key.pem"

[[engine]]
name   = "default"
katago = "/path/to/katago"
model  = "/path/to/model.bin.gz"
config = "/path/to/analysis.cfg"

[[token]]
value = "<64 hex chars from `mirai-server --generate-token`>"
name  = "laptop"
max_subs = 4"#;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_cert")]
    pub cert: PathBuf,
    #[serde(default = "default_key")]
    pub key: PathBuf,
    /// `[[engine]]` blocks, in file order. The first one is what `Open { engine: None }`
    /// resolves to.
    #[serde(default, rename = "engine")]
    pub engines: Vec<EngineCfg>,
    /// `[[token]]` blocks. An empty list rejects every client.
    #[serde(default, rename = "token")]
    pub tokens: Vec<TokenCfg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCfg {
    pub name: String,
    pub katago: PathBuf,
    pub model: PathBuf,
    pub config: PathBuf,
    /// KataGo's `logDir`. Defaults to the temp directory.
    pub log_dir: Option<PathBuf>,
    /// `numAnalysisThreads` — how many positions this engine searches at once.
    pub analysis_threads: Option<u16>,
    /// `numSearchThreadsPerAnalysisThread`.
    pub search_threads: Option<u16>,
    /// `nnCacheSizePowerOfTwo`.
    pub nn_cache_size_power_of_two: Option<u8>,
    /// How long to wait for the version/model handshake. First-run OpenCL tuning is slow;
    /// raise this if a cold GPU cache times out.
    pub startup_timeout_s: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCfg {
    pub value: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_max_subs")]
    pub max_subs: u32,
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_string()
}
fn default_cert() -> PathBuf {
    PathBuf::from("cert.pem")
}
fn default_key() -> PathBuf {
    PathBuf::from("key.pem")
}
fn default_max_subs() -> u32 {
    4
}

impl ServerConfig {
    /// Parses `text` as if it lived at `path`, resolving relative paths against `path`'s
    /// directory.
    pub fn parse(text: &str, path: &Path) -> anyhow::Result<ServerConfig> {
        let mut cfg: ServerConfig = toml::from_str(text)?;
        let base = path.parent().unwrap_or(Path::new("."));
        cfg.resolve(base);
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> anyhow::Result<ServerConfig> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        ServerConfig::parse(&text, path)
            .with_context(|| format!("parsing {}", path.display()))
    }

    fn resolve(&mut self, base: &Path) {
        rebase(base, &mut self.cert);
        rebase(base, &mut self.key);
        for e in &mut self.engines {
            rebase(base, &mut e.katago);
            rebase(base, &mut e.model);
            rebase(base, &mut e.config);
            if let Some(dir) = &mut e.log_dir {
                rebase(base, dir);
            }
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        for (i, e) in self.engines.iter().enumerate() {
            if e.name.is_empty() {
                bail!("[[engine]] #{} has an empty name", i + 1);
            }
            if self.engines[..i].iter().any(|p| p.name == e.name) {
                bail!("two [[engine]] blocks are both named {:?}", e.name);
            }
        }
        for (i, t) in self.tokens.iter().enumerate() {
            if t.value.is_empty() {
                bail!("[[token]] #{} has an empty value", i + 1);
            }
        }
        Ok(())
    }

    /// The address to bind, with `--listen` taking precedence over the file.
    pub fn listen_addr(&self, override_listen: Option<&str>) -> anyhow::Result<SocketAddr> {
        let spec = override_listen.unwrap_or(&self.listen);
        resolve_listen(spec)
    }
}

impl EngineCfg {
    pub fn to_local_config(&self) -> LocalEngineConfig {
        let mut lc = LocalEngineConfig::new(
            self.name.clone(),
            self.katago.clone(),
            self.model.clone(),
            self.config.clone(),
        );
        if let Some(dir) = &self.log_dir {
            lc.log_dir = dir.clone();
        }
        lc.analysis_threads = self.analysis_threads;
        lc.search_threads = self.search_threads;
        lc.nn_cache_size_power_of_two = self.nn_cache_size_power_of_two;
        if let Some(s) = self.startup_timeout_s {
            lc.startup_timeout = Duration::from_secs(s);
        }
        lc
    }
}

fn rebase(base: &Path, p: &mut PathBuf) {
    if p.is_relative() {
        *p = base.join(&*p);
    }
}

/// Accepts a literal socket address, or a `host:port` that needs a DNS lookup.
pub fn resolve_listen(spec: &str) -> anyhow::Result<SocketAddr> {
    if let Ok(addr) = spec.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let mut it = spec
        .to_socket_addrs()
        .with_context(|| format!("resolving listen address {spec:?}"))?;
    it.next()
        .with_context(|| format!("listen address {spec:?} resolved to nothing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
        listen = "127.0.0.1:19678"
        cert = "certs/cert.pem"
        key = "/etc/mirai/key.pem"

        [[engine]]
        name = "default"
        katago = "bin/katago"
        model = "/models/b18.bin.gz"
        config = "analysis.cfg"
        analysis_threads = 4
        search_threads = 8

        [[engine]]
        name = "human"
        katago = "/usr/bin/katago"
        model = "/models/human.bin.gz"
        config = "/etc/katago/analysis.cfg"

        [[token]]
        value = "aa"
        name = "laptop"
        max_subs = 2

        [[token]]
        value = "bb"
    "#;

    fn parse(text: &str) -> anyhow::Result<ServerConfig> {
        ServerConfig::parse(text, Path::new("/srv/mirai/server.toml"))
    }

    #[test]
    fn relative_paths_resolve_against_the_config_directory() {
        let cfg = parse(FULL).unwrap();
        assert_eq!(cfg.cert, Path::new("/srv/mirai/certs/cert.pem"));
        // Absolute paths are left alone.
        assert_eq!(cfg.key, Path::new("/etc/mirai/key.pem"));
        assert_eq!(cfg.engines[0].katago, Path::new("/srv/mirai/bin/katago"));
        assert_eq!(cfg.engines[0].config, Path::new("/srv/mirai/analysis.cfg"));
        assert_eq!(cfg.engines[1].katago, Path::new("/usr/bin/katago"));
    }

    #[test]
    fn engine_order_and_thread_overrides_survive_the_round_trip() {
        let cfg = parse(FULL).unwrap();
        assert_eq!(cfg.engines.len(), 2);
        // `Open { engine: None }` must pick the first block in file order.
        assert_eq!(cfg.engines[0].name, "default");
        let lc = cfg.engines[0].to_local_config();
        assert_eq!(lc.analysis_threads, Some(4));
        assert_eq!(lc.search_threads, Some(8));
        let lc = cfg.engines[1].to_local_config();
        assert_eq!(lc.analysis_threads, None);
    }

    #[test]
    fn token_max_subs_defaults_and_listen_override_wins() {
        let cfg = parse(FULL).unwrap();
        assert_eq!(cfg.tokens[0].max_subs, 2);
        assert_eq!(cfg.tokens[1].max_subs, 4, "default max_subs");
        assert_eq!(cfg.tokens[1].name, "");

        let from_file = cfg.listen_addr(None).unwrap();
        assert_eq!(from_file.to_string(), "127.0.0.1:19678");
        let overridden = cfg.listen_addr(Some("0.0.0.0:1")).unwrap();
        assert_eq!(overridden.to_string(), "0.0.0.0:1");
    }

    #[test]
    fn an_empty_config_still_yields_usable_defaults() {
        let cfg = parse("").unwrap();
        assert_eq!(cfg.listen, DEFAULT_LISTEN);
        assert_eq!(cfg.cert, Path::new("/srv/mirai/cert.pem"));
        assert!(cfg.engines.is_empty() && cfg.tokens.is_empty());
    }

    #[test]
    fn the_documented_minimal_example_parses() {
        let cfg = parse(MINIMAL_EXAMPLE).unwrap();
        assert_eq!(cfg.engines.len(), 1);
        assert_eq!(cfg.tokens.len(), 1);
        assert_eq!(cfg.tokens[0].max_subs, 4);
    }

    #[test]
    fn typos_and_duplicates_are_rejected_rather_than_silently_ignored() {
        let typo = parse("[[engine]]\nname='a'\nkatago='k'\nmodel='m'\nconfig='c'\nanalysis_thread=4\n");
        assert!(typo.is_err(), "a mistyped key must not be dropped on the floor");

        let dup = parse(
            "[[engine]]\nname='a'\nkatago='k'\nmodel='m'\nconfig='c'\n\
             [[engine]]\nname='a'\nkatago='k'\nmodel='m'\nconfig='c'\n",
        );
        assert!(dup.is_err(), "duplicate engine names make `Open` ambiguous");

        assert!(parse("[[token]]\nvalue=''\n").is_err(), "empty token");
    }
}
