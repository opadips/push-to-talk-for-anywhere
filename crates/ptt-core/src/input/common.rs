//! The binding/`rdev` input backend shared by Linux (`platform::x11`, an
//! X11/evdev grab) and macOS (`platform::cgevent`, a CGEventTap).
//!
//! `rdev` requires a *static* callback (`Fn(Event)` on Linux, `FnMut` on
//! macOS) — there is no `client_data`, so the state machine between the
//! callbacks and the runner lives in statics, exactly like the Windows hook
//! in [`crate::input::hook`] (`SHARED`, one `Mutex`). What is shared here is
//! the whole observable behavior of that hook: the binding comparison, the
//! press latches, the chord matcher and the press-and-hold capture — minus
//! the Windows-only pieces that have no counterpart:
//!
//! * no stall diagnostics — there is no low-level hook list a foreign
//!   application can slow down; `rdev` owns its own event source;
//! * no `GetAsyncKeyState` resync (spec §5) — that resync exists because
//!   Windows silently drops the low-level hooks and loses releases; on this
//!   stack releases are never dropped, and neither platform exposes an
//!   equivalent async state to muddy track;
//! * no raw-input second listener (bug 3 is a Windows quirk).
//!
//! ## Blocking, and the fallback (spec §1)
//!
//! With `swallow`, the bound input is supposed to be *consumed*. `rdev` can
//! only consume events when it can *grab* the input devices: an evdev grab
//! on Linux (X11 required too, because the grab relays mouse coordinates
//! through the X server) or a CGEventTap on macOS (accessibility
//! permission). When the grab cannot be had, the loop falls back to plain
//! listening — every event flows through this module's routing untouched,
//! so the push-to talk keeps *working*, the user just sees the bound key
//! reach other applications, and one warning says so once.
//!
//! ## The thread cannot be stopped
//!
//! Neither `rdev::grab` nor `rdev::listen` has a stop: the callback closes
//! over a Unix conditional or a macOS tap that the crate never returns. So
//! [`BackendInputSource::stop`] clears all shared state (no events can reach
//! a cleared `event_tx`) and leaves the loop thread parked in its call until
//! the process ends — [`ensure_loop`] never starts a second one. The Windows
//! hook thread *can* quit (`WM_QUIT`); this is the one behavioral difference
//! in `stop`.

use super::{Binding, InputEvent, InputSource, MouseButton};
use crate::error::{Error, Result};
use crate::input::chord::{
    CaptureAccumulator, CaptureAnswer, ChordMatcher, ChordState, KeyEvent, Outcome,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};

// --- platform selection ------------------------------------------------------

#[cfg(target_os = "linux")]
#[path = "x11.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "cgevent.rs"]
mod platform;

// --- shared state ------------------------------------------------------------

/// State shared between the `rdev` callbacks (input thread) and the runner
/// (any thread). Mirrors `hook.rs`'s `Shared` field by field, plus the two
/// flags the failure modes above need.
///
/// One `Mutex` rather than loose atomics so a rebind can update the binding
/// and the swallow flag as a single unit. Only ever held for a compare plus
/// a non-blocking channel send, and never across anything that blocks —
/// the callbacks here are OS dispatch paths just like hook procedures.
struct Shared {
    binding: Option<Binding>,
    swallow: bool,
    /// The optional second binding: while `Some`, its presses are forwarded
    /// as [`InputEvent::ToggleDown`] (its releases are never forwarded).
    toggle: Option<Binding>,
    /// Whether the toggle binding is consumed instead of reaching other
    /// applications — independent of the PTT `swallow`.
    toggle_swallow: bool,
    capture_tx: Option<Sender<Binding>>,
    event_tx: Option<Sender<InputEvent>>,
    /// Debounce latch: exactly one `BindingDown` per physical press, so
    /// auto-repeat cannot chatter (plan §9 M2). Only the plain
    /// [`Binding::Key`]/[`Binding::Mouse`] path consults it; chords debounce
    /// through [`ChordState::active`]'s edge instead.
    key_down: bool,
    mouse_down: bool,
    /// The same debounce for the toggle binding, one latch per route.
    toggle_key_down: bool,
    toggle_mouse_down: bool,
    /// The pure chord state machine (spec §5): told about every key and
    /// mouse event, stepped once per [`Binding::Chord`] binding.
    matcher: ChordMatcher,
    /// The PTT chord's own activation state, so a PTT chord and a toggle
    /// chord can be active side by side.
    ptt_chord: ChordState,
    toggle_chord: ChordState,
    /// The press-and-hold capture accumulator (spec §7), armed alongside
    /// `capture_tx`.
    capture_accumulator: CaptureAccumulator,
    /// Whether the loop thread is in blocking mode (`rdev::grab`): only a
    /// blocking event source can consume events, so swallow answers are
    /// honoured only when this is up (see the module docs).
    blocking: bool,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();

/// True once a loop thread runs (or is racing to start) — the thread cannot
/// be stopped, so it at least never gets duplicated across sessions.
static LOOP_RUNNING: AtomicBool = AtomicBool::new(false);

/// `warn!` exactly once per process when the loop falls back to non-blocking
/// listening (see [`platform::loop_thread_disk`]-adjacent code): every event
/// still works, but swallows do not.
static BLOCKING_UNAVAILABLE_WARNED: AtomicBool = AtomicBool::new(false);

fn shared() -> MutexGuard<'static, Shared> {
    SHARED
        .get_or_init(|| {
            Mutex::new(Shared {
                binding: None,
                swallow: false,
                toggle: None,
                toggle_swallow: false,
                capture_tx: None,
                event_tx: None,
                key_down: false,
                mouse_down: false,
                toggle_key_down: false,
                toggle_mouse_down: false,
                matcher: ChordMatcher::default(),
                ptt_chord: ChordState::default(),
                toggle_chord: ChordState::default(),
                capture_accumulator: CaptureAccumulator::default(),
                blocking: false,
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Forget every chord activation trace and report whether the PTT chord was
/// engaged when it happened (see `hook.rs`): a rebind, a restart or
/// re-grab must not inherit an "active" chord from before.
///
/// Same discipline as `hook.rs`: the `true` half is the hot-mic guard, and
/// whoever calls this with `true` owes an `InputEvent::BindingUp` emitted
/// **outside** the lock ([`emit_up_for_reset`]).
fn reset_chord_state(shared: &mut Shared) -> bool {
    shared.matcher.clear_last();
    let ptt_engaged = shared.ptt_chord.reset();
    shared.toggle_chord.reset();
    ptt_engaged
}

/// The `InputEvent::BindingUp` owed by a reset site that found the PTT side
/// still talking (see `hook.rs` — the same reset sites owe it here).
fn emit_up_for_reset(owed: bool, event_tx: Option<Sender<InputEvent>>) {
    if !owed {
        return;
    }
    if let Some(tx) = event_tx {
        let _ = tx.send(InputEvent::BindingUp);
    }
}

/// A [`CaptureAnswer`] on the pre-chord `Sender<Binding>` channel, folded
/// into the public [`Binding`] shape (in `hook.rs`, same as here).
fn answer_binding(answer: CaptureAnswer) -> Binding {
    match answer {
        CaptureAnswer::Chord(chord) => Binding::Chord(chord),
        CaptureAnswer::Plain(binding) => binding,
    }
}

/// The loop thread's mode switch (see the module docs): the platform loop
/// calls this once up front (optimistic) and again if its source turns out
/// to be non-blocking.
pub(super) fn set_blocking(on: bool) {
    shared().blocking = on;
}

fn emit(shared: &mut Shared, event: InputEvent) {
    if let Some(tx) = &shared.event_tx {
        let _ = tx.send(event);
    }
}

// --- route: one rdev event through the shared machine ------------------------

/// Map one `rdev` key onto this app's key language — the exact walk the
/// Windows hook does (`whence the vk, whence the scan, whence the side`):
/// modifiers are reported under their *generic* vk with the side signal
/// packed the way [`crate::input::chord::modifier_slot`] expects (`extended` for
/// Ctrl/Alt, the scan code for Shift, the vk itself for the Win keys), so
/// every binding/` Binding::matches` logic in this crate behaves on this
/// platform like it does on Windows (spec §4).
pub(crate) struct KeyReport {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
}

/// Walk the table for one key; `None` when this platform's key has no
/// Windows-vk counterpart worth binding (`Unknown`, macOS's `Function`).
///
/// Scan codes are the PS/2 set-1 make codes (`KBDLLHOOKSTRUCT`-style,
/// `0x2A` for left Shift and so on); extended keys carry the "virtual" scan
/// of their E0 prefix and `extended = true`. The exact codes live in the
/// big table below — every row is unit-tested against the Windows key
/// groups the app's `KEY_NAMES` table names.
pub(crate) fn rdev_key_report(key: rdev::Key) -> Option<KeyReport> {
    KEY_TABLE
        .iter()
        .find(|(rdev_key, _, _, _)| rdev_key == &key)
        .map(|&(_, vk, scan, extended)| KeyReport { vk, scan, extended })
}

/// `rdev` reports mouse buttons by name — except the two side buttons on
/// Linux, which arrive as `Unknown(8)`/`Unknown(9)` (BTN_SIDE/BTN_EXTRA:
/// rdev-0.5's conversion table stops at `BTN_MIDDLE`), and macOS not at all
/// beyond Left/Right. A `Button::Unknown(n)` here could be anything — a
/// wheel nudge in a horizontal scroll reports through `Wheel`, but exotic
/// mice, pen barrels and remote buttons all funnel into the same numbers —
/// so it is *not* mapped by number: an `Unknown` never matches a binding
/// and never gets consumed (the same pass-through an unmapped key gets).
/// The documented gap: X1/X2 cannot be bound on Linux, X1/X2/Middle are
/// never seen on macOS.
pub(crate) fn rdev_mouse_button(button: rdev::Button) -> Option<MouseButton> {
    match button {
        rdev::Button::Left => Some(MouseButton::Left),
        rdev::Button::Right => Some(MouseButton::Right),
        rdev::Button::Middle => Some(MouseButton::Middle),
        rdev::Button::Unknown(_) => None,
    }
}

/// The one key table both platforms route through: `(rdev key, vk, scan,
/// extended)`. Kept near [`rdev_key_report`] with a unit test per group so
/// a typo'd code shows up in `cargo test` and not in a user's binding.
const KEY_TABLE: &[(rdev::Key, u16, u16, bool)] = &[
    // --- modifiers: generic vk + the side signal --------------------------
    (rdev::Key::ShiftLeft, 0x10, 0x2A, false),
    (rdev::Key::ShiftRight, 0x10, 0x36, false),
    (rdev::Key::ControlLeft, 0x11, 0x1D, false),
    (rdev::Key::ControlRight, 0x11, 0x1D, true),
    (rdev::Key::Alt, 0x12, 0x38, false),
    (rdev::Key::AltGr, 0x12, 0x38, true),
    (rdev::Key::MetaLeft, 0x5B, 0x5B, false),
    (rdev::Key::MetaRight, 0x5C, 0x5C, false),
    // --- the keys KEY_NAMES names by caption ------------------------------
    (rdev::Key::Backspace, 0x08, 0x0E, false),
    (rdev::Key::Tab, 0x09, 0x0F, false),
    (rdev::Key::Return, 0x0D, 0x1C, false),
    (rdev::Key::CapsLock, 0x14, 0x3A, false),
    (rdev::Key::Escape, 0x1B, 0x01, false),
    (rdev::Key::Space, 0x20, 0x39, false),
    (rdev::Key::PageUp, 0x21, 0x49, true),
    (rdev::Key::PageDown, 0x22, 0x51, true),
    (rdev::Key::End, 0x23, 0x4F, true),
    (rdev::Key::Home, 0x24, 0x47, true),
    (rdev::Key::LeftArrow, 0x25, 0x4B, true),
    (rdev::Key::UpArrow, 0x26, 0x48, true),
    (rdev::Key::RightArrow, 0x27, 0x4D, true),
    (rdev::Key::DownArrow, 0x28, 0x50, true),
    (rdev::Key::PrintScreen, 0x2C, 0xB7, true),
    (rdev::Key::Insert, 0x2D, 0x52, true),
    (rdev::Key::Delete, 0x2E, 0x53, true),
    (rdev::Key::ScrollLock, 0x91, 0x46, false),
    (rdev::Key::Pause, 0x13, 0x45, false),
    (rdev::Key::NumLock, 0x90, 0x45, true),
    // --- main letter/number/punctuation block -----------------------------
    (rdev::Key::BackQuote, 0xC0, 0x29, false),
    (rdev::Key::Num1, 0x31, 0x02, false),
    (rdev::Key::Num2, 0x32, 0x03, false),
    (rdev::Key::Num3, 0x33, 0x04, false),
    (rdev::Key::Num4, 0x34, 0x05, false),
    (rdev::Key::Num5, 0x35, 0x06, false),
    (rdev::Key::Num6, 0x36, 0x07, false),
    (rdev::Key::Num7, 0x37, 0x08, false),
    (rdev::Key::Num8, 0x38, 0x09, false),
    (rdev::Key::Num9, 0x39, 0x0A, false),
    (rdev::Key::Num0, 0x30, 0x0B, false),
    (rdev::Key::Minus, 0xBD, 0x0C, false),
    (rdev::Key::Equal, 0xBB, 0x0D, false),
    (rdev::Key::KeyQ, 0x51, 0x10, false),
    (rdev::Key::KeyW, 0x57, 0x11, false),
    (rdev::Key::KeyE, 0x45, 0x12, false),
    (rdev::Key::KeyR, 0x52, 0x13, false),
    (rdev::Key::KeyT, 0x54, 0x14, false),
    (rdev::Key::KeyY, 0x59, 0x15, false),
    (rdev::Key::KeyU, 0x55, 0x16, false),
    (rdev::Key::KeyI, 0x49, 0x17, false),
    (rdev::Key::KeyO, 0x4F, 0x18, false),
    (rdev::Key::KeyP, 0x50, 0x19, false),
    (rdev::Key::LeftBracket, 0xDB, 0x1A, false),
    (rdev::Key::RightBracket, 0xDD, 0x1B, false),
    (rdev::Key::KeyA, 0x41, 0x1E, false),
    (rdev::Key::KeyS, 0x53, 0x1F, false),
    (rdev::Key::KeyD, 0x44, 0x20, false),
    (rdev::Key::KeyF, 0x46, 0x21, false),
    (rdev::Key::KeyG, 0x47, 0x22, false),
    (rdev::Key::KeyH, 0x48, 0x23, false),
    (rdev::Key::KeyJ, 0x4A, 0x24, false),
    (rdev::Key::KeyK, 0x4B, 0x25, false),
    (rdev::Key::KeyL, 0x4C, 0x26, false),
    (rdev::Key::SemiColon, 0xBA, 0x27, false),
    (rdev::Key::Quote, 0xDE, 0x28, false),
    (rdev::Key::BackSlash, 0xDC, 0x2B, false),
    (rdev::Key::IntlBackslash, 0xDC, 0x56, false),
    (rdev::Key::KeyZ, 0x5A, 0x2C, false),
    (rdev::Key::KeyX, 0x58, 0x2D, false),
    (rdev::Key::KeyC, 0x43, 0x2E, false),
    (rdev::Key::KeyV, 0x56, 0x2F, false),
    (rdev::Key::KeyB, 0x42, 0x30, false),
    (rdev::Key::KeyN, 0x4E, 0x31, false),
    (rdev::Key::KeyM, 0x4D, 0x32, false),
    (rdev::Key::Comma, 0xBC, 0x33, false),
    (rdev::Key::Dot, 0xBE, 0x34, false),
    (rdev::Key::Slash, 0xBF, 0x35, false),
    // --- numpad -------------------------------------------------------------
    (rdev::Key::KpReturn, 0x0D, 0x9C, true),
    (rdev::Key::KpMinus, 0x6D, 0x4A, false),
    (rdev::Key::KpPlus, 0x6B, 0x4E, false),
    (rdev::Key::KpMultiply, 0x6A, 0x37, false),
    (rdev::Key::KpDivide, 0x6F, 0xB5, true),
    (rdev::Key::Kp0, 0x60, 0x52, false),
    (rdev::Key::Kp1, 0x61, 0x4F, false),
    (rdev::Key::Kp2, 0x62, 0x50, false),
    (rdev::Key::Kp3, 0x63, 0x51, false),
    (rdev::Key::Kp4, 0x64, 0x4B, false),
    (rdev::Key::Kp5, 0x65, 0x4C, false),
    (rdev::Key::Kp6, 0x66, 0x4D, false),
    (rdev::Key::Kp7, 0x67, 0x47, false),
    (rdev::Key::Kp8, 0x68, 0x48, false),
    (rdev::Key::Kp9, 0x69, 0x49, false),
    (rdev::Key::KpDelete, 0x6E, 0x53, true),
    // --- function keys ------------------------------------------------------
    (rdev::Key::F1, 0x70, 0x3B, false),
    (rdev::Key::F2, 0x71, 0x3C, false),
    (rdev::Key::F3, 0x72, 0x3D, false),
    (rdev::Key::F4, 0x73, 0x3E, false),
    (rdev::Key::F5, 0x74, 0x3F, false),
    (rdev::Key::F6, 0x75, 0x40, false),
    (rdev::Key::F7, 0x76, 0x41, false),
    (rdev::Key::F8, 0x77, 0x42, false),
    (rdev::Key::F9, 0x78, 0x43, false),
    (rdev::Key::F10, 0x79, 0x44, false),
    (rdev::Key::F11, 0x7A, 0x57, false),
    (rdev::Key::F12, 0x7B, 0x58, false),
];
// `rdev::Key::Function` (macOS) and `Unknown(_)` stay unmapped on purpose:
// Fn never produces a character to bind, and Unknown could be *any* key
// depending on the machine (see [`super::platform`] mapping for the details).

/// Map an `rdev` event onto this module's machine and say whether the event
/// must be consumed — what a blocking event source turns into "do not
/// forward" (`grab` callback answers `None`), and what the fallback listener
/// simply forwards anyway. `Some(original)` is the answer for everything this
/// app does not consume: unmapped keys, mouse moves and wheel events.
fn route_rdev_event(event: rdev::Event) -> Option<rdev::Event> {
    let block = match event.event_type {
        rdev::EventType::KeyPress(key) | rdev::EventType::KeyRelease(key) => {
            match rdev_key_report(key) {
                Some(report) => {
                    let event = match event.event_type {
                        rdev::EventType::KeyPress(_) => KeyEvent::Down {
                            vk: report.vk,
                            scan: report.scan,
                            extended: report.extended,
                        },
                        _ => KeyEvent::Up {
                            vk: report.vk,
                            scan: report.scan,
                            extended: report.extended,
                        },
                    };
                    route_key(event)
                }
                // Unmapped key: never a binding, never consumed.
                None => false,
            }
        }
        rdev::EventType::ButtonPress(button) => {
            rdev_mouse_button(button).is_some_and(|button| route_mouse(button, true))
        }
        rdev::EventType::ButtonRelease(button) => {
            rdev_mouse_button(button).is_some_and(|button| route_mouse(button, false))
        }
        // Moves and wheel: never a binding, never consumed.
        _ => false,
    };
    if block {
        // rdev-0.5's convention: a `grab` callback's `None` consumes the
        // event; a `listen` callback never gets to consume anyway —
        // `route_*` return the swallow answer only in blocking mode.
        None
    } else {
        Some(event)
    }
}

/// Keyboard path (plan §11): the only comparison the binding gets is its own
/// input. Returns `true` when the event is consumed (only ever answered in
/// blocking mode — see the module docs; the rest of the machine runs
/// regardless so a capture or a chord never silently half-works when the
/// grab could not be had).
fn route_key(event: KeyEvent) -> bool {
    let mut shared = shared();
    let blocking = shared.blocking;

    // The matcher learns every key event — modifier state must be correct
    // the moment a chord is bound, even while none is (spec §6).
    shared.matcher.note_key(event);

    // Press-and-hold capture (spec §7): the whole sequence is consumed so
    // the focused application never keeps a modifier it did not see
    // released, and the accumulator answers with a chord or the plain
    // binding an unmodified press stands for. `swallows()` is asked *before*
    // `note_key` — its ordering contract.
    let capturing = shared.capture_accumulator.swallows();
    let answer = shared.capture_accumulator.note_key(event);
    // A `true` discard: fall through to the ordinary route below — the
    // discarded key may still *be* the binding, so it must get the binding
    // comparison too. Logged after the guard is gone.
    let discarded_lone_modifier = if capturing {
        // Releasing every accumulated modifier completes a modifier-only
        // chord; a lone modifier is discarded and capture re-arms — the
        // "a bare modifier cannot be held to talk" rule (spec §7).
        let answer = answer.or_else(|| shared.capture_accumulator.note_release_all());
        if let Some(answer) = answer {
            if let Some(tx) = shared.capture_tx.take() {
                let _ = tx.send(answer_binding(answer));
            }
            return blocking;
        }
        if !shared.capture_accumulator.discarded_lone_modifier() {
            // Mid-sequence (a modifier pressed or released): keep
            // waiting — and keep swallowing, when we can.
            return blocking;
        }
        true
    } else {
        false
    };

    // Compare the key against both bindings under this one guard (plan §11:
    // only the bound inputs are ever looked at). The press passes through
    // only when it matches *neither* — the toggle must get its chance even
    // when the key is not the PTT binding.
    let key = match event {
        KeyEvent::Down { vk, .. } => vk,
        KeyEvent::Up { vk, .. } => vk,
    };
    let pressed = matches!(event, KeyEvent::Down { .. });
    let ptt = shared
        .binding
        .is_some_and(|binding| binding.matches_key(key));
    let toggle = shared.toggle.is_some_and(|toggle| toggle.matches_key(key));
    let ptt_chord = match shared.binding {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    let toggle_chord = match shared.toggle {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    if !ptt && !toggle && ptt_chord.is_none() && toggle_chord.is_none() {
        return false;
    }

    let mut swallowed = false;
    if ptt {
        let forward = if pressed {
            // Auto-repeat: one down per physical press (plan §9 M2).
            let first = !shared.key_down;
            shared.key_down = true;
            first
        } else {
            shared.key_down = false;
            true
        };
        if forward {
            let event = if pressed {
                InputEvent::BindingDown
            } else {
                InputEvent::BindingUp
            };
            emit(&mut shared, event);
        }
        // Consumed: Caps Lock must not toggle, the app must not see it.
        swallowed = shared.swallow;
    }
    if toggle {
        if pressed {
            // One ToggleDown per physical press: auto-repeat cannot
            // chatter; the release emits nothing, it only clears the latch.
            if !shared.toggle_key_down {
                shared.toggle_key_down = true;
                emit(&mut shared, InputEvent::ToggleDown);
            }
        } else {
            shared.toggle_key_down = false;
        }
        // A press is consumed if *either* matching binding asked for it.
        swallowed |= shared.toggle_swallow;
    }

    // Chord bindings: one `step` per chord binding, only ever for
    // `Binding::Chord` — `ptt`/`toggle` above are false for one. Sequence as
    // in `hook.rs`: repeat/block asks first (they must see the state before
    // `step` moves it), then the outcomes, then the emissions.
    if ptt_chord.is_some() || toggle_chord.is_some() {
        let ptt_swallow = shared.swallow;
        let toggle_swallow = shared.toggle_swallow;
        let Shared {
            matcher,
            ptt_chord: ptt_state,
            toggle_chord: toggle_state,
            ..
        } = &mut *shared;
        // Auto-repeat of the completing member while the chord is engaged
        // is swallowed like the press it repeats (spec §5 rule 3).
        let ptt_repeat =
            ptt_chord.is_some_and(|chord| matcher.swallows_repeat(&chord, ptt_swallow, ptt_state));
        let toggle_repeat = toggle_chord
            .is_some_and(|chord| matcher.swallows_repeat(&chord, toggle_swallow, toggle_state));
        // …and the blocked half of the same promise (a modifier-first
        // release): the final member's repeat downs and its eventual key-up
        // owe the focused application a swallow.
        let ptt_blocked = ptt_chord
            .is_some_and(|chord| matcher.swallows_while_blocked(&chord, ptt_swallow, ptt_state));
        let toggle_blocked = toggle_chord.is_some_and(|chord| {
            matcher.swallows_while_blocked(&chord, toggle_swallow, toggle_state)
        });
        let ptt_outcome = ptt_chord.and_then(|chord| matcher.step(&chord, ptt_swallow, ptt_state));
        let toggle_outcome =
            toggle_chord.and_then(|chord| matcher.step(&chord, toggle_swallow, toggle_state));
        swallowed |= ptt_repeat | ptt_blocked | toggle_repeat | toggle_blocked;
        if let Some(Outcome::Engaged { swallow }) = ptt_outcome {
            emit(&mut shared, InputEvent::BindingDown);
            swallowed |= swallow;
        }
        if let Some(Outcome::Released { swallow }) = ptt_outcome {
            emit(&mut shared, InputEvent::BindingUp);
            swallowed |= swallow;
        }
        // A toggle works on the press edge only — its release emits nothing.
        if let Some(Outcome::Engaged { swallow }) = toggle_outcome {
            emit(&mut shared, InputEvent::ToggleDown);
            swallowed |= swallow;
        }
        if let Some(Outcome::Released { swallow }) = toggle_outcome {
            swallowed |= swallow;
        }
    }

    // Swallows are honoured only in blocking mode (`shared.blocking`): in
    // plain-listening fallback the event physically cannot be consumed, so
    // the app keeps seeing the bound key. Emissions went out either way.
    let consumed = swallowed & blocking;
    // Never log while the shared guard is held — it allocates.
    drop(shared);
    if discarded_lone_modifier {
        tracing::info!("a lone modifier cannot be held to talk — capture starts over");
    }
    consumed
}

/// Mouse path (plan §7), including the side buttons (plan §1) where the
/// platform sees them — Linux evdev reports X1/X2 as `Button::Unknown`, and
/// macOS never reports the middle or side buttons at all (both
/// `rdev`'s limitation; Windows is the platform where all five work).
fn route_mouse(button: MouseButton, pressed: bool) -> bool {
    let mut shared = shared();
    let blocking = shared.blocking;

    // The matcher learns every mouse event too — a chord whose final member
    // is a button completes here (spec §6).
    shared.matcher.note_mouse(button, pressed);

    // Press-and-hold capture (spec §7): a button *press* completes the
    // sequence, with the accumulated modifiers or alone as a plain mouse
    // binding. `swallows()` is asked before `note_mouse` — its ordering
    // contract.
    let capturing = shared.capture_accumulator.swallows();
    let answer = if pressed {
        shared.capture_accumulator.note_mouse(button)
    } else {
        None
    };
    if capturing {
        if let Some(answer) = answer {
            if let Some(tx) = shared.capture_tx.take() {
                let _ = tx.send(answer_binding(answer));
            }
            return blocking;
        }
        // Mid-sequence: keep waiting — and keep swallowing (when we can).
        return blocking;
    }

    // Compare against both bindings under this one guard (plan §11), like
    // the keyboard path: a button that is neither binding passes through.
    let ptt = shared
        .binding
        .is_some_and(|binding| binding.matches_mouse(button));
    let toggle = shared
        .toggle
        .is_some_and(|toggle| toggle.matches_mouse(button));
    let ptt_chord = match shared.binding {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    let toggle_chord = match shared.toggle {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    if !ptt && !toggle && ptt_chord.is_none() && toggle_chord.is_none() {
        return false;
    }

    let mut swallowed = false;
    if ptt {
        let forward = if pressed {
            let first = !shared.mouse_down;
            shared.mouse_down = true;
            first
        } else {
            shared.mouse_down = false;
            true
        };
        if forward {
            let event = if pressed {
                InputEvent::BindingDown
            } else {
                InputEvent::BindingUp
            };
            emit(&mut shared, event);
        }
        swallowed = shared.swallow;
    }
    if toggle {
        if pressed {
            // One ToggleDown per physical press; a release only clears the
            // latch and emits nothing.
            if !shared.toggle_mouse_down {
                shared.toggle_mouse_down = true;
                emit(&mut shared, InputEvent::ToggleDown);
            }
        } else {
            shared.toggle_mouse_down = false;
        }
        // Consumed if *either* matching binding asked for it (plan §4).
        swallowed |= shared.toggle_swallow;
    }

    // Chord bindings — the mouse twin of the keyboard block, without
    // `swallows_repeat`: a mouse button generates no auto-repeat. Sequence
    // as in `hook.rs` (the mouse client of the machine cannot outlive this
    // event anyway; the button's release swallows via `blocked`).
    if ptt_chord.is_some() || toggle_chord.is_some() {
        let ptt_swallow = shared.swallow;
        let toggle_swallow = shared.toggle_swallow;
        let Shared {
            matcher,
            ptt_chord: ptt_state,
            toggle_chord: toggle_state,
            ..
        } = &mut *shared;
        let ptt_blocked = ptt_chord
            .is_some_and(|chord| matcher.swallows_while_blocked(&chord, ptt_swallow, ptt_state));
        let toggle_blocked = toggle_chord.is_some_and(|chord| {
            matcher.swallows_while_blocked(&chord, toggle_swallow, toggle_state)
        });
        // Asked before `step`, which clears `blocked` on that very release.
        swallowed |= ptt_blocked | toggle_blocked;
        let ptt_outcome = ptt_chord.and_then(|chord| matcher.step(&chord, ptt_swallow, ptt_state));
        let toggle_outcome =
            toggle_chord.and_then(|chord| matcher.step(&chord, toggle_swallow, toggle_state));
        if let Some(Outcome::Engaged { swallow }) = ptt_outcome {
            emit(&mut shared, InputEvent::BindingDown);
            swallowed |= swallow;
        }
        if let Some(Outcome::Released { swallow }) = ptt_outcome {
            emit(&mut shared, InputEvent::BindingUp);
            swallowed |= swallow;
        }
        // A toggle works on the press edge only — its release emits nothing.
        if let Some(Outcome::Engaged { swallow }) = toggle_outcome {
            emit(&mut shared, InputEvent::ToggleDown);
            swallowed |= swallow;
        }
        if let Some(Outcome::Released { swallow }) = toggle_outcome {
            swallowed |= swallow;
        }
    }

    // Swallows are honoured only in blocking mode — see [`route_key`].
    swallowed & blocking
}

// --- the InputSource itself --------------------------------------------------

/// Input source backed by `rdev` on Linux/macOS (plan §7 for the Windows
/// ancestor). One thread, forever (see the module docs); the shared state
/// carries each session.
#[derive(Default)]
pub struct BackendInputSource;

impl BackendInputSource {
    pub fn new() -> Self {
        Self
    }
}

impl InputSource for BackendInputSource {
    fn start(
        &mut self,
        binding: Binding,
        swallow: bool,
        toggle: Option<(Binding, bool)>,
        tx: Sender<InputEvent>,
    ) -> Result<()> {
        // Fresh session state first, exactly like `hook.rs` starts: whatever
        // the previous session left behind must not leak into this one.
        clear_shared_state();

        // Preflight verified on every start (plan §7 "installed or failed"):
        // permissions can be revoked while the loop thread — un-stoppable by
        // design — is still parked. A fresh start with a morning-run X
        // session and an evening-revoked keyring/ring permission must fail
        // loudly, not fall into the silent fallback.
        platform::preflight()?;
        ensure_loop()?;

        {
            let mut shared = shared();
            shared.binding = Some(binding);
            shared.swallow = swallow;
            shared.toggle = toggle.map(|(binding, _)| binding);
            shared.toggle_swallow = toggle.is_some_and(|(_, swallow)| swallow);
            shared.event_tx = Some(tx);
            shared.capture_tx = None;
            shared.key_down = false;
            shared.mouse_down = false;
            shared.toggle_key_down = false;
            shared.toggle_mouse_down = false;
            shared.capture_accumulator = CaptureAccumulator::default();
            // No `BindingUp` owed here — `clear_shared_state` above just ran
            // this reset for the session that ended, and `event_tx` at this
            // point belongs to the brand-new session, which never saw a
            // `BindingDown`.
            reset_chord_state(&mut shared);
        }
        Ok(())
    }

    fn set_binding(&mut self, binding: Binding, swallow: bool, toggle: Option<(Binding, bool)>) {
        // The `BindingUp` owed by what this rebind leaves talking — a chord
        // `reset_chord_state` finds engaged, or the plain PTT key/button
        // latch the guard below clears while the input is still held — is
        // sent after the guard is gone (`hook.rs` documents every site).
        let (owed, event_tx) = {
            let mut shared = shared();
            shared.binding = Some(binding);
            shared.swallow = swallow;
            shared.toggle = toggle.map(|(binding, _)| binding);
            shared.toggle_swallow = toggle.is_some_and(|(_, swallow)| swallow);
            // Read *before* the latches are cleared: this session already
            // emitted a `BindingDown` for an input the user may still be
            // holding, and once `binding` is replaced its release matches
            // nothing — the hot mic this emission prevents.
            let plain_down = shared.key_down || shared.mouse_down;
            shared.key_down = false;
            shared.mouse_down = false;
            // The toggle latches owe nothing: `ToggleDown` has no up by
            // design.
            shared.toggle_key_down = false;
            shared.toggle_mouse_down = false;
            // A stale activation from the previous binding must not leak
            // into the new one; held modifiers are event-derived and stay.
            let ptt_engaged = reset_chord_state(&mut shared);
            (ptt_engaged || plain_down, shared.event_tx.clone())
        };
        emit_up_for_reset(owed, event_tx);
    }

    fn capture_next(&mut self) -> Receiver<Binding> {
        let (tx, rx) = channel();
        {
            let mut shared = shared();
            shared.capture_tx = Some(tx);
            // The accumulator owns the press-and-hold sequence now (spec §7).
            shared.capture_accumulator.arm();
        }
        rx
    }

    fn cancel_capture(&mut self) {
        // Nothing may keep eating the next key press once nobody is waiting
        // for it (the window answered by itself, or it was closed).
        let mut shared = shared();
        shared.capture_tx = None;
        shared.capture_accumulator = CaptureAccumulator::default();
    }

    fn stop(&mut self) {
        clear_shared_state();
        // No `BindingUp` owed here — the session is ending, `event_tx` was
        // cleared above, and the microphone is handed back on quit
        // regardless (plan §6.2), same reason as `hook.rs`.
    }
}

/// Everything a restart or a stop wipes, in one place (the latches, the
/// capture state, the chord traces) — what a *stopped* session can never
/// emit through again.
fn clear_shared_state() {
    let mut shared = shared();
    shared.event_tx = None;
    shared.capture_tx = None;
    shared.key_down = false;
    shared.mouse_down = false;
    shared.toggle_key_down = false;
    shared.toggle_mouse_down = false;
    shared.capture_accumulator = CaptureAccumulator::default();
    reset_chord_state(&mut shared);
}

/// Start the permanent loop thread once per process. The thread owns the
/// `rdev` grab/listen call until the process exits (see the module docs).
///
/// `ready` travels before the grab: on this stack the difference between
/// "listening" and "grabbing" is only observable *in* events, and the
/// preflight above already verified the conditions under which `rdev` can
/// acquire its source at all — the grab itself is then free to fall back.
fn ensure_loop() -> Result<()> {
    if LOOP_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let (ready_tx, ready_rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("ptt-input-loop".into())
        .spawn(move || platform::loop_thread(ready_tx))
        .map_err(|error| {
            LOOP_RUNNING.store(false, Ordering::SeqCst);
            Error::InputHook(error.to_string())
        });
    match spawned {
        Err(error) => Err(error),
        Ok(thread) => match ready_rx.recv() {
            Ok(Ok(())) => {
                std::mem::forget(thread); // the thread is permanent by design
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = thread.join();
                LOOP_RUNNING.store(false, Ordering::SeqCst);
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                LOOP_RUNNING.store(false, Ordering::SeqCst);
                Err(Error::InputHook(
                    "the input loop thread exited before installing its event source".into(),
                ))
            }
        },
    }
}

/// The fallback's one-time notice (a `warn!` inside the callbacks would
/// repeat forever; the loop emits the notice when it actually falls back).
pub(super) fn warn_blocking_unavailable(message: &str) {
    if !BLOCKING_UNAVAILABLE_WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        rdev_key_report, rdev_mouse_button, route_key, route_rdev_event, set_blocking, shared,
        KeyReport,
    };
    use super::{Binding, InputEvent, MouseButton};
    use crate::input::chord::modifier_slot;
    use crate::input::chord::{Chord, ChordKey, Modifiers, Side};
    use std::sync::mpsc::channel;
    use std::sync::Mutex;

    /// The shared state is process-global (like the hook), so tests that
    /// drive it must not interleave.
    static TEST_SERIAL: Mutex<()> = Mutex::new(());

    fn key(vk: u16, scan: u16, extended: bool) -> rdev::Key {
        // Find the rdev key whose table row matches, so tests name only the
        // Windows codes (the app's language), never rdev's.
        KEY_TABLE
            .iter()
            .find(|&&(_, mapped_vk, mapped_scan, mapped_ext)| {
                (mapped_vk, mapped_scan, mapped_ext) == (vk, scan, extended)
            })
            .map(|&(rdev_key, _, _, _)| rdev_key)
            .expect("table row exists — tests rewrite the table by hand")
    }

    /// Reset the shared state so the next test starts from scratch (the
    /// statics are process-wide, exactly like the Windows hook's).
    fn fresh_state() {
        // Stop semantics, minus the thread (there is none here).
        let mut shared = shared();
        shared.binding = None;
        shared.toggle = None;
        shared.swallow = false;
        shared.toggle_swallow = false;
        shared.event_tx = None;
        shared.capture_tx = None;
        shared.key_down = false;
        shared.mouse_down = false;
        shared.toggle_key_down = false;
        shared.toggle_mouse_down = false;
        shared.capture_accumulator = crate::input::chord::CaptureAccumulator::default();
        shared.blocking = false;
        shared.matcher.clear_last();
        shared.ptt_chord.reset();
        shared.toggle_chord.reset();
    }

    fn bind(binding: Binding, swallow: bool) {
        let mut shared = shared();
        shared.binding = Some(binding);
        shared.swallow = swallow;
        shared.key_down = false;
        shared.mouse_down = false;
    }

    fn events() -> (
        std::sync::mpsc::Sender<InputEvent>,
        std::sync::mpsc::Receiver<InputEvent>,
    ) {
        channel()
    }

    // --- the key table -----------------------------------------------------

    #[test]
    fn the_table_names_the_keys_the_windows_hook_would_report() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Tab and Caps Lock, exactly as `KBDLLHOOKSTRUCT` reports them on
        // Windows (and as hook.rs's raw tests pin them).
        assert_eq!(
            report_for(key(0x09, 0x0F, false)),
            Some((0x09, 0x0F, false))
        );
        assert_eq!(
            report_for(key(0x14, 0x3A, false)),
            Some((0x14, 0x3A, false))
        );
    }

    #[test]
    fn modifiers_report_their_generic_vk_with_the_side_signal() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            report_for(key(0x10, 0x2A, false)),
            Some((0x10, 0x2A, false))
        );
        assert_eq!(
            report_for(key(0x10, 0x36, false)),
            Some((0x10, 0x36, false))
        );
        assert_eq!(
            report_for(key(0x11, 0x1D, false)),
            Some((0x11, 0x1D, false))
        );
        assert_eq!(report_for(key(0x11, 0x1D, true)), Some((0x11, 0x1D, true)));
        assert_eq!(
            report_for(key(0x12, 0x38, false)),
            Some((0x12, 0x38, false))
        );
        assert_eq!(report_for(key(0x12, 0x38, true)), Some((0x12, 0x38, true)));
        assert_eq!(
            report_for(key(0x5B, 0x5B, false)),
            Some((0x5B, 0x5B, false))
        );
        assert_eq!(
            report_for(key(0x5C, 0x5C, false)),
            Some((0x5C, 0x5C, false))
        );
    }

    #[test]
    fn every_side_signal_lands_in_the_right_chord_slot() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // The side-signal contract: generic vk + extended/scan recover the
        // *physical* slot the chord matcher needs (spec §4). rdev reports
        // side-specific keys; the table's job is to translate them back.
        for (vk, scan, extended, expected_slot) in [
            (
                0x10u16,
                0x2Au16,
                false,
                crate::input::chord::Slot::LeftShift,
            ),
            (0x10, 0x36, false, crate::input::chord::Slot::RightShift),
            (0x11, 0x1D, false, crate::input::chord::Slot::LeftCtrl),
            (0x11, 0x1D, true, crate::input::chord::Slot::RightCtrl),
            (0x12, 0x38, false, crate::input::chord::Slot::LeftAlt),
            (0x12, 0x38, true, crate::input::chord::Slot::RightAlt),
            (0x5B, 0x5B, false, crate::input::chord::Slot::LeftWin),
            (0x5C, 0x5C, false, crate::input::chord::Slot::RightWin),
        ] {
            let report = rdev_key_report(key(vk, scan, extended)).expect("mapped");
            assert_eq!(
                modifier_slot(report.vk, report.scan, report.extended),
                Some(expected_slot),
                "vk {vk:#04x} scan {scan:#04x} extended {extended}"
            );
        }
    }

    #[test]
    fn every_letter_digit_and_bindingable_code_is_in_the_table() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Anything `vk_from_name` can resolve as a single key must be
        // bindable on this platform too: one table row per code.
        let mut missing = Vec::new();
        for vk in [
            0x08, 0x09, 0x0D, 0x10, 0x11, 0x12, 0x13, 0x14, 0x1B, 0x20, 0x21, 0x22, 0x23, 0x24,
            0x25, 0x26, 0x27, 0x28, 0x2C, 0x2D, 0x2E, 0x5B, 0x5C, 0x90, 0x91, 0xBB, 0xBC, 0xBD,
            0xBE, 0xBA, 0xBF, 0xC0, 0xDB, 0xDC, 0xDD, 0xDE,
        ] {
            if !KEY_TABLE.iter().any(|&(_, mapped, _, _)| mapped == vk) {
                missing.push(vk);
            }
        }
        for vk in 0x30..=0x39 {
            if !KEY_TABLE.iter().any(|&(_, mapped, _, _)| mapped == vk) {
                missing.push(vk);
            }
        }
        for vk in 0x41..=0x5A {
            if !KEY_TABLE.iter().any(|&(_, mapped, _, _)| mapped == vk) {
                missing.push(vk);
            }
        }
        // 0x6C is `VK_SEPARATOR` — a keypad-Enter-as-separator Windows oddity
        // `key_name` cannot name (`KEY_NAMES` has no caption for it), so it
        // is deliberately outside the "must be bindable" set here.
        for vk in [0x60..=0x6B, 0x6D..=0x6F].into_iter().flatten() {
            if !KEY_TABLE.iter().any(|&(_, mapped, _, _)| mapped == vk) {
                missing.push(vk);
            }
        }
        for vk in 0x70..=0x7B {
            if !KEY_TABLE.iter().any(|&(_, mapped, _, _)| mapped == vk) {
                missing.push(vk);
            }
        }
        assert!(
            missing.is_empty(),
            "bindable vk codes missing from KEY_TABLE: {missing:#02x?}"
        );
    }

    fn report_for(rdev_key: rdev::Key) -> Option<(u16, u16, bool)> {
        rdev_key_report(rdev_key).map(|KeyReport { vk, scan, extended }| (vk, scan, extended))
    }

    #[test]
    fn unknown_keys_stay_unmapped_and_pass_through() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // `Unknown(_)` is deliberately not in the table: it could be
        // *anything* (see `rdev_mouse_button`), and conserving them means
        // never consuming a key nothing here can name.
        let mut miss_count = 0;
        for code in 200..210 {
            if rdev_key_report(rdev::Key::Unknown(code)).is_none() {
                miss_count += 1;
            }
        }
        assert_eq!(miss_count, 10, "every Unknown stays unmapped");
        assert!(report_for(key(0x70, 0x3B, false)).is_some(), "F1");
    }

    // --- mouse buttons -----------------------------------------------------

    #[test]
    fn the_three_named_buttons_map_and_unknown_do_not() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            rdev_mouse_button(rdev::Button::Left),
            Some(MouseButton::Left)
        );
        assert_eq!(
            rdev_mouse_button(rdev::Button::Right),
            Some(MouseButton::Right)
        );
        assert_eq!(
            rdev_mouse_button(rdev::Button::Middle),
            Some(MouseButton::Middle)
        );
        assert_eq!(
            rdev_mouse_button(rdev::Button::Unknown(8)),
            None,
            "X1 on Linux"
        );
        assert_eq!(
            rdev_mouse_button(rdev::Button::Unknown(9)),
            None,
            "X2 on Linux"
        );
    }

    // --- the plain binding path ---------------------------------------------

    #[test]
    fn a_press_down_and_its_release_emit_one_down_and_one_up() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        let (tx, rx) = events();
        {
            let mut shared = shared();
            shared.event_tx = Some(tx);
        }
        bind(
            Binding::Key {
                vk: 0x14,
                scan: 0x3A,
            },
            false,
        );

        assert!(
            !route_key(crate::input::chord::KeyEvent::Down {
                vk: 0x14,
                scan: 0x3A,
                extended: false
            }),
            "no swallow, nothing consumed"
        );
        assert!(
            !route_key(crate::input::chord::KeyEvent::Down {
                vk: 0x14,
                scan: 0x3A,
                extended: false
            }),
            "auto-repeat emits nothing"
        );
        assert!(!route_key(crate::input::chord::KeyEvent::Up {
            vk: 0x14,
            scan: 0x3A,
            extended: false
        }));
        assert_eq!(rx.recv().unwrap(), InputEvent::BindingDown);
        assert_eq!(rx.recv().unwrap(), InputEvent::BindingUp);
        assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    }

    #[test]
    fn swallows_are_honoured_only_when_the_source_can_block() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        bind(
            Binding::Key {
                vk: 0x14,
                scan: 0x3A,
            },
            true,
        );

        // Listening fallback: events work, nothing is consumed.
        set_blocking(false);
        assert!(!route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x14,
            scan: 0x3A,
            extended: false
        }));

        // Grabbing: the same press is consumed.
        set_blocking(true);
        assert!(route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x14,
            scan: 0x3A,
            extended: false
        }));
        // The auto-repeat of a swallowed binding is swallowed too (plan §9 M2)
        assert!(
            route_key(crate::input::chord::KeyEvent::Down {
                vk: 0x14,
                scan: 0x3A,
                extended: false
            }),
            "auto-repeat is swallowed"
        );
        assert!(
            route_key(crate::input::chord::KeyEvent::Up {
                vk: 0x14,
                scan: 0x3A,
                extended: false
            }),
            "the release ends the swallow"
        );

        // `swallow = false` never consumes, whatever the mode.
        fresh_state();
        bind(
            Binding::Key {
                vk: 0x14,
                scan: 0x3A,
            },
            false,
        );
        set_blocking(true);
        assert!(!route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x14,
            scan: 0x3A,
            extended: false
        }));
    }

    #[test]
    fn a_side_specific_modifier_binding_fires_on_the_generic_press() {
        // The settings window offers 0xA0..=0xA5 as single keys (plan §9 M4)
        // while this platform always reports generic 0x10/0x11/0x12 with a
        // side signal — `matches_key` normalises both sides, so the set-up
        // is identical to the Windows behavior (spec §1's last row).
        let (tx, rx) = events();
        fresh_state();
        {
            let mut shared = shared();
            shared.event_tx = Some(tx);
        }
        bind(Binding::Key { vk: 0xA2, scan: 0 }, false);
        assert!(!route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x11,
            scan: 0x1D,
            extended: false
        }));
        assert_eq!(rx.recv().unwrap(), InputEvent::BindingDown);
        assert!(!route_key(crate::input::chord::KeyEvent::Up {
            vk: 0x11,
            scan: 0x1D,
            extended: false
        }));
        assert_eq!(rx.recv().unwrap(), InputEvent::BindingUp);
    }

    #[test]
    fn a_key_that_is_not_the_binding_passes_through() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        bind(
            Binding::Key {
                vk: 0x14,
                scan: 0x3A,
            },
            true,
        );
        set_blocking(true);
        assert!(!route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x41,
            scan: 0x1E,
            extended: false
        }));
    }

    // --- chords ------------------------------------------------------------

    fn ctrl_n() -> Binding {
        Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        })
    }

    #[test]
    fn a_chord_engages_on_its_completing_member_only() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        let (tx, rx) = events();
        {
            let mut shared = shared();
            shared.event_tx = Some(tx);
        }
        bind(ctrl_n(), true);
        set_blocking(true);

        // Modifiers keep flowing to other applications while held (spec §5):
        assert!(
            !route_key(crate::input::chord::KeyEvent::Down {
                vk: 0x11,
                scan: 0x1D,
                extended: false
            }),
            "Ctrl held is not consumed"
        );
        // The completing member is consumed and engages the chord.
        assert!(route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x4E,
            scan: 49,
            extended: false
        }));
        assert_eq!(rx.recv().unwrap(), InputEvent::BindingDown);
        // Auto-repeat of the completed chord is swallowed, silently.
        assert!(route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x4E,
            scan: 49,
            extended: false
        }));
        assert!(rx.try_recv().is_err());

        // N released first: a blocked release owes the focused app a
        // swallow, but nothing is emitted — the Ctrl up ends the chord.
        assert!(route_key(crate::input::chord::KeyEvent::Up {
            vk: 0x4E,
            scan: 49,
            extended: false
        }));
        assert!(!route_key(crate::input::chord::KeyEvent::Up {
            vk: 0x11,
            scan: 0x1D,
            extended: false
        }));
        assert_eq!(rx.recv().unwrap(), InputEvent::BindingUp);
    }

    #[test]
    fn a_rebind_owes_a_binding_up_for_a_still_engaged_chord() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        let (tx, rx) = events();
        {
            let mut shared = shared();
            shared.event_tx = Some(tx);
        }
        bind(ctrl_n(), true);
        set_blocking(true);
        route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x11,
            scan: 0x1D,
            extended: false,
        });
        route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x4E,
            scan: 49,
            extended: false,
        });
        assert_eq!(
            rx.recv().unwrap(),
            InputEvent::BindingDown,
            "the chord engaged"
        );

        // Rebind while the chord is engaged: exactly `set_binding`'s body
        // (that method would also `platform::preflight`, which tests cannot
        // run — so the contract is replayed here, and the emission contract
        // exercised through the spendable helper).
        let (owed, event_tx) = {
            let mut shared = shared();
            shared.binding = Some(Binding::Key {
                vk: 0x14,
                scan: 0x3A,
            });
            shared.swallow = false;
            let plain_down = shared.key_down || shared.mouse_down;
            shared.key_down = false;
            shared.mouse_down = false;
            shared.toggle_key_down = false;
            shared.toggle_mouse_down = false;
            let ptt_engaged = super::reset_chord_state(&mut shared);
            (ptt_engaged || plain_down, shared.event_tx.clone())
        };
        super::emit_up_for_reset(owed, event_tx);
        assert_eq!(
            rx.recv().unwrap(),
            InputEvent::BindingUp,
            "the hot mic is muted"
        );
    }

    // --- press-and-hold capture (spec §7) -----------------------------------

    #[test]
    fn capture_answers_with_the_chord_the_sequence_stands_for() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        let (binding_tx, binding_rx) = channel::<Binding>();
        {
            let mut shared = shared();
            shared.event_tx = None;
            shared.capture_tx = Some(binding_tx);
            shared.capture_accumulator.arm();
        }
        set_blocking(true);

        // Ctrl held, N pressed: the sequence completes on the press.
        assert!(
            route_key(crate::input::chord::KeyEvent::Down {
                vk: 0x11,
                scan: 0x1D,
                extended: false
            }),
            "capture swallows every member"
        );
        assert!(
            route_key(crate::input::chord::KeyEvent::Down {
                vk: 0x4E,
                scan: 49,
                extended: false
            }),
            "the completing press is swallowed"
        );
        let answer = binding_rx.recv().unwrap();
        match answer {
            Binding::Chord(chord) => {
                assert_eq!(chord.modifiers.ctrl, Side::Left);
                assert_eq!(chord.key, Some(ChordKey::Key { vk: 0x4E, scan: 49 }));
            }
            other => panic!("expected a chord, got {other:?}"),
        }

        // The answered capture is spent: normal routing is over.
        assert!(shared().capture_accumulator.is_spent());
    }

    #[test]
    fn a_lone_modifier_press_is_discarded_and_capture_rearms() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        let (binding_tx, _binding_rx) = channel::<Binding>();
        {
            let mut shared = shared();
            shared.capture_tx = Some(binding_tx);
            shared.capture_accumulator.arm();
        }
        set_blocking(true);

        // The Win key pressed, then released with nothing else: a lone Win
        // cannot bind (no plain_vk), so it is *discarded* — the capture
        // starts over, and the Win up otherwise passes through the ordinary
        // route (nothing bound anyway). A lone Ctrl would instead answer
        // `Plain` (it binds as that key, the Alt default's sibling rule).
        assert!(
            route_key(crate::input::chord::KeyEvent::Down {
                vk: 0x5B,
                scan: 0x5B,
                extended: false
            }),
            "capture swallows the Win press"
        );
        assert!(
            !route_key(crate::input::chord::KeyEvent::Up {
                vk: 0x5B,
                scan: 0x5B,
                extended: false
            }),
            "the Win release is not re-swallowed - the discard does not consume it twice"
        );
        // The accumulator re-armed itself for the next sequence.
        assert!(!shared().capture_accumulator.is_spent());
    }

    #[test]
    fn capture_never_consumes_athing_outside_blocking_mode() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        let (binding_tx, binding_rx) = channel::<Binding>();
        {
            let mut shared = shared();
            shared.capture_tx = Some(binding_tx);
            shared.capture_accumulator.arm();
        }
        set_blocking(false);

        // The answer still arrives (the window keeps working); the events
        // just cannot be held back.
        assert!(!route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x11,
            scan: 0x1D,
            extended: false
        }));
        assert!(!route_key(crate::input::chord::KeyEvent::Down {
            vk: 0x4E,
            scan: 49,
            extended: false
        }));
        assert!(binding_rx.recv().is_ok());
    }

    #[test]
    fn rdev_events_route_through_the_whole_machine() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fresh_state();
        set_blocking(true);
        // Nothing bound: every mapped event still forwards.
        let event = rdev::Event {
            time: std::time::SystemTime::now(),
            name: None,
            event_type: rdev::EventType::KeyPress(key(0x41, 0x1E, false)),
        };
        assert!(
            route_rdev_event(event).is_some(),
            "unbound and unswallowed keys flow"
        );
        // An unmapped key: forwarded, always — nothing here names it.
        let unknown = rdev::Event {
            time: std::time::SystemTime::now(),
            name: None,
            event_type: rdev::EventType::KeyPress(rdev::Key::Unknown(123)),
        };
        assert!(route_rdev_event(unknown).is_some());
    }

    use super::KEY_TABLE;
}
