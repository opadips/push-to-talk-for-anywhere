//! The on-screen talk-state badge window (overlay design spec): lifecycle
//! and placement. The page (`ui/overlay.html`) owns presentation; this
//! module owns creating, positioning and destroying the window.

use crate::app::AppState;
use ptt_core::config::OverlayConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use tracing::warn;

/// The badge window's label and its size in logical pixels. The pill the
/// page draws fills the window edge to edge, so the configured distance is
/// the distance between the visible badge and the screen edge.
const OVERLAY_LABEL: &str = "overlay";
const OVERLAY_W: f64 = 76.0;
const OVERLAY_H: f64 = 36.0;

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

/// Build the badge window and put it at its configured spot. The window is
/// created hidden, sized and placed in *physical* pixels from the primary
/// monitor's own scale factor, and only then shown — a logical position or
/// size would be converted with the scale of whichever monitor the window
/// happens to start on, which put the badge in the wrong place on mixed-DPI
/// setups. After this it is never hidden again: a hidden WebView2 stops
/// painting, so the first frame after a `show()` could be a stale one.
fn create(app: &AppHandle, overlay: &OverlayConfig) {
    let window =
        WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("overlay.html".into()))
            .inner_size(OVERLAY_W, OVERLAY_H)
            .visible(false)
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .resizable(false)
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
            place(app, &window, overlay);
            if let Err(error) = window.show() {
                warn!("showing the overlay window: {error}");
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

/// Size and move the badge (also used for position/distance changes without
/// a mode change), all in physical pixels.
fn place(app: &AppHandle, window: &WebviewWindow, overlay: &OverlayConfig) {
    let Some((x, y, w, h)) = badge_rect(app, overlay) else {
        return;
    };
    if let Err(error) = window.set_size(PhysicalSize::new(w, h)) {
        warn!("sizing the overlay window: {error}");
    }
    if let Err(error) = window.set_position(PhysicalPosition::new(x, y)) {
        warn!("moving the overlay window: {error}");
    }
}

/// The badge's physical rectangle `(x, y, width, height)` on the primary
/// monitor — or, when there is none, on whatever monitor the settings
/// window is on (multi-monitor remains a spec non-goal; this only keeps the
/// badge reachable). It is placed inside the monitor's *work area*, so a
/// badge at the bottom edge sits above the taskbar instead of under it.
fn badge_rect(app: &AppHandle, overlay: &OverlayConfig) -> Option<(i32, i32, u32, u32)> {
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
    let area = monitor.work_area();
    let w = (OVERLAY_W * scale).round() as u32;
    let h = (OVERLAY_H * scale).round() as u32;
    let distance = (f64::from(overlay.distance) * scale).round() as i32;
    let (x, y) = overlay_position(
        &overlay.position,
        distance,
        (area.size.width, area.size.height),
        (w, h),
    );
    Some((area.position.x + x, area.position.y + y, w, h))
}

/// The badge's top-left corner inside an `area` (width, height) for the
/// given edge — `distance` pixels from the edges; `*_center` centers on the
/// horizontal axis. Unknown values were already normalized by
/// `Config::validate`. Everything is in physical pixels.
fn overlay_position(
    position: &str,
    distance: i32,
    area: (u32, u32),
    badge: (u32, u32),
) -> (i32, i32) {
    let (aw, ah) = (area.0 as i32, area.1 as i32);
    let (bw, bh) = (badge.0 as i32, badge.1 as i32);

    let x = match position {
        "top-left" | "bottom-left" => distance,
        "top-center" | "bottom-center" => (aw - bw) / 2,
        // `top-right`, `bottom-right` and anything unvalidated.
        _ => aw - bw - distance,
    };
    let y = if position.starts_with("top") {
        distance
    } else {
        ah - bh - distance
    };
    (x.max(0), y.max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_place_the_badge_on_the_requested_edge() {
        // 76x36 badge, 1920x1040 work area, distance 24.
        let (area, badge) = ((1920, 1040), (76, 36));
        assert_eq!(
            overlay_position("bottom-right", 24, area, badge),
            (1820, 980)
        );
        assert_eq!(overlay_position("bottom-left", 24, area, badge), (24, 980));
        assert_eq!(overlay_position("top-left", 24, area, badge), (24, 24));
        assert_eq!(overlay_position("top-right", 24, area, badge), (1820, 24));
        assert_eq!(overlay_position("top-center", 24, area, badge), (922, 24));
        assert_eq!(
            overlay_position("bottom-center", 24, area, badge),
            (922, 980)
        );
    }

    #[test]
    fn a_huge_distance_never_pushes_the_badge_off_screen() {
        assert_eq!(
            overlay_position("bottom-right", 5000, (800, 600), (76, 36)),
            (0, 0)
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
