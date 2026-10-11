//! `WH_KEYBOARD_LL` / `WH_MOUSE_LL` implementation of [`InputSource`] (plan §7).
//!
//! One dedicated thread installs both hooks and pumps their message loop.
//! The callbacks do the bare minimum — compare the event against the bound
//! input, forward a press/release over a channel, return — because plan §7
//! forbids any audio/COM work inside them and plan §11 forbids looking at
//! anything but the bound input.

use super::chord::{
    CaptureAccumulator, CaptureAnswer, ChordMatcher, ChordState, KeyEvent, Outcome, Role,
};
use super::{Binding, InputEvent, InputSource, MouseButton};
use crate::diagnostics;
use crate::error::{Error, Result};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    GetCurrentThreadId, GetSystemTimes, GetThreadTimes, OpenThread,
    THREAD_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, LASTINPUTINFO, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU,
    VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWKEYBOARD, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassW, SetTimer, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED,
    MSG, MSLLHOOKSTRUCT, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL, PBT_APMRESUMESUSPEND,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_POWERBROADCAST, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
    XBUTTON1, XBUTTON2,
};

/// `WM_WTS_SESSION_CHANGE` (winuser.h) — windows-rs does not define it.
const WM_WTS_SESSION_CHANGE: u32 = 0x02B1;

/// `WM_INPUT` (winuser.h) — a raw-input report for the notify window.
const WM_INPUT: u32 = 0x00FF;

/// `RI_KEY_BREAK` (winuser.h) — a raw keyboard report for a key *release*.
const RI_KEY_BREAK: u16 = 0x0001;

/// `RI_KEY_E0` (winuser.h) — a raw keyboard report carries the E0 (extended)
/// prefix, the side signal for Ctrl and Alt in the capture accumulator.
const RI_KEY_E0: u16 = 0x0002;

/// Raw keyboard reports: HID usage page 0x01 (generic desktop), usage 0x06.
const HID_KEYBOARD_PAGE: u16 = 0x01;
const HID_KEYBOARD_USAGE: u16 = 0x06;

/// State shared between the hook callbacks (hook thread) and the runner
/// (any thread).
///
/// One `Mutex` rather than loose atomics so a rebind can update the binding
/// and the swallow flag as a single unit. It is only ever held for a compare
/// plus a non-blocking channel send — orders of magnitude below the Windows
/// slow-hook threshold, so the hooks cannot be dropped for being slow.
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
    /// The same debounce for the toggle binding, one latch per hook.
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
}

/// The hook callbacks and the runner share one lazily built guard: the chord
/// types have no `const` constructor, and a `OnceLock`'s first caller builds
/// the value exactly once, race-free.
static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();

/// Thread id of the thread that owns the hooks, so [`HookInputSource::stop`]
/// can end its message loop with `WM_QUIT` (plan §7).
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

/// The hook pair currently installed, held as `isize` because `HHOOK`
/// wraps a raw pointer and cannot live in a `static`. Only the hook thread
/// touches these — the notify window procedure runs on that same thread.
static KEYBOARD_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

/// The notify window, as raw bits, so any thread can hand it to
/// `RegisterRawInputDevices` (raw input needs an explicit target window).
static NOTIFY_HWND: AtomicIsize = AtomicIsize::new(0);

/// One-time markers so a machine's log shows *whether each callback is ever
/// invoked at all* (bug 3: mouse captures arrived, key presses never did).
/// `swap` per event is nanoseconds; only the first event pays for the log.
static KEYBOARD_FIRST_CALL: AtomicBool = AtomicBool::new(false);
static KEYBOARD_FIRST_INJECTED: AtomicBool = AtomicBool::new(false);
static MOUSE_FIRST_CALL: AtomicBool = AtomicBool::new(false);

/// Log `message` exactly once — safe inside the hook callbacks.
fn log_once(flag: &AtomicBool, message: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        tracing::info!("{message}");
    }
}

/// Class name for the notify window, kept alive for the life of the
/// process so the pointer handed to `RegisterClassW` never dangles.
static CLASS_NAME: OnceLock<Vec<u16>> = OnceLock::new();

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
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Forget every chord activation trace and report whether the PTT chord was
/// engaged when it happened. Called wherever the press latches are reset: a
/// rebind, a restart or a rehook must not inherit an "active" chord from
/// before, and a release Windows dropped must not leave one `blocked` until
/// restart (spec §5's desync guard, plan §8 Step 1).
///
/// The `true` half is the hot-mic guard: an engaged chord has already
/// emitted `BindingDown`, and once the state is cleared the key-up that
/// should end the talk finds an inactive chord — `step` answers nothing for
/// it — so the microphone would stay open forever. Whoever calls this with
/// `true` must therefore emit `InputEvent::BindingUp`, **outside** the lock
/// (see [`emit_up_for_reset`]). Which sites do so is deliberate:
///
/// * [`HookInputSource::start`] — no. Its `self.stop()` above already ran
///   this reset, so it can only ever find the state inactive; and
///   `event_tx` at that point belongs to the brand-new session, which never
///   saw a `BindingDown` — an unpaired up would be wrong.
/// * [`HookInputSource::set_binding`] — yes. A rebind reaches a live
///   session exactly like a rehook does. The plain `key_down`/`mouse_down`
///   latches the same site clears owe their own `BindingUp` for the same
///   reason — and unlike a chord they have no release left to fall back on:
///   once `binding` is replaced, the still-held old key's key-up is
///   compared against the *new* binding, matches nothing, and the engine
///   never hears a `BindingUp` for it (this comment used to claim the
///   opposite, that the plain path "survives a mid-press rebind because its
///   release forwards unconditionally" — it does not, which is why
///   `set_binding` emits for the latches too).
/// * [`HookInputSource::stop`] — no. The session is ending: `event_tx` is
///   already cleared above the reset, the engine is on its way out, and
///   the microphone is handed back on quit regardless (plan §6.2).
/// * [`rehook`] — yes for the chord, the path a user hits on every unlock
///   or resume; no for the plain latches, whose input is still held and
///   whose binding is unchanged, so their own release still forwards the
///   `BindingUp` (see `rehook`).
///
/// The toggle's engagement emits `ToggleDown` and its release emits
/// nothing by design, so `toggle_chord` — and the plain toggle latches —
/// never owe an emission here.
fn reset_chord_state(shared: &mut Shared) -> bool {
    shared.matcher.clear_last();
    let ptt_engaged = shared.ptt_chord.reset();
    shared.toggle_chord.reset();
    ptt_engaged
}

/// The `InputEvent::BindingUp` owed by a reset site that found the PTT
/// side still talking: a chord [`reset_chord_state`] reported engaged,
/// **or** a plain `key_down`/`mouse_down` latch the site cleared while the
/// key or button was still physically down — that latch's press already
/// emitted `BindingDown`, and after the reset the release that should end
/// the talk may no longer match anything (see [`HookInputSource::set_binding`]).
/// One `BindingUp` mutes the microphone either way.
///
/// `event_tx` is cloned under the shared guard and the send happens **after
/// it is gone** — the `call_next` contract allows only a compare plus a
/// send while `Shared` is held, and the reset paths keep that promise by
/// doing nothing under the guard but clearing state. `None` (no session
/// listening) drops the event silently, exactly like `emit`.
fn emit_up_for_reset(owed: bool, event_tx: Option<Sender<InputEvent>>) {
    if !owed {
        return;
    }
    if let Some(tx) = event_tx {
        let _ = tx.send(InputEvent::BindingUp);
    }
}

/// Whether Windows's own asynchronous key state currently reports `key`
/// down — the high bit of [`GetAsyncKeyState`]'s return value (spec §5).
fn async_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: `GetAsyncKeyState` takes a virtual-key code and returns that
    // key's async state; no pointers, no preconditions to violate.
    (unsafe { GetAsyncKeyState(key.0 as i32) } as u16) & 0x8000 != 0
}

/// The four generic modifier roles as Windows currently believes them
/// (spec §5's desync guard). Read *before* the caller takes the shared
/// lock, so nothing that could block ever runs while the guard is held
/// (`call_next`'s contract).
fn generic_roles_down() -> [(Role, bool); 4] {
    [
        (Role::Ctrl, async_down(VK_CONTROL)),
        (Role::Shift, async_down(VK_SHIFT)),
        (Role::Alt, async_down(VK_MENU)),
        // The one Win role covers both physical Win keys.
        (Role::Win, async_down(VK_LWIN) || async_down(VK_RWIN)),
    ]
}

/// Apply the resync under the lock the caller already holds: Windows *does*
/// drop low-level hooks, so a key release can be lost and its modifier
/// would stay "held" in the matcher forever — disabling every chord that
/// asks for that role until restart. The policy is
/// [`ChordMatcher::resync_role`]'s: a role Windows reports *up* is cleared,
/// a role it reports *down* is left to the events — the resync never guesses
/// which side was held.
///
/// Log-only beyond the modifier state itself: no rehook, no thread-priority
/// change, no event forwarded or swallowed here (spec §5). Nothing here
/// allocates: `Shared` may only ever be held for a compare plus a send.
fn apply_resync(shared: &mut Shared, roles: [(Role, bool); 4]) {
    for (role, down) in roles {
        shared.matcher.resync_role(role, down);
    }
}

/// The one-line `ctrl=down shift=up …` summary of `roles` for the log.
/// Built by the caller *after* it releases the shared lock: formatting
/// allocates, and the lock is never held across allocating work.
fn resync_summary(roles: [(Role, bool); 4]) -> String {
    roles
        .iter()
        .map(|(role, down)| {
            let state = if *down { "down" } else { "up" };
            format!("{}={state}", role.name().to_ascii_lowercase())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A [`CaptureAnswer`] on the pre-chord `Sender<Binding>` channel: `Plain`
/// is already a [`Binding`], and `Chord` becomes [`Binding::Chord`] — the
/// variant exists for exactly this, so neither kind of answer is lost.
fn answer_binding(answer: CaptureAnswer) -> Binding {
    match answer {
        CaptureAnswer::Chord(chord) => Binding::Chord(chord),
        CaptureAnswer::Plain(binding) => binding,
    }
}

/// Input source backed by the low-level keyboard and mouse hooks (plan §7).
#[derive(Default)]
pub struct HookInputSource {
    thread: Option<JoinHandle<()>>,
}

impl HookInputSource {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputSource for HookInputSource {
    fn start(
        &mut self,
        binding: Binding,
        swallow: bool,
        toggle: Option<(Binding, bool)>,
        tx: Sender<InputEvent>,
    ) -> Result<()> {
        // Fresh session, fresh beats (spec §4): zero the counters and the
        // stall-rate gate before the hook thread exists. Any beat a running
        // worker wrote meanwhile self-heals on its next ≤50 ms tick, and the
        // watchdog needs two seconds of staleness before it reports anything.
        diagnostics::reset();
        self.stop();
        // Read the four generic modifier roles *before* taking the lock —
        // see `generic_roles_down`. A user already holding Ctrl when the
        // session starts is tracked from the first event, and a chord
        // cannot be left believing a modifier from a previous session.
        let roles = generic_roles_down();
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
            // No `BindingUp` owed here — see `reset_chord_state`: `stop()`
            // above already ran this reset and cleared the plain
            // `key_down`/`mouse_down` latches for the session that ended,
            // so both can only be found inactive/false here; and
            // `event_tx` at this point belongs to the brand-new session,
            // which has sent no `BindingDown` — an unpaired up would be
            // wrong.
            reset_chord_state(&mut shared);
            apply_resync(&mut shared, roles);
        }
        tracing::debug!("chord modifier resync: {}", resync_summary(roles));

        let (ready_tx, ready_rx) = channel();
        let thread = thread::Builder::new()
            .name("ptt-input-hook".into())
            .spawn(move || hook_thread(ready_tx))
            .map_err(|error| Error::InputHook(error.to_string()))?;
        self.thread = Some(thread);

        // Only report success once the hooks are really installed (plan §7).
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.stop();
                Err(error)
            }
            Err(_) => {
                self.stop();
                Err(Error::InputHook(
                    "the hook thread exited before installing the hooks".into(),
                ))
            }
        }
    }

    fn set_binding(&mut self, binding: Binding, swallow: bool, toggle: Option<(Binding, bool)>) {
        // The `BindingUp` owed by what this rebind leaves talking — a
        // chord `reset_chord_state` finds engaged, or the plain PTT
        // key/button latch the guard below clears while the input is still
        // held — is sent after the guard is gone; see `reset_chord_state`
        // for why this site emits and `start`/`stop` do not.
        let (owed, event_tx) = {
            let mut shared = shared();
            shared.binding = Some(binding);
            shared.swallow = swallow;
            shared.toggle = toggle.map(|(binding, _)| binding);
            shared.toggle_swallow = toggle.is_some_and(|(_, swallow)| swallow);
            // Read *before* the latches are cleared. `true` means this
            // session already emitted a `BindingDown` for a key or button
            // the user is still holding, and once `binding` is replaced
            // that input's release matches nothing — no `BindingUp` of its
            // own would ever reach the engine, and the microphone would
            // stay open until the new binding is pressed for the first
            // time (the hot mic this emission exists to prevent).
            let plain_down = shared.key_down || shared.mouse_down;
            shared.key_down = false;
            shared.mouse_down = false;
            // The toggle latches owe nothing: `ToggleDown` has no up by
            // design, so nothing is outstanding for the engine.
            shared.toggle_key_down = false;
            shared.toggle_mouse_down = false;
            // A stale activation from the previous binding must not leak into
            // the new one; held modifiers are event-derived and stay (spec §5).
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
            // The accumulator owns the press-and-hold sequence now (spec §7):
            // it swallows every member and answers with a chord or the plain
            // binding an unmodified press stands for.
            shared.capture_accumulator.arm();
        }
        // Also listen at the raw-input level: while the settings window owns
        // the keyboard focus, the low-level chain never reaches our hook
        // (bug 3), so the hook alone cannot see the press the user is asked
        // to make. Removed again the moment the capture is over — answered
        // by whichever reporter saw the press first, the other one finding
        // it already spent (`CaptureAccumulator::is_spent`) — while a
        // modifier that answers nothing keeps the listener alive, or the
        // completing key would have nowhere left to arrive.
        add_raw_keyboard();
        rx
    }

    fn cancel_capture(&mut self) {
        // Nothing may keep eating the next key press once nobody is waiting
        // for it (the window answered by itself, or it was closed).
        {
            let mut shared = shared();
            shared.capture_tx = None;
            shared.capture_accumulator = CaptureAccumulator::default();
        }
        remove_raw_keyboard();
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let id = HOOK_THREAD.load(Ordering::SeqCst);
            if id != 0 {
                // WM_QUIT ends the loop; the loop unhooks both callbacks.
                let _ = unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            }
            let _ = thread.join();
            HOOK_THREAD.store(0, Ordering::SeqCst);
        }
        let mut shared = shared();
        shared.event_tx = None;
        shared.capture_tx = None;
        shared.key_down = false;
        shared.mouse_down = false;
        shared.toggle_key_down = false;
        shared.toggle_mouse_down = false;
        shared.capture_accumulator = CaptureAccumulator::default();
        // No `BindingUp` owed here — see `reset_chord_state`: the session
        // is ending, `event_tx` was cleared above (there is no sender left
        // to tell even if one were owed), and quit hands the microphone
        // back regardless of any `BindingUp`. The plain
        // `key_down`/`mouse_down` latches cleared here owe nothing either,
        // for the very same reason: a held PTT key must not outlive the
        // session, and this session's channel is already gone.
        reset_chord_state(&mut shared);
    }
}

/// Hook thread: install both hooks, listen for the broadcasts that require
/// re-installing them (plan §7), then pump messages until `WM_QUIT`.
fn hook_thread(ready: Sender<Result<()>>) {
    HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);

    let (keyboard, mouse) = match unsafe { install_hooks() } {
        Ok(hooks) => hooks,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    store_hooks(keyboard, mouse);
    tracing::info!("input hooks installed (keyboard + mouse)");
    log_hooks_timeout();

    // A hidden window on this thread is what receives resume/unlock
    // broadcasts; without it the hooks would silently stop working after a
    // sleep (plan §7's manual test). Created before the session is told
    // "ready" so a capture armed immediately afterwards already has the
    // raw-input target (bug 3).
    let window = unsafe { create_notify_window() }.ok();
    // Stored *before* `ready` is sent: a capture armed right after start-up
    // must already find the raw-input target.
    if let Some(hwnd) = window {
        NOTIFY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        // Heartbeat (spec §4 D2): the notify window beats once a second so
        // the watchdog can tell a stalled message pump from an idle machine.
        // Log-only — the timer carries no work. A failed timer (0) would
        // silently kill stall detection, so it is worth a warning.
        let heartbeat_timer = unsafe { SetTimer(Some(hwnd), 1, 1_000, None) };
        if heartbeat_timer == 0 {
            tracing::warn!("cannot start the hook-thread heartbeat timer");
        }
    }
    let _ = ready.send(Ok(()));

    // Silent-removal watchdog (spec §4 D3): a separate thread that checks the
    // recorded beats every two seconds and logs a finding when the pump
    // stalls, the hooks go silent while input still arrives, or the session
    // worker stalls. Log-only — it never rehooks or changes input behaviour.
    let (stop_tx, stop_rx) = channel::<()>();
    let watchdog_thread = thread::Builder::new()
        .name("ptt-input-watchdog".into())
        .spawn(move || watchdog(stop_rx))
        .ok();

    if let Some(hwnd) = window {
        // Not fatal: the session keeps running, only re-installation is lost.
        let _ = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) };
    }

    unsafe { message_loop() };

    // Stop the watchdog at once: dropping the sender wakes it out of its
    // two-second wait, so this join does not lag session stop by up to 2 s.
    drop(stop_tx);
    if let Some(watchdog_thread) = watchdog_thread {
        let _ = watchdog_thread.join();
    }

    if let Some(hwnd) = window {
        NOTIFY_HWND.store(0, Ordering::SeqCst);
        let _ = unsafe { WTSUnRegisterSessionNotification(hwnd) };
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    drop_hooks();

    // Thread is going away: dropping the senders wakes a waiting runner.
    let mut shared = shared();
    shared.event_tx = None;
    shared.capture_tx = None;
}

unsafe fn install_hooks() -> Result<(HHOOK, HHOOK)> {
    let module = GetModuleHandleW(PCWSTR::null())?;
    let hmod = HINSTANCE(module.0);

    let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hmod), 0)?;
    let mouse = match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hmod), 0) {
        Ok(mouse) => mouse,
        Err(error) => {
            let _ = UnhookWindowsHookEx(keyboard);
            return Err(error.into());
        }
    };
    Ok((keyboard, mouse))
}

unsafe fn message_loop() {
    let mut msg = MSG::default();
    loop {
        // Where the thread stands (spec §4): parked here, the one-second
        // heartbeat timer can still fire, so a stall reported in this phase
        // means the timer stopped rather than the thread.
        diagnostics::note_hook_phase(diagnostics::HookPhase::Pumping);
        // 0 means WM_QUIT, negative means an error: either way, stop.
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
}

/// Watchdog loop (spec §4 D3): waits up to two seconds for a stop signal,
/// then checks the recorded beats and logs one warning per finding. A stop
/// signal or a dropped sender ends it — the hook thread drops the sender to
/// wake it immediately instead of waiting out the timeout. Log-only.
fn watchdog(stop_rx: Receiver<()>) {
    let mut watchdog = diagnostics::Watchdog::new(diagnostics::tick_ms());
    // Wait-state probe (spec §4): the previous CPU/system reading, so a
    // stall can be told apart from a spin or a starvation by the delta
    // across it. Seeded once here — the hook thread id is stored before
    // this thread spawns — so even a stall reported on the very first check
    // has a baseline; then taken one watchdog tick apart, a window that
    // sits inside the stall, since a stall is >5 s old when reported.
    let mut previous: Option<WaitState> = sample_wait_state();
    loop {
        match stop_rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // Read before checking: both this sample and the last predate the
        // finding's report, so the window is inside the stall. Watchdog
        // thread only — never a hook callback.
        let current = sample_wait_state();
        for finding in watchdog.check(&diagnostics::snapshot(system_last_input_tick())) {
            let message = finding.message();
            // The tracing line can die with the process (non-blocking
            // appender + hard kill); the synchronous copy cannot. Watchdog
            // thread only — never a hook callback.
            diagnostics::record_finding(&message);
            tracing::warn!("{message}");

            // Follow a hook-thread stall with what the thread was doing:
            // executing, blocked, or starved — each points at a different
            // fix (spec §4). Both readings must exist or the probe stays
            // quiet rather than guess.
            if matches!(finding, diagnostics::Finding::HookThreadStalled { .. }) {
                if let (Some(before), Some(after)) = (previous, current) {
                    let window = after.tick.wrapping_sub(before.tick) as u64 * 10_000;
                    if let Some(cause) = diagnostics::StallCause::from_deltas(
                        after.hook_cpu.saturating_sub(before.hook_cpu),
                        after.system_idle.saturating_sub(before.system_idle),
                        after.system_total.saturating_sub(before.system_total),
                        window,
                    ) {
                        let cause_message = cause.message();
                        diagnostics::record_finding(&cause_message);
                        tracing::warn!("{cause_message}");
                    }
                }

                // ... which call it was standing in (spec §4): the
                // wait-state probe says *blocked*; this says blocked in
                // *which* call — our own mutex or the system-wide hook
                // chain — and the two imply different fixes.
                let phase_message = diagnostics::current_hook_phase().message();
                diagnostics::record_finding(&phase_message);
                tracing::warn!("{phase_message}");
            }
        }
        previous = current;
    }
}

/// Last system-wide input tick from `GetLastInputInfo`, or `0` when Windows
/// will not say. A `0` keeps the hooks-silent finding quiet rather than
/// claiming input is flowing (spec §4 D3).
fn system_last_input_tick() -> u32 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a correctly sized, writable LASTINPUTINFO.
    if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        info.dwTime
    } else {
        0
    }
}

/// One wait-state probe reading (spec §4): cumulative CPU and idle counts
/// in 100-ns units, stamped with the tick it was taken at.
#[derive(Clone, Copy)]
struct WaitState {
    /// `GetTickCount` when the reading was taken.
    tick: u32,
    /// Hook thread kernel + user time (100-ns units).
    hook_cpu: u64,
    /// Machine-wide idle time (100-ns units).
    system_idle: u64,
    /// Machine-wide kernel + user time, which on Windows already includes
    /// idle — the denominator for the machine's idle share (100-ns units).
    system_total: u64,
}

/// Read the hook thread's CPU consumption and the machine's idle/busy split
/// for the wait-state probe (spec §4). `None` when there is no hook thread
/// yet or Windows will not say — the probe then logs nothing rather than
/// guess. Watchdog thread only: `OpenThread` takes a query-only handle that
/// is closed before returning, and nothing here touches the input path.
fn sample_wait_state() -> Option<WaitState> {
    let thread_id = HOOK_THREAD.load(Ordering::SeqCst);
    if thread_id == 0 {
        return None;
    }
    // SAFETY: `OpenThread` needs only query access for `GetThreadTimes`;
    // the handle is closed on every path below, and the four out-times are
    // valid, writable `FILETIME` locals that outlive the call.
    let handle = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, thread_id) }.ok()?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let thread_times =
        unsafe { GetThreadTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    // SAFETY: the handle came from the `OpenThread` above and is closed
    // exactly once here.
    let _ = unsafe { CloseHandle(handle) };
    thread_times.ok()?;

    let mut idle = FILETIME::default();
    let mut system_kernel = FILETIME::default();
    let mut system_user = FILETIME::default();
    // SAFETY: three valid, writable `FILETIME` locals; `GetSystemTimes`
    // fills each it is given and touches nothing else.
    unsafe {
        GetSystemTimes(
            Some(&mut idle),
            Some(&mut system_kernel),
            Some(&mut system_user),
        )
    }
    .ok()?;

    Some(WaitState {
        tick: diagnostics::tick_ms(),
        hook_cpu: filetime_100ns(kernel) + filetime_100ns(user),
        system_idle: filetime_100ns(idle),
        system_total: filetime_100ns(system_kernel) + filetime_100ns(system_user),
    })
}

/// A `FILETIME` is a count of 100-ns intervals split across two 32-bit
/// halves; recombine without losing the high word.
fn filetime_100ns(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

/// Log the effective `LowLevelHooksTimeout` once per hook-thread start
/// (spec §4 D5). Purely informational: any failure logs "not set or
/// unreadable" and changes nothing — the hooks install either way.
fn log_hooks_timeout() {
    let mut value = [0u8; 4];
    let mut size = value.len() as u32;
    // SAFETY: `value`/`size` outlive the call and match the requested DWORD
    // type; `None` for the type out-parameter is allowed.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Desktop"),
            w!("LowLevelHooksTimeout"),
            RRF_RT_REG_DWORD,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.0 == 0 && size == 4 {
        tracing::info!(
            "Windows LowLevelHooksTimeout = {} ms",
            u32::from_le_bytes(value)
        );
    } else {
        tracing::info!("Windows LowLevelHooksTimeout is not set or unreadable");
    }
}

/// Remember the freshly installed pair.
fn store_hooks(keyboard: HHOOK, mouse: HHOOK) {
    KEYBOARD_HOOK.store(keyboard.0 as isize, Ordering::SeqCst);
    MOUSE_HOOK.store(mouse.0 as isize, Ordering::SeqCst);
}

/// Detach the current pair from the statics and unhook it, if any.
fn drop_hooks() {
    for raw in [
        KEYBOARD_HOOK.swap(0, Ordering::SeqCst),
        MOUSE_HOOK.swap(0, Ordering::SeqCst),
    ] {
        if raw != 0 {
            let _ = unsafe { UnhookWindowsHookEx(HHOOK(raw as *mut std::ffi::c_void)) };
        }
    }
}

/// Plan §7: Windows may drop low-level hooks across a suspend or the secure
/// desktop of the lock screen, so every resume / session unlock re-installs
/// them.
///
/// The new pair goes in *before* the old one is released: a failed
/// re-install leaves working hooks in place, and the handful of duplicated
/// events in between are absorbed by the press latch (auto-repeat and a
/// repeated release are already ignored by the state machine, plan §5).
fn rehook() {
    tracing::info!("re-installing the input hooks (resume or unlock)");
    let Ok((keyboard, mouse)) = (unsafe { install_hooks() }) else {
        tracing::warn!("re-install failed; keeping the current hooks");
        return;
    };
    drop_hooks();
    store_hooks(keyboard, mouse);
    tracing::info!("input hooks re-installed");

    // A key held across the transition must not leave a stuck latch; a
    // duplicate press this may cause is ignored while already talking.
    // Chords need the same recovery one step further: Windows may have
    // dropped a *release* while the hooks were gone, so the chord state is
    // reset and the four generic modifier roles are re-derived from
    // `GetAsyncKeyState` — read before the lock, see `generic_roles_down`.
    let roles = generic_roles_down();
    let (ptt_engaged, capture_cancelled, event_tx) = {
        let mut shared = shared();
        // The plain latches are cleared here too — but unlike a rebind,
        // this site owes them no `BindingUp`: a rehook does not touch
        // `binding`, so the key or button the user is still holding keeps
        // matching it, and that input's own release forwards the
        // `BindingUp` when they finally let go. Talking for as long as
        // they hold PTT is what they asked for; emitting here would mute a
        // genuinely held key mid-press and leave a second, unpaired up
        // behind on the release. (The toggle latches owe nothing either —
        // `ToggleDown` has no up by design.)
        shared.key_down = false;
        shared.mouse_down = false;
        shared.toggle_key_down = false;
        shared.toggle_mouse_down = false;
        // A capture armed when the hooks dropped is mid-sequence over
        // events that are gone: its accumulated modifiers would decide the
        // answer wrongly (a stale Ctrl would turn the next press into a
        // chord nobody pressed). So it is not merely disarmed — the whole
        // capture is *cancelled*: `capture_tx` goes too, which is the one
        // signal the session loop can see (`pump`'s `try_recv` reports the
        // disconnect, drops its reply and lets the settings page leave its
        // capturing state so a fresh capture can be started), and the
        // raw-input sink registered for it is removed below.
        let capture_cancelled = shared.capture_tx.take().is_some();
        shared.capture_accumulator = CaptureAccumulator::default();
        let ptt_engaged = reset_chord_state(&mut shared);
        apply_resync(&mut shared, roles);
        // The sender is cloned under the guard so the `BindingUp` owed to
        // an engaged chord can be sent once it is gone (`call_next`).
        (ptt_engaged, capture_cancelled, shared.event_tx.clone())
    };
    // Every action below runs with no guard held (see `call_next`): the
    // mic-muting up first — the whole point of Finding 1 — then the
    // capture teardown, then the log.
    emit_up_for_reset(ptt_engaged, event_tx);
    // Accepted and documented, not silently skipped: an *engaged chord's*
    // final key or button that the user is still holding here owes the
    // focused application a swallow on its eventual release — its press
    // was swallowed at engagement, and after the reset above `step`
    // answers nothing for that release, so it is forwarded. Swallowing it
    // would take state the reset just cleared: "this member's press was
    // swallowed and its release is still owed a swallow" is exactly the
    // activation trace `reset_chord_state` exists to forget. Re-deriving
    // it as a `ChordState::blocked` would be worse, not better — `reset`
    // clears `blocked` on purpose because a release Windows dropped may
    // have made it stale (spec §5's lost-release guard), and a stale
    // block swallows the user's *next real press* of that key. So the
    // focused application sees one unbalanced key-up — harmless, an
    // application balances key-downs, not key-ups — and the privacy
    // direction stays safe either way.
    if capture_cancelled {
        // Leaving `RIDEV_INPUTSINK` registered for a capture that can no
        // longer complete would cost a `GetRawInputData` plus a `Vec`
        // allocation on the hook thread for every keystroke for the rest
        // of the process — the teardown `cancel_capture` would have done.
        remove_raw_keyboard();
        tracing::info!("capture cancelled by the rehook — it can be started again");
    }
    tracing::debug!("chord modifier resync: {}", resync_summary(roles));
}

/// Hidden top-level window on the hook thread — the only way to receive
/// `WM_POWERBROADCAST` and the WTS session notifications.
unsafe fn create_notify_window() -> Result<HWND> {
    let module = GetModuleHandleW(PCWSTR::null())?;
    let instance = HINSTANCE(module.0);
    let class = class_name();

    let window_class = WNDCLASSW {
        lpfnWndProc: Some(notify_proc),
        hInstance: instance,
        lpszClassName: class,
        ..Default::default()
    };
    // Already registered by an earlier session in this process: harmless.
    let _ = unsafe { RegisterClassW(&window_class) };

    // No WS_VISIBLE: the window is never shown, only enumerated for the
    // broadcasts it exists to receive.
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class,
            class,
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?
    };
    Ok(hwnd)
}

/// The registered class name, NUL-terminated and alive for the process.
fn class_name() -> PCWSTR {
    let name = CLASS_NAME.get_or_init(|| "ptt-tool-notify\0".encode_utf16().collect());
    PCWSTR(name.as_ptr())
}

/// Ask Windows for raw keyboard reports while a capture is armed.
///
/// This exists because of bug 3: while the settings window's WebView2 owns
/// the keyboard focus, key presses never reach our low-level keyboard hook
/// (mouse input and unfocused keyboard input are unaffected — observed on
/// the partner's machine). Raw input is delivered to the notify window
/// regardless of focus and regardless of any other hook in the low-level
/// chain, so "press any key" can always see the press.
fn add_raw_keyboard() {
    let target = NOTIFY_HWND.load(Ordering::SeqCst);
    if target == 0 {
        return;
    }
    let device = [RAWINPUTDEVICE {
        usUsagePage: HID_KEYBOARD_PAGE,
        usUsage: HID_KEYBOARD_USAGE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: HWND(target as *mut _),
    }];
    if let Err(error) =
        unsafe { RegisterRawInputDevices(&device, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }
    {
        tracing::warn!("cannot listen to raw keyboard input: {error}");
    }
}

/// Stop listening again — a capture listens only between arm and report
/// (plan §11: the app looks at nothing but what the task needs).
fn remove_raw_keyboard() {
    let device = [RAWINPUTDEVICE {
        usUsagePage: HID_KEYBOARD_PAGE,
        usUsage: HID_KEYBOARD_USAGE,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND::default(),
    }];
    let _ =
        unsafe { RegisterRawInputDevices(&device, std::mem::size_of::<RAWINPUTDEVICE>() as u32) };
}

/// One parsed raw keyboard report for an armed capture (bug 3's capture
/// path): which binding the report stands for — [`raw_binding`]'s pure
/// decision, `None` for a release — plus the raw keys, so a release can
/// still be turned into the `Up` event the capture accumulator must see.
///
/// Up from down **is** distinguishable in the raw data: `RAWKEYBOARD.Flags`
/// carries `RI_KEY_BREAK` for a release (and `RI_KEY_E0` for the extended
/// prefix, the side signal for Ctrl and Alt, so the accumulator records the
/// side that was actually pressed). Before this report existed the path
/// asked only for the press half, which is why `note_release_all()` never
/// ran here — a `Ctrl+Shift` modifier-only capture over Raw Input never
/// answered, and an unseen Ctrl release left a stale slot that turned a
/// later plain `N` into `Chord{ctrl, N}`.
struct RawKeyReport {
    /// `Some(Binding::Key)` for a press; `None` for a release — a release
    /// is never a binding ([`raw_binding`]'s rule), but it is still an
    /// event the accumulator has to consume.
    binding: Option<Binding>,
    /// `RAWKEYBOARD.VKey`.
    vk: u16,
    /// `RAWKEYBOARD.MakeCode`.
    scan: u16,
    /// `RI_KEY_E0` — the side signal for Ctrl and Alt.
    extended: bool,
}

impl RawKeyReport {
    /// The event to hand the capture accumulator. The direction comes from
    /// [`raw_binding`]'s decision — a press stands for a binding, a
    /// release for none — because a release has no binding to carry its
    /// keys, so the report keeps them: its `Up` is what makes
    /// [`CaptureAccumulator::note_release_all`] fire on this path, the
    /// only way a modifier-only capture completes (spec §7).
    fn event(&self) -> KeyEvent {
        match self.binding {
            Some(Binding::Key { vk, scan }) => KeyEvent::Down {
                vk,
                scan,
                extended: self.extended,
            },
            // Only a release lands here — `raw_binding` maps every release
            // to `None` — and it needs the raw keys precisely because it
            // has no binding of its own.
            _ => KeyEvent::Up {
                vk: self.vk,
                scan: self.scan,
                extended: self.extended,
            },
        }
    }
}

/// Parse the `RAWINPUT` keyboard report behind `WM_INPUT`, or `None` when
/// it is not one (the pure decisions are [`raw_binding`] and
/// [`RawKeyReport::event`]).
unsafe fn raw_keyboard_report(lparam: LPARAM) -> Option<RawKeyReport> {
    let hraw = HRAWINPUT(lparam.0 as *mut _);
    let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    // First call: ask for the needed size (returns 0 with `size` filled).
    if unsafe { GetRawInputData(hraw, RID_INPUT, None, &mut size, header) } != 0 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    let read = unsafe {
        GetRawInputData(
            hraw,
            RID_INPUT,
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
            header,
        )
    };
    // A keyboard report is `RAWINPUTHEADER + RAWKEYBOARD` — 40 bytes on
    // 64-bit Windows. `size_of::<RAWINPUT>()` is the *union* (sized for its
    // largest member, the mouse report) and is 48, so comparing against it
    // rejected every single keyboard report and left this fallback dead.
    let data_offset = std::mem::offset_of!(RAWINPUT, data);
    let needed = data_offset + std::mem::size_of::<RAWKEYBOARD>();
    if read == u32::MAX || (read as usize) < needed || buffer.len() < needed {
        return None;
    }
    // Copy the two parts out by value instead of viewing the buffer as a
    // `&RAWINPUT`: the buffer is a byte `Vec` that is smaller than a whole
    // `RAWINPUT` (and not necessarily aligned for one).
    let header = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<RAWINPUTHEADER>()) };
    if header.dwType != RIM_TYPEKEYBOARD.0 {
        return None;
    }
    let keyboard =
        unsafe { std::ptr::read_unaligned(buffer.as_ptr().add(data_offset).cast::<RAWKEYBOARD>()) };
    Some(RawKeyReport {
        binding: raw_binding(
            keyboard.VKey,
            keyboard.MakeCode,
            keyboard.Flags & RI_KEY_BREAK != 0,
        ),
        vk: keyboard.VKey,
        scan: keyboard.MakeCode,
        extended: keyboard.Flags & RI_KEY_E0 != 0,
    })
}

/// Pure decision part of the raw-input capture path (see
/// [`RawKeyReport`]): a key *press* becomes the binding; a release never
/// does (the hook's capture branch has the same rule for the events it
/// does see). The release is still an event — [`RawKeyReport::event`]
/// turns this `None` into the `Up` the accumulator consumes.
fn raw_binding(vkey: u16, makecode: u16, key_up: bool) -> Option<Binding> {
    (!key_up).then_some(Binding::Key {
        vk: vkey,
        scan: makecode,
    })
}

/// Notify-window procedure: re-install the hooks when Windows says the
/// machine resumed or the user came back from the lock screen (plan §7).
unsafe extern "system" fn notify_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Every message this window receives is handled here (spec §4), so a
    // stall in this phase localizes to the window procedure as a whole.
    diagnostics::note_hook_phase(diagnostics::HookPhase::NotifyWindow);
    // A raw keyboard report for an armed capture (bug 3): deliver it the
    // same way the low-level hook branch does — in **both** directions now
    // — then stop listening again, but only once the capture is over,
    // either because this report answered it or because the low-level hook
    // answered it first and this duplicate found the accumulator already
    // spent. Whichever reporter sees the press first wins; the other still
    // has to tear the sink down.
    if message == WM_INPUT {
        if let Some(report) = unsafe { raw_keyboard_report(lparam) } {
            // bug 3's raw-input fallback: while the settings window owns the
            // keyboard focus the low-level hook may see nothing at all, so
            // the accumulator is the only place a capture can be
            // mid-sequence. Presses and releases alike arrive here —
            // `RI_KEY_BREAK` tells them apart — and the accumulator answers
            // with a chord or the plain binding an unmodified press stands
            // for. `swallows()` is asked before `note_key` — its ordering
            // contract — and the event is noted only while a capture is
            // armed; whichever reporter sees the press first wins, the
            // other finds the accumulator already spent (`is_spent`).
            // `None` while no answer was produced; `spent` says whether the
            // capture is over either way — see `CaptureAccumulator::is_spent`.
            let (answered, discarded, spent) = {
                let mut shared = shared();
                let capturing = shared.capture_accumulator.swallows();
                let answer = if capturing {
                    shared
                        .capture_accumulator
                        .note_key(report.event())
                        // A key-up of the last held modifier completes a
                        // modifier-only chord here — the half of spec §7
                        // this path used to drop on the floor, which is
                        // why a `Ctrl+Shift` capture over Raw Input never
                        // answered (and why a release this path never saw
                        // left a stale Ctrl slot behind).
                        .or_else(|| shared.capture_accumulator.note_release_all())
                } else {
                    None
                };
                // Fresh with every release-all evaluation, and read only
                // while one could have run — a stale flag from an earlier
                // capture must not be logged as this report's doing.
                let discarded = capturing && shared.capture_accumulator.discarded_lone_modifier();
                // Asked *after* noting, so an answer this very report
                // produced counts as spent too.
                let spent = shared.capture_accumulator.is_spent();
                let answered = answer.map(|answer| {
                    let bound = answer_binding(answer);
                    if let Some(tx) = shared.capture_tx.take() {
                        let _ = tx.send(bound);
                    }
                    bound
                });
                (answered, discarded, spent)
            };
            // Everything below runs after the guard is gone: no formatting
            // — hence no allocation — ever happens while `Shared` is held.
            if let Some(bound) = answered {
                tracing::info!("captured {bound:?} (raw input)");
            }
            if discarded {
                // The hook logs this very same line for the events it sees;
                // on the machine this fallback exists for, its branch never
                // runs, so the raw path is the only place left that can.
                tracing::info!(
                    "a lone modifier cannot be held to talk — capture starts over (raw input)"
                );
            }
            // Tear the sink down the moment the capture is over — this
            // report answered it, or the low-level hook answered it first
            // and this duplicate found the accumulator spent. Leaving
            // `RIDEV_INPUTSINK` registered after that would cost a
            // `GetRawInputData` plus a `Vec` allocation on the hook thread
            // for every keystroke for the rest of the process. Only a
            // genuine mid-capture event — a modifier pressed, or one
            // released while another member is still down — answers nothing
            // and is not spent, and leaves the listener alive: on the very
            // machine this fallback exists for, tearing the sink down there
            // would destroy the only remaining path for the completing key
            // to arrive, and the capture would hang.
            if answered.is_some() || spent {
                remove_raw_keyboard();
            }
        }
        // "An application that processes WM_INPUT must return TRUE" — 0.
        return LRESULT(0);
    }
    // Heartbeat arm (spec §4 D2): every timer tick records a message-loop
    // beat, so a stalled pump shows up as a watchdog finding. Log-only.
    if message == WM_TIMER {
        diagnostics::note_hook_thread();
        return LRESULT(0);
    }
    let resumed = message == WM_POWERBROADCAST
        && matches!(
            wparam.0 as u32,
            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND | PBT_APMRESUMECRITICAL
        );
    if resumed || message == WM_WTS_SESSION_CHANGE {
        rehook();
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Forward the event the hook callback decided to keep.
fn emit(shared: &mut Shared, event: InputEvent) {
    if let Some(tx) = &shared.event_tx {
        let _ = tx.send(event);
    }
}

/// Forward to the next hook in the system-wide chain with the phase marked
/// (spec §4). This is the one call inside a low-level callback that can wait
/// on *another application's* hook — an overlay, a game's anti-cheat — so
/// the stall probe has to see the thread enter it. The phase is saved and
/// restored around the call, so a stall *after* it must not still read as
/// "inside CallNextHookEx".
///
/// **Contract: the caller must never hold the shared mutex across this
/// call.** Windows can re-enter our own hook while we are inside it; a
/// nested callback that then blocked on the mutex this frame still owned
/// would deadlock the hook thread permanently. That is the Apex freeze the
/// watchdog caught in the field — every one of its 43 stall reports read
/// `hook-thread phase: waiting for the shared binding mutex`, with 0% CPU
/// on an idle machine. Both callbacks `drop` their guard immediately before
/// each call for that reason.
unsafe fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let previous = diagnostics::current_hook_phase();
    diagnostics::note_hook_phase(diagnostics::HookPhase::CallingNextHook);
    let result = unsafe { CallNextHookEx(None, code, wparam, lparam) };
    diagnostics::note_hook_phase(previous);
    result
}

/// `WH_KEYBOARD_LL` measuring wrapper (spec §4 D1).
///
/// Records a callback beat on every entry — including `code < 0` — then times
/// the inner work. `wait_ms` is how long the event waited for the hook to run
/// (Windows' own event timestamp vs now), `exec_ms` how long our code took;
/// either crossing its threshold logs a rate-limited stall warning. Log-only:
/// it never rehooks, reprioritises, or changes what the hook does.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    diagnostics::note_keyboard_cb();
    diagnostics::note_hook_phase(diagnostics::HookPhase::CallbackEntered);
    if code < 0 {
        return unsafe { keyboard_proc_inner(code, wparam, lparam) };
    }
    // SAFETY: for `code >= 0` Windows hands a valid KBDLLHOOKSTRUCT in
    // `lparam`; every installation of this hook relies on that.
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let wait_ms = unsafe { GetTickCount() }.wrapping_sub(info.time);
    let started = std::time::Instant::now();
    let result = unsafe { keyboard_proc_inner(code, wparam, lparam) };
    let exec_ms = started.elapsed().as_millis() as u64;
    if let Some(message) = diagnostics::maybe_stall_warning("keyboard", wait_ms, exec_ms) {
        tracing::warn!("{message}");
    }
    result
}

/// `WH_KEYBOARD_LL` callback (plan §7): compare, send, return.
unsafe extern "system" fn keyboard_proc_inner(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    log_once(
        &KEYBOARD_FIRST_CALL,
        "keyboard hook callback alive (first keyboard event)",
    );
    if code < 0 {
        return unsafe { call_next(code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // Never act on synthetic input (plan §7) — otherwise our own swallow
    // logic or an automation tool could bounce the mic.
    if info.flags.contains(LLKHF_INJECTED) {
        log_once(
            &KEYBOARD_FIRST_INJECTED,
            "keyboard hook sees synthetic (injected) input — first of possibly many",
        );
        return unsafe { call_next(code, wparam, lparam) };
    }

    let message = wparam.0 as u32;
    let pressed = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    let released = matches!(message, WM_KEYUP | WM_SYSKEYUP);
    if !pressed && !released {
        return unsafe { call_next(code, wparam, lparam) };
    }
    let vk = info.vkCode as u16;
    let scan = info.scanCode as u16;
    // The side signal for Ctrl and Alt (spec §4); Shift's side is in the
    // scan code, the Win keys have distinct vks.
    let extended = info.flags.contains(LLKHF_EXTENDED);
    let event = if pressed {
        KeyEvent::Down { vk, scan, extended }
    } else {
        KeyEvent::Up { vk, scan, extended }
    };

    // The one wait this callback can do on *our own* state (spec §4): mark
    // the approach, so a stall here reads as contention on the mutex rather
    // than as the hook chain beyond us.
    //
    // The press-and-hold capture below takes this guard for itself and lets
    // it go again before the binding comparison re-takes it. Two short
    // sections rather than one long hold because of what a *discarded* lone
    // modifier needs: the accumulator discards it — "a bare modifier cannot
    // be held to talk" (spec §7) — and that discard has to stay observable
    // in the log, while nothing that allocates, tracing included, may run
    // while `Shared` is held (the deadlock contract in `call_next`). Each
    // section is still only a compare plus a send.
    let discarded_lone_modifier = 'capture: {
        diagnostics::note_hook_phase(diagnostics::HookPhase::AcquiringShared);
        let mut shared = shared();
        diagnostics::note_hook_phase(diagnostics::HookPhase::HoldingShared);

        // The matcher learns every key event — modifier state must be correct
        // the moment a chord is bound, even while none is (spec §6).
        shared.matcher.note_key(event);

        // Press-and-hold capture (spec §7): the whole sequence is consumed so
        // the focused application never keeps a modifier it did not see
        // released, and the accumulator answers with a chord or the plain
        // binding an unmodified press stands for.
        //
        // `swallows()` is asked *before* `note_key` — its ordering contract:
        // noting disarms the accumulator the instant an answer is produced, so
        // asking afterwards would let the completing press escape. The event is
        // never dropped on the floor either way: `note_key` sees it in both
        // cases, and the original event is forwarded only when no capture was
        // armed.
        let capturing = shared.capture_accumulator.swallows();
        let answer = shared.capture_accumulator.note_key(event);
        if !capturing {
            break 'capture false;
        }
        // Releasing every accumulated modifier completes a modifier-only
        // chord; a lone modifier is discarded and capture re-arms — the
        // "a bare modifier cannot be held to talk" rule the session loop
        // used to enforce now lives in the accumulator (spec §7).
        let answer = answer.or_else(|| shared.capture_accumulator.note_release_all());
        // True only for that discard: every other event that answers nothing
        // — a modifier pressed or released mid-sequence — leaves it false.
        let discarded = shared.capture_accumulator.discarded_lone_modifier();
        if let Some(answer) = answer {
            if let Some(tx) = shared.capture_tx.take() {
                // Nothing that allocates while `Shared` is held — the
                // deadlock contract in `call_next`. In particular no log
                // line here: the session logs the very same
                // `captured {binding:?}` when it receives the answer, off
                // the hook thread entirely.
                let _ = tx.send(answer_binding(answer));
            }
            return LRESULT(1);
        }
        if !discarded {
            // Mid-sequence (a modifier pressed or released): keep waiting —
            // and keep swallowing.
            return LRESULT(1);
        }
        // The discard: swallowed like every other member of the sequence,
        // and logged below, where no guard is held.
        discarded
    };
    if discarded_lone_modifier {
        // The session loop used to see the bare modifier arrive and log
        // this itself; the accumulator discards it inside the hook now, so
        // the hook is the only place left that can say so.
        tracing::info!("a lone modifier cannot be held to talk — capture starts over");
    }

    // Compare the key against both bindings under this one guard (plan §11:
    // only the bound inputs are ever looked at). The press passes through
    // only when it matches *neither* — the toggle must get its chance even
    // when the key is not the PTT binding.
    diagnostics::note_hook_phase(diagnostics::HookPhase::AcquiringShared);
    let mut shared = shared();
    diagnostics::note_hook_phase(diagnostics::HookPhase::HoldingShared);
    let ptt = shared
        .binding
        .is_some_and(|binding| binding.matches_key(vk));
    let toggle = shared.toggle.is_some_and(|toggle| toggle.matches_key(vk));
    // Only `Binding::Chord` ever reaches the matcher; `Key` and `Mouse`
    // keep the existing comparison completely untouched (spec §6).
    let ptt_chord = match shared.binding {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    let toggle_chord = match shared.toggle {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    if !ptt && !toggle && ptt_chord.is_none() && toggle_chord.is_none() {
        // Never hold `SHARED` across `CallNextHookEx` — Windows can re-enter
        // our hook during the call and deadlock us on this mutex (`call_next`).
        drop(shared);
        return unsafe { call_next(code, wparam, lparam) };
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
        // A press is consumed if *either* matching binding asked for it;
        // same key bound to both is not special-cased — both events fire.
        swallowed |= shared.toggle_swallow;
    }

    // A chord binding: one `step` per chord binding, and only ever for
    // `Binding::Chord` — `ptt`/`toggle` above are false for one. `Engaged`
    // is the edge that emits (auto-repeat repeats the *down*, never this
    // edge, so one physical press yields one `BindingDown`), `Released`
    // ends the chord; both report a swallow only for the chord's
    // completing member, so every modifier keeps reaching other
    // applications and `Ctrl+C` keeps working (spec §5).
    //
    // Both outcomes are computed before anything is emitted: a
    // `MutexGuard`'s fields cannot be borrowed disjointly at a call site,
    // and `emit` needs the whole guard again.
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
        // is swallowed like the press it repeats (spec §5 rule 3): `step`
        // answers nothing for a repeat, so without this the focused
        // application would receive the chord at keyboard-repeat rate for
        // as long as the user holds it — precisely the guarantee the
        // plain-key press latch above gives. Decided before `step`, so the
        // press that engages the chord is never its own repeat.
        let ptt_repeat =
            ptt_chord.is_some_and(|chord| matcher.swallows_repeat(&chord, ptt_swallow, ptt_state));
        let toggle_repeat = toggle_chord
            .is_some_and(|chord| matcher.swallows_repeat(&chord, toggle_swallow, toggle_state));
        // …and the blocked half of the same promise (a modifier-first
        // release): `step` reports nothing while `blocked`, yet the final
        // member's repeat downs and its eventual key-up owe the focused
        // application a swallow — its press was swallowed at engagement,
        // so anything else leaves an unbalanced key-down there. Likewise
        // asked before `step`, which clears `blocked` on that key-up.
        let ptt_blocked = ptt_chord
            .is_some_and(|chord| matcher.swallows_while_blocked(&chord, ptt_swallow, ptt_state));
        let toggle_blocked = toggle_chord.is_some_and(|chord| {
            matcher.swallows_while_blocked(&chord, toggle_swallow, toggle_state)
        });
        let ptt_outcome = ptt_chord.and_then(|chord| matcher.step(&chord, ptt_swallow, ptt_state));
        let toggle_outcome =
            toggle_chord.and_then(|chord| matcher.step(&chord, toggle_swallow, toggle_state));
        // A repeat or a blocked member consumes nothing new — they only
        // join the swallow, already gated on the binding's own flag.
        swallowed |= ptt_repeat | ptt_blocked | toggle_repeat | toggle_blocked;
        // The disjoint borrows end with their last use above.
        if let Some(Outcome::Engaged { swallow }) = ptt_outcome {
            emit(&mut shared, InputEvent::BindingDown);
            swallowed |= swallow;
        }
        if let Some(Outcome::Released { swallow }) = ptt_outcome {
            emit(&mut shared, InputEvent::BindingUp);
            swallowed |= swallow;
        }
        // A toggle works on the press edge only — its release emits
        // nothing, exactly like the single-input toggle above.
        if let Some(Outcome::Engaged { swallow }) = toggle_outcome {
            emit(&mut shared, InputEvent::ToggleDown);
            swallowed |= swallow;
        }
        if let Some(Outcome::Released { swallow }) = toggle_outcome {
            swallowed |= swallow;
        }
    }

    if swallowed {
        return LRESULT(1);
    }
    // Every decision above already happened under the guard; see `call_next`
    // for why the mutex must be gone before this call.
    drop(shared);
    unsafe { call_next(code, wparam, lparam) }
}

/// `WH_MOUSE_LL` measuring wrapper (spec §4 D1) — the mouse twin of
/// [`keyboard_proc`]: record a beat, time the inner work, report a stall.
/// Log-only.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    diagnostics::note_mouse_cb();
    diagnostics::note_hook_phase(diagnostics::HookPhase::CallbackEntered);
    if code < 0 {
        return unsafe { mouse_proc_inner(code, wparam, lparam) };
    }
    // SAFETY: for `code >= 0` Windows hands a valid MSLLHOOKSTRUCT in
    // `lparam`; every installation of this hook relies on that.
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let wait_ms = unsafe { GetTickCount() }.wrapping_sub(info.time);
    let started = std::time::Instant::now();
    let result = unsafe { mouse_proc_inner(code, wparam, lparam) };
    let exec_ms = started.elapsed().as_millis() as u64;
    if let Some(message) = diagnostics::maybe_stall_warning("mouse", wait_ms, exec_ms) {
        tracing::warn!("{message}");
    }
    result
}

/// `WH_MOUSE_LL` callback (plan §7), including the side buttons (plan §1).
unsafe extern "system" fn mouse_proc_inner(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    log_once(
        &MOUSE_FIRST_CALL,
        "mouse hook callback alive (first mouse event)",
    );
    if code < 0 {
        return unsafe { call_next(code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    if info.flags & LLMHF_INJECTED != 0 {
        return unsafe { call_next(code, wparam, lparam) };
    }

    let message = wparam.0 as u32;
    let pressed = matches!(
        message,
        WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN | WM_XBUTTONDOWN
    );
    let released = matches!(
        message,
        WM_LBUTTONUP | WM_MBUTTONUP | WM_RBUTTONUP | WM_XBUTTONUP
    );
    if !pressed && !released {
        return unsafe { call_next(code, wparam, lparam) };
    }

    // For the X buttons the high word of mouseData holds which one it was.
    let button = match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP => MouseButton::Left,
        WM_RBUTTONDOWN | WM_RBUTTONUP => MouseButton::Right,
        WM_MBUTTONDOWN | WM_MBUTTONUP => MouseButton::Middle,
        WM_XBUTTONDOWN | WM_XBUTTONUP => match (info.mouseData >> 16) as u16 {
            XBUTTON1 => MouseButton::X1,
            XBUTTON2 => MouseButton::X2,
            _ => return unsafe { call_next(code, wparam, lparam) },
        },
        _ => return unsafe { call_next(code, wparam, lparam) },
    };

    diagnostics::note_hook_phase(diagnostics::HookPhase::AcquiringShared);
    let mut shared = shared();
    diagnostics::note_hook_phase(diagnostics::HookPhase::HoldingShared);

    // The matcher learns every mouse event too — a chord whose final member
    // is a button completes here (spec §6).
    shared.matcher.note_mouse(button, pressed);

    // Press-and-hold capture (spec §7): a button *press* completes the
    // sequence, with the accumulated modifiers or alone as a plain mouse
    // binding. `swallows()` is asked before `note_mouse` — its ordering
    // contract — and the event is forwarded only when no capture was armed.
    let capturing = shared.capture_accumulator.swallows();
    let answer = if pressed {
        // `note_mouse` documents itself as a press; a release during
        // capture is swallowed below but never completes the sequence.
        shared.capture_accumulator.note_mouse(button)
    } else {
        None
    };
    if capturing {
        if let Some(answer) = answer {
            if let Some(tx) = shared.capture_tx.take() {
                // Compare and a channel send only — see the keyboard path
                // for why there is no log line under the guard.
                let _ = tx.send(answer_binding(answer));
            }
            return LRESULT(1);
        }
        // Mid-sequence: keep waiting — and keep swallowing.
        return LRESULT(1);
    }

    // Compare against both bindings under this one guard (plan §11), like
    // the keyboard hook: a button that is neither binding passes through.
    let ptt = shared
        .binding
        .is_some_and(|binding| binding.matches_mouse(button));
    let toggle = shared
        .toggle
        .is_some_and(|toggle| toggle.matches_mouse(button));
    // Only `Binding::Chord` ever reaches the matcher; `Key` and `Mouse`
    // keep the existing comparison completely untouched (spec §6).
    let ptt_chord = match shared.binding {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    let toggle_chord = match shared.toggle {
        Some(Binding::Chord(chord)) => Some(chord),
        _ => None,
    };
    if !ptt && !toggle && ptt_chord.is_none() && toggle_chord.is_none() {
        // Never hold `SHARED` across `CallNextHookEx` — see `call_next`.
        drop(shared);
        return unsafe { call_next(code, wparam, lparam) };
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

    // Chord bindings — the mouse twin of the keyboard path's block: a
    // chord whose final member is this button engages or releases here,
    // with the same completing-member-only swallow (spec §5). Outcomes
    // first, emissions second — see the keyboard path's comment. No
    // `swallows_repeat` here: a mouse button generates no auto-repeat, and
    // a second down of the bound button cannot arrive while the chord is
    // engaged (any release in between would already have ended it).
    // `swallows_while_blocked` *is* needed, though: a chord ending in this
    // button blocks when a modifier is released first, and the button's
    // release then owes the focused application a swallow — its press was
    // swallowed at engagement.
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
        // A toggle works on the press edge only — its release emits
        // nothing, exactly like the single-input toggle above.
        if let Some(Outcome::Engaged { swallow }) = toggle_outcome {
            emit(&mut shared, InputEvent::ToggleDown);
            swallowed |= swallow;
        }
        if let Some(Outcome::Released { swallow }) = toggle_outcome {
            swallowed |= swallow;
        }
    }

    if swallowed {
        return LRESULT(1);
    }
    // Every decision above already happened under the guard; see `call_next`
    // for why the mutex must be gone before this call.
    drop(shared);
    unsafe { call_next(code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the fallback compared the byte count `GetRawInputData`
    /// returns for a keyboard report against `size_of::<RAWINPUT>()`. A
    /// keyboard report is header + `RAWKEYBOARD`; `RAWINPUT` is the union
    /// sized for the bigger mouse report. The old guard could never pass.
    #[test]
    fn a_keyboard_report_is_smaller_than_the_whole_rawinput_union() {
        let data_offset = std::mem::offset_of!(RAWINPUT, data);
        let keyboard_report = data_offset + std::mem::size_of::<RAWKEYBOARD>();
        assert!(
            keyboard_report < std::mem::size_of::<RAWINPUT>(),
            "{keyboard_report} must be < {} — the old guard rejected every key",
            std::mem::size_of::<RAWINPUT>()
        );
        #[cfg(target_pointer_width = "64")]
        assert_eq!((keyboard_report, std::mem::size_of::<RAWINPUT>()), (40, 48));
    }

    #[test]
    fn a_raw_press_is_the_binding_and_a_release_is_nothing() {
        // Tab and Caps Lock exactly as the low-level hook reports them.
        assert_eq!(
            raw_binding(9, 15, false),
            Some(Binding::Key { vk: 9, scan: 15 })
        );
        assert_eq!(
            raw_binding(20, 58, false),
            Some(Binding::Key { vk: 20, scan: 58 })
        );
        assert_eq!(raw_binding(9, 15, true), None);
    }

    #[test]
    fn a_raw_release_becomes_an_up_event_even_though_it_is_no_binding() {
        // `RI_KEY_BREAK` in `RAWKEYBOARD.Flags` is the up/down distinction
        // this path now relies on: a release is never a binding, yet the
        // capture accumulator must still see its `Up` — that is what fires
        // `note_release_all`, the only way a modifier-only capture
        // (`Ctrl+Shift`) can ever complete over Raw Input.
        let press = RawKeyReport {
            binding: raw_binding(0x11, 0x1D, false),
            vk: 0x11,
            scan: 0x1D,
            extended: false,
        };
        assert_eq!(
            press.event(),
            KeyEvent::Down {
                vk: 0x11,
                scan: 0x1D,
                extended: false
            },
            "a press arrives as the Down the old press-only path noted"
        );

        let release = RawKeyReport {
            binding: raw_binding(0x11, 0x1D, true),
            vk: 0x11,
            scan: 0x1D,
            extended: true,
        };
        assert_eq!(
            release.event(),
            KeyEvent::Up {
                vk: 0x11,
                scan: 0x1D,
                extended: true
            },
            "a release carries its keys into the Up, binding or no binding"
        );
    }
}
