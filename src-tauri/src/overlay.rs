//! The on-screen talk-state badge window (overlay design spec): lifecycle
//! and placement. The page (`ui/overlay.html`) owns presentation; this
//! module owns creating, positioning and destroying the window.

use crate::app::AppState;
use ptt_core::config::OverlayConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, LogicalPosition, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tracing::warn;

/// The badge window's label and its size in logical pixels (the spec's
/// 64×64 — the dot plus its glow padding).
const OVERLAY_LABEL: &str = "overlay";
const OVERLAY_SIZE: f64 = 64.0;

/// The overlay config the existing window was built with — `None` while a
/// destroy/rebuild is in flight. Change-gates [`sync_overlay`] so an
/// unrelated Save (volume, hotkey …) never reloads the badge page.
static APPLIED: Mutex<Option<OverlayConfig>> = Mutex::new(None);

/// Armed when a destroy is meant to be followed by a rebuild; consumed by
/// [`recreate_after_destroy`]. A rebuild cannot simply follow the
/// `destroy()` call: tauri-runtime-wry always proxies destroy to the event
/// loop (`tauri-runtime-wry lib.rs:2129` — inline handling would panic),
/// and the window label is only freed when the `Destroyed` event arrives,
/// which tauri core processes (`on_window_close`) *before* the window
/// event handlers run. Recreating from that handler is therefore the
/// first safe moment.
static RECREATE_PENDING: AtomicBool = AtomicBool::new(false);

/// What one sync pass should do — pure, so the lifecycle (which this
/// feature already got wrong once) is pinned by a unit test.
#[derive(Debug, PartialEq, Eq)]
enum SyncAction {
    /// Leave everything as it is.
    Nothing,
    /// A destroy-for-rebuild is already in flight.
    InFlight,
    /// Build the window (`enabled = true`, no window yet).
    Create,
    /// Position/distance changed: move the window, no page reload.
    Move,
    /// Mode changed: the page reads its mode once per load — rebuild.
    Rebuild,
    /// The feature was switched off: destroy, nothing to rebuild.
    Destroy,
}

fn decide(has_window: bool, applied: Option<OverlayConfig>, overlay: &OverlayConfig) -> SyncAction {
    if !has_window {
        return if overlay.enabled {
            SyncAction::Create
        } else {
            SyncAction::Nothing
        };
    }
    match applied {
        None => SyncAction::InFlight,
        Some(current) if current == *overlay => SyncAction::Nothing,
        Some(current) if overlay.enabled && current.mode == overlay.mode => SyncAction::Move,
        Some(_) if overlay.enabled => SyncAction::Rebuild,
        Some(_) => SyncAction::Destroy,
    }
}

/// Bring the `overlay` window in line with `config.overlay` — create it,
/// move it, rebuild it (mode change) or destroy it. Disabled means no
/// window at all (never merely a hidden one — a hidden WebView2 would
/// still cost RAM against the plan §12 idle budget).
///
/// Safe to call from any thread; the window work itself runs on the main
/// thread (a settings save arrives on a blocking thread).
pub fn sync_overlay(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || sync_on_main_thread(&handle)) {
        warn!("cannot sync the overlay window: {error}");
    }
}

/// Rebuild hook for the app's window-event handler: once the `overlay`
/// window has actually been destroyed (its label freed), a queued rebuild
/// runs against the *current* config.
pub fn recreate_after_destroy(app: &AppHandle) {
    if RECREATE_PENDING.swap(false, Ordering::SeqCst) {
        sync_on_main_thread(app);
    }
}

/// [`sync_overlay`]'s body — runs on the main thread.
fn sync_on_main_thread(app: &AppHandle) {
    let overlay = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .overlay
        .clone();
    let applied = APPLIED.lock().unwrap().clone();
    let window = app.get_webview_window(OVERLAY_LABEL);

    match decide(window.is_some(), applied, &overlay) {
        SyncAction::Nothing | SyncAction::InFlight => {}
        SyncAction::Create => {
            // Only reached with no window: any stale rebuild flag or
            // applied-config bookkeeping dies here.
            RECREATE_PENDING.store(false, Ordering::SeqCst);
            *APPLIED.lock().unwrap() = None;
            create(app, &overlay);
        }
        SyncAction::Move => {
            // Position/distance only: move the window — no page reload.
            if let Some(window) = window {
                place(app, &window, &overlay);
                *APPLIED.lock().unwrap() = Some(overlay);
            }
        }
        SyncAction::Rebuild => {
            RECREATE_PENDING.store(true, Ordering::SeqCst);
            if let Some(window) = window {
                if let Err(error) = window.destroy() {
                    warn!("destroying the overlay window: {error}");
                    RECREATE_PENDING.store(false, Ordering::SeqCst);
                } else {
                    *APPLIED.lock().unwrap() = None;
                }
            } else {
                RECREATE_PENDING.store(false, Ordering::SeqCst);
            }
        }
        SyncAction::Destroy => {
            if let Some(window) = window {
                if let Err(error) = window.destroy() {
                    warn!("destroying the overlay window: {error}");
                } else {
                    *APPLIED.lock().unwrap() = None;
                }
            }
        }
    }
}

/// Build the badge window at its configured spot (the page starts with an
/// invisible dot, so a fresh window never flashes).
fn create(app: &AppHandle, overlay: &OverlayConfig) {
    let Some((x, y)) = badge_xy(app, overlay) else {
        return;
    };

    let window =
        WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("overlay.html".into()))
            .inner_size(OVERLAY_SIZE, OVERLAY_SIZE)
            .position(x, y)
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .build();

    match window {
        Ok(window) => {
            // Review Focus 2: the badge must never swallow a click. There is
            // no builder option for this — it flips WS_EX_TRANSPARENT at the
            // Win32 level once the window exists.
            if let Err(error) = window.set_ignore_cursor_events(true) {
                warn!("the overlay badge is NOT click-through: {error}");
            }
            *APPLIED.lock().unwrap() = Some(overlay.clone());
        }
        Err(error) => {
            // The badge is cosmetic: a failed window is logged, never fatal.
            warn!("creating the overlay window: {error}");
            *APPLIED.lock().unwrap() = None;
        }
    }
}

/// Move an existing badge (position/distance change without a mode change).
fn place(app: &AppHandle, window: &WebviewWindow, overlay: &OverlayConfig) {
    if let Some((x, y)) = badge_xy(app, overlay) {
        if let Err(error) = window.set_position(LogicalPosition::new(x, y)) {
            warn!("moving the overlay window: {error}");
        }
    }
}

/// The badge's logical top-left corner on the primary monitor — or, when
/// there is none, on whatever monitor the settings window is on, shifted
/// into that monitor's screen coordinates (multi-monitor remains a spec
/// non-goal; this only keeps the badge reachable). Positions are logical
/// pixels: the builder and `set_position` take logical coordinates, the
/// monitors report physical size plus a scale factor.
fn badge_xy(app: &AppHandle, overlay: &OverlayConfig) -> Option<(f64, f64)> {
    let monitor = match app.primary_monitor() {
        Ok(Some(monitor)) => Some(monitor),
        Ok(None) => app
            .get_webview_window("main")
            .and_then(|window| window.current_monitor().ok().flatten()),
        Err(error) => {
            warn!("reading the primary monitor: {error}");
            None
        }
    };
    let Some(monitor) = monitor else {
        warn!("no monitor for the overlay badge; not shown");
        return None;
    };

    let scale = monitor.scale_factor();
    let screen_w = (f64::from(monitor.size().width) / scale).round() as u32;
    let screen_h = (f64::from(monitor.size().height) / scale).round() as u32;
    let (x, y) = overlay_position(&overlay.position, overlay.distance, screen_w, screen_h);
    // The primary monitor's origin is (0,0); a fallback monitor's is not.
    let origin_x = f64::from(monitor.position().x) / scale;
    let origin_y = f64::from(monitor.position().y) / scale;
    Some((origin_x + x, origin_y + y))
}

/// The badge's top-left corner for the given edge — `distance` logical
/// pixels from the screen edges; `*_center` centers on the horizontal
/// axis. Unknown values were already normalized by `Config::validate`.
fn overlay_position(position: &str, distance: u32, screen_w: u32, screen_h: u32) -> (f64, f64) {
    let d = f64::from(distance);
    let w = f64::from(screen_w);
    let h = f64::from(screen_h);

    let x = match position {
        "top-left" | "bottom-left" => d,
        "top-center" | "bottom-center" => (w - OVERLAY_SIZE) / 2.0,
        // `top-right`, `bottom-right` and anything unvalidated.
        _ => w - OVERLAY_SIZE - d,
    };
    let y = if position.starts_with("top") {
        d
    } else {
        h - OVERLAY_SIZE - d
    };
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_place_the_badge_on_the_requested_edge() {
        // 64x64 window, 1920x1080 screen, distance 24.
        assert_eq!(
            overlay_position("bottom-right", 24, 1920, 1080),
            (1832.0, 992.0)
        );
        assert_eq!(
            overlay_position("bottom-left", 24, 1920, 1080),
            (24.0, 992.0)
        );
        assert_eq!(overlay_position("top-left", 24, 1920, 1080), (24.0, 24.0));
        assert_eq!(
            overlay_position("top-right", 24, 1920, 1080),
            (1832.0, 24.0)
        );
        assert_eq!(
            overlay_position("top-center", 24, 1920, 1080),
            (928.0, 24.0)
        );
        assert_eq!(
            overlay_position("bottom-center", 24, 1920, 1080),
            (928.0, 992.0)
        );
    }

    #[test]
    fn sync_decisions_follow_the_window_lifecycle() {
        let on = OverlayConfig {
            enabled: true,
            ..OverlayConfig::default()
        };
        let off = OverlayConfig::default();
        let moved = OverlayConfig {
            position: "top-left".into(),
            ..on.clone()
        };
        let mode_changed = OverlayConfig {
            mode: "always".into(),
            ..on.clone()
        };

        // No window: build one iff the feature is on.
        assert_eq!(decide(false, None, &on), SyncAction::Create);
        assert_eq!(decide(false, None, &off), SyncAction::Nothing);
        // A window with nothing applied yet means a rebuild is in flight.
        assert_eq!(decide(true, None, &on), SyncAction::InFlight);
        // Unchanged (e.g. a volume-only Save): never reload the page.
        assert_eq!(decide(true, Some(on.clone()), &on), SyncAction::Nothing);
        // Position/distance only: move, no reload.
        assert_eq!(decide(true, Some(on.clone()), &moved), SyncAction::Move);
        // The mode changed: the page reads it once per load — rebuild.
        assert_eq!(
            decide(true, Some(on.clone()), &mode_changed),
            SyncAction::Rebuild
        );
        // Switched off: destroy, and nothing is queued to rebuild.
        assert_eq!(decide(true, Some(on.clone()), &off), SyncAction::Destroy);
    }
}
