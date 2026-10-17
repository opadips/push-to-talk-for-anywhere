//! Linux event source: an `rdev` *grab* (evdev + X11 + uinput) or, when the
//! grab cannot be had, plain listening.
//!
//! The grab works at the kernel level — `rdev` opens every `/dev/input`
//! device, reads it, and replays its events through uinput. That needs a
//! writable `/dev/input/event*` set and a writable `/dev/uinput`, plus a
//! usable X display for the mouse-coordinate relay (`rdev`'s grab
//! architecture moves the pointer through X). The preflight refuses with
//! the *specific* missing piece; the loop falls back to plain `rdev::listen`
//! (X11 XRecord) and lets the one-time warning say what the fallback costs:
//! the bound input then also reaches other applications.
//!
//! Under Wayland there is no X display for native sessions, and even with
//! XWayland the relay's coordinates do not survive the compositor — the
//! preflight refuses with an explanation instead of half-working.

use crate::error::{Error, Result};
use std::path::Path;
use std::sync::mpsc::Sender;

/// Everything needed to ever *see* a key: an X display, read-write evdev
/// access and a writable uinput device.
///
/// Checked on every `start`, not once at startup only: permissions can be
/// revoked (or the session can change display server) while the loop thread
/// — not stoppable by design — is already parked in its call. Detected here,
/// the failure becomes a clear error for a fresh session rather than a
/// silent fall-back to listening happening for no reason anyone can see.
pub(super) fn preflight() -> Result<()> {
    display_available()?;
    evdev_access()?;
    Ok(())
}

/// The relay needs an X display, reachable *now*.
fn display_available() -> Result<()> {
    let has_display = std::env::var_os("DISPLAY")
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    if has_display {
        return Ok(());
    }
    // The likeliest reason a Linux user has no DISPLAY: Wayland. Name it in
    // the error so the fix is one search away instead of a mystery.
    if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland") {
        return Err(Error::InputHook(
            "no X display on this Wayland session (DISPLAY unset): global \
             push-to-talk needs X11 or an X11 desktop — Wayland is not \
             supported yet"
                .into(),
        ));
    }
    Err(Error::InputHook(
        "no X display found (DISPLAY unset): global push-to-talk on Linux \
         needs an X11 display"
            .into(),
    ))
}

/// Read-write over every `/dev/input/event*`, plus the writable `/dev/uinput`
/// the grab replays through — the exact set `rdev::grab` opens up front.
fn evdev_access() -> Result<()> {
    let entries = std::fs::read_dir("/dev/input").map_err(|_| {
        Error::InputHook("/dev/input does not exist: no input devices found".into())
    })?;
    let mut any = false;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("event") {
            continue;
        }
        any = true;
        let path = entry.path();
        if !is_rw(&path) {
            return Err(Error::InputHook(format!(
                "cannot open {} read-write: the input grab needs it to hold \
                 keys — add your user to the `input` group (and `uinput` \
                 group) and log out and in again",
                path.display()
            )));
        }
    }
    if !any {
        return Err(Error::InputHook(
            "no /dev/input/event* devices found: no input devices exist".into(),
        ));
    }
    if !is_rw(Path::new("/dev/uinput")) {
        return Err(Error::InputHook(
            "cannot open /dev/uinput read-write: the input grab needs it to \
             replay consumed keys — add your user to the `uinput` group and \
             log out and in again"
                .into(),
        ));
    }
    Ok(())
}

/// Open read-write: the only probe that answers "may this process use this
/// device". The handle is dropped again immediately.
fn is_rw(path: &Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .is_ok()
}

/// The loop thread: grab for real, or fall back and say so — one thread for
/// the life of the process (see `common`'s docs on why it never stops).
pub(super) fn loop_thread(ready: Sender<Result<()>>) {
    // The session is live: speak up before the one call that can stall —
    // the preflight only has to have been right about the *first* setup.
    let _ = ready.send(Ok(()));
    // Optimistic blocking mode: rdev fires callbacks only *inside* the
    // grab/listen call, so nothing can route a swallow before the source
    // exists. If the grab is refused, this flips back off before listen.
    super::set_blocking(true);

    // `rdev::grab`'s Linux callback is `Fn(Event) -> Option<Event>`;
    // returning `None` consumes the event — precisely what
    // [`super::route_rdev_event`]'s answer encodes.
    if rdev::grab(super::route_rdev_event).is_ok() {
        tracing::info!("the input loop holds the keyboard (grab active)");
        // Practically unreachable: rdev's grab returns only on a setup
        // error, and the preflight verified every one of those.
        return;
    }

    // No grab: this session still functions, the user just sees the bound
    // input reach other applications — said once (loop-lived thread).
    super::set_blocking(false);
    super::warn_blocking_unavailable(
        "key blocking is unavailable — falling back to plain listening: \
         push-to-talk still works, but the bound input also reaches other \
         applications",
    );
    if let Err(error) = rdev::listen(|event| {
        let _ = super::route_rdev_event(event);
    }) {
        tracing::error!("the input listener failed: {error:?}");
    }
}
