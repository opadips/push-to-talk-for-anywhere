//! macOS event source: an `rdev` *CGEventTap* (a default-option event tap on
//! the HID system event tap), or — when the tap cannot be created — plain
//! listening via a passive tap.
//!
//! Creating a tap that consumes events needs the accessibility permission
//! (`System Settings → Privacy & Security → Accessibility`); without it both
//! the tap creation *and* the passive listener see nothing at all, so the
//! preflight is the permission probe [`AXIsProcessTrusted`], answered with
//! the fix in the error text rather than a silent dead list (events never
//! arriving is the useless failure mode here).
//!
//! rdev-0.5's button vocabulary on macOS is just Left and Right: no middle,
//! no X1, no X2 ever reaches the callback (its CGEvent-to-`Button` mapping
//! stops there; Windows is the platform where all five work — the app's
//! documented platform-table difference). Keys map through
//! [`super::KEY_TABLE`] like on Linux.
//!
//! `rdev::grab` consumes by answering `None` in its tap callback — the
//! closest a macOS tap gets to the Windows "return 1, stop the chain",
//! and exactly the question [`super::route_rdev_event`]'s answer encodes.

use crate::error::{Error, Result};
use objc2_application_services::AXIsProcessTrusted;
use std::sync::mpsc::Sender;

/// The accessibility permission probe. Runs on every `start`: permissions
/// can be revoked while the loop thread — not stoppable by design — sits in
/// its call, and a *fresh* session deserves a fresh, clear answer.
pub(super) fn preflight() -> Result<()> {
    // SAFETY: plain boolean probe; no pointers, no side effects.
    let trusted = unsafe { AXIsProcessTrusted() };
    if trusted {
        return Ok(());
    }
    Err(Error::InputHook(
        "accessibility permission is missing: grant this app \
         Accessibility in System Settings → Privacy & Security → \
         Accessibility — without it push-to-talk cannot receive key presses"
            .into(),
    ))
}

/// The loop thread: grab for real, or fall back to listening and say so —
/// one thread for the life of the process (see `common`'s docs on why it
/// never stops).
pub(super) fn loop_thread(ready: Sender<Result<()>>) {
    // The session is live: speak up before the tap install stalls anything.
    let _ = ready.send(Ok(()));
    // Optimistic blocking mode: see `x11::loop_thread` — same discipline.
    super::set_blocking(true);

    // `rdev::grab`'s macOS callback is `FnMut(Event) -> Option<Event>`;
    // `None` consumes the event — exactly what
    // [`super::route_rdev_event`]'s answer encodes.
    if rdev::grab(super::route_rdev_event).is_ok() {
        tracing::info!("the input loop holds the event tap (grab active)");
        // Practically unreachable: rdev's grab returns only when tap
        // creation failed, and the preflight verified permission.
        return;
    }

    // No tap: this session still functions, the user just sees the bound
    // input reach other applications — said once, since the thread lives on.
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
