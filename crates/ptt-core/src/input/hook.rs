//! `WH_KEYBOARD_LL` / `WH_MOUSE_LL` implementation of [`InputSource`] (plan §7).
//!
//! One dedicated thread installs both hooks and pumps their message loop.
//! The callbacks do the bare minimum — compare the event against the bound
//! input, forward a press/release over a channel, return — because plan §7
//! forbids any audio/COM work inside them and plan §11 forbids looking at
//! anything but the bound input.

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
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWKEYBOARD, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassW, SetTimer, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL, PBT_APMRESUMESUSPEND,
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
    /// auto-repeat cannot chatter (plan §9 M2).
    key_down: bool,
    mouse_down: bool,
    /// The same debounce for the toggle binding, one latch per hook.
    toggle_key_down: bool,
    toggle_mouse_down: bool,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
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
});

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
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        }

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
        let mut shared = shared();
        shared.binding = Some(binding);
        shared.swallow = swallow;
        shared.toggle = toggle.map(|(binding, _)| binding);
        shared.toggle_swallow = toggle.is_some_and(|(_, swallow)| swallow);
        shared.key_down = false;
        shared.mouse_down = false;
        shared.toggle_key_down = false;
        shared.toggle_mouse_down = false;
    }

    fn capture_next(&mut self) -> Receiver<Binding> {
        let (tx, rx) = channel();
        shared().capture_tx = Some(tx);
        // Also listen at the raw-input level: while the settings window owns
        // the keyboard focus, the low-level chain never reaches our hook
        // (bug 3), so the hook alone cannot see the press the user is asked
        // to make. Removed again the moment a press arrives.
        add_raw_keyboard();
        rx
    }

    fn cancel_capture(&mut self) {
        // Nothing may keep eating the next key press once nobody is waiting
        // for it (the window answered by itself, or it was closed).
        shared().capture_tx = None;
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
    let mut shared = shared();
    shared.key_down = false;
    shared.mouse_down = false;
    shared.toggle_key_down = false;
    shared.toggle_mouse_down = false;
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

/// The binding a raw keyboard report stands for, if it is a press at all
/// (bug 3's capture path; the pure decision is [`raw_binding`]).
unsafe fn raw_keyboard_binding(lparam: LPARAM) -> Option<Binding> {
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
    raw_binding(
        keyboard.VKey,
        keyboard.MakeCode,
        keyboard.Flags & RI_KEY_BREAK != 0,
    )
}

/// Pure decision part of [`raw_keyboard_binding`]: a key *press* becomes
/// the binding; a release never does (the hook's capture branch has the
/// same rule for the events it does see).
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
    // same way the low-level hook branch does, then stop listening again —
    // whichever reporter sees the press first wins, the other finds the
    // capture already spent.
    if message == WM_INPUT {
        if let Some(binding) = unsafe { raw_keyboard_binding(lparam) } {
            let taken = {
                let mut shared = shared();
                shared.capture_tx.take()
            };
            if let Some(tx) = taken {
                tracing::info!("captured {binding:?} (raw input)");
                let _ = tx.send(binding);
            }
            // The sink exists only for one capture; this press spent it.
            remove_raw_keyboard();
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
/// restored around the call because two of the sites run while holding the
/// shared mutex, and a stall later in the callback must not still read as
/// "inside CallNextHookEx".
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

    // The one wait this callback can do on *our own* state (spec §4): mark
    // the approach, so a stall here reads as contention on the mutex rather
    // than as the hook chain beyond us.
    diagnostics::note_hook_phase(diagnostics::HookPhase::AcquiringShared);
    let mut shared = shared();
    diagnostics::note_hook_phase(diagnostics::HookPhase::HoldingShared);

    // "Press a key to bind" mode: the input becomes the binding and never
    // reaches another window (plan §4). Only a *press* counts: the release
    // of the key that armed the capture (Enter/Space on the "Change" button)
    // would otherwise be taken for the answer.
    if pressed {
        if let Some(tx) = shared.capture_tx.take() {
            let _ = tx.send(Binding::Key { vk, scan });
            return LRESULT(1);
        }
    }

    // Compare the key against both bindings under this one guard (plan §11:
    // only the bound inputs are ever looked at). The press passes through
    // only when it matches *neither* — the toggle must get its chance even
    // when the key is not the PTT binding.
    let ptt = shared
        .binding
        .is_some_and(|binding| binding.matches_key(vk));
    let toggle = shared.toggle.is_some_and(|toggle| toggle.matches_key(vk));
    if !ptt && !toggle {
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

    if swallowed {
        return LRESULT(1);
    }
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

    if let Some(tx) = shared.capture_tx.take() {
        let _ = tx.send(Binding::Mouse(button));
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
    if !ptt && !toggle {
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

    if swallowed {
        return LRESULT(1);
    }
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
}
