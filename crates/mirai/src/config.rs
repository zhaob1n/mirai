// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `$XDG_CONFIG_HOME/mirai/config.toml` — engine profiles and preferences.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use mirai_core::RuleSet;
use mirai_engine::EngineTuning;
use serde::{Deserialize, Serialize};

/// The user-specific XDG root and the ordered system roots are kept separate because
/// `~/.katago/models` belongs between them in model discovery.
struct XdgRoots {
    home: Option<PathBuf>,
    system: Vec<PathBuf>,
}

/// Looks for a usable KataGo installation without guessing arbitrary package layouts.
fn discover_katago() -> Option<(PathBuf, PathBuf)> {
    let katago = which("katago")?;
    let model = discover_models().into_iter().next()?;
    Some((katago, model))
}

/// Every discovered model, merged in directory-priority order.
pub(crate) fn discover_models() -> Vec<PathBuf> {
    let home = directories::BaseDirs::new().map(|base| base.home_dir().to_path_buf());
    merge_candidates(model_dirs(xdg_data_roots(), home.as_deref()), network_files)
}

/// Ordered model locations: user XDG data, KataGo's own home, then system XDG data.
fn model_dirs(roots: XdgRoots, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(roots.system.len() * 2 + 3);
    if let Some(root) = roots.home.as_deref() {
        dirs.extend(namespaced_dirs(root, "models"));
    }
    if let Some(home) = home {
        dirs.push(home.join(".katago/models"));
    }
    for root in roots.system {
        dirs.extend(namespaced_dirs(&root, "models"));
    }
    dirs
}

/// Every custom analysis config, merged in directory-priority order.
///
/// Discovery is only a chooser aid: silently adopting a file would disable mirai's managed
/// defaults and measured tuning.
pub(crate) fn discover_analysis_configs() -> Vec<PathBuf> {
    merge_candidates(
        analysis_config_dirs(xdg_config_roots()),
        analysis_config_files,
    )
}

/// Ordered custom-analysis locations in the KataGo and mirai XDG namespaces.
fn analysis_config_dirs(roots: XdgRoots) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity((roots.system.len() + usize::from(roots.home.is_some())) * 2);
    if let Some(root) = roots.home.as_deref() {
        dirs.extend(namespaced_dirs(root, "cfg/analysis"));
    }
    for root in roots.system {
        dirs.extend(namespaced_dirs(&root, "cfg/analysis"));
    }
    dirs
}

fn namespaced_dirs(root: &Path, suffix: &str) -> [PathBuf; 2] {
    [
        root.join("katago").join(suffix),
        root.join("mirai").join(suffix),
    ]
}

fn merge_candidates(dirs: Vec<PathBuf>, scan: fn(&Path) -> Vec<PathBuf>) -> Vec<PathBuf> {
    let mut merged = Vec::new();
    for dir in dirs {
        for candidate in scan(&dir) {
            if !merged.contains(&candidate) {
                merged.push(candidate);
            }
        }
    }
    merged
}

fn xdg_data_roots() -> XdgRoots {
    xdg_roots(
        "XDG_DATA_HOME",
        ".local/share",
        "XDG_DATA_DIRS",
        &["/usr/local/share", "/usr/share"],
    )
}

fn xdg_config_roots() -> XdgRoots {
    xdg_roots(
        "XDG_CONFIG_HOME",
        ".config",
        "XDG_CONFIG_DIRS",
        &["/etc/xdg"],
    )
}

fn xdg_roots(
    home_var: &str,
    home_fallback: &str,
    dirs_var: &str,
    dirs_fallback: &[&str],
) -> XdgRoots {
    let home = std::env::var_os(home_var)
        .filter(|value| Path::new(value).is_absolute())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|base| base.home_dir().join(home_fallback)));
    let system = std::env::var_os(dirs_var);
    XdgRoots {
        home,
        system: xdg_dir_list(system.as_deref(), dirs_fallback),
    }
}

fn xdg_dir_list(value: Option<&OsStr>, fallback: &[&str]) -> Vec<PathBuf> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => std::env::split_paths(&OsString::from(value))
            .filter(|path| path.is_absolute())
            .collect(),
        None => fallback.iter().map(PathBuf::from).collect(),
    }
}

/// The first executable named `name` on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Every `*.bin.gz` directly inside `dir`, newest first.
fn network_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.to_str().is_some_and(|s| s.ends_with(".bin.gz")) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        files.push((modified, path));
    }
    files.sort_by(|(a_time, a_path), (b_time, b_path)| {
        b_time.cmp(a_time).then_with(|| a_path.cmp(b_path))
    });
    files.into_iter().map(|(_, path)| path).collect()
}

/// Every analysis `*.cfg` directly inside `dir`.
///
/// The conventional `analysis.cfg` comes first, then other matching files newest first. A
/// plain `gtp.cfg` is never offered for `katago analysis`.
fn analysis_config_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        if !name.contains("analysis") || !name.ends_with(".cfg") {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        files.push((name == "analysis.cfg", modified, path));
    }
    files.sort_by(
        |(a_conventional, a_time, a_path), (b_conventional, b_time, b_path)| {
            b_conventional
                .cmp(a_conventional)
                .then_with(|| b_time.cmp(a_time))
                .then_with(|| a_path.cmp(b_path))
        },
    );
    files.into_iter().map(|(_, _, path)| path).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("i/o error on {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} is not valid TOML: {1}")]
    Parse(PathBuf, toml::de::Error),
    #[error("could not serialise the configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("no home directory available for the configuration")]
    NoHome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ProfileKind {
    Local {
        katago: PathBuf,
        model: PathBuf,
        /// A KataGo analysis config to use instead of the one mirai generates. `None` —
        /// the default — means mirai writes the config itself from the tuning below.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        config: Option<PathBuf>,
        /// `numAnalysisThreads`. `None` means mirai's default with a generated config, or
        /// whatever the file says with a custom one.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        analysis_threads: Option<u16>,
        /// `numSearchThreadsPerAnalysisThread`, same rule.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        search_threads: Option<u16>,
        /// `nnMaxBatchSize`, same rule. Ignored when `config` is set: a custom file has to
        /// carry this key anyway, since KataGo will not start without it.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        nn_max_batch_size: Option<u16>,
        /// `nnCacheSizePowerOfTwo`, same rule.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        nn_cache_size_power_of_two: Option<u8>,
    },
    Remote {
        url: String,
        token: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        engine: Option<String>,
        /// TOFU pin, filled in on first connect.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cert_sha256: Option<String>,
    },
}

impl ProfileKind {
    /// The tuning a local profile runs with: mirai's defaults, with whatever the user
    /// changed in Preferences applied over them.
    ///
    /// Meaningful only for a profile with no custom `config`; with one, KataGo reads the
    /// user's file and only the two thread values are passed as overrides.
    pub fn tuning(&self) -> EngineTuning {
        let base = EngineTuning::default();
        let ProfileKind::Local {
            analysis_threads,
            search_threads,
            nn_max_batch_size,
            nn_cache_size_power_of_two,
            ..
        } = self
        else {
            return base;
        };
        EngineTuning {
            analysis_threads: analysis_threads.unwrap_or(base.analysis_threads),
            search_threads: search_threads.unwrap_or(base.search_threads),
            nn_max_batch_size: nn_max_batch_size.unwrap_or(base.nn_max_batch_size),
            nn_cache_size_power_of_two: nn_cache_size_power_of_two
                .unwrap_or(base.nn_cache_size_power_of_two),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineProfile {
    pub name: String,
    #[serde(flatten)]
    pub kind: ProfileKind,
}

impl EngineProfile {
    pub fn is_local(&self) -> bool {
        matches!(self.kind, ProfileKind::Local { .. })
    }

    /// A one-line summary for the preferences list.
    pub fn subtitle(&self) -> String {
        match &self.kind {
            ProfileKind::Local { model, .. } => model
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| model.display().to_string()),
            ProfileKind::Remote { url, engine, .. } => match engine {
                Some(e) => format!("{url} ({e})"),
                None => url.clone(),
            },
        }
    }
}

/// Ceiling on the candidates stored per node. Display may be unbounded — that is one board —
/// but a [`mirai_core::NodeAnalysis`] is kept for every node and written into SGF, so the
/// stored copy always has a limit. 50 is the maximum the preferences row offers, so no
/// non-zero setting is affected by it.
pub const MAX_STORED_CANDIDATES: usize = 50;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisSettings {
    pub live_max_visits: u32,
    pub report_interval_ms: u16,
    pub batch_visits: u32,
    /// `0` means "every candidate the engine returned"; resolve it with
    /// [`AnalysisSettings::suggestion_limit`] rather than reading it directly.
    pub max_suggestions: u8,
}

impl AnalysisSettings {
    /// How many candidates to draw and to list.
    pub fn suggestion_limit(&self) -> usize {
        if self.max_suggestions == 0 {
            usize::MAX
        } else {
            self.max_suggestions as usize
        }
    }

    /// [`Self::suggestion_limit`], clamped to what is worth keeping on every node.
    pub fn stored_suggestion_limit(&self) -> usize {
        self.suggestion_limit().min(MAX_STORED_CANDIDATES)
    }
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        AnalysisSettings {
            live_max_visits: 1_000_000,
            report_interval_ms: 100,
            batch_visits: 100,
            max_suggestions: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum StrengthSetting {
    Visits { visits: u32 },
    Time { time_ms: u32 },
    Human { profile: String },
}

impl Default for StrengthSetting {
    fn default() -> Self {
        StrengthSetting::Visits { visits: 800 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaySettings {
    pub strength: StrengthSetting,
    pub temperature: f32,
    pub resign_threshold: f32,
    pub resign_streak: u8,
    pub rules: RuleSet,
}

impl Default for PlaySettings {
    fn default() -> Self {
        PlaySettings {
            strength: StrengthSetting::default(),
            temperature: 0.0,
            resign_threshold: 0.05,
            resign_streak: 3,
            rules: RuleSet::Chinese,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub show_coordinates: bool,
    pub show_move_numbers: bool,
    pub ownership_overlay: bool,
    pub policy_overlay: bool,
    pub save_analysis_in_sgf: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            show_coordinates: true,
            show_move_numbers: false,
            ownership_overlay: false,
            policy_overlay: false,
            save_analysis_in_sgf: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_engine: Option<String>,
    #[serde(rename = "engine_profile", skip_serializing_if = "Vec::is_empty")]
    pub engine_profiles: Vec<EngineProfile>,
    pub analysis: AnalysisSettings,
    pub play: PlaySettings,
    pub ui: UiSettings,
}

impl Config {
    /// `$XDG_CONFIG_HOME/mirai/config.toml`.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        let dirs = directories::ProjectDirs::from("io.github", "mirai", "mirai")
            .ok_or(ConfigError::NoHome)?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// `$XDG_DATA_HOME/mirai`, for the autosave and KataGo logs.
    pub fn data_dir() -> Result<PathBuf, ConfigError> {
        let dirs = directories::ProjectDirs::from("io.github", "mirai", "mirai")
            .ok_or(ConfigError::NoHome)?;
        Ok(dirs.data_dir().to_path_buf())
    }

    /// Loads the configuration, seeding a first-run one when the file does not exist.
    /// A missing file is not an error; a malformed one is.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::seeded()),
            Err(e) => Err(ConfigError::Io(path.to_path_buf(), e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io(dir.to_path_buf(), e))?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).map_err(|e| ConfigError::Io(path.to_path_buf(), e))
    }

    /// Writes the configuration, keeping edits another window has made meanwhile.
    ///
    /// Every window holds the `Config` it loaded when it opened, so the plain overwrite
    /// above would revert whatever a second window changed since — the last window to close
    /// would win, silently. This is a three-way merge at the TOML level: only the keys that
    /// differ between `base` (what this window loaded) and `self` (what it holds now) are
    /// written over the file as it stands, so untouched keys keep the file's values.
    pub fn save_merged(&self, base: &Config, path: &Path) -> Result<(), ConfigError> {
        if self == base && path.exists() {
            return Ok(());
        }
        let current = toml::Value::try_from(self)?;
        let previous = toml::Value::try_from(base)?;
        // `toml::from_str`, not `str::parse`: the latter reads a bare value, so a whole
        // document silently comes back as an error and every other window's edit with it.
        let mut merged = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
            // A missing or unreadable file is simply one with nothing to preserve.
            .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
        overlay(&previous, &current, &mut merged);

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io(dir.to_path_buf(), e))?;
        }
        let text = toml::to_string_pretty(&merged)?;
        std::fs::write(path, text).map_err(|e| ConfigError::Io(path.to_path_buf(), e))
    }

    /// A first-run configuration: a working local profile if a KataGo installation can be
    /// found, otherwise an empty profile list so the UI prompts for one.
    pub fn seeded() -> Config {
        let mut cfg = Config::default();
        if let Some((katago, model)) = discover_katago() {
            cfg.engine_profiles.push(EngineProfile {
                name: "local-default".into(),
                kind: ProfileKind::Local {
                    katago,
                    model,
                    // No analysis config and no tuning: mirai generates both.
                    config: None,
                    analysis_threads: None,
                    search_threads: None,
                    nn_max_batch_size: None,
                    nn_cache_size_power_of_two: None,
                },
            });
            cfg.active_engine = Some("local-default".into());
        }
        cfg
    }

    pub fn profile(&self, name: &str) -> Option<&EngineProfile> {
        self.engine_profiles.iter().find(|p| p.name == name)
    }

    pub fn profile_mut(&mut self, name: &str) -> Option<&mut EngineProfile> {
        self.engine_profiles.iter_mut().find(|p| p.name == name)
    }

    /// The profile the app should use: the configured one, else the first available.
    pub fn active_profile(&self) -> Option<&EngineProfile> {
        self.active_engine
            .as_deref()
            .and_then(|n| self.profile(n))
            .or_else(|| self.engine_profiles.first())
    }

    /// Records a TOFU pin for a remote profile.
    pub fn set_pin(&mut self, name: &str, fingerprint: &str) {
        if let Some(p) = self.profile_mut(name)
            && let ProfileKind::Remote { cert_sha256, .. } = &mut p.kind
        {
            *cert_sha256 = Some(fingerprint.to_string());
        }
    }
}

/// Applies the `previous → current` difference onto `file`, leaving everything else alone.
///
/// Recursing per key rather than replacing whole tables is the point: two windows editing
/// different keys of the same `[analysis]` table must both survive.
fn overlay(previous: &toml::Value, current: &toml::Value, file: &mut toml::Value) {
    if previous == current {
        return;
    }
    let (Some(prev), Some(cur)) = (previous.as_table(), current.as_table()) else {
        *file = current.clone();
        return;
    };
    let Some(out) = file.as_table_mut() else {
        *file = current.clone();
        return;
    };
    for (name, value) in cur {
        match (prev.get(name), out.get_mut(name)) {
            (Some(before), Some(target)) => overlay(before, value, target),
            // Untouched by this window but absent from the file: nothing to preserve.
            (Some(before), None) if before == value => {}
            _ => {
                out.insert(name.clone(), value.clone());
            }
        }
    }
    for name in prev.keys() {
        if !cur.contains_key(name) {
            out.remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discovery returns every valid network, newest first within each directory, and merges
    /// directories without duplicating a path.
    #[test]
    fn network_discovery_merges_every_bin_gz_and_ignores_everything_else() {
        let base = std::env::temp_dir().join("mirai-discovery-test");
        let first = base.join("first");
        let second = base.join("second");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        assert!(network_files(&first).is_empty());

        for name in [
            "analysis.cfg",
            "katago",
            "notes.bin.gz.txt",
            "model.bin.gz.tmp",
        ] {
            std::fs::write(first.join(name), "x").unwrap();
        }
        assert!(
            network_files(&first).is_empty(),
            "only *.bin.gz is a network"
        );

        std::fs::write(first.join("old.bin.gz"), "x").unwrap();
        // Second-resolution timestamps on some filesystems would otherwise tie.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(first.join("new.bin.gz"), "x").unwrap();
        std::fs::write(second.join("other.bin.gz"), "x").unwrap();
        assert_eq!(
            merge_candidates(
                vec![first.clone(), first.clone(), second.clone()],
                network_files,
            ),
            vec![
                first.join("new.bin.gz"),
                first.join("old.bin.gz"),
                second.join("other.bin.gz"),
            ],
            "directory priority, in-directory recency and exact-path de-duplication must hold"
        );

        assert!(network_files(&base.join("absent")).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn xdg_system_directories_keep_precedence_and_ignore_relative_entries() {
        let encoded = std::env::join_paths(["/opt/katago-a", "relative", "/opt/katago-b"])
            .expect("join XDG path list");
        assert_eq!(
            xdg_dir_list(Some(&encoded), &["/fallback"]),
            ["/opt/katago-a", "/opt/katago-b"]
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            xdg_dir_list(Some(OsStr::new("")), &["/usr/local/share", "/usr/share"]),
            ["/usr/local/share", "/usr/share"]
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn discovery_directories_follow_the_documented_order() {
        let models = model_dirs(
            XdgRoots {
                home: Some("/home/user/data".into()),
                system: vec!["/opt/share-a".into(), "/opt/share-b".into()],
            },
            Some(Path::new("/home/user")),
        );
        assert_eq!(
            models,
            [
                "/home/user/data/katago/models",
                "/home/user/data/mirai/models",
                "/home/user/.katago/models",
                "/opt/share-a/katago/models",
                "/opt/share-a/mirai/models",
                "/opt/share-b/katago/models",
                "/opt/share-b/mirai/models",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
        );

        let configs = analysis_config_dirs(XdgRoots {
            home: Some("/home/user/config".into()),
            system: vec!["/etc/xdg-a".into(), "/etc/xdg-b".into()],
        });
        assert_eq!(
            configs,
            [
                "/home/user/config/katago/cfg/analysis",
                "/home/user/config/mirai/cfg/analysis",
                "/etc/xdg-a/katago/cfg/analysis",
                "/etc/xdg-a/mirai/cfg/analysis",
                "/etc/xdg-b/katago/cfg/analysis",
                "/etc/xdg-b/mirai/cfg/analysis",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn analysis_config_discovery_returns_every_analysis_config_but_not_gtp() {
        let dir = std::env::temp_dir().join(format!("mirai-analysis-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gtp.cfg"), "x").unwrap();
        assert!(analysis_config_files(&dir).is_empty());

        std::fs::write(dir.join("analysis_example.cfg"), "x").unwrap();
        std::fs::write(dir.join("analysis.cfg"), "x").unwrap();
        assert_eq!(
            analysis_config_files(&dir),
            vec![dir.join("analysis.cfg"), dir.join("analysis_example.cfg")],
            "the conventional config comes first without hiding other choices"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two windows each hold the config they loaded. One changing a setting must not revert
    /// the other's change to a different setting — the defect that made the last window to
    /// close win.
    #[test]
    fn a_save_keeps_another_windows_edit() {
        let dir = std::env::temp_dir().join(format!("mirai-cfg-{}", std::process::id()));
        let path = dir.join("merge.toml");
        let _ = std::fs::remove_dir_all(&dir);

        // Both windows opened on this.
        let base = Config::default();
        base.save(&path).expect("write the baseline");

        // The other window raised the visit cap and wrote it out.
        let mut other = base.clone();
        other.analysis.live_max_visits = 4242;
        other.save_merged(&base, &path).expect("other window saves");

        // This window only ever touched a display toggle.
        let mut mine = base.clone();
        mine.ui.show_coordinates = !base.ui.show_coordinates;
        mine.save_merged(&base, &path).expect("this window saves");

        let merged = Config::load(&path).expect("reload");
        assert_eq!(
            merged.analysis.live_max_visits, 4242,
            "the other edit was reverted"
        );
        assert_eq!(merged.ui.show_coordinates, mine.ui.show_coordinates);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting a profile has to reach the file even though the merge is additive per key.
    #[test]
    fn a_removed_profile_is_removed_from_the_file() {
        let dir = std::env::temp_dir().join(format!("mirai-cfg-rm-{}", std::process::id()));
        let path = dir.join("merge.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let base = Config {
            engine_profiles: vec![EngineProfile {
                name: "gone".into(),
                kind: ProfileKind::Remote {
                    url: "mirai://h".into(),
                    token: "t".into(),
                    engine: None,
                    cert_sha256: None,
                },
            }],
            ..Config::default()
        };
        base.save(&path).expect("write the baseline");

        let mut mine = base.clone();
        mine.engine_profiles.clear();
        mine.save_merged(&base, &path).expect("save");

        assert!(
            Config::load(&path)
                .expect("reload")
                .engine_profiles
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `0` is the only value that means something other than a count, and the stored copy
    /// must stay bounded whatever the board is asked to draw.
    #[test]
    fn zero_suggestions_means_every_candidate_but_storage_stays_capped() {
        let mut settings = AnalysisSettings::default();
        assert_eq!(settings.suggestion_limit(), 10);
        assert_eq!(settings.stored_suggestion_limit(), 10);
        settings.max_suggestions = 0;
        assert_eq!(settings.suggestion_limit(), usize::MAX);
        assert_eq!(settings.stored_suggestion_limit(), MAX_STORED_CANDIDATES);
    }

    #[test]
    fn round_trips_through_toml_with_both_profile_kinds() {
        let cfg = Config {
            active_engine: Some("workstation".into()),
            engine_profiles: vec![
                EngineProfile {
                    name: "local-default".into(),
                    kind: ProfileKind::Local {
                        katago: "/opt/katago".into(),
                        model: "/opt/model.bin.gz".into(),
                        config: Some("/opt/analysis.cfg".into()),
                        analysis_threads: Some(2),
                        search_threads: Some(16),
                        nn_max_batch_size: None,
                        nn_cache_size_power_of_two: None,
                    },
                },
                EngineProfile {
                    name: "workstation".into(),
                    kind: ProfileKind::Remote {
                        url: "mirai://192.168.1.10:9678".into(),
                        token: "deadbeef".into(),
                        engine: Some("default".into()),
                        cert_sha256: None,
                    },
                },
            ],
            analysis: AnalysisSettings {
                batch_visits: 2000,
                ..Default::default()
            },
            play: PlaySettings {
                temperature: 0.35,
                ..Default::default()
            },
            ui: UiSettings {
                ownership_overlay: true,
                ..Default::default()
            },
        };

        let text = toml::to_string_pretty(&cfg).expect("serialise");
        let back: Config = toml::from_str(&text).expect("parse back");
        assert_eq!(
            back, cfg,
            "config did not survive a TOML round trip:\n{text}"
        );
        assert!(back.active_profile().unwrap().name == "workstation");
    }

    #[test]
    fn parses_the_documented_config_shape() {
        // Exactly the layout documented in the plan.
        let text = r#"
active_engine = "local-default"

[[engine_profile]]
name = "local-default"
kind = "local"
katago = "/k/katago"
model  = "/k/default.bin.gz"
config = "/k/analysis.cfg"
analysis_threads = 2
search_threads = 16

[[engine_profile]]
name = "workstation"
kind = "remote"
url = "mirai://192.168.1.10:9678"
token = "tok"
engine = "default"
cert_sha256 = "ab12"

[analysis]
live_max_visits = 1000000
report_interval_ms = 100
batch_visits = 1000
max_suggestions = 10

[play]
strength = { kind = "visits", visits = 800 }
temperature = 0.0
resign_threshold = 0.05
resign_streak = 3

[ui]
show_coordinates = true
show_move_numbers = false
ownership_overlay = false
policy_overlay = false
save_analysis_in_sgf = false
"#;
        let cfg: Config = toml::from_str(text).expect("documented shape must parse");
        assert_eq!(cfg.engine_profiles.len(), 2);
        assert!(cfg.engine_profiles[0].is_local());
        assert_eq!(cfg.analysis.live_max_visits, 1_000_000);
        assert_eq!(cfg.play.strength, StrengthSetting::Visits { visits: 800 });
        match &cfg.engine_profiles[1].kind {
            ProfileKind::Remote { cert_sha256, .. } => {
                assert_eq!(cert_sha256.as_deref(), Some("ab12"))
            }
            _ => panic!("second profile should be remote"),
        }
    }

    #[test]
    fn missing_file_yields_a_seeded_config_not_an_error() {
        let path = std::env::temp_dir().join("mirai-no-such-config-9f3a.toml");
        let _ = std::fs::remove_file(&path);
        let cfg = Config::load(&path).expect("a missing config is not an error");
        // Seeding is environment-dependent; what must hold is that it parses and that a
        // seeded profile is consistent with `active_engine`.
        if let Some(active) = &cfg.active_engine {
            assert!(cfg.profile(active).is_some());
        }
    }

    #[test]
    fn malformed_config_is_an_error() {
        let path = std::env::temp_dir().join("mirai-bad-config-9f3a.toml");
        std::fs::write(&path, "active_engine = [unclosed").unwrap();
        assert!(matches!(Config::load(&path), Err(ConfigError::Parse(..))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pins_are_recorded_on_remote_profiles_only() {
        let mut cfg = Config {
            engine_profiles: vec![EngineProfile {
                name: "r".into(),
                kind: ProfileKind::Remote {
                    url: "mirai://h".into(),
                    token: "t".into(),
                    engine: None,
                    cert_sha256: None,
                },
            }],
            ..Default::default()
        };
        cfg.set_pin("r", "abc");
        match &cfg.profile("r").unwrap().kind {
            ProfileKind::Remote { cert_sha256, .. } => {
                assert_eq!(cert_sha256.as_deref(), Some("abc"))
            }
            _ => unreachable!(),
        }
    }
}
