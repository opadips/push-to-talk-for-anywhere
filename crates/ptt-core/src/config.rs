//! Config model, load/save and validation (plan §8).
//!
//! M3 adds the rest of the §8 document (`version`, `[binding]`, `[app]`,
//! `device_id`, `on_exit`) plus load, validation, migration and atomic
//! saves. Loading **never fails**: a missing, unreadable or corrupt file
//! yields defaults with an explanation the caller can log.

use crate::error::{Error, Result};
use crate::input::chord::{Chord, ChordKey, Modifiers, Side};
use crate::input::{Binding, MouseButton};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The version this build writes (plan §8).
pub const CURRENT_VERSION: u32 = 1;

/// Plan §8: `release_delay_ms` is validated to 0–2000 ms.
pub const MAX_RELEASE_DELAY_MS: u64 = 2000;

/// Overlay distance from the screen edges is validated to 0–200 px.
pub const MAX_OVERLAY_DISTANCE: u32 = 200;

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
    /// `[overlay]` — the on-screen talk-state badge (overlay design spec).
    #[serde(default)]
    pub overlay: OverlayConfig,
    /// `[toggle]` — the optional second key that latches talk on/off.
    /// Unbound unless the user opts in.
    #[serde(default)]
    pub toggle: ToggleConfig,
}

/// `[binding]` in `config.toml` (plan §8). The plain field types mirror the
/// file exactly; [`Config::binding`] turns them into an [`input::Binding`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingConfig {
    /// `"key"`, `"mouse"` or `"chord"`.
    pub kind: String,
    pub vk: u16,
    pub scan: u16,
    /// `""` unless `kind = "mouse"`.
    pub mouse_button: String,
    /// Chord modifier roles: `"off"`, `"any"`, `"left"` or `"right"`.
    /// Additive (spec §8): a file from before chords existed omits them and
    /// deserialises to `"off"` — the same binding as before, unchanged.
    pub ctrl: String,
    pub shift: String,
    pub alt: String,
    pub win: String,
    pub swallow: bool,
}

/// `[toggle]` in `config.toml` — the optional second binding that toggles
/// talk on/off instead of push-to-talk. Same shape as [`BindingConfig`],
/// but its neutral state is **unbound** (`kind = ""`): a config without a
/// `[toggle]` section behaves exactly like before the feature existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToggleConfig {
    /// `"key"`, `"mouse"`, `"chord"` or `""` (unbound).
    pub kind: String,
    pub vk: u16,
    pub scan: u16,
    /// `""` unless `kind = "mouse"`.
    pub mouse_button: String,
    /// Chord modifier roles: `"off"`, `"any"`, `"left"` or `"right"` —
    /// additive like [`BindingConfig::ctrl`], so an older file loads
    /// unchanged.
    pub ctrl: String,
    pub shift: String,
    pub alt: String,
    pub win: String,
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

/// `[overlay]` in `config.toml` — the on-screen pulsing-dot badge. Off by
/// default so the plan §12 idle budget is untouched unless asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// Master switch: no overlay window exists at all when false.
    pub enabled: bool,
    /// `"talk-only"` (badge only while talking) or `"always"` (dim while
    /// muted, bright while talking).
    pub mode: String,
    /// One of `top|bottom` × `left|center|right`, e.g. `"bottom-right"`.
    pub position: String,
    /// Distance from the screen edges in pixels (0–200).
    pub distance: u32,
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
            overlay: OverlayConfig::default(),
            toggle: ToggleConfig::default(),
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
            ctrl: "off".to_string(),
            shift: "off".to_string(),
            alt: "off".to_string(),
            win: "off".to_string(),
            swallow: true,
        }
    }
}

impl Default for ToggleConfig {
    fn default() -> Self {
        Self {
            kind: String::new(),
            vk: 0,
            scan: 0,
            mouse_button: String::new(),
            ctrl: "off".to_string(),
            shift: "off".to_string(),
            alt: "off".to_string(),
            win: "off".to_string(),
            swallow: true,
        }
    }
}

impl BindingConfig {
    /// The chord this block describes (spec §8), or an error naming the
    /// first field that cannot be trusted — an unknown side word, an
    /// unknown mouse button, or an invalid chord — for the same "cannot be
    /// parsed" treatment an unknown `kind` gets.
    fn chord(&self) -> std::result::Result<Chord, String> {
        chord_from(
            self.vk,
            self.scan,
            &self.mouse_button,
            &self.ctrl,
            &self.shift,
            &self.alt,
            &self.win,
        )
    }

    /// Put the chord side fields back to `"off"` — a non-chord binding
    /// leaves no modifiers behind in the file.
    fn clear_chord_fields(&mut self) {
        self.ctrl = "off".to_string();
        self.shift = "off".to_string();
        self.alt = "off".to_string();
        self.win = "off".to_string();
    }
}

impl ToggleConfig {
    /// [`BindingConfig::chord`] for the toggle block.
    fn chord(&self) -> std::result::Result<Chord, String> {
        chord_from(
            self.vk,
            self.scan,
            &self.mouse_button,
            &self.ctrl,
            &self.shift,
            &self.alt,
            &self.win,
        )
    }

    /// [`BindingConfig::clear_chord_fields`] for the toggle block.
    fn clear_chord_fields(&mut self) {
        self.ctrl = "off".to_string();
        self.shift = "off".to_string();
        self.alt = "off".to_string();
        self.win = "off".to_string();
    }
}

/// The chord the flat `kind = "chord"` fields describe (spec §8), or an
/// error naming the first field that cannot be trusted: an unknown side
/// word, an unknown mouse button, or a chord [`Chord::is_valid`] rejects —
/// a bare single modifier is still not a binding (spec §3). `vk == 0` with
/// no mouse button marks the modifier-only form, exactly as the spec's
/// example config says.
///
/// A non-empty `mouse_button` **wins** over a non-zero `vk`. Spec §8 says
/// `mouse_button` is set *instead of* `vk` when the final member is a
/// button, and every writer in this file zeroes whichever field it does not
/// use ([`Config::set_binding`]), so both can only be filled by a
/// hand-edited file — where the button is the more specific claim about the
/// final member. The rule lives here because this function only reads the
/// flat fields; the writer never has to arbitrate.
fn chord_from(
    vk: u16,
    scan: u16,
    mouse_button: &str,
    ctrl: &str,
    shift: &str,
    alt: &str,
    win: &str,
) -> std::result::Result<Chord, String> {
    let modifiers = Modifiers {
        ctrl: side_from(ctrl).ok_or_else(|| side_problem("ctrl", ctrl))?,
        shift: side_from(shift).ok_or_else(|| side_problem("shift", shift))?,
        alt: side_from(alt).ok_or_else(|| side_problem("alt", alt))?,
        win: side_from(win).ok_or_else(|| side_problem("win", win))?,
    };
    let key = if !mouse_button.is_empty() {
        let button = MouseButton::from_name(mouse_button)
            .map_err(|_| format!("mouse_button {mouse_button:?} is unknown"))?;
        Some(ChordKey::Mouse(button))
    } else if vk != 0 {
        Some(ChordKey::Key { vk, scan })
    } else {
        None
    };
    let chord = Chord { modifiers, key };
    if chord.is_valid() {
        Ok(chord)
    } else {
        Err(format!("{} is not a valid chord", chord.label()))
    }
}

/// What to tell the user when a chord side field is not one of the four
/// words (spec §8).
fn side_problem(field: &str, value: &str) -> String {
    format!("{field} side {value:?} is unknown (expected off|any|left|right)")
}

/// One side field's meaning: `"off"`, `"any"`, `"left"` or `"right"`
/// (spec §8), case-insensitively like [`MouseButton::from_name`].
fn side_from(text: &str) -> Option<Side> {
    match text.to_ascii_lowercase().as_str() {
        "off" => Some(Side::Off),
        "any" => Some(Side::Any),
        "left" => Some(Side::Left),
        "right" => Some(Side::Right),
        _ => None,
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

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "talk-only".to_string(),
            position: "bottom-right".to_string(),
            distance: 24,
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

        if !matches!(self.overlay.mode.as_str(), "talk-only" | "always") {
            messages.push(format!(
                "overlay mode {:?} is unknown; using \"talk-only\"",
                self.overlay.mode
            ));
            self.overlay.mode = "talk-only".to_string();
        }

        if !matches!(
            self.overlay.position.as_str(),
            "top-left"
                | "top-center"
                | "top-right"
                | "bottom-left"
                | "bottom-center"
                | "bottom-right"
        ) {
            messages.push(format!(
                "overlay position {:?} is unknown; using \"bottom-right\"",
                self.overlay.position
            ));
            self.overlay.position = "bottom-right".to_string();
        }

        if self.overlay.distance > MAX_OVERLAY_DISTANCE {
            messages.push(format!(
                "overlay distance {} is out of range; using {MAX_OVERLAY_DISTANCE}",
                self.overlay.distance
            ));
            self.overlay.distance = MAX_OVERLAY_DISTANCE;
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
            // Spec §8: a chord the file cannot describe — an unknown side
            // word, an unknown mouse button, or an invalid (bare-modifier)
            // chord — falls back exactly like an unknown kind, and the
            // message names the field that was wrong.
            "chord" => {
                if let Err(problem) = self.binding.chord() {
                    messages.push(format!(
                        "binding kind \"chord\" is unusable ({problem}); using the default key"
                    ));
                    self.binding = BindingConfig::default();
                }
            }
            other => {
                messages.push(format!(
                    "binding kind {other:?} is unknown; using the default key"
                ));
                self.binding = BindingConfig::default();
            }
        }

        match self.toggle.kind.as_str() {
            "" => {}
            "key" if self.toggle.vk == 0 => {
                messages.push("toggle kind \"key\" with vk 0; unbinding the toggle".into());
                self.toggle = ToggleConfig::default();
            }
            "key" => {}
            "mouse" if MouseButton::from_name(&self.toggle.mouse_button).is_err() => {
                messages.push(format!(
                    "toggle mouse_button {:?} is unknown; unbinding the toggle",
                    self.toggle.mouse_button
                ));
                self.toggle = ToggleConfig::default();
            }
            "mouse" => {}
            // Same fallback as the binding chord: an unusable chord unbinds
            // the toggle rather than leaving a half-parsed one behind.
            "chord" => {
                if let Err(problem) = self.toggle.chord() {
                    messages.push(format!(
                        "toggle kind \"chord\" is unusable ({problem}); unbinding the toggle"
                    ));
                    self.toggle = ToggleConfig::default();
                }
            }
            other => {
                messages.push(format!(
                    "toggle kind {other:?} is unknown; unbinding the toggle"
                ));
                self.toggle = ToggleConfig::default();
            }
        }

        messages
    }

    /// The bound input this config asks for (plan §4).
    pub fn binding(&self) -> Binding {
        if self.binding.kind == "chord" {
            // An unusable chord cannot have survived `validate`, but a
            // caller may hold an in-memory config that never went through
            // it; behave like the unknown-`kind` fallback and ask for the
            // default key rather than half a chord.
            return match self.binding.chord() {
                Ok(chord) => Binding::Chord(chord),
                Err(_) => Binding::Key {
                    vk: DEFAULT_VK,
                    scan: DEFAULT_SCAN,
                },
            };
        }
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
                self.binding.clear_chord_fields();
            }
            Binding::Mouse(button) => {
                self.binding.kind = "mouse".to_string();
                self.binding.mouse_button = button.name().to_string();
                self.binding.clear_chord_fields();
            }
            Binding::Chord(chord) => {
                self.binding.kind = "chord".to_string();
                self.binding.ctrl = chord.modifiers.ctrl.name().to_string();
                self.binding.shift = chord.modifiers.shift.name().to_string();
                self.binding.alt = chord.modifiers.alt.name().to_string();
                self.binding.win = chord.modifiers.win.name().to_string();
                match chord.key {
                    // A final mouse button lives in `mouse_button`, with
                    // `vk` zeroed (spec §8); a keyed chord claims both `vk`
                    // and `scan`; a modifier-only chord leaves them 0.
                    Some(ChordKey::Key { vk, scan }) => {
                        self.binding.vk = vk;
                        self.binding.scan = scan;
                        self.binding.mouse_button.clear();
                    }
                    Some(ChordKey::Mouse(button)) => {
                        self.binding.vk = 0;
                        self.binding.scan = 0;
                        self.binding.mouse_button = button.name().to_string();
                    }
                    None => {
                        self.binding.vk = 0;
                        self.binding.scan = 0;
                        self.binding.mouse_button.clear();
                    }
                }
            }
        }
        self.binding.swallow = swallow;
    }

    /// The optional toggle input this config asks for, or `None` while the
    /// toggle is unbound (its neutral state).
    pub fn toggle_binding(&self) -> Option<Binding> {
        match self.toggle.kind.as_str() {
            "key" if self.toggle.vk != 0 => Some(Binding::Key {
                vk: self.toggle.vk,
                scan: self.toggle.scan,
            }),
            "mouse" => MouseButton::from_name(&self.toggle.mouse_button)
                .ok()
                .map(Binding::Mouse),
            // As with `binding()`: an unusable chord behaves unbound.
            "chord" => self.toggle.chord().ok().map(Binding::Chord),
            _ => None,
        }
    }

    /// Store the optional toggle the way the file represents it — `None`
    /// clears it back to the unbound default.
    pub fn set_toggle_binding(&mut self, binding: Option<&Binding>, swallow: bool) {
        match binding {
            None => {
                self.toggle.kind.clear();
                self.toggle.vk = 0;
                self.toggle.scan = 0;
                self.toggle.mouse_button.clear();
                self.toggle.clear_chord_fields();
            }
            Some(Binding::Key { vk, scan }) => {
                self.toggle.kind = "key".to_string();
                self.toggle.vk = *vk;
                self.toggle.scan = *scan;
                self.toggle.mouse_button.clear();
                self.toggle.clear_chord_fields();
            }
            Some(Binding::Mouse(button)) => {
                self.toggle.kind = "mouse".to_string();
                self.toggle.mouse_button = button.name().to_string();
                self.toggle.clear_chord_fields();
            }
            Some(Binding::Chord(chord)) => {
                self.toggle.kind = "chord".to_string();
                self.toggle.ctrl = chord.modifiers.ctrl.name().to_string();
                self.toggle.shift = chord.modifiers.shift.name().to_string();
                self.toggle.alt = chord.modifiers.alt.name().to_string();
                self.toggle.win = chord.modifiers.win.name().to_string();
                match chord.key {
                    Some(ChordKey::Key { vk, scan }) => {
                        self.toggle.vk = vk;
                        self.toggle.scan = scan;
                        self.toggle.mouse_button.clear();
                    }
                    Some(ChordKey::Mouse(button)) => {
                        self.toggle.vk = 0;
                        self.toggle.scan = 0;
                        self.toggle.mouse_button = button.name().to_string();
                    }
                    None => {
                        self.toggle.vk = 0;
                        self.toggle.scan = 0;
                        self.toggle.mouse_button.clear();
                    }
                }
            }
        }
        self.toggle.swallow = swallow;
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

    // --- toggle ----------------------------------------------------------

    #[test]
    fn toggle_is_unbound_by_default() {
        let cfg = Config::default();
        assert_eq!(cfg.toggle.kind, "");
        assert_eq!(cfg.toggle_binding(), None);
    }

    #[test]
    fn the_toggle_binding_round_trips_through_the_file() {
        let path = scratch("toggle-roundtrip.toml");
        let mut cfg = Config::default();
        cfg.set_toggle_binding(Some(&Binding::Mouse(MouseButton::X2)), false);

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert_eq!(report.source, LoadSource::File);
        assert_eq!(
            loaded.toggle_binding(),
            Some(Binding::Mouse(MouseButton::X2))
        );
        assert!(!loaded.toggle.swallow);
    }

    #[test]
    fn an_invalid_toggle_falls_back_to_unbound_with_a_message() {
        let invalid = [
            ToggleConfig {
                kind: "banana".into(),
                ..ToggleConfig::default()
            },
            ToggleConfig {
                kind: "mouse".into(),
                mouse_button: "nope".into(),
                ..ToggleConfig::default()
            },
            ToggleConfig {
                kind: "key".into(),
                vk: 0,
                ..ToggleConfig::default()
            },
        ];

        for toggle in invalid {
            let mut cfg = Config {
                toggle: toggle.clone(),
                ..Config::default()
            };

            let messages = cfg.validate();

            assert!(
                messages.iter().any(|m| m.contains("toggle")),
                "{toggle:?}: {messages:?}"
            );
            assert_eq!(cfg.toggle.kind, "", "{toggle:?} falls back to unbound");
            assert_eq!(cfg.toggle_binding(), None);
        }
    }

    #[test]
    fn a_config_without_a_toggle_section_loads_as_unbound() {
        let path = scratch("no-toggle.toml");
        std::fs::write(
            &path,
            r##"version = 1
enabled = true

[binding]
kind = "mouse"
mouse_button = "x2"
"##,
        )
        .unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(report.source, LoadSource::File);
        assert_eq!(cfg.toggle, ToggleConfig::default());
        assert_eq!(cfg.toggle_binding(), None);
    }

    // --- chords (spec §8: additive fields, no version bump) --------------

    fn ctrl_n_chord() -> Binding {
        Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        })
    }

    /// Spec §8's own example shape: `Ctrl` held with a left mouse button as
    /// the final member.
    fn ctrl_left_mouse_chord() -> Binding {
        Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Mouse(MouseButton::Left)),
        })
    }

    #[test]
    fn a_chord_round_trips_through_the_file() {
        let path = scratch("chord-roundtrip.toml");
        let mut cfg = Config::default();
        cfg.set_binding(&ctrl_n_chord(), true);

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert_eq!(report.source, LoadSource::File);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(loaded.binding(), ctrl_n_chord());
        assert_eq!(loaded.binding.ctrl, "any");
        assert_eq!(loaded.binding.shift, "off");
        assert_eq!(loaded.binding.vk, 0x4E, "the final key keeps its fields");
    }

    #[test]
    fn a_modifier_only_chord_round_trips() {
        let path = scratch("modifier-only-chord.toml");
        let chord = Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                shift: Side::Any,
                ..Default::default()
            },
            key: None,
        });
        let mut cfg = Config::default();
        cfg.set_binding(&chord, true);

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(loaded.binding(), chord);
        assert_eq!(
            loaded.binding.vk, 0,
            "spec §8: 0 marks a modifier-only chord"
        );
        assert_eq!(loaded.binding.ctrl, "any");
        assert_eq!(loaded.binding.shift, "any");
        assert_eq!(loaded.binding.alt, "off");
        assert_eq!(loaded.binding.win, "off");
    }

    #[test]
    fn a_pinned_side_survives_the_file() {
        let path = scratch("pinned-side-chord.toml");
        let chord = Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Right,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        });
        let mut cfg = Config::default();
        cfg.set_binding(&chord, true);

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(loaded.binding(), chord, "Right Ctrl+N comes back pinned");
        assert_eq!(loaded.binding.ctrl, "right");
    }

    #[test]
    fn a_config_without_chord_fields_loads_exactly_as_before() {
        let path = scratch("pre-chord-file.toml");
        std::fs::write(
            &path,
            r##"version = 1
enabled = true

[binding]
kind = "key"
vk = 0x41
scan = 0x1E
swallow = false
"##,
        )
        .unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(report.source, LoadSource::File);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(
            cfg.binding(),
            Binding::Key {
                vk: 0x41,
                scan: 0x1E
            },
            "an old file binds exactly what it said"
        );
        for (field, expected) in [
            (&cfg.binding.ctrl, "off"),
            (&cfg.binding.shift, "off"),
            (&cfg.binding.alt, "off"),
            (&cfg.binding.win, "off"),
        ] {
            assert_eq!(field, expected, "a missing side field means off");
        }
    }

    #[test]
    fn an_unparseable_side_falls_back_to_the_default_binding() {
        let path = scratch("bad-side.toml");
        std::fs::write(
            &path,
            "[binding]\nkind = \"chord\"\nctrl = \"sideways\"\nvk = 0x4E\nscan = 49\n",
        )
        .unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(cfg.binding(), Config::default().binding());
        assert_eq!(cfg.binding.kind, "key", "the unusable block is replaced");
        assert!(
            report.messages.iter().any(|m| m.contains("chord")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn a_bare_modifier_chord_falls_back_because_it_is_invalid() {
        let path = scratch("bare-modifier-chord.toml");
        // `vk = 0` marks a modifier-only chord (spec §8); one modifier with
        // no key is the bare single modifier "press any key" has always
        // rejected ("cannot be held to talk").
        std::fs::write(
            &path,
            "[binding]\nkind = \"chord\"\nctrl = \"any\"\nvk = 0\n",
        )
        .unwrap();

        let (cfg, report) = Config::load(&path);

        assert_eq!(cfg.binding(), Config::default().binding());
        assert!(
            report.messages.iter().any(|m| m.contains("chord")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn a_toggle_chord_round_trips() {
        let path = scratch("toggle-chord.toml");
        let mut cfg = Config::default();
        cfg.set_toggle_binding(Some(&ctrl_n_chord()), false);

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(loaded.toggle_binding(), Some(ctrl_n_chord()));
        assert_eq!(loaded.toggle.kind, "chord");
        assert_eq!(loaded.toggle.ctrl, "any");
        assert!(!loaded.toggle.swallow);
    }

    #[test]
    fn a_chord_ending_in_a_mouse_button_round_trips() {
        let path = scratch("mouse-chord.toml");
        let mut cfg = Config::default();
        cfg.set_binding(&ctrl_left_mouse_chord(), true);

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(loaded.binding(), ctrl_left_mouse_chord());
        assert_eq!(loaded.binding.kind, "chord");
        assert_eq!(
            loaded.binding.mouse_button, "left",
            "the button keeps its field"
        );
        assert_eq!(loaded.binding.vk, 0, "spec §8: set instead of vk");
        assert_eq!(loaded.binding.scan, 0);
        assert_eq!(loaded.binding.ctrl, "any");
        assert_eq!(loaded.binding.shift, "off");
        assert_eq!(loaded.binding.alt, "off");
        assert_eq!(loaded.binding.win, "off");
    }

    #[test]
    fn a_toggle_chord_ending_in_a_mouse_button_round_trips() {
        let path = scratch("toggle-mouse-chord.toml");
        let mut cfg = Config::default();
        cfg.set_toggle_binding(Some(&ctrl_left_mouse_chord()), false);

        cfg.save(&path).expect("save works");

        let (loaded, report) = Config::load(&path);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(loaded.toggle_binding(), Some(ctrl_left_mouse_chord()));
        assert_eq!(loaded.toggle.mouse_button, "left");
        assert_eq!(loaded.toggle.vk, 0);
        assert_eq!(loaded.toggle.ctrl, "any");
        assert_eq!(loaded.toggle.win, "off");
        assert!(!loaded.toggle.swallow);
    }

    #[test]
    fn an_unknown_mouse_button_inside_a_chord_falls_back_and_names_the_field() {
        // The two blocks share `chord_from`, so one table covers both arms.
        let mut binding_block = Config::default();
        binding_block.binding.kind = "chord".into();
        binding_block.binding.ctrl = "any".into();
        binding_block.binding.mouse_button = "thumb".into();

        let mut toggle_block = Config::default();
        toggle_block.toggle.kind = "chord".into();
        toggle_block.toggle.ctrl = "any".into();
        toggle_block.toggle.mouse_button = "thumb".into();

        for (mut cfg, block) in [(binding_block, "binding"), (toggle_block, "toggle")] {
            let messages = cfg.validate();

            assert!(
                messages.iter().any(|m| {
                    m.contains(block) && m.contains("mouse_button") && m.contains("thumb")
                }),
                "{block}: {messages:?}"
            );
            assert_eq!(
                cfg.binding(),
                Config::default().binding(),
                "{block}: the unusable chord is replaced, not half-kept"
            );
            assert_eq!(cfg.toggle_binding(), None, "{block} falls back");
        }
    }

    #[test]
    fn a_mouse_button_wins_over_a_leftover_vk() {
        // Spec §8 writes `mouse_button` *instead of* `vk`, so only a
        // hand-edited file can carry both; there the button is the more
        // specific claim about the final member and must win.
        let path = scratch("chord-mouse-beats-vk.toml");
        std::fs::write(
            &path,
            "[binding]\nkind = \"chord\"\nctrl = \"any\"\nmouse_button = \"left\"\nvk = 0x41\nscan = 0x1E\n",
        )
        .unwrap();

        let (cfg, report) = Config::load(&path);

        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(
            cfg.binding(),
            ctrl_left_mouse_chord(),
            "the mouse button, not the leftover vk, is the final member"
        );
        assert_eq!(
            cfg.binding.vk, 0x41,
            "the leftover vk is left in the file, merely unused"
        );
    }

    // --- overlay ---------------------------------------------------------

    #[test]
    fn overlay_defaults_match_the_spec() {
        let overlay = OverlayConfig::default();
        assert!(!overlay.enabled, "opt-in: off by default");
        assert_eq!(overlay.mode, "talk-only");
        assert_eq!(overlay.position, "bottom-right");
        assert_eq!(overlay.distance, 24);
    }

    #[test]
    fn overlay_section_is_optional_and_round_trips() {
        // A config file written before the feature: no [overlay] at all.
        let legacy = "version = 1\nenabled = true\n";
        let parsed: Config = toml::from_str(legacy).expect("legacy config parses");
        assert_eq!(parsed.overlay, OverlayConfig::default());

        let overlay = OverlayConfig {
            enabled: true,
            mode: "always".into(),
            position: "top-left".into(),
            distance: 80,
        };
        let config = Config {
            overlay: overlay.clone(),
            ..Config::default()
        };
        let text = toml::to_string(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.overlay, overlay);
    }

    #[test]
    fn overlay_validate_falls_back_and_clamps() {
        let mut config = Config {
            overlay: OverlayConfig {
                enabled: true,
                mode: "sideways".into(),
                position: "middle".into(),
                distance: 9999,
            },
            ..Config::default()
        };
        let warnings = config.validate();
        assert_eq!(config.overlay.mode, "talk-only");
        assert_eq!(config.overlay.position, "bottom-right");
        assert_eq!(config.overlay.distance, 200);
        assert_eq!(warnings.len(), 3);
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
