//! `WH_KEYBOARD_LL` / `WH_MOUSE_LL` implementation of [`InputSource`] (plan §7).
//!
//! One dedicated thread installs both hooks and pumps their message loop.
//! The callbacks do the bare minimum — compare the event against the bound
//! input, forward a press/release over a channel, return — because plan §7
//! forbids any audio/COM work inside them and plan §11 forbids looking at
//! anything but the bound input.

use super::{Binding, InputEvent, InputSource, MouseButton};
use crate::error::{Error, Result};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT,
    PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL, PBT_APMRESUMESUSPEND, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_POWERBROADCAST, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW, WS_OVERLAPPED, XBUTTON1, XBUTTON2,
};

/// `WM_WTS_SESSION_CHANGE` (winuser.h) — windows-rs does not define it.
const WM_WTS_SESSION_CHANGE: u32 = 0x02B1;

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
    capture_tx: Option<Sender<Binding>>,
    event_tx: Option<Sender<InputEvent>>,
    /// Debounce latch: exactly one `BindingDown` per physical press, so
    /// auto-repeat cannot chatter (plan §9 M2).
    key_down: bool,
    mouse_down: bool,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    binding: None,
    swallow: false,
    capture_tx: None,
    event_tx: None,
    key_down: false,
    mouse_down: false,
});

/// Thread id of the thread that owns the hooks, so [`HookInputSource::stop`]
/// can end its message loop with `WM_QUIT` (plan §7).
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

/// The hook pair currently installed, held as `isize` because `HHOOK`
/// wraps a raw pointer and cannot live in a `static`. Only the hook thread
/// touches these — the notify window procedure runs on that same thread.
static KEYBOARD_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

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
    fn start(&mut self, binding: Binding, swallow: bool, tx: Sender<InputEvent>) -> Result<()> {
        self.stop();
        {
            let mut shared = shared();
            shared.binding = Some(binding);
            shared.swallow = swallow;
            shared.event_tx = Some(tx);
            shared.capture_tx = None;
            shared.key_down = false;
            shared.mouse_down = false;
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

    fn set_binding(&mut self, binding: Binding, swallow: bool) {
        let mut shared = shared();
        shared.binding = Some(binding);
        shared.swallow = swallow;
        shared.key_down = false;
        shared.mouse_down = false;
    }

    fn capture_next(&mut self) -> Receiver<Binding> {
        let (tx, rx) = channel();
        shared().capture_tx = Some(tx);
        rx
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
    let _ = ready.send(Ok(()));

    // A hidden window on this thread is what receives resume/unlock
    // broadcasts; without it the hooks would silently stop working after a
    // sleep (plan §7's manual test).
    let window = unsafe { create_notify_window() }.ok();
    if let Some(hwnd) = window {
        // Not fatal: the session keeps running, only re-installation is lost.
        let _ = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) };
    }

    unsafe { message_loop() };

    if let Some(hwnd) = window {
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
    let Ok((keyboard, mouse)) = (unsafe { install_hooks() }) else {
        return;
    };
    drop_hooks();
    store_hooks(keyboard, mouse);

    // A key held across the transition must not leave a stuck latch; a
    // duplicate press this may cause is ignored while already talking.
    let mut shared = shared();
    shared.key_down = false;
    shared.mouse_down = false;
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

/// Notify-window procedure: re-install the hooks when Windows says the
/// machine resumed or the user came back from the lock screen (plan §7).
unsafe extern "system" fn notify_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
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

/// `WH_KEYBOARD_LL` callback (plan §7): compare, send, return.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // Never act on synthetic input (plan §7) — otherwise our own swallow
    // logic or an automation tool could bounce the mic.
    if info.flags.contains(LLKHF_INJECTED) {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    let message = wparam.0 as u32;
    let pressed = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    let released = matches!(message, WM_KEYUP | WM_SYSKEYUP);
    if !pressed && !released {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let vk = info.vkCode as u16;
    let scan = info.scanCode as u16;

    let mut shared = shared();

    // "Press a key to bind" mode: the input becomes the binding and never
    // reaches another window (plan §4).
    if let Some(tx) = shared.capture_tx.take() {
        let _ = tx.send(Binding::Key { vk, scan });
        return LRESULT(1);
    }

    let Some(binding) = shared.binding else {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    };
    if !binding.matches_key(vk) {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

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

    if shared.swallow {
        // Consumed: Caps Lock must not toggle, the app must not see it.
        return LRESULT(1);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// `WH_MOUSE_LL` callback (plan §7), including the side buttons (plan §1).
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    if info.flags & LLMHF_INJECTED != 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
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
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    // For the X buttons the high word of mouseData holds which one it was.
    let button = match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP => MouseButton::Left,
        WM_RBUTTONDOWN | WM_RBUTTONUP => MouseButton::Right,
        WM_MBUTTONDOWN | WM_MBUTTONUP => MouseButton::Middle,
        WM_XBUTTONDOWN | WM_XBUTTONUP => match (info.mouseData >> 16) as u16 {
            XBUTTON1 => MouseButton::X1,
            XBUTTON2 => MouseButton::X2,
            _ => return unsafe { CallNextHookEx(None, code, wparam, lparam) },
        },
        _ => return unsafe { CallNextHookEx(None, code, wparam, lparam) },
    };

    let mut shared = shared();

    if let Some(tx) = shared.capture_tx.take() {
        let _ = tx.send(Binding::Mouse(button));
        return LRESULT(1);
    }

    let Some(binding) = shared.binding else {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    };
    if !binding.matches_mouse(button) {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

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

    if shared.swallow {
        return LRESULT(1);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
