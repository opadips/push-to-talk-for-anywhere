//! The hold-to-talk session (plan §9 M2): `ptt ptt --key <vk>` keeps the
//! microphone muted and unmutes it only while the bound input is held.
//!
//! The loop itself is platform independent — input arrives on a channel — so
//! it is unit-tested with a fake microphone; only the hook that feeds it is
//! Windows-specific.

use anyhow::Result;

// The loop is reached from the Windows session and from the unit tests,
// which run anywhere; a non-Windows binary never starts it.
#[cfg(any(windows, test))]
use ptt_core::audio::MicController;
#[cfg(any(windows, test))]
use ptt_core::config::{Config, LoadSource};
#[cfg(any(windows, test))]
use ptt_core::engine::Engine;
#[cfg(any(windows, test))]
use ptt_core::input::InputEvent;
#[cfg(any(windows, test))]
use std::path::Path;
#[cfg(any(windows, test))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(windows, test))]
use std::sync::mpsc::{Receiver, RecvTimeoutError};
#[cfg(any(windows, test))]
use std::time::{Duration, Instant};

/// How often the loop wakes while nothing is pending; it doubles as the
/// quit-flag check interval.
#[cfg(any(windows, test))]
const POLL: Duration = Duration::from_millis(50);

/// Set by Enter, by Ctrl+C and by the shutdown handler; read by the loop
/// (plan §6.2: every way out of the session must be a *clean* way out).
#[cfg(windows)]
static QUIT: AtomicBool = AtomicBool::new(false);

/// Ctrl+C must quit through the normal path — restore, then exit — instead
/// of letting the console kill the process mid-talk (plan §6.2).
#[cfg(windows)]
unsafe extern "system" fn on_console_ctrl(_control_type: u32) -> windows::core::BOOL {
    QUIT.store(true, Ordering::Relaxed);
    windows::Win32::Foundation::TRUE
}

/// Feed the engine from `events` until the channel closes or `quit` is set.
///
/// The release deadline is re-derived from the state machine after every
/// wake-up, so a spurious timeout can never drop it and leave the microphone
/// open (plan §5: `ReleasePending` always has a path back to `Muted`).
#[cfg(any(windows, test))]
pub fn run_loop<C: MicController>(
    engine: &mut Engine<C>,
    events: Receiver<InputEvent>,
    quit: &AtomicBool,
) -> Result<()> {
    let mut deadline: Option<Instant> = None;

    loop {
        if quit.load(Ordering::Relaxed) {
            break;
        }

        let now = Instant::now();
        let timeout = deadline
            .map(|at| at.saturating_duration_since(now))
            .unwrap_or(POLL)
            .min(POLL);

        match events.recv_timeout(timeout) {
            Ok(event) => {
                let _ = engine.input(event, Instant::now())?;
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                if deadline.is_some_and(|at| now >= at) {
                    let _ = engine.tick(now)?;
                }
            }
            // The input source went away: nothing can be said any more.
            Err(RecvTimeoutError::Disconnected) => break,
        }

        deadline = match engine.state() {
            ptt_core::state::State::ReleasePending { deadline } => Some(*deadline),
            _ => None,
        };
    }

    Ok(())
}

/// Run the hold-to-talk session until the user quits (Windows, plan §7/§9).
/// Load `config.toml`, apply the `ptt ptt` flag overrides and persist the
/// result (plan §8): the file is the source of truth, a flag is a one-off
/// override, and an unreadable file is set aside instead of being silently
/// destroyed. Returns the effective config plus everything worth logging.
#[cfg(any(windows, test))]
pub fn resolve_config(path: &Path, spec: &crate::cli::PttSpec) -> Result<(Config, Vec<String>)> {
    let (mut config, report) = Config::load(path);
    let corrupt = report.source == LoadSource::Corrupt;
    let mut messages = report.messages;

    if corrupt {
        let backup = path.with_extension("toml.bad");
        match std::fs::rename(path, &backup) {
            Ok(()) => messages.push(format!("unreadable config kept at {}", backup.display())),
            Err(error) => messages.push(format!("could not keep the unreadable config: {error}")),
        }
    }

    if let Some(device) = &spec.device {
        config.audio.device_id = device.clone();
    }
    if let Some(binding) = &spec.binding {
        let swallow = spec.swallow.unwrap_or(config.binding.swallow);
        config.set_binding(binding, swallow);
    } else if let Some(swallow) = spec.swallow {
        config.binding.swallow = swallow;
    }
    if let Some(release_delay_ms) = spec.release_delay_ms {
        config.audio.release_delay_ms = release_delay_ms;
    }

    messages.extend(config.validate());
    config.save(path)?;
    Ok((config, messages))
}

#[cfg(windows)]
pub fn run(spec: &crate::cli::PttSpec) -> Result<()> {
    use crate::cli;
    use ptt_core::audio::wasapi::WasapiController;
    use ptt_core::audio::DEFAULT_DEVICE;
    use ptt_core::config::{default_path, OnExit};
    use ptt_core::failsafe::{self, Recovery, StateFile};
    use ptt_core::input::hook::HookInputSource;
    use ptt_core::input::InputSource;
    use std::sync::mpsc::channel;
    use tracing::{info, warn};
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    // Plan §9 M3: one session at a time.
    crate::instance::acquire()?;

    // Plan §8: the file decides, a flag is a one-off override, and the
    // result is written back so the next run starts from it.
    let config_path = default_path();
    let (config, messages) = resolve_config(&config_path, spec)?;
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

    // Plan §6.1: remember the user's own mute state *before* touching it.
    let original = controller.get_mute()?;
    let state_path = failsafe::default_path();
    failsafe::record(&state_path, &config.audio.device_id, original)?;

    // Plan §6.3: if the session panics, hand the microphone back on the way
    // out. The hook builds its own controller, so it works on whatever
    // thread the panic happened on.
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
    let on_exit = config.audio.on_exit;
    let device_id = config.audio.device_id.clone();
    let mut engine = Engine::new(controller, config);
    engine.enable(Instant::now())?;

    let (tx, rx) = channel();
    let mut source = HookInputSource::new();
    if let Err(error) = source.start(binding, swallow, tx) {
        // Nothing has been held yet: undo and clear the record.
        engine.mic().set_mute(original)?;
        failsafe::mark_clean(&state_path)?;
        return Err(error.into());
    }

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

    let result = run_loop(&mut engine, rx, &QUIT);

    source.stop();
    let shutdown = engine.shutdown(Instant::now()).map(|_| ());
    // Plan §6.2 + §6.6: hand the microphone back the way `[audio] on_exit`
    // asks for, and clear the record either way.
    let restored = match on_exit {
        OnExit::Restore => failsafe::restore(
            engine.mic(),
            &StateFile {
                device_id,
                original_muted: original,
                dirty: true,
                pid: std::process::id(),
            },
            &state_path,
        ),
        OnExit::Unmute => engine
            .mic()
            .set_mute(false)
            .and_then(|()| failsafe::mark_clean(&state_path)),
    };

    result?;
    shutdown?;
    restored?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PttSpec;
    use ptt_core::audio::DeviceInfo;
    use ptt_core::config::{AudioConfig, LoadSource};
    use ptt_core::error::Result as CoreResult;
    use ptt_core::input::{Binding, MouseButton};
    use std::path::PathBuf;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};

    /// Cloneable fake microphone: every clone sees the same call log (plan
    /// §10's "fake MicController", shared across the loop's worker thread).
    #[derive(Clone, Default)]
    struct SharedMic {
        calls: Arc<Mutex<Vec<bool>>>,
    }

    impl SharedMic {
        fn calls(&self) -> Vec<bool> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MicController for SharedMic {
        fn list_capture_devices(&self) -> CoreResult<Vec<DeviceInfo>> {
            Ok(Vec::new())
        }

        fn get_mute(&self) -> CoreResult<bool> {
            Ok(*self.calls.lock().unwrap().last().unwrap_or(&true))
        }

        fn set_mute(&self, muted: bool) -> CoreResult<()> {
            self.calls.lock().unwrap().push(muted);
            Ok(())
        }
    }

    fn config(release_delay_ms: u64) -> Config {
        Config {
            audio: AudioConfig {
                release_delay_ms,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
    }

    #[test]
    fn events_reaching_the_loop_drive_the_microphone() {
        let mic = SharedMic::default();
        let mut engine = Engine::new(mic.clone(), config(0));
        engine.enable(Instant::now()).unwrap();

        let (tx, rx) = channel();
        tx.send(InputEvent::BindingDown).unwrap();
        tx.send(InputEvent::BindingUp).unwrap();
        drop(tx); // closing the channel ends the loop

        run_loop(&mut engine, rx, &AtomicBool::new(false)).unwrap();

        assert_eq!(mic.calls(), vec![true, false, true]);
    }

    #[test]
    fn the_quit_flag_ends_the_loop_without_further_microphone_calls() {
        let mic = SharedMic::default();
        let mut engine = Engine::new(mic.clone(), config(120));
        engine.enable(Instant::now()).unwrap();

        let (_tx, rx) = channel(); // sender stays alive: only quit can end this
        let quit = AtomicBool::new(true);
        run_loop(&mut engine, rx, &quit).unwrap();

        assert_eq!(mic.calls(), vec![true], "only the initial mute");
    }

    #[test]
    fn the_release_delay_elapses_before_the_microphone_mutes() {
        let mic = SharedMic::default();
        let mut engine = Engine::new(mic.clone(), config(120));
        engine.enable(Instant::now()).unwrap();

        let (tx, rx) = channel();
        tx.send(InputEvent::BindingDown).unwrap();
        tx.send(InputEvent::BindingUp).unwrap();
        let released_at = Instant::now();

        let quit = Arc::new(AtomicBool::new(false));
        let worker = {
            let quit = Arc::clone(&quit);
            std::thread::spawn(move || run_loop(&mut engine, rx, quit.as_ref()))
        };

        // Several POLL wake-ups happen before the deadline; they must not
        // drop it (that would leave the microphone open forever).
        let mut muted_at = None;
        for _ in 0..200 {
            if mic.calls() == vec![true, false, true] {
                muted_at = Some(Instant::now());
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        quit.store(true, Ordering::Relaxed);
        worker.join().unwrap().unwrap();

        let muted_at = muted_at.expect("the microphone muted after the delay");
        let waited = muted_at.duration_since(released_at);
        assert!(
            waited >= Duration::from_millis(120),
            "muted before the release delay: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(2),
            "muted far too late: {waited:?}"
        );
        assert_eq!(mic.calls(), vec![true, false, true]);
    }

    // --- config resolution (plan §8) -------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ptt-run-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

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
    fn cli_overrides_win_over_the_file() -> Result<()> {
        let path = scratch("overrides.toml");
        write_file_config(&path);

        let spec = PttSpec {
            device: Some("flag-device".into()),
            binding: Some(Binding::Key { vk: 0x41, scan: 0 }),
            release_delay_ms: Some(0),
            swallow: Some(true),
        };
        let (cfg, _messages) = resolve_config(&path, &spec)?;

        assert_eq!(cfg.audio.release_delay_ms, 0);
        assert_eq!(cfg.audio.device_id, "flag-device");
        assert_eq!(cfg.binding(), Binding::Key { vk: 0x41, scan: 0 });
        assert!(cfg.binding.swallow);
        Ok(())
    }

    #[test]
    fn the_config_file_decides_when_no_flag_is_given() -> Result<()> {
        let path = scratch("file-decides.toml");
        write_file_config(&path);

        let (cfg, _messages) = resolve_config(&path, &PttSpec::default())?;

        assert_eq!(cfg.audio.release_delay_ms, 500, "the file's delay is used");
        assert_eq!(cfg.audio.device_id, "file-device");
        assert_eq!(cfg.binding(), Binding::Mouse(MouseButton::X1));
        assert!(!cfg.binding.swallow, "the file's swallow flag is used");
        Ok(())
    }

    #[test]
    fn a_first_run_creates_the_config_file() -> Result<()> {
        let path = scratch("first-run.toml");
        let _ = std::fs::remove_file(&path);

        let (cfg, _messages) = resolve_config(&path, &PttSpec::default())?;

        assert!(path.exists(), "config.toml is written for the user to edit");
        assert_eq!(cfg, Config::default());
        let (reloaded, report) = Config::load(&path);
        assert_eq!(report.source, LoadSource::File, "what was written reloads");
        assert_eq!(reloaded, Config::default());
        Ok(())
    }

    #[test]
    fn an_unreadable_config_is_kept_as_evidence_and_replaced_by_defaults() -> Result<()> {
        let path = scratch("corrupt-resolve.toml");
        std::fs::write(&path, "not [valid toml").unwrap();

        let (cfg, messages) = resolve_config(&path, &PttSpec::default())?;

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
        Ok(())
    }
}
