//! `ptt-cli` as a library: the console plumbing both front-ends share.
//!
//! The `ptt` binary is the manual-testing console of plan §9; the Tauri tray
//! app (plan §9 M4) reuses its logging setup, its single-instance guard and
//! its fail-safe wiring rather than growing a second copy of them.

pub mod cli;
pub mod instance;
pub mod logging;
pub mod ptt;
