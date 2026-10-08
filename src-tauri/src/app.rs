//! App state and the session lifecycle behind the tray and the settings
//! window (plan §9 M4).
//!
//! Two rules keep this file honest: the `config` and `session` mutexes are
//! never held at the same time (they are always taken in that order), and
//! microphone work only ever happens on the session's own worker thread.

use ptt_core::config::{default_path, Config, Overrides};
use ptt_core::session::{SessionHandle, SessionState};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tracing::{info, warn};

/// What the tray icon and the settings window render (plan §9 M4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UiStatus {
    /// One of the three tray states.
    pub state: SessionState,
    /// The Enable/Disable toggle as persisted in `config.toml`.
    pub enabled: bool,
    /// Is a session really pumping right now?
    pub running: bool,
    /// The last failure — an engine error (plan §9 M4) or the app's own
    /// trouble starting a session. Shown in the settings window.
    pub error: Option<String>,
}

/// Everything the commands, the tray and the poller work on.
#[derive(Default)]
pub struct AppState {
    /// `config.toml` as last saved; the settings window edits a copy.
    pub config: Mutex<Config>,
    /// The running session — `None` while it is being restarted.
    pub session: Mutex<Option<SessionHandle>>,
    /// Why the app could not start a session (busy, microphone gone, …).
    error: Mutex<Option<String>>,
    /// The single-instance lock is taken once per process: a second
    /// `CreateMutexW` reports "already exists" for *this* process too.
    instance_held: AtomicBool,
}

impl AppState {
    /// A consistent snapshot for the tray, the poller and the UI.
    pub fn status(&self) -> UiStatus {
        let enabled = self.config.lock().unwrap().enabled;
        let session = self.session.lock().unwrap();
        match session.as_ref() {
            Some(session) => {
                // A worker that stopped must never look alive: `finished`
                // also implies `status()` already carries its reason.
                let dead = session.finished();
                let status = session.status();
                let app_error = self.error.lock().unwrap().clone();
                UiStatus {
                    state: if dead {
                        SessionState::Disabled
                    } else {
                        status.state
                    },
                    enabled,
                    running: !dead,
                    error: status.error.or(app_error),
                }
            }
            None => UiStatus {
                state: SessionState::Disabled,
                enabled,
                running: false,
                error: self.error.lock().unwrap().clone(),
            },
        }
    }
}

/// Remember a failure so the settings window can show it (plan §9 M4);
/// success clears it — an engine error is remembered by the session itself.
pub fn remember<T>(state: &AppState, outcome: &Result<T, String>) {
    *state.error.lock().unwrap() = outcome.as_ref().err().cloned();
}

/// Load `config.toml` the way the plan §8 says: the file decides, a corrupt
/// one is replaced by defaults, and neither may stop the app from starting.
fn load_config() -> Config {
    let path = default_path();
    match Config::resolve(&path, &Overrides::default()) {
        Ok((config, messages)) => {
            for message in messages {
                info!("{message}");
            }
            config
        }
        Err(error) => {
            let (config, report) = Config::load(&path);
            warn!(
                "cannot write {} ({error}); using what is readable",
                path.display()
            );
            for message in report.messages {
                info!("{message}");
            }
            config
        }
    }
}

/// Plan §6.4: a run that died mid-talk gets its microphone back before this
/// session touches anything. Windows only — the audio stack is (plan §1).
#[cfg(windows)]
fn recover_abandoned() {
    use ptt_core::audio::wasapi::WasapiController;
    use ptt_core::failsafe::{self, Recovery};

    match failsafe::recover(
        &failsafe::default_path(),
        ptt_cli::instance::process_running,
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
}

#[cfg(not(windows))]
fn recover_abandoned() {}

/// Plan §6.3: hand the microphone back if the app panics. The hook builds
/// its own controller, so it works on whatever thread dies.
fn install_panic_hook() {
    let state_path = ptt_core::failsafe::default_path();
    ptt_core::failsafe::install_panic_hook(move || {
        let _ = &state_path;
        #[cfg(windows)]
        {
            use ptt_core::audio::wasapi::WasapiController;
            match ptt_core::failsafe::restore_dirty(&state_path, |device| {
                WasapiController::new(device.to_string())
            }) {
                Ok(true) => eprintln!("ptt-tool: microphone restored after a panic"),
                Ok(false) => {}
                Err(error) => eprintln!("ptt-tool: restoring after a panic failed: {error}"),
            }
        }
    });
}

/// Bring a session up with the config as it stands (plan §7).
fn start_session(state: &AppState) -> Result<(), String> {
    let config = state.config.lock().unwrap().clone();

    // Plan §9 M3: one hold-to-talk session at a time. If a `ptt ptt` console
    // session owns the microphone, say so instead of fighting it — and try
    // again the next time the tray asks (the console releases the lock when
    // it quits). The lock is taken at most once per process: a second
    // `CreateMutexW` here would report "already exists" for *this* process.
    if !state.instance_held.load(Ordering::Relaxed) {
        #[cfg(windows)]
        ptt_cli::instance::acquire().map_err(|error| error.to_string())?;
        state.instance_held.store(true, Ordering::Relaxed);
    }

    match spawn_session(&config) {
        Ok(session) => {
            *state.session.lock().unwrap() = Some(session);
            *state.error.lock().unwrap() = None;
            Ok(())
        }
        Err(error) => {
            *state.error.lock().unwrap() = Some(error.clone());
            Err(error)
        }
    }
}

/// Windows: the real microphone and the real hooks (plan §7).
#[cfg(windows)]
fn spawn_session(config: &Config) -> Result<SessionHandle, String> {
    use ptt_core::audio::wasapi::WasapiController;
    use ptt_core::input::hook::HookInputSource;

    SessionHandle::start(
        config.clone(),
        WasapiController::new(config.audio.device_id.clone()),
        HookInputSource::new(),
        ptt_core::failsafe::default_path(),
        config.enabled,
    )
    .map_err(|error| error.to_string())
}

/// The audio stack and the hooks are Windows-only (plan §1).
#[cfg(not(windows))]
fn spawn_session(_config: &Config) -> Result<SessionHandle, String> {
    Err(format!(
        "hold-to-talk runs on Windows only (this build targets {})",
        std::env::consts::OS
    ))
}

/// Stop the running session: hooks down, microphone handed back, record
/// cleared (plan §6.2, §6.6).
pub fn stop_session(state: &AppState) -> Result<(), String> {
    match state.session.lock().unwrap().take() {
        Some(session) => session.stop().map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

/// Tray menu and settings window: arm or disarm the machine and persist the
/// choice (plan §8: `enabled` survives a restart).
pub fn set_enabled(state: &AppState, enabled: bool) -> Result<(), String> {
    let mut config = state.config.lock().unwrap().clone();
    if config.enabled != enabled {
        config.enabled = enabled;
        save_config(&config)?;
        *state.config.lock().unwrap() = config;
    }

    // Apply it — live if a session is running, by starting one otherwise.
    let start_needed = match state.session.lock().unwrap().as_ref() {
        Some(session) => {
            session.set_enabled(enabled);
            false
        }
        None => true,
    };
    if start_needed {
        start_session(state)?;
    }
    Ok(())
}

/// The settings window's Save: validate, persist, and apply to the live
/// session (plan §9 M4 — settings survive a restart).
pub fn apply_settings(state: &AppState, mut settings: Config) -> Result<Config, String> {
    // Plan §8: out-of-range values are corrected, never rejected (§9 M4's
    // sliders cannot produce them anyway).
    for message in settings.validate() {
        info!("{message}");
    }

    let current = state.config.lock().unwrap().clone();
    if settings != current {
        save_config(&settings)?;
        *state.config.lock().unwrap() = settings.clone();

        if current.audio != settings.audio {
            // A new device, delay or on-exit needs a fresh session (plan §7:
            // the controller is built from the device id).
            if let Err(error) = stop_session(state) {
                warn!("stopping the session before applying: {error}");
            }
            start_session(state)?;
        } else if current.binding() != settings.binding()
            || current.binding.swallow != settings.binding.swallow
        {
            // The hook takes a new binding without a restart (plan §4).
            if let Some(session) = state.session.lock().unwrap().as_ref() {
                session.rebind(settings.binding(), settings.binding.swallow);
            }
        } else if current.enabled != settings.enabled {
            set_enabled(state, settings.enabled)?;
        }
    }

    // Self-heal: a session that died on an engine failure must not stay
    // dead. The window shows the error (plan §9 M4); Save is how the user
    // asks for another go.
    let healthy = state
        .session
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|session| !session.finished());
    if !healthy {
        if let Err(error) = stop_session(state) {
            warn!("stopping the finished session: {error}");
        }
        start_session(state)?;
    }
    Ok(settings)
}

/// Tray menu's Enable/Disable (plan §9 M4).
pub fn toggle_enabled(app: &AppHandle) {
    let state = app.state::<AppState>();
    let enabled = !state.config.lock().unwrap().enabled;
    let outcome = set_enabled(&state, enabled);
    remember(&state, &outcome);
    if let Err(error) = outcome {
        warn!("cannot switch the session: {error}");
    }
}

/// Persist `config.toml` atomically (plan §8).
fn save_config(config: &Config) -> Result<(), String> {
    config
        .save(&default_path())
        .map_err(|error| format!("cannot save config.toml: {error}"))
}

/// The settings window, shown or brought back to the front (plan §9 M4:
/// closing it only hides it).
pub fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Tray menu's Quit: the microphone goes back before the process does
/// (plan §6.2).
pub fn quit(app: &AppHandle) {
    if let Err(error) = stop_session(&app.state::<AppState>()) {
        warn!("stopping the session: {error}");
    }
    app.exit(0);
}

/// Capture devices for the settings dropdown (plan §9 M4).
pub fn list_devices(config: &Config) -> Result<Vec<ptt_core::audio::DeviceInfo>, String> {
    #[cfg(windows)]
    {
        use ptt_core::audio::wasapi::WasapiController;
        // `new` only records the id: listing works even if that device is
        // currently unplugged, which is exactly when the user needs the list.
        WasapiController::new(config.audio.device_id.clone())
            .list_capture_devices()
            .map_err(|error| error.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        Err("the microphone runs on Windows only".to_string())
    }
}

/// First start: config, fail-safe, panic hook, session, tray (plan §9 M4).
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config();
    *app.state::<AppState>().config.lock().unwrap() = config.clone();

    recover_abandoned();
    install_panic_hook();

    if let Err(error) = start_session(&app.state::<AppState>()) {
        warn!("{error}");
    }

    let (tray, toggle) = crate::tray::build(app)?;
    let handle = app.handle().clone();
    std::thread::spawn(move || crate::tray::poll(handle, tray, toggle));

    sync_autostart(app.handle(), config.app.start_with_windows);

    // The window always exists (a second launch focuses it), it is just not
    // visible yet — `start_hidden` decides whether it shows at start-up.
    if !config.app.start_hidden {
        show_settings(app.handle());
    }
    Ok(())
}

/// Start-with-Windows follows `config.toml` (plan §8: the file decides).
/// A failure is logged, never fatal — the checkbox stays truthful in the UI
/// on the next save.
fn sync_autostart(app: &AppHandle, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;

    let outcome = {
        let autolaunch = app.autolaunch();
        if enabled {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        }
    };
    match outcome {
        Ok(()) => info!(
            "start-with-Windows is {}",
            if enabled { "on" } else { "off" }
        ),
        Err(error) => warn!("cannot update start-with-Windows: {error}"),
    }
}
