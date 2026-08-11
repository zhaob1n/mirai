// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `$XDG_CONFIG_HOME/mirai/config.toml` — engine profiles and preferences.

use std::path::{Path, PathBuf};

use mirai_core::RuleSet;
use mirai_engine::EngineTuning;
use serde::{Deserialize, Serialize};

/// Bundled KataGo shipped with LizzieYzy. Only ever used to seed a first-run profile;
/// nothing in the code depends on it existing.
const SEED_KATAGO: &str =
    "/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/linux-x64/katago";
const SEED_MODEL: &str = "/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/weights/default.bin.gz";

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisSettings {
    pub live_max_visits: u32,
    pub report_interval_ms: u16,
    pub batch_visits: u32,
    pub max_suggestions: u8,
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        AnalysisSettings {
            live_max_visits: 1_000_000,
            report_interval_ms: 100,
            batch_visits: 1000,
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

    /// A first-run configuration. If the bundled KataGo is present, seed a working local
    /// profile from it; otherwise leave the profile list empty so the UI can prompt.
    pub fn seeded() -> Config {
        let mut cfg = Config::default();
        if Path::new(SEED_KATAGO).exists() && Path::new(SEED_MODEL).exists() {
            cfg.engine_profiles.push(EngineProfile {
                name: "local-default".into(),
                kind: ProfileKind::Local {
                    katago: SEED_KATAGO.into(),
                    model: SEED_MODEL.into(),
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
        assert_eq!(merged.analysis.live_max_visits, 4242, "the other edit was reverted");
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

        assert!(Config::load(&path).expect("reload").engine_profiles.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(back, cfg, "config did not survive a TOML round trip:\n{text}");
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
