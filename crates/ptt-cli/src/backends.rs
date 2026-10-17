//! The concrete platform backends the front-ends wire into the session
//! (spec §2, §3, §5).
//!
//! The library's session code only ever sees the traits from `ptt-core`
//! ([`ptt_core::audio::MicController`], [`ptt_core::sound::SoundPlayer`]);
//! these constructors are the one place the Unix ports name the real types:
//! PulseAudio + rdev's evdev/X11 grab on Linux, CoreAudio + a CGEventTap on
//! macOS. The Windows call sites build their WASAPI / winmm / hook backends
//! inline (they predate this module and keep their shape), hence no Windows
//! arm here.

/// The platform's microphone controller for `device_id` (spec §3).
#[cfg(target_os = "linux")]
pub fn microphone(device_id: impl Into<String>) -> ptt_core::audio::pulse::PulseController {
    ptt_core::audio::pulse::PulseController::new(device_id)
}

#[cfg(target_os = "macos")]
pub fn microphone(device_id: impl Into<String>) -> ptt_core::audio::coreaudio::CoreAudioController {
    ptt_core::audio::coreaudio::CoreAudioController::new(device_id)
}

/// The platform's cue player (spec §5): PulseAudio's simple API on Linux,
/// rodio over CoreAudio on macOS.
#[cfg(target_os = "linux")]
pub fn sound_player() -> ptt_core::sound::pulse::PulsePlayer {
    ptt_core::sound::pulse::PulsePlayer::new()
}

#[cfg(target_os = "macos")]
pub fn sound_player() -> ptt_core::sound::rodio::RodioPlayer {
    ptt_core::sound::rodio::RodioPlayer::new()
}

/// The input backend behind the binding — one type on every Unix port: an
/// `rdev` grab (or plain listen as the fall-back), X11/evdev on Linux and a
/// CGEventTap on macOS (spec §2).
#[cfg(unix)]
pub fn input_source() -> ptt_core::input::common::BackendInputSource {
    ptt_core::input::common::BackendInputSource::new()
}
