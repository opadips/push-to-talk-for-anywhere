//! `ptt-core` — the pure logic of the push-to-talk tool.
//!
//! No UI, no Tauri, no network. Windows-specific behaviour stays behind the
//! traits in [`audio`] and [`input`] so the state machine and the engine can
//! be unit-tested without a microphone, a keyboard, or a running Windows
//! session (plan §0 and §4).

pub mod audio;
pub mod config;
pub mod engine;
pub mod error;
pub mod failsafe;
pub mod input;
pub mod session;
pub mod state;
