//! `ptt` — a tiny binary for manually exercising `ptt-core`.
//!
//! Subcommands (`devices`, `mute`, `unmute`, `status`, `ptt`) are added by
//! milestones M1 and M2 of the implementation plan; until then it only
//! reports the version so the binary can be smoke-tested in CI.

fn main() {
    println!("ptt {}", env!("CARGO_PKG_VERSION"));
}
