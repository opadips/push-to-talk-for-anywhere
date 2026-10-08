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
use ptt_core::config::{AudioConfig, Config};
#[cfg(any(windows, test))]
use ptt_core::engine::Engine;
#[cfg(any(windows, test))]
use ptt_core::input::InputEvent;
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
#[cfg(windows)]
pub fn run(spec: &crate::cli::PttSpec) -> Result<()> {
    use crate::cli;
    use ptt_core::audio::wasapi::WasapiController;
    use ptt_core::audio::DEFAULT_DEVICE;
    use ptt_core::input::hook::HookInputSource;
    use ptt_core::input::InputSource;
    use std::sync::mpsc::channel;
    use std::sync::Arc;

    let requested = spec
        .device
        .clone()
        .unwrap_or_else(|| DEFAULT_DEVICE.to_string());
    let controller = WasapiController::new(requested);
    // Same fail-fast validation as the other commands (plan §9, M1).
    cli::resolve_selection(&controller, spec.device.as_deref())?;

    // Plan §6: remember the user's own mute state and hand it back on exit.
    let original = controller.get_mute()?;

    let config = Config {
        audio: AudioConfig {
            release_delay_ms: spec.release_delay_ms,
        },
        ..Config::default()
    };
    let mut engine = Engine::new(controller, config);
    engine.enable(Instant::now())?;

    let (tx, rx) = channel();
    let mut source = HookInputSource::new();
    if let Err(error) = source.start(spec.binding, spec.swallow, tx) {
        engine.mic().set_mute(original)?;
        return Err(error.into());
    }

    println!(
        "ptt: {} bound to {} — microphone muted, unmuted while held",
        if spec.swallow {
            "input"
        } else {
            "input (not swallowed)"
        },
        spec.binding.describe()
    );
    println!("ptt: press Enter to quit");

    let quit = Arc::new(AtomicBool::new(false));
    {
        let quit = Arc::clone(&quit);
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            quit.store(true, Ordering::Relaxed);
        });
    }

    let result = run_loop(&mut engine, rx, quit.as_ref());

    source.stop();
    let shutdown = engine.shutdown(Instant::now()).map(|_| ());
    let restored = engine.mic().set_mute(original);

    result?;
    shutdown?;
    restored?;
    println!(
        "ptt: microphone restored to {}",
        if original { "muted" } else { "unmuted" }
    );
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
    use ptt_core::audio::DeviceInfo;
    use ptt_core::error::Result as CoreResult;
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
            audio: AudioConfig { release_delay_ms },
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
}
