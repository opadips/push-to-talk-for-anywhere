//! The hold-to-talk session (plan §9 M2): `ptt ptt --key <vk>` keeps the
//! microphone muted and unmutes it only while the bound input is held.
//!
//! The loop itself lives in [`ptt_core::session`] so the tray app (plan §9
//! M4) runs exactly the same one; this module is the console around it:
//! flag overrides, fail-safe recovery, the Enter/Ctrl+C exit path and the
//! output.

use anyhow::Result;

/// Set by Enter, by Ctrl+C and by the shutdown handler; read by the wait
/// loop (plan §6.2: every way out of the session must be a *clean* way out).
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
static QUIT: AtomicBool = AtomicBool::new(false);

/// Ctrl+C must quit through the normal path — restore, then exit — instead
/// of letting the console kill the process mid-talk (plan §6.2).
#[cfg(windows)]
unsafe extern "system" fn on_console_ctrl(_control_type: u32) -> windows::core::BOOL {
    QUIT.store(true, Ordering::Relaxed);
    windows::Win32::Foundation::TRUE
}

/// Run the hold-to-talk session until the user quits (Windows, plan §7/§9).
#[cfg(windows)]
pub fn run(spec: &crate::cli::PttSpec) -> Result<()> {
    use crate::cli;
    use ptt_core::audio::wasapi::WasapiController;
    use ptt_core::audio::{MicController, DEFAULT_DEVICE};
    use ptt_core::config::{default_path, Config, Overrides};
    use ptt_core::failsafe::{self, Recovery};
    use ptt_core::input::hook::HookInputSource;
    use ptt_core::session::SessionHandle;
    use std::time::Duration;
    use tracing::{info, warn};
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    // Plan §9 M3: one session at a time.
    crate::instance::acquire()?;

    // Plan §8: the file decides, a flag is a one-off override, and the
    // result is written back so the next run starts from it.
    let config_path = default_path();
    let (config, messages) = Config::resolve(&config_path, &Overrides::from(spec))?;
    for message in messages {
        info!("{message}");
    }

    // Plan §6.4: a run that died mid-talk gets its microphone back before
    // this session touches anything.
    match failsafe::recover(
        &failsafe::default_path(),
        crate::instance::process_running,
        |device| WasapiController::new(device.to_string()),
    ) {
        Ok(Recovery::Restored {
            device_id,
            original_muted,
        }) => info!(
            "recovered the microphone abandoned by an earlier run ({device_id}: {})",
            if original_muted { "muted" } else { "unmuted" }
        ),
        Ok(Recovery::Busy { pid }) => info!("the recorded session (pid {pid}) is still running"),
        Ok(Recovery::Nothing) => {}
        Err(error) => warn!("could not recover the recorded state: {error}"),
    }

    let controller = WasapiController::new(config.audio.device_id.clone());
    let requested =
        (config.audio.device_id != DEFAULT_DEVICE).then_some(config.audio.device_id.as_str());
    // Same fail-fast validation as the other commands (plan §9, M1).
    cli::resolve_selection(&controller, requested)?;

    // Plan §6.1: remember the user's own mute state *before* taking over —
    // this copy is only for the goodbye line; the session records its own.
    let original = controller.get_mute()?;

    // Plan §6.3: if the session panics, hand the microphone back on the way
    // out. The hook builds its own controller, so it works on whatever
    // thread the panic happened on.
    let state_path = failsafe::default_path();
    let panic_path = state_path.clone();
    failsafe::install_panic_hook(move || {
        let outcome = failsafe::restore_dirty(&panic_path, |device| {
            WasapiController::new(device.to_string())
        });
        match outcome {
            Ok(true) => eprintln!("ptt: microphone restored after a panic"),
            Ok(false) => {}
            Err(error) => eprintln!("ptt: restoring after a panic failed: {error}"),
        }
    });

    let binding = config.binding();
    let swallow = config.binding.swallow;
    let release_delay_ms = config.audio.release_delay_ms;
    let device_id = config.audio.device_id.clone();

    // Plan §8 (M3 ruling): `enabled` is the tray's toggle; running `ptt ptt`
    // *is* the enable, so this session comes up armed either way.
    let session =
        SessionHandle::start(config, controller, HookInputSource::new(), state_path, true)?;

    println!(
        "ptt: {} bound to {} — microphone muted, unmuted while held ({release_delay_ms} ms release delay)",
        if swallow {
            "input"
        } else {
            "input (not swallowed)"
        },
        binding.describe()
    );
    println!("ptt: press Enter to quit (Ctrl+C also quits)");
    println!("ptt: config {}", config_path.display());
    println!(
        "ptt: log    {}",
        crate::logging::log_dir().join("ptt.log").display()
    );
    info!(
        "hold-to-talk started on {device_id}: {}",
        binding.describe()
    );

    // Plan §6.2: Ctrl+C joins the same clean-exit path as Enter.
    QUIT.store(false, Ordering::Relaxed);
    unsafe { SetConsoleCtrlHandler(Some(on_console_ctrl), true) }
        .map_err(|error| anyhow::anyhow!("cannot install the Ctrl+C handler: {error}"))?;
    {
        std::thread::spawn(|| {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            QUIT.store(true, Ordering::Relaxed);
        });
    }

    // Both ways out of the console land here — and a session that died on
    // its own (the engine failed) is reported rather than ignored.
    while !QUIT.load(Ordering::Relaxed) && !session.finished() {
        std::thread::sleep(Duration::from_millis(50));
    }

    // The worker took the hooks down and handed the microphone back already;
    // this joins it and reports what it found (plan §6.2, §6.6).
    session.stop()?;
    let handed_back = if original { "muted" } else { "unmuted" };
    info!("session ended, microphone handed back {handed_back}");
    println!("ptt: microphone restored to {handed_back}");
    Ok(())
}

/// The audio stack and the input hooks are Windows-only (plan §1).
#[cfg(not(windows))]
pub fn run(_spec: &crate::cli::PttSpec) -> Result<()> {
    anyhow::bail!(
        "hold-to-talk runs on Windows only (this build targets {})",
        std::env::consts::OS
    )
}
