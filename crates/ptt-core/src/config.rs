//! Config model, load/save and validation (plan §8).
//!
//! M3 adds the rest of the §8 document (`version`, `[binding]`, `[app]`,
//! `device_id`, `on_exit`) plus load, validation, migration and atomic
//! saves. Loading **never fails**: a missing, unreadable or corrupt file
//! yields defaults with an explanation the caller can log.

use crate::error::{Error, Result};
use crate::input::{Binding, MouseButton};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The version this build writes (plan §8).
pub const CURRENT_VERSION: u32 = 1;

/// Plan §8: `release_delay_ms` is validated to 0–2000 ms.
pub const MAX_RELEASE_DELAY_MS: u64 = 2000;

/// Plan §8 default binding: Caps Lock.
const DEFAULT_VK: u16 = 0x14;
const DEFAULT_SCAN: u16 = 0x3A;

/// Effective configuration (plan §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// Master switch: when false the app sits in [`crate::state::State::Disabled`].
    pub enabled: bool,
    pub binding: BindingConfig,
    pub audio: AudioConfig,
    pub sounds: SoundsConfig,
    pub app: AppConfig,
}

/// `[binding]` in `config.toml` (plan §8). The plain field types mirror the
/// file exactly; [`Config::binding`] turns them into an [`input::Binding`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingConfig {
    /// `"key"` or `"mouse"`.
    pub kind: String,
    pub vk: u16,
    pub scan: u16,
    /// `""` unless `kind = "mouse"`.
    pub mouse_button: String,
    pub swallow: bool,
}

/// `[audio]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Endpoint id, or the special value `"default"` to follow the system
    /// default (plan §7: store the id, never an index).
    pub device_id: String,
    /// Hold-off before muting after key release, so word endings are not
    /// clipped (plan §1: default 200 ms; §8: 0–2000 ms).
    pub release_delay_ms: u64,
    pub on_exit: OnExit,
}

/// `[audio] on_exit` (plan §6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnExit {
    /// Hand back the mute state found at start-up (default).
    Restore,
    /// Always leave the microphone open.
    Unmute,
}

/// `[sounds]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundsConfig {
    pub enabled: bool,
    /// 0.0 – 1.0 (plan §8).
    pub volume: f32,
}

/// `[app]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub start_with_windows: bool,
    pub start_hidden: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            enabled: true,
            binding: BindingConfig::default(),
            audio: AudioConfig::default(),
            sounds: SoundsConfig::default(),
            app: AppConfig::default(),
        }
    }
}

impl Default for BindingConfig {
    fn default() -> Self {
        Self {
            kind: "key".to_string(),
            vk: DEFAULT_VK,
            scan: DEFAULT_SCAN,
            mouse_button: String::new(),
            swallow: true,
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device_id: "default".to_string(),
            release_delay_ms: 200,
            on_exit: OnExit::Restore,
        }
    }
}

impl Default for SoundsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 0.5,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            start_hidden: true,
        }
    }
}

/// Where a [`Config::load`] got its data from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSource {
    /// The file existed and parsed.
    File,
    /// No file yet: defaults are correct.
    Missing,
    /// Unreadable or unparsable: defaults were substituted.
    Corrupt,
}

/// What `load` had to say — the caller logs these (plan §8: "log … never
/// crash").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    pub source: LoadSource,
    pub messages: Vec<String>,
}

/// One-off overrides for a single run — the `ptt ptt` flags (plan §8). Every
/// field left `None` means "the file decides"; the tray app, which has no
/// flags, resolves with [`Overrides::default`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub device: Option<String>,
    pub binding: Option<Binding>,
    pub release_delay_ms: Option<u64>,
    pub swallow: Option<bool>,
}

impl Config {
    /// Load `path`, apply `overrides` on top and write the result back (plan
    /// §8): the file is the source of truth, an override is a one-off, and
    /// an unreadable file is set aside as evidence instead of being silently
    /// destroyed. Returns the effective config plus everything worth
    /// logging.
    pub fn resolve(path: &Path, overrides: &Overrides) -> Result<(Config, Vec<String>)> {
        let (mut config, report) = Config::load(path);
        let corrupt = report.source == LoadSource::Corrupt;
        let mut messages = report.messages;

        if corrupt {
            let backup = path.with_extension("toml.bad");
            match std::fs::rename(path, &backup) {
                Ok(()) => messages.push(format!("unreadable config kept at {}", backup.display())),
                Err(error) => {
                    messages.push(format!("could not keep the unreadable config: {error}"))
                }
            }
        }

        if let Some(device) = &overrides.device {
            config.audio.device_id = device.clone();
        }
        if let Some(binding) = &overrides.binding {
            let swallow = overrides.swallow.unwrap_or(config.binding.swallow);
            config.set_binding(binding, swallow);
        } else if let Some(swallow) = overrides.swallow {
            config.binding.swallow = swallow;
        }
        if let Some(release_delay_ms) = overrides.release_delay_ms {
            config.audio.release_delay_ms = release_delay_ms;
        }

        messages.extend(config.validate());
        config.save(path)?;
        Ok((config, messages))
    }
    /// Read `path`. Never fails: a missing, unreadable or corrupt file
    /// yields defaults plus an explanation (plan §8).
    pub fn load(path: &Path) -> (Config, LoadReport) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (
                    Config::default(),
                    LoadReport {
                        source: LoadSource::Missing,
                        messages: vec![format!("no config at {}; using defaults", path.display())],
                    },
                );
            }
            Err(error) => {
                return (
                    Config::default(),
                    LoadReport {
                        source: LoadSource::Corrupt,
                        messages: vec![format!(
                            "cannot read {} ({error}); using defaults",
                            path.display()
                        )],
                    },
                );
            }
        };

        let mut config: Config = match toml::from_str(&text) {
            Ok(config) => config,
            Err(error) => {
                return (
                    Config::default(),
                    LoadReport {
                        source: LoadSource::Corrupt,
                        messages: vec![format!("config is corrupt ({error}); using defaults")],
                    },
                );
            }
        };

        let mut messages = Vec::new();
        if let Some(previous) = config.migrate() {
            messages.push(format!(
                "migrated config from version {previous} to {CURRENT_VERSION}"
            ));
        }
        messages.extend(config.validate());

        (
            config,
            LoadReport {
                source: LoadSource::File,
                messages,
            },
        )
    }

    /// Write atomically: a sibling temp file, then a rename over the target
    /// (plan §8).
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|error| Error::Write(error.to_string()))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }

    /// Bring the file to [`CURRENT_VERSION`] (plan §8: keep a migration
    /// function from day one). Returns the previous version when it changed.
    pub fn migrate(&mut self) -> Option<u32> {
        if self.version == CURRENT_VERSION {
            return None;
        }
        let previous = self.version;
        self.version = CURRENT_VERSION;
        Some(previous)
    }

    /// Clamp values outside the plan §8 ranges and repair an unusable
    /// binding; returns one message per correction.
    pub fn validate(&mut self) -> Vec<String> {
        let mut messages = Vec::new();

        if self.audio.release_delay_ms > MAX_RELEASE_DELAY_MS {
            messages.push(format!(
                "release_delay_ms {} is out of range; using {MAX_RELEASE_DELAY_MS}",
                self.audio.release_delay_ms
            ));
            self.audio.release_delay_ms = MAX_RELEASE_DELAY_MS;
        }

        if !(0.0..=1.0).contains(&self.sounds.volume) {
            let clamped = self.sounds.volume.clamp(0.0, 1.0);
            messages.push(format!(
                "volume {} is out of range; using {clamped}",
                self.sounds.volume
            ));
            self.sounds.volume = clamped;
        }

        match self.binding.kind.as_str() {
            "key" if self.binding.vk == 0 => {
                messages.push("binding kind \"key\" with vk 0; using the default key".into());
                self.binding = BindingConfig::default();
            }
            "key" => {}
            "mouse" if MouseButton::from_name(&self.binding.mouse_button).is_err() => {
                messages.push(format!(
                    "mouse_button {:?} is unknown; using x1",
                    self.binding.mouse_button
                ));
                self.binding.mouse_button = MouseButton::X1.name().to_string();
            }
            "mouse" => {}
            other => {
                messages.push(format!(
                    "binding kind {other:?} is unknown; using the default key"
                ));
                self.binding = BindingConfig::default();
            }
        }

        messages
    }

    /// The bound input this config asks for (plan §4).
    pub fn binding(&self) -> Binding {
        if self.binding.kind == "mouse" {
            if let Ok(button) = MouseButton::from_name(&self.binding.mouse_button) {
                return Binding::Mouse(button);
            }
        }
        Binding::Key {
            vk: self.binding.vk,
            scan: self.binding.scan,
        }
    }

    /// Store a binding the way the file represents it (plan §8).
    pub fn set_binding(&mut self, binding: &Binding, swallow: bool) {
        match *binding {
            Binding::Key { vk, scan } => {
                self.binding.kind = "key".to_string();
                self.binding.vk = vk;
                self.binding.scan = scan;
                self.binding.mouse_button.clear();
            }
            Binding::Mouse(button) => {
                self.binding.kind = "mouse".to_string();
                self.binding.mouse_button = button.name().to_string();
            }
        }
        self.binding.swallow = swallow;
    }
}

/// `%APPDATA%\ptt-tool\config.toml` (plan §2).
pub fn default_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("ptt-tool")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ptt-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn defaults_match_the_plan() {
        let cfg = Config::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.audio.release_delay_ms, 200);
        assert!(cfg.sounds.enabled);
        assert_eq!(cfg.sounds.volume, 0.5);
    }

    #[test]
    fn default_path_lives_under_ptt_tool() {
        assert!(default_path().ends_with(std::path::Path::new("ptt-tool").join("config.toml")));
    }

    // --- load ------------------------------------------------------------

    #[test]
    fn a_missing_file_yields_defaults_and_says_so() {
        let path = scratch("missing.toml");
        let _ = std::fs::remove_file(&path);

        let (cfg, report) = Config::load(&path);

        assert_eq!(cfg, Config::default());
        assert_eq!(report.source, LoadSource::Missing);
        assert!(
            report.messages.iter().any(|m| m.contains("defaults")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn a_valid_file_is_loaded_as_is() {
        let path = scratch("valid.toml");
        std::fs::write(
            &path,
            r##"version = 1
enabled = true

[binding]
kind = "mouse"
vk = 0x14
scan = 0x3A
mouse_button = "x2"
swallow = false

[audio]
device_id = "{0.0.1.00000000}.abc"
release_delay_ms = 50
on_exit = "unmute"

[sounds]
enabled = false
volume = 0.25

[app]
start_with_windows = true
start_hidden = false
"##,
        )
        .unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(report.source, LoadSource::File);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.binding.kind, "mouse");
        assert_eq!(cfg.binding.mouse_button, "x2");
        assert!(!cfg.binding.swallow);
        assert_eq!(cfg.audio.device_id, "{0.0.1.00000000}.abc");
        assert_eq!(cfg.audio.release_delay_ms, 50);
        assert_eq!(cfg.audio.on_exit, OnExit::Unmute);
        assert!(!cfg.sounds.enabled);
        assert_eq!(cfg.sounds.volume, 0.25);
        assert!(cfg.app.start_with_windows);
        assert!(!cfg.app.start_hidden);
    }

    #[test]
    fn a_corrupt_file_falls_back_to_defaults() {
        let path = scratch("corrupt.toml");
        std::fs::write(&path, "this is [not toml at all").unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(cfg, Config::default());
        assert_eq!(report.source, LoadSource::Corrupt);
        assert!(
            report.messages.iter().any(|m| m.contains("defaults")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn out_of_range_values_are_corrected_and_reported() {
        let path = scratch("range.toml");
        std::fs::write(
            &path,
            "[audio]\nrelease_delay_ms = 5000\n[sounds]\nvolume = 4.0\n",
        )
        .unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(cfg.audio.release_delay_ms, 2000, "plan §8 upper bound");
        assert_eq!(cfg.sounds.volume, 1.0, "plan §8 upper bound");
        assert!(report
            .messages
            .iter()
            .any(|m| m.contains("release_delay_ms")));
        assert!(report.messages.iter().any(|m| m.contains("volume")));
    }

    #[test]
    fn an_unknown_binding_kind_falls_back_to_the_default_binding() {
        let path = scratch("kind.toml");
        std::fs::write(&path, "[binding]\nkind = \"thumb\"\n").unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(cfg.binding(), Config::default().binding());
        assert_eq!(cfg.binding.kind, "key");
        assert!(report.messages.iter().any(|m| m.contains("kind")));
    }

    #[test]
    fn an_older_version_is_migrated_to_the_current_one() {
        let path = scratch("old.toml");
        std::fs::write(&path, "version = 0\n").unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(cfg.version, CURRENT_VERSION);
        assert!(
            report.messages.iter().any(|m| m.contains("version 0")),
            "{:?}",
            report.messages
        );
    }

    // --- save ------------------------------------------------------------

    #[test]
    fn save_then_load_round_trips() {
        let path = scratch("roundtrip.toml");
        let mut cfg = Config::default();
        cfg.audio.release_delay_ms = 75;
        cfg.sounds.volume = 0.9;
        cfg.enabled = false;

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert_eq!(loaded, cfg, "what went in comes out");
        assert_eq!(report.source, LoadSource::File);
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "the temporary file is gone (atomic rename)"
        );
    }

    #[test]
    fn save_reports_io_failures_instead_of_panicking() {
        let path = scratch("unwritable.toml");
        let impossible = path.join("\0").join("config.toml");

        let err = Config::default()
            .save(&impossible)
            .expect_err("cannot write there");
        assert!(!err.to_string().is_empty());
    }

    // --- binding ---------------------------------------------------------

    #[test]
    fn the_config_binding_converts_to_the_input_binding() {
        assert_eq!(
            Config::default().binding(),
            Binding::Key {
                vk: DEFAULT_VK,
                scan: DEFAULT_SCAN
            }
        );

        let mut cfg = Config::default();
        cfg.binding.kind = "mouse".into();
        cfg.binding.mouse_button = "x1".into();
        assert_eq!(cfg.binding(), Binding::Mouse(MouseButton::X1));
    }

    #[test]
    fn set_binding_writes_the_fields_the_file_uses() {
        let mut cfg = Config::default();

        cfg.set_binding(&Binding::Mouse(MouseButton::X2), false);
        assert_eq!(cfg.binding.kind, "mouse");
        assert_eq!(cfg.binding.mouse_button, "x2");
        assert!(!cfg.binding.swallow);

        cfg.set_binding(
            &Binding::Key {
                vk: 0x41,
                scan: 0x1e,
            },
            true,
        );
        assert_eq!(cfg.binding.kind, "key");
        assert_eq!(cfg.binding.vk, 0x41);
        assert_eq!(cfg.binding.scan, 0x1e);
        assert_eq!(cfg.binding.mouse_button, "");
        assert!(cfg.binding.swallow);
    }

    // --- resolve: file first, overrides on top (plan §8) ------------------

    /// A `config.toml` with values nothing on the command line asked for.
    fn write_file_config(path: &Path) {
        let mut file = Config::default();
        file.audio.release_delay_ms = 500;
        file.audio.device_id = "file-device".into();
        file.binding.kind = "mouse".into();
        file.binding.mouse_button = "x1".into();
        file.binding.swallow = false;
        file.save(path).unwrap();
    }

    #[test]
    fn overrides_win_over_the_file() {
        let path = scratch("overrides.toml");
        write_file_config(&path);

        let overrides = Overrides {
            device: Some("flag-device".into()),
            binding: Some(Binding::Key { vk: 0x41, scan: 0 }),
            release_delay_ms: Some(0),
            swallow: Some(true),
        };
        let (cfg, _messages) = Config::resolve(&path, &overrides).unwrap();

        assert_eq!(cfg.audio.release_delay_ms, 0);
        assert_eq!(cfg.audio.device_id, "flag-device");
        assert_eq!(cfg.binding(), Binding::Key { vk: 0x41, scan: 0 });
        assert!(cfg.binding.swallow);
    }

    #[test]
    fn the_config_file_decides_when_no_override_is_given() {
        let path = scratch("file-decides.toml");
        write_file_config(&path);

        let (cfg, _messages) = Config::resolve(&path, &Overrides::default()).unwrap();

        assert_eq!(cfg.audio.release_delay_ms, 500, "the file's delay is used");
        assert_eq!(cfg.audio.device_id, "file-device");
        assert_eq!(cfg.binding(), Binding::Mouse(MouseButton::X1));
        assert!(!cfg.binding.swallow, "the file's swallow flag is used");
    }

    #[test]
    fn resolving_a_first_run_creates_the_config_file() {
        let path = scratch("first-run.toml");
        let _ = std::fs::remove_file(&path);

        let (cfg, _messages) = Config::resolve(&path, &Overrides::default()).unwrap();

        assert!(path.exists(), "config.toml is written for the user to edit");
        assert_eq!(cfg, Config::default());
        let (reloaded, report) = Config::load(&path);
        assert_eq!(report.source, LoadSource::File, "what was written reloads");
        assert_eq!(reloaded, Config::default());
    }

    #[test]
    fn an_unreadable_config_is_kept_as_evidence_and_replaced_by_defaults() {
        let path = scratch("corrupt-resolve.toml");
        std::fs::write(&path, "not [valid toml").unwrap();

        let (cfg, messages) = Config::resolve(&path, &Overrides::default()).unwrap();

        assert_eq!(cfg, Config::default(), "defaults take over");
        assert!(
            path.with_extension("toml.bad").exists(),
            "the broken file is kept for the user to look at"
        );
        let (reloaded, report) = Config::load(&path);
        assert_eq!(report.source, LoadSource::File, "the file is valid again");
        assert_eq!(reloaded, Config::default());
        assert!(
            messages.iter().any(|m| m.contains(".bad")),
            "the backup location is logged: {messages:?}"
        );
    }
}
