//! The tray icon and its menu (plan §9 M4: three distinct state icons, and
//! a menu with Enable/Disable, Open settings and Quit).

use crate::app::AppState;
use ptt_core::session::SessionState;
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

/// How often the tray, the menu and the window are brought in step with the
/// session. Fast enough that "Talking" appears as you press the key.
const REFRESH: Duration = Duration::from_millis(100);

/// The three state icons of plan §9 M4, embedded at build time.
pub fn icon(state: SessionState) -> tauri::Result<Image<'static>> {
    let bytes = match state {
        SessionState::Disabled => &include_bytes!("../../assets/tray-disabled.png")[..],
        SessionState::Muted => &include_bytes!("../../assets/tray-muted.png")[..],
        SessionState::Talking => &include_bytes!("../../assets/tray-talking.png")[..],
    };
    Image::from_bytes(bytes)
}

/// The first menu entry is always the *action*: "Enable" while it is off,
/// "Disable" while it is on.
fn action(enabled: bool) -> &'static str {
    if enabled {
        "Disable"
    } else {
        "Enable"
    }
}

/// Build the tray: the icon for the current state, the menu, and the two
/// click routes (plan §9 M4).
pub fn build(app: &tauri::App) -> tauri::Result<(TrayIcon<Wry>, MenuItem<Wry>)> {
    let status = app.state::<AppState>().status();

    let toggle = MenuItem::with_id(app, "toggle", action(status.enabled), true, None::<&str>)?;
    let open = MenuItem::with_id(app, "settings", "Open settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let tray = TrayIconBuilder::with_id("main")
        .icon(icon(status.state)?)
        .tooltip("Push-to-Talk")
        .menu(&menu)
        // Windows convention: right click opens the menu, left click opens
        // the settings window (plan §9 M4).
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => crate::app::toggle_enabled(app),
            "settings" => crate::app::show_settings(app),
            "quit" => crate::app::quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = event
            {
                crate::app::show_settings(tray.app_handle());
            }
        })
        .build(app)?;

    Ok((tray, toggle))
}

/// Keep the tray icon, the menu entry and the window in step with the
/// session: this is what makes the state *live* in the UI (plan §9 M4).
pub fn poll(app: AppHandle, tray: TrayIcon<Wry>, toggle: MenuItem<Wry>) {
    let mut last: Option<(SessionState, bool, bool, Option<String>)> = None;

    loop {
        std::thread::sleep(REFRESH);
        let status = app.state::<AppState>().status();
        let seen = (
            status.state,
            status.enabled,
            status.running,
            status.error.clone(),
        );
        if last.as_ref() == Some(&seen) {
            continue;
        }
        last = Some(seen);

        if let Ok(image) = icon(status.state) {
            let _ = tray.set_icon(Some(image));
        }
        let _ = toggle.set_text(action(status.enabled));
        let _ = app.emit("status", &status);
    }
}
