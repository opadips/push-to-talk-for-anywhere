//! `ptt-core` — the pure logic of the push-to-talk tool.
//!
//! No UI, no Tauri, no network. Platform-specific behaviour stays behind the
//! traits in [`audio`] and [`input`] (and behind [`paths`]' per-OS rules) so
//! the state machine and the engine can be unit-tested without a microphone,
//! a keyboard, or a running desktop session (plan §0 and §4). Windows ships
//! with the original low-level-hook / WASAPI / winmm implementations;
//! Linux (X11) and macOS use the rdev-, PulseAudio- and CoreAudio-based
//! backends in those modules.

pub mod audio;
pub mod config;
pub mod diagnostics;
pub mod engine;
pub mod error;
pub mod failsafe;
pub mod input;
pub mod paths;
pub mod session;
pub mod sound;
pub mod state;
