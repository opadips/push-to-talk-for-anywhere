//! `ptt-cli` as a library: the console plumbing both front-ends share.
//!
//! The `ptt` binary is the manual-testing console of plan §9; the Tauri tray
//! app (plan §9 M4) reuses its logging setup, its single-instance guard and
//! its fail-safe wiring rather than growing a second copy of them.

/// The backends each desktop wires in (spec §2/§3/§5).
pub mod backends;
pub mod cli;
pub mod instance;
pub mod logging;
pub mod ptt;
