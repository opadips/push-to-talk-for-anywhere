# Push-to-Talk for Windows: Implementation Plan (v1)

Working title: `ptt-tool` (rename later; keep the name in one place).

## 0. Instructions for the agent

- Work **one milestone at a time**, in order. Do not start a milestone until the previous one meets its acceptance criteria.
- One commit (or a small series) per milestone, with a short imperative message (`feat: ...`, `fix: ...`).
- Before every commit run: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- **Stay inside the scope in section 1.** If something seems worth adding, write it in `docs/IDEAS.md` and continue.
- If a requirement is ambiguous or an API behaves differently than this plan assumes, stop and ask instead of guessing.
- Keep all Windows-specific code behind small traits (see section 4) so the logic can be unit-tested without a microphone or a keyboard.

## 1. Product goal and scope

**Goal:** a lightweight Windows tray app that keeps the system microphone **muted by default** and **unmutes it only while a user-chosen key or mouse button is held**. It works for any app that uses the mic (browser calls, Discord, games), regardless of which window has focus.

### In scope for v1
- Hold-to-talk with one rebindable keyboard key or mouse button (including side buttons XButton1/XButton2).
- Select the capture device (or follow the system default).
- Configurable release delay (default 200 ms) so word endings are not clipped.
- Option to swallow the bound key so it does not reach other apps (e.g. Caps Lock).
- Tray icon with clear states: **Disabled / Muted / Talking**.
- Optional sound cues on talk start and stop.
- Start with Windows, single instance, hide-to-tray.
- **Fail-safe mic recovery** (see section 6). This is a core requirement, not polish.
- Installer and a README.

### Out of scope for v1 (put in `docs/IDEAS.md`, do not build)
Toggle mode, key combinations with modifiers, per-app profiles, noise suppression, mute lock, on-screen overlay, Raw Input backend, macOS/Linux, any network feature or telemetry.

## 2. Tech stack

| Layer | Choice |
|---|---|
| Language | Rust (stable, 2021 edition or newer) |
| Windows APIs | `windows` crate (windows-rs): Core Audio (`IMMDeviceEnumerator`, `IAudioEndpointVolume`), `SetWindowsHookExW` low-level hooks |
| App shell | Tauri v2 (tray, settings window, autostart, single-instance plugins) |
| Frontend | Svelte or React + TypeScript + Vite (pick one, keep it small) |
| Config | `serde` + TOML in `%APPDATA%\ptt-tool\config.toml` |
| State/recovery file | JSON in `%LOCALAPPDATA%\ptt-tool\state.json` |
| Logging | `tracing` + `tracing-appender` (rolling file in `%LOCALAPPDATA%\ptt-tool\logs`) |
| Sounds | `rodio` with embedded short WAV files |
| Errors | `thiserror` in libraries, `anyhow` in the app |
| CI | GitHub Actions on `windows-latest` |
| Packaging | Tauri bundler (NSIS or MSI); code signing step left configurable |

## 3. Repository layout

```
ptt-tool/
├── Cargo.toml                  # workspace
├── crates/
│   ├── ptt-core/               # NO UI, NO Tauri. Pure logic + traits + Windows impls
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── state.rs        # pure state machine (section 5)
│   │       ├── config.rs       # config model, load/save, validation
│   │       ├── audio/
│   │       │   ├── mod.rs      # MicController trait
│   │       │   └── wasapi.rs   # Windows Core Audio implementation
│   │       ├── input/
│   │       │   ├── mod.rs      # InputSource trait, Binding type
│   │       │   └── hook.rs     # WH_KEYBOARD_LL / WH_MOUSE_LL implementation
│   │       ├── failsafe.rs     # recovery file, restore logic
│   │       └── engine.rs       # wires input -> state machine -> audio
│   └── ptt-cli/                # tiny binary for manual testing of the core
├── src-tauri/                  # Tauri app (tray, commands, autostart, single instance)
├── ui/                         # settings frontend
├── assets/sounds/              # talk_start.wav, talk_stop.wav
├── docs/
│   ├── IDEAS.md
│   └── MANUAL_TESTS.md
└── .github/workflows/ci.yml
```

`ptt-core` must compile and be testable without Tauri.

## 4. Key interfaces

```rust
// audio/mod.rs
pub struct DeviceInfo { pub id: String, pub name: String, pub is_default: bool }

pub trait MicController: Send {
    fn list_capture_devices(&self) -> Result<Vec<DeviceInfo>>;
    fn get_mute(&self) -> Result<bool>;
    fn set_mute(&self, muted: bool) -> Result<()>;
}

// input/mod.rs
pub enum Binding {
    Key { vk: u16, scan: u16 },
    Mouse(MouseButton),      // Left, Right, Middle, X1, X2
}

pub enum InputEvent { BindingDown, BindingUp }

pub trait InputSource {
    fn start(&mut self, binding: Binding, swallow: bool, tx: Sender<InputEvent>) -> Result<()>;
    fn set_binding(&mut self, binding: Binding, swallow: bool);
    fn capture_next(&mut self) -> Receiver<Binding>;   // for "press a key to bind"
    fn stop(&mut self);
}
```

## 5. State machine (`state.rs`)

Implement as a **pure function** so it is fully unit-testable with a fake clock.

```rust
pub enum State { Disabled, Muted, Talking, ReleasePending { deadline: Instant } }

pub enum Event { Enable, Disable, PttDown, PttUp, Tick(Instant), Shutdown }

pub enum Action { Mute, Unmute, ScheduleTick(Instant), PlayStartSound, PlayStopSound, None }

pub fn step(state: State, event: Event, cfg: &Config, now: Instant) -> (State, Vec<Action>);
```

Rules:
- `Muted` + `PttDown` → `Talking`, actions: `Unmute`, `PlayStartSound`.
- `Talking` + `PttUp` → `ReleasePending`, action: `ScheduleTick(now + release_delay)` (if delay is 0, go straight to `Muted` with `Mute`).
- `ReleasePending` + `PttDown` → `Talking` (cancel pending mute, no start sound again).
- `ReleasePending` + `Tick` past the deadline → `Muted`, actions: `Mute`, `PlayStopSound`.
- Ignore repeated `PttDown` while already `Talking` (the OS sends auto-repeat key events).
- `Disable` and `Shutdown` → restore the user's original mute state (handled by the engine via the fail-safe module).
- Add property-style tests: any sequence of events ends in a consistent state and never leaves the mic unmuted without a pending path back to `Muted`.

## 6. Fail-safe requirements (critical)

The worst bug this app can have is leaving the user's microphone stuck muted (or stuck open). Implement all of the following:

1. **Record original state:** at engine start, read the current mute state and device ID and write `state.json` with `{ device_id, original_muted, dirty: true, pid }`.
2. **Clean exit:** on tray Quit, `Disable`, panic, `Ctrl+C`, and session end (`WM_ENDSESSION` / Tauri exit event), restore the original mute state, then set `dirty: false`.
3. **Panic hook:** install `std::panic::set_hook` that attempts restore before the process dies.
4. **Crash recovery on next launch:** if `state.json` has `dirty: true` and the recorded `pid` is not running, restore the recorded `original_muted` on startup and log it.
5. **Optional watchdog (milestone 6):** a tiny helper process that waits on the parent process handle and restores the mic if the parent is killed.
6. **Setting:** "On exit: restore previous state / always unmute". Default: restore previous state.

## 7. Windows implementation notes

### Core Audio (`audio/wasapi.rs`)
- Call `CoInitializeEx` (multithreaded) on every thread that touches COM.
- Default device: `IMMDeviceEnumerator::GetDefaultAudioEndpoint(eCapture, eConsole)`.
- Mute: activate `IAudioEndpointVolume` on the endpoint, then `SetMute(bool, null)` / `GetMute()`.
- Store the **endpoint ID string** (`IMMDevice::GetId`) in config, never a list index. Support the special value `"default"` meaning "follow the system default".
- Friendly names: `OpenPropertyStore` → `PKEY_Device_FriendlyName`.
- Re-resolve the endpoint if an operation fails (device unplugged or default changed). Optionally register an `IMMNotificationClient` to react to default-device changes.
- If the device does not support endpoint mute, log a clear error and show it in the UI. Do not silently fall back to changing volume in v1.

### Input hooks (`input/hook.rs`)
- Run on a **dedicated thread** with its own message loop (`GetMessageW`). Install `WH_KEYBOARD_LL` and `WH_MOUSE_LL` there. Unhook on stop.
- The hook callback must be a plain `extern "system" fn`, so use a `static`/`OnceLock` to reach the channel sender and the current binding.
- **Keep the callback tiny.** Compare against the binding, send an event over the channel, return. Do not touch COM or do audio work inside the callback. Windows silently removes hooks that respond too slowly.
- Ignore injected events (`LLKHF_INJECTED` / `LLMHF_INJECTED`) to avoid feedback loops with other tools.
- Swallowing: return `1` for both down and up of the bound input when `swallow` is true. Otherwise call `CallNextHookEx`.
- Mouse side buttons: `WM_XBUTTONDOWN` / `WM_XBUTTONUP`, button identified by the high word of `mouseData`.
- Debounce auto-repeat: only emit `BindingDown` on the first down until an up is seen.
- Re-install hooks after session unlock / resume from sleep if hook health checks fail.
- "Press a key to bind" mode: the hook thread emits the next input as a `Binding` and does not forward it.

### Known limitations to document in the README
- A non-elevated process cannot see input while an **elevated (admin)** window has focus (UIPI). Document this and add an option later to run elevated at login.
- Some anti-cheat systems are sensitive to global hooks. Test with the user's games and document results. Keep `InputSource` swappable so a Raw Input backend can be added later.
- Some audio drivers or apps reset the mute state on their own. Not handled in v1 (mute lock is a later feature).

## 8. Config model (`config.toml`)

```toml
version = 1
enabled = true

[binding]
kind = "key"            # "key" | "mouse"
vk = 0x14               # Caps Lock (example)
scan = 0x3A
mouse_button = ""       # "x1" | "x2" | "middle" when kind = "mouse"
swallow = true

[audio]
device_id = "default"
release_delay_ms = 200
on_exit = "restore"     # "restore" | "unmute"

[sounds]
enabled = true
volume = 0.5

[app]
start_with_windows = false
start_hidden = true
```

- Validate on load (release delay 0–2000 ms, volume 0–1). Fall back to defaults and log on a corrupt file; never crash.
- Keep a `version` field and a migration function from day one.
- Writes must be atomic (write temp file, then rename).

## 9. Milestones

### M0: Scaffold and CI
- Cargo workspace, crates, Tauri app skeleton that opens and quits, `ui/` with a placeholder page.
- GitHub Actions: fmt, clippy, test, and `cargo build` on `windows-latest`.
- **Done when:** CI is green on an empty-but-compiling project.

### M1: Audio control
- Implement `MicController` over Core Audio. `ptt-cli` commands: `devices`, `mute`, `unmute`, `status` (optional `--device <id>`).
- **Done when:** the CLI visibly toggles the mic's mute state in Windows Sound settings, and device listing shows names and the default flag.

### M2: Input hook and state machine
- Implement `state.rs` with unit tests (all transitions from section 5, fake clock).
- Implement the hook `InputSource`. `ptt-cli ptt --key <vk>` holds the mic muted and unmutes it only while the key is held.
- **Done when:** hold-to-talk works with another window focused; the release delay works; auto-repeat does not cause chatter; the swallow option stops Caps Lock from toggling.

### M3: Config, fail-safe, robustness
- Config load/save/validation, `state.json` recovery, panic hook, clean-exit restore, logging, single instance.
- Engine ties input → state machine → audio, with the audio call on a separate worker thread from the hook thread.
- **Done when:** killing the process mid-talk and relaunching restores the original mute state; corrupt config falls back safely; all steps in `docs/MANUAL_TESTS.md` pass.

### M4: Tauri tray and settings UI
- Tray icon with three distinct state icons and a menu (Enable/Disable, Open settings, Quit).
- Settings window: bind key/button ("press any key"), device dropdown, release delay slider, swallow toggle, sound toggle and volume, start-with-Windows, on-exit behavior. Closing the window hides to tray.
- Tauri commands expose engine state to the UI; the UI shows live state (Muted / Talking) and any error from the engine (e.g. device unavailable).
- Tauri `autostart` and `single-instance` plugins.
- **Done when:** the app can be fully used without the CLI; settings persist across restarts; the second launch focuses the existing instance.

### M5: Polish and release
- Embedded start/stop sounds, UI contrast at WCAG AA, keyboard-accessible settings, clear error states.
- NSIS/MSI installer, version stamping, release workflow producing artifacts on tag, README with screenshots, limitations section, privacy statement (no network, no telemetry).
- **Done when:** a clean Windows 10/11 VM can install, configure, use, and uninstall the app; uninstall leaves the mic in a sane state.

### M6 (stretch, only after v1 ships)
Watchdog helper process, toggle mode, mute lock via `IAudioEndpointVolumeCallback`, Raw Input backend, overlay.

## 10. Testing

**Automated**
- Unit tests for `state.rs` (every transition, release-delay edge cases, repeated down events, disable during talking).
- Config tests (defaults, validation, migration, corrupt file).
- Engine tests using a fake `MicController` and a fake `InputSource`: assert the exact sequence of `set_mute` calls.

**Manual (write the checklist in `docs/MANUAL_TESTS.md`)**
- Works while a fullscreen game, a browser call, and Discord are focused.
- Device unplugged or default device changed while running.
- Sleep/resume and lock/unlock keep the hotkey working.
- Task Manager kill while talking, then relaunch: mic restored.
- Rapid taps, holding for minutes, two quick presses within the release delay.
- Bound key swallowed vs not swallowed.
- Mouse side buttons.
- Elevated window focused (confirm documented limitation).
- Start with Windows, hidden start, single instance.

## 11. Security, privacy, and trust

- No network access anywhere in the app. State this in the README.
- Hooks only compare against the one bound input and never log or store other keystrokes. Add a test or code comment that makes this explicit, since global hooks look suspicious to users and antivirus.
- Code-sign release builds when a certificate is available. Expect some antivirus false positives from hook usage and note this in the README.
- Choose a license (MIT or Apache-2.0) before the first public release.

## 12. Definition of done for v1

1. All M0–M5 acceptance criteria met.
2. Microphone is never left in the wrong state after exit, crash, or kill (verified by the manual tests).
3. Idle resource use is negligible (target: under 1% CPU and under ~50 MB RAM with the settings window closed; measure and record).
4. CI green; installer builds on tag.
5. README documents limitations (elevated windows, anti-cheat, drivers that reset mute).
