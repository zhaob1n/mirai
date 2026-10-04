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
use mirai_engine::{EngineTuning, LocalEngineConfig, TuningOverrides};
use serde::Deserialize;

use crate::session::MAX_HELLO_FIELD;

pub const DEFAULT_LISTEN: &str = "0.0.0.0:9678";

/// The all-zero token shipped in `server.example.toml`. It is in the repository, so a
/// config that still has it must not start: anyone who has read the example could
/// authenticate.
const EXAMPLE_TOKEN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The token placeholder printed when no server config exists. Copying that
/// snippet verbatim must not grant access to anyone who knows its public text.
const MINIMAL_TOKEN_PLACEHOLDER: &str = "<64 hex chars from `mirai-server --generate-token`>";

/// The minimal file a first-time user has to write. Shown verbatim when the config is
/// missing, so the error is actionable without opening documentation.
pub const MINIMAL_EXAMPLE: &str = r#"listen = "0.0.0.0:9678"
cert   = "cert.pem"          # generated on first start if missing
key    = "key.pem"

[[engine]]
name   = "default"
katago = "/path/to/katago"
model  = "/path/to/model.bin.gz"
# config = "/path/to/analysis.cfg"   # omit to let mirai generate one

[[token]]
value = "<64 hex chars from `mirai-server --generate-token`>"
name  = "laptop"
max_subs = 64"#;

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
    /// A KataGo analysis config. Omit it and the server writes one into `log_dir` from
    /// the tuning below, the same file the desktop application generates.
    pub config: Option<PathBuf>,
    /// KataGo's `logDir`, which also holds a generated config. Defaults to the directory
    /// `main` passes to [`EngineCfg::to_local_config`], this user's own data directory.
    pub log_dir: Option<PathBuf>,
    /// `numAnalysisThreads` — how many positions this engine searches at once.
    pub analysis_threads: Option<u16>,
    /// `numSearchThreadsPerAnalysisThread`.
    pub search_threads: Option<u16>,
    /// `nnMaxBatchSize`. Only used when `config` is omitted; a custom file must set it.
    pub nn_max_batch_size: Option<u16>,
    /// `nnCacheSizePowerOfTwo`. Only used when `config` is omitted; a custom file owns it.
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
    /// Requests this token may have outstanding — queued in KataGo or running — across
    /// all its connections. KataGo schedules them (`numAnalysisThreads` at a time, then
    /// by priority and arrival), so this is a guard against a runaway client, not the
    /// concurrency limit.
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
/// Covers the desktop client's own worst case — a whole-game sweep keeps up to 16
/// positions in flight, plus live analysis and a play or score request per window —
/// several times over, so several windows or devices can share one token.
fn default_max_subs() -> u32 {
    64
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
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .with_context(|| format!("checking {}", path.display()))?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                tracing::warn!(
                    config = %path.display(),
                    "server config is accessible by other users and contains bearer tokens; chmod 600 this file"
                );
            }
        }
        ServerConfig::parse(&text, path).with_context(|| format!("parsing {}", path.display()))
    }

    fn resolve(&mut self, base: &Path) {
        rebase(base, &mut self.cert);
        rebase(base, &mut self.key);
        for e in &mut self.engines {
            rebase(base, &mut e.katago);
            rebase(base, &mut e.model);
            if let Some(config) = &mut e.config {
                rebase(base, config);
            }
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
            // A longer token could never be presented: the server refuses such a Hello.
            if t.value.len() > MAX_HELLO_FIELD {
                bail!(
                    "[[token]] #{} is longer than {MAX_HELLO_FIELD} bytes; no client could present it",
                    i + 1
                );
            }
            if t.max_subs == 0 {
                bail!(
                    "[[token]] #{} has max_subs = 0, which would refuse every search",
                    i + 1
                );
            }
            if t.value == EXAMPLE_TOKEN || t.value == MINIMAL_TOKEN_PLACEHOLDER {
                bail!(
                    "[[token]] #{} is the example placeholder; replace it with the output of `mirai-server --generate-token`",
                    i + 1
                );
            }
            if self.tokens[..i].iter().any(|p| p.value == t.value) {
                bail!("two [[token]] blocks have the same value");
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
    /// `default_log_dir` is where an engine without `log_dir` keeps KataGo's logs and its
    /// generated config; `None` when this user has no home directory to put it in. It is
    /// created private, and used only if nobody else can write to it: the temp directory
    /// this once defaulted to let another local user pre-create the directory and swap or
    /// redirect the generated config.
    pub fn to_local_config(
        &self,
        default_log_dir: Option<&Path>,
    ) -> anyhow::Result<LocalEngineConfig> {
        let log_dir = match (&self.log_dir, default_log_dir) {
            (Some(dir), _) => dir.clone(),
            (None, Some(dir)) => {
                mirai_proto::atomic::private_dir(dir)
                    .with_context(|| format!("preparing the log directory {}", dir.display()))?;
                dir.to_path_buf()
            }
            (None, None) => {
                bail!("no home directory to keep KataGo's logs in; set log_dir in this [[engine]]")
            }
        };
        let overrides = TuningOverrides {
            analysis_threads: self.analysis_threads,
            search_threads: self.search_threads,
            nn_max_batch_size: self.nn_max_batch_size,
            nn_cache_size_power_of_two: self.nn_cache_size_power_of_two,
        };
        // No config file given: write the one mirai would generate, with this block's
        // tuning applied over the defaults. A user-supplied file keeps its own tuning
        // but the two thread counts.
        let custom = self.config.is_some();
        let config = match &self.config {
            Some(path) => path.clone(),
            None => EngineTuning::with_overrides(overrides)
                .write_to(&log_dir)
                .with_context(|| {
                    format!("writing the analysis config into {}", log_dir.display())
                })?,
        };

        let mut lc = LocalEngineConfig::new(
            self.name.clone(),
            self.katago.clone(),
            self.model.clone(),
            config,
        );
        lc.log_dir = log_dir;
        lc.apply_config_overrides(custom, overrides);
        if let Some(s) = self.startup_timeout_s {
            lc.startup_timeout = Duration::from_secs(s);
        }
        Ok(lc)
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
        assert_eq!(
            cfg.engines[0].config.as_deref(),
            Some(Path::new("/srv/mirai/analysis.cfg"))
        );
        assert_eq!(cfg.engines[1].katago, Path::new("/usr/bin/katago"));
    }

    #[test]
    fn engine_order_and_thread_overrides_survive_the_round_trip() {
        let cfg = parse(FULL).unwrap();
        assert_eq!(cfg.engines.len(), 2);
        // `Open { engine: None }` must pick the first block in file order.
        assert_eq!(cfg.engines[0].name, "default");
        let logs = std::env::temp_dir().join(format!("mirai-server-rt-{}", std::process::id()));
        let lc = cfg.engines[0].to_local_config(Some(&logs)).unwrap();
        let _ = std::fs::remove_dir_all(&logs);
        assert_eq!(lc.analysis_threads, Some(4));
        assert_eq!(lc.search_threads, Some(8));
    }

    /// An engine block with no `config` gets the file mirai generates, and the tuning
    /// lands in that file rather than being passed twice.
    #[test]
    fn an_engine_without_a_config_file_gets_a_generated_one() {
        let dir = std::env::temp_dir().join(format!(
            "mirai-server-generated-cfg-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let text = format!(
            "[[engine]]\nname = \"gen\"\nkatago = \"/usr/bin/katago\"\n\
             model = \"/models/b18.bin.gz\"\nlog_dir = {:?}\nsearch_threads = 12\n",
            dir.display().to_string()
        );
        let cfg = parse(&text).unwrap();
        assert!(cfg.engines[0].config.is_none());

        let lc = cfg.engines[0].to_local_config(None).unwrap();
        let written = std::fs::read_to_string(&lc.config).expect("the config was written");
        assert!(written.contains("numSearchThreadsPerAnalysisThread = 12"));
        assert!(
            written.contains("nnMaxBatchSize"),
            "katago requires this key"
        );
        assert_eq!(
            (lc.analysis_threads, lc.search_threads),
            (None, None),
            "a generated config is the only source of truth for its own keys"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no `log_dir`, the logs and the generated config go to the default directory,
    /// which is created for this user alone; with no default either there is nowhere
    /// safe to put them, and the engine is refused rather than sent to a shared one.
    #[cfg(unix)]
    #[test]
    fn an_engine_without_a_log_dir_uses_a_private_default() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("mirai-server-logdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let default = base.join("mirai").join("katago-logs");
        let cfg = parse("[[engine]]\nname='a'\nkatago='k'\nmodel='m'\n").unwrap();

        let lc = cfg.engines[0].to_local_config(Some(&default)).unwrap();

        assert_eq!(lc.log_dir, default);
        assert_eq!(lc.config.parent(), Some(default.as_path()));
        assert_eq!(
            std::fs::metadata(&default).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(cfg.engines[0].to_local_config(None).is_err());

        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = cfg.engines[0].to_local_config(Some(&default)).unwrap_err();
        assert!(
            format!("{error:#}").contains("other users write"),
            "the refusal must say why: {error:#}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn token_max_subs_and_listen_override_wins() {
        let cfg = parse(FULL).unwrap();
        assert_eq!(cfg.tokens[0].max_subs, 2);
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
    fn the_documented_minimal_example_requires_a_real_token() {
        let err = parse(MINIMAL_EXAMPLE).unwrap_err();
        assert!(format!("{err:#}").contains("example placeholder"));
        let cfg = parse(&MINIMAL_EXAMPLE.replace(MINIMAL_TOKEN_PLACEHOLDER, "secret")).unwrap();
        assert_eq!(cfg.engines.len(), 1);
        assert_eq!(cfg.tokens.len(), 1);
        assert_eq!(cfg.tokens[0].max_subs, 64);
    }

    #[test]
    fn typos_and_duplicates_are_rejected_rather_than_silently_ignored() {
        let typo =
            parse("[[engine]]\nname='a'\nkatago='k'\nmodel='m'\nconfig='c'\nanalysis_thread=4\n");
        assert!(
            typo.is_err(),
            "a mistyped key must not be dropped on the floor"
        );

        let dup = parse(
            "[[engine]]\nname='a'\nkatago='k'\nmodel='m'\nconfig='c'\n\
             [[engine]]\nname='a'\nkatago='k'\nmodel='m'\nconfig='c'\n",
        );
        assert!(dup.is_err(), "duplicate engine names make `Open` ambiguous");

        assert!(parse("[[token]]\nvalue=''\n").is_err(), "empty token");
        let long = "v".repeat(MAX_HELLO_FIELD + 1);
        assert!(
            parse(&format!("[[token]]\nvalue='{long}'\n")).is_err(),
            "a token no Hello may carry"
        );
        assert!(
            parse("[[token]]\nvalue='v'\nmax_subs=0\n").is_err(),
            "a token that can open nothing"
        );

        let dup_token = parse("[[token]]\nvalue='same'\n[[token]]\nvalue='same'\n");
        assert!(
            dup_token.is_err(),
            "duplicate token values make quotas ambiguous"
        );
    }

    #[test]
    fn the_example_placeholder_token_is_refused() {
        let err = parse(&format!("[[token]]\nvalue = \"{EXAMPLE_TOKEN}\"\n")).unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("example placeholder"),
            "the refusal must say to replace it: {text}"
        );
    }

    #[test]
    fn the_shipped_example_is_refused_until_its_token_is_replaced() {
        let raw = include_str!("../server.example.toml");
        let err = parse(raw).unwrap_err();
        assert!(
            format!("{err:#}").contains("example placeholder"),
            "{err:#}"
        );
        assert!(parse(&raw.replace(EXAMPLE_TOKEN, "ab")).is_ok());
    }
}
