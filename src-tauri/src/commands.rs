//! The commands the settings window calls (plan §9 M4).

use crate::app::{self, AppState, UiStatus};
use ptt_core::audio::DeviceInfo;
use ptt_core::config::Config;
use ptt_core::input::Binding;
use tauri::{AppHandle, Manager, State};
use tracing::{info, warn};

/// The saved settings the form starts from (plan §8: `config.toml`).
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Config {
    state.config.lock().unwrap().clone()
}

/// Live state for the status chip and the error banner (plan §9 M4).
#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> UiStatus {
    state.status()
}

/// The name the window shows for a binding — "Caps Lock", "Mouse button 4
/// (back)" — so the form never has to translate virtual-key codes itself.
#[tauri::command]
pub fn binding_label(settings: Config) -> String {
    settings.binding().label()
}

/// The device dropdown (plan §9 M4).
#[tauri::command]
pub fn get_devices(state: State<'_, AppState>) -> Result<Vec<DeviceInfo>, String> {
    let config = state.config.lock().unwrap().clone();
    app::list_devices(&config)
}

/// Save the form: validate, persist, and apply it to the running session.
/// Blocking (it may restart the session), so it runs off the async runtime.
#[tauri::command]
pub async fn save_settings(app: AppHandle, settings: Config) -> Result<Config, String> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let outcome = app::apply_settings(&state, settings);
        app::remember(&state, &outcome);
        outcome
    })
    .await
    .map_err(|error| error.to_string())?;
    if outcome.is_ok() {
        // A save can change the overlay: it applies live, no restart
        // (overlay design spec).
        crate::overlay::sync_overlay(&app);
    }
    outcome
}

/// The settings window's own Enable/Disable (the tray menu has its path).
#[tauri::command]
pub async fn set_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let outcome = app::set_enabled(&state, enabled);
        app::remember(&state, &outcome);
        outcome
    })
    .await
    .map_err(|error| error.to_string())?
}

/// "Press any key" (plan §9 M4): resolves with the next press or button.
///
/// The hook stays in capture mode until something is pressed, so this waits
/// as long as it takes — the window disables its button meanwhile, and only
/// one capture can be outstanding.
#[tauri::command]
pub async fn capture_binding(app: AppHandle) -> Result<Binding, String> {
    info!("capture requested from the settings window");
    let captured = {
        let state = app.state::<AppState>();
        let session = state.session.lock().unwrap();
        // A finished worker cannot arm the hook, so refuse here rather than
        // wait forever for a key that will never be reported.
        session
            .as_ref()
            .filter(|session| !session.finished())
            .map(|session| session.capture())
            .ok_or_else(|| {
                let message = "the session is not running — check the tray menu".to_string();
                warn!("capture refused: {message}");
                message
            })?
    };

    tauri::async_runtime::spawn_blocking(move || captured.recv())
        .await
        .map_err(|error| {
            warn!("capture task failed: {error}");
            error.to_string()
        })?
        .map_err(|error| {
            warn!("capture ended without a key: {error:?}");
            "the session stopped before a key was pressed".to_string()
        })
}

/// The window found the key itself (see `capture()` in the settings UI), so
/// the global capture must stop waiting — otherwise the hook would keep
/// swallowing the next key or mouse button the user presses anywhere.
/// Harmless when nothing is armed.
#[tauri::command]
pub fn cancel_capture(state: State<'_, AppState>) {
    if let Some(session) = state.session.lock().unwrap().as_ref() {
        session.cancel_capture();
    }
}
