//! The on-screen talk-state badge window (overlay design spec): lifecycle
//! and placement. The page (`ui/overlay.html`) owns presentation; this
//! module owns creating, positioning and destroying the window.

use crate::app::AppState;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tracing::warn;

/// The badge window's label and its size in logical pixels (the spec's
/// 64×64 — the dot plus its glow padding).
const OVERLAY_LABEL: &str = "overlay";
const OVERLAY_SIZE: f64 = 64.0;

/// Bring the `overlay` window in line with `config.overlay`: destroy it
/// (never merely hide it — a hidden WebView2 would still cost RAM, so the
/// plan §12 idle budget stays untouched) and, when the feature is enabled,
/// recreate it at the configured spot.
///
/// Safe to call from any thread; the window work itself runs on the main
/// thread (a settings save arrives on a blocking thread).
pub fn sync_overlay(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || sync_on_main_thread(&handle)) {
        warn!("cannot sync the overlay window: {error}");
    }
}

/// [`sync_overlay`]'s body — runs on the main thread, where window
/// creation belongs.
fn sync_on_main_thread(app: &AppHandle) {
    let overlay = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .overlay
        .clone();

    if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
        if let Err(error) = window.destroy() {
            warn!("destroying the overlay window: {error}");
        }
    }

    if !overlay.enabled {
        return;
    }

    // Placement is relative to the primary monitor (multi-monitor is an
    // explicit non-goal, spec §"Non-goals"). Positions are logical pixels:
    // the builder takes logical coordinates, the monitor reports physical
    // size plus a scale factor.
    let monitor = match app.primary_monitor() {
        Ok(Some(monitor)) => monitor,
        Ok(None) => {
            warn!("no primary monitor; the overlay badge is not shown");
            return;
        }
        Err(error) => {
            warn!("reading the primary monitor: {error}");
            return;
        }
    };
    let scale = monitor.scale_factor();
    let screen_w = (f64::from(monitor.size().width) / scale).round() as u32;
    let screen_h = (f64::from(monitor.size().height) / scale).round() as u32;
    let (x, y) = overlay_position(&overlay.position, overlay.distance, screen_w, screen_h);

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

    if let Err(error) = window {
        // The badge is cosmetic: a failed window is logged, never fatal.
        warn!("creating the overlay window: {error}");
    }
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
}
