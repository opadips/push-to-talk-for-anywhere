#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! `ptt-tool` — the app shell: tray, settings window, autostart.
//!
//! Engine wiring (input -> state machine -> audio, fail-safe) arrives in
//! milestone M3; the tray and the settings UI in M4. See
//! IMPLEMENTATION_PLAN.md §9.

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
