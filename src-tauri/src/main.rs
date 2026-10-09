#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! `ptt-tool` — the app shell: tray icon and menu, settings window,
//! autostart, single instance, and the hold-to-talk session behind them
//! (plan §9 M4).
//!
//! The session itself is [`ptt_core::session`] — the same worker the `ptt`
//! console runs — so the tray and the CLI can never drift apart.

mod app;
mod commands;
mod overlay;
mod tray;

use tauri::Manager;

// Plan §9 M4: the settings window must show the *embedded* UI. Tauri's
// `cfg(dev)` (the `custom-protocol` feature being off, Cargo.toml) makes the
// codegen embed no assets and point the window at `devUrl`
// (http://localhost:1420) — on a machine without the dev server WebView2
// shows "localhost refused to connect". Every build must therefore carry the
// feature; if one ever loses it, this fails at compile time.
const _: () = assert!(
    !cfg!(dev),
    "cfg(dev) is active: the window would load devUrl instead of the embedded settings UI"
);

fn main() {
    // Plan §2: rolling file log; the guard must outlive the process.
    if let Some(guard) = ptt_cli::logging::init() {
        std::mem::forget(guard);
    }

    tauri::Builder::default()
        // Registered first: a second launch asks the running instance to
        // show its settings window (plan §9 M4's "focus the existing
        // instance") and then gives up.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            app::show_settings(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(app::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::get_status,
            commands::get_devices,
            commands::binding_label,
            commands::save_settings,
            commands::set_enabled,
            commands::capture_binding,
            commands::cancel_capture,
        ])
        // Closing the settings window only hides it: the session keeps
        // running from the tray (plan §9 M4).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            // The badge rebuilds here: `Destroyed` is the first moment its
            // label is free again (tauri core removes it in `on_window_close`
            // before these handlers run — overlay.rs has the full ordering).
            if let tauri::WindowEvent::Destroyed = event {
                if window.label() == "overlay" {
                    overlay::recreate_after_destroy(window.app_handle());
                }
            }
        })
        .setup(app::setup)
        .build(tauri::generate_context!())
        .expect("error while building the ptt-tool application")
        .run(|app: &tauri::AppHandle, event: tauri::RunEvent| {
            // Plan §6.2: however the process ends, the microphone goes back
            // first. Quit already did it — this is the last line of defence.
            if let tauri::RunEvent::Exit = event {
                if let Err(error) = app::stop_session(&app.state::<app::AppState>()) {
                    tracing::warn!("stopping the session at exit: {error}");
                }
            }
        });
}
