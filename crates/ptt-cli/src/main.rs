//! `ptt` — a tiny binary for manually exercising `ptt-core`.
//!
//! Commands: `devices`, `mute`, `unmute`, `status` (plan §9, milestone M1)
//! and `ptt` — the hold-to-talk session (plan §9, milestone M2).

mod cli;
mod ptt;

use anyhow::Result;
use ptt_core::audio::MicController;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = cli::parse(&args)?;

    // Hold-to-talk owns an interactive session instead of printing a line
    // (plan §9, M2).
    if let cli::Command::Ptt(spec) = &command {
        return ptt::run(spec);
    }

    let output = match &command {
        cli::Command::Help => cli::USAGE.to_string(),
        other => {
            let ctl = controller(other)?;
            cli::execute(other, &*ctl)?
        }
    };
    println!("{output}");
    Ok(())
}

/// Build the controller for a parsed command (Windows, plan §7).
#[cfg(windows)]
fn controller(command: &cli::Command) -> Result<Box<dyn MicController>> {
    use ptt_core::audio::{wasapi::WasapiController, DEFAULT_DEVICE};

    let requested = match command {
        cli::Command::Mute { device }
        | cli::Command::Unmute { device }
        | cli::Command::Status { device } => {
            device.clone().unwrap_or_else(|| DEFAULT_DEVICE.to_string())
        }
        cli::Command::Devices | cli::Command::Help => DEFAULT_DEVICE.to_string(),
        // Unreachable: 'ptt ptt' is handled by ptt::run before this point.
        cli::Command::Ptt(_) => DEFAULT_DEVICE.to_string(),
    };
    Ok(Box::new(WasapiController::new(requested)))
}

/// The audio stack is Windows-only (plan §1: no macOS/Linux support).
#[cfg(not(windows))]
fn controller(_command: &cli::Command) -> Result<Box<dyn MicController>> {
    anyhow::bail!(
        "ptt controls the microphone on Windows only (this build targets {})",
        std::env::consts::OS
    )
}
