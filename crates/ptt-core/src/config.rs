//! Config model (plan §8).
//!
//! M2 ships only the part the state machine reads — release delay and sound
//! settings — shaped exactly as §8 nests them, so load/save/validation and
//! migration (M3) can grow around it without touching `state.rs`.

/// Effective configuration (plan §8).
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Master switch: when false the app sits in [`crate::state::State::Disabled`].
    pub enabled: bool,
    pub audio: AudioConfig,
    pub sounds: SoundsConfig,
}

/// `[audio]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// Hold-off before muting after key release, so word endings are not
    /// clipped (plan §1: default 200 ms; §8: 0–2000 ms).
    pub release_delay_ms: u64,
}

/// `[sounds]` in `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundsConfig {
    pub enabled: bool,
    /// 0.0 – 1.0 (plan §8).
    pub volume: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            audio: AudioConfig {
                release_delay_ms: 200,
            },
            sounds: SoundsConfig {
                enabled: true,
                volume: 0.5,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_plan() {
        let cfg = Config::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.audio.release_delay_ms, 200);
        assert!(cfg.sounds.enabled);
        assert_eq!(cfg.sounds.volume, 0.5);
    }
}
