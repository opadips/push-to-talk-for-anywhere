# Manual tests (Windows)

Run on a clean Windows 10/11 machine before every release. Tick nothing that
you did not actually observe.

## Environment

- OS version / build:
- App version / commit:
- Audio device:
- Date / tester:

## Running the checks

Two ways to drive it: `ptt.exe` in a terminal for the low-level checks, and
the `ptt-tool` tray app (M4) for the product itself. On Windows, build both
(`ui/dist` is generated, so the settings window must exist before Cargo runs):

```text
npm ci --prefix ui              # first time only
npm run build --prefix ui       # repeat after editing the settings window
cargo build --release           # target\release\ptt.exe and ptt-tool.exe
```

```text
target\release\ptt-tool.exe            # the tray app (M4)

ptt.exe devices                         # list mics (M1)
ptt.exe status                          # 'muted' / 'unmuted' (M1)
ptt.exe mute            /  ptt.exe unmute

ptt.exe ptt                             # no flags: everything from config.toml (M3)
ptt.exe ptt --key 0x14                  # hold-to-talk on Caps Lock (M2)
ptt.exe ptt --key 0x14 --no-swallow     # let the key reach other windows
ptt.exe ptt --key 0x14 --release-delay 0
ptt.exe ptt --mouse x1                  # bind a side button instead
ptt.exe ptt --key 0x41 --device {id}    # bind a key, pick the mic
```

The `ptt` session prints its binding, mutes the mic, and unmutes it while the
bound input is held. Press **Enter** to quit (or **Ctrl+C**); either way it
restores the mute state it found at start-up.

The tray app (M4) owns the microphone the same way: it starts muted, the icon
shows Disabled / Muted / Talking, **left click** opens the settings window,
**right click** opens the menu (Enable/Disable, Open settings, Quit), and
**closing the window only hides it**. Quit from the menu always hands the
microphone back. Only one hold-to-talk session can exist at a time — if
`ptt.exe ptt` is already running, the app says so in its window instead of
fighting it, and the same works the other way round.

The session also prints the two files it owns (M3):

- config: `%APPDATA%\ptt-tool\config.toml`
- log: `%LOCALAPPDATA%\ptt-tool\logs\ptt.log`

Start/stop sound cues do not exist until M5, so the sound parts of the
checklist cannot be observed yet.

## Checklist

### Core hold-to-talk (M2)
- [ ] Mic stays muted until the bound key is held; unmutes while held.
- [ ] Works while a **fullscreen game** has focus.
- [ ] Works while a **browser call** (Meet/Teams in Chrome/Edge) has focus.
- [ ] Works while **Discord** has focus.
- [ ] Release delay: word endings are not clipped at default 200 ms.
- [ ] Rapid taps do not chatter; auto-repeat does not re-trigger start sound.
- [ ] Two quick presses inside the release delay behave correctly.
- [ ] Bound key with `swallow = true` does not reach other apps (Caps Lock does not toggle).
- [ ] Bound key with `swallow = false` still reaches other apps.
- [ ] Mouse side buttons (X1/X2) work as bindings.

### Audio devices (M1)
- [ ] Device list shows friendly names and the default flag.
- [ ] Selecting a specific device mutes/unmutes that device.
- [ ] `device_id = "default"` follows a change of system default device.
- [ ] Unplugging the active device while running: clear error in UI, no crash.

### Fail-safe (M3) — critical
- [ ] Task Manager kill while talking, then relaunch: mic restored to original state.
- [ ] Quit with Enter **and** with Ctrl+C: mic restored to original state.
- [ ] Panic (forced): mic restored.
- [ ] Corrupt `config.toml`: falls back to defaults, logs, no crash, and the
      broken file is kept as `config.toml.bad`.
- [ ] Two instances: the second launch refuses to start with
      "ptt is already running".
- [ ] Flags persist: run `ptt.exe ptt --release-delay 500`, quit, then run
      `ptt.exe ptt` plain — the log line reports a 500 ms release delay.
- [ ] Log file exists at `%LOCALAPPDATA%\ptt-tool\logs\ptt.log` and records
      the session start.
- [ ] `state.json` in `%LOCALAPPDATA%\ptt-tool\` ends every test with
      `"dirty": false`.

### Sleep / lock (M2-M3)
- [ ] Sleep + resume: hotkey still works.
- [ ] Lock + unlock: hotkey still works.
- [ ] After both: hold-to-talk still unmutes while held (the hooks were
      re-installed, plan §7).

### Tray & settings (M4)
Launch `target\release\ptt-tool.exe` (start with no `ptt.exe` session running).

- [ ] App starts with the mic **muted**; the tray icon reads Muted, the window
      is hidden (or shown once if `start_hidden = false`).
- [ ] Tray icon shows three visually distinct states: Disabled / Muted /
      Talking (hold the bound key to see Talking).
- [ ] Tray tooltip reads "Push-to-Talk".
- [ ] Tray menu: **Enable / Disable** switches the hotkey off/on — while
      Disabled the bound key does nothing and the icon dims.
- [ ] Left click opens the settings window; right click opens the menu.
- [ ] Settings window: Change → "Press any key or button" captures a
      keyboard key **and** a mouse button; the label updates (e.g. "Caps
      Lock", "Mouse button 4 (back)").
- [ ] A bare modifier (Ctrl, Shift, Alt, Win alone) is not accepted as a
      binding — capture keeps waiting until a real key/button arrives.
- [ ] Settings window: **On-screen keyboard** opens a keyboard; clicking a key
      closes it and the label updates (e.g. "A", "F9"); Save, then hold that
      key — the mic unmutes. The currently bound key is highlighted green.
- [ ] On-screen keyboard: the Win keys are greyed out and do nothing when
      clicked; Esc, ✕ or a click outside closes it without changing the
      binding; Tab stays inside the dialog.
- [ ] On-screen keyboard: pick Left Ctrl / Left Shift / Left Alt (and the right
      ones), Save, hold it — the mic unmutes. With "Also block the key" on, a
      yellow warning appears; with it off, the key keeps working elsewhere.
      (Right Alt may act as AltGr on some layouts.)
- [ ] On-screen keyboard, Numpad tab: Numpad 0–9 bind with Num Lock **on**
      (label "Numpad 5"); punctuation keys show as "OEM n (…)" and bind the key
      that is physically there on a non-US layout too.
- [ ] Device dropdown lists the mics (plus "System default") and the
      selection survives Save + restart.
- [ ] Release delay slider (0–2000 ms): hold the key, release — the mic stays
      unmuted for exactly the set delay.
- [ ] Swallow off → the bound key still reaches other applications while
      held; swallow on → it does not.
- [ ] Sounds toggle + volume are persisted.
- [ ] Sound cues: pressing the bound key plays `talk_start.wav` (your
      "pushing" recording) as the mic opens; releasing it plays
      `talk_stop.wav` ("leaving") when the mic closes — i.e. after the
      release delay. A quick tap plays start, then stop. No second start cue
      when you press again within the release delay.
- [ ] Sound volume: move the slider, Save (no restart needed) — the next cue
      is louder/quieter; 0% is silent. Untick "Play the talk / release
      sounds" + Save — no cue plays. The sounds come out of the default
      playback device and do not affect the app's mic muting.
- [ ] Start with Windows adds/removes `ptt-tool` under
      `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
- [ ] `start_hidden = true` → relaunch shows **no window**, only the tray icon.
- [ ] On-exit choice (hand the microphone back / leave it unmuted) persists
      and is honoured on Quit.
- [ ] Closing the settings window **hides** it to the tray; the session keeps
      running (mic still mute-unmute on the hotkey).
- [ ] Second launch of `ptt-tool.exe` exits immediately and focuses the first
      instance's window — no second tray icon.
- [ ] Only one hold-to-talk owner: with `ptt.exe ptt` running, the app window
      reports the error instead of fighting it (and vice versa).
- [ ] Engine error surface: unplug the active mic while enabled → the window
      shows the error banner and the icon dims; plug back in, press **Save**
      → session recovers.
- [ ] Quit from the tray menu: `state.json` ends with `"dirty": false`.
- [ ] Full acceptance: on a fresh machine the app is usable **without the
      CLI** — launch → Change key → pick mic → Save → talk.

### Polish & release (M5)
- [ ] Sound cues play on talk start/stop; volume setting respected; mute setting silences them.
- [ ] Settings UI is fully keyboard accessible; contrast passes WCAG AA.
- [ ] Clean VM: install → configure → use → uninstall.
- [ ] After uninstall, the mic is in a sane state (not stuck muted).

### On-screen overlay (badge) — M-1…M-7

Turn it on first: settings → **Overlay** → tick the box → **Save**. The
overlay is opt-in; with `[overlay] enabled = false` (the default) no window
exists at all.

- [ ] **M-1 Appearance** — only a small glowing dot is visible: no white
      box, no rectangular shadow, no frame, no taskbar or Alt-Tab entry.
      Talking shows a bright red pulsing dot; "Always" mode shows a dim
      grey dot while muted.
- [ ] **M-2 talk-only** (default): the dot appears on key-down and fades
      out after release — and the badge stays bright for the whole
      **release delay**, disappearing only when the mic actually mutes.
      Afterwards nothing is left on screen. While Disabled, the badge
      never shows (either mode).
- [ ] **M-3 Click-through** — park the badge over something clickable
      underneath (e.g. a button at the edge of a window): the click
      reaches the window below; the badge never swallows it.
- [ ] **M-4 Focus** — in talk-only mode, press and release the hotkey
      repeatedly **while typing in another app or a game**: focus must
      never jump to the badge — typing continues, the game keeps
      receiving keys. (The known-risk check; record what you observe.)
- [ ] **M-5 Positions** — all six positions render at the chosen corner /
      edge, the centre variants are horizontally centred, and the
      distance slider (0–200 px) moves the badge as set.
- [ ] **M-6 Live apply** — change position / mode / distance and **Save**:
      the badge updates immediately, no restart. Untick the overlay and
      Save: the badge vanishes immediately; re-tick + Save: it returns.
- [ ] **M-7 Game smoke test** — with a fullscreen / borderless game
      focused, use the hotkey with the overlay on: the badge tracks
      talking, the game never loses input or focus, and performance is
      unchanged in practice.

### Toggle key (press once to talk) — TG-1…TG-9

Set it up in settings → **Hotkey**: press **Change**, then press a key or
mouse button (the same capture as the PTT binding, or pick the key on the
on-screen keyboard — see OSK-1); **Clear** removes it. An unbound toggle
reads **"Not set"** — the default. TG-2 onwards assume a toggle key is
bound and Saved.

- [ ] **TG-1 Fresh config** — on a fresh `config.toml` the toggle row
      reads "Not set" (nothing bound), and the app behaves exactly as
      before: only the PTT key opens the mic.
- [ ] **TG-2 Flip open / closed** — pick a toggle key, **Save**, press it
      once: the mic opens with the start cue and the app shows Talking
      (tray icon, settings chip, overlay dot). Press it again: after the
      release delay the mic mutes, with the stop cue.
- [ ] **TG-3 Live apply** — change the toggle binding and **Save**: it
      applies without restarting — hold the PTT key right after saving and
      there is no dead window (both keys respond immediately).
- [ ] **TG-4 PTT inert while latched** — while the toggle has the mic
      open, holding and releasing the PTT key does nothing: the mic stays
      open, no chatter, no extra start/stop cues.
- [ ] **TG-5 Disable / Enable** — while latched, tray **Disable** → state
      Disabled (the latch is cleared). Disabling mid-session does not by
      itself mute the microphone: the mic is handed back when the session
      stops (Quit), exactly as before this feature — do not flag that as a
      failure. Press the toggle key to re-enable — the app goes straight
      back into the latch (mic open, Talking), and the tray menu item and
      the settings button must then read **Disable** (the app is on).
- [ ] **TG-6 Clear** — **Clear** the toggle key, **Save**: the row reads
      "Not set" again and the key is inert (pressing it does nothing).
- [ ] **TG-7 Swallow** — "Also block the toggle key from other
      applications" is per-key: with it on, the toggle key does nothing in
      other apps (Caps Lock does not toggle); with it off, the key passes
      through to them.
- [ ] **TG-8 Auto-repeat** — hold the toggle key down: auto-repeat flips
      the mic exactly once (hook latch + 100 ms engine debounce), and
      releasing it does nothing — no flip on release.
- [ ] **TG-9 Sounds off** — untick "Play the talk / release sounds" +
      Save: toggling is silent but still flips the mic open/closed.

### On-screen keyboard for the toggle key — OSK-1…OSK-2

- [ ] **OSK-1 Pick for toggle** — settings → Hotkey: **On-screen keyboard**
      on the *Toggle key* row opens the dialog with the **toggle** key
      highlighted (not the PTT key); pick a key → only the toggle row
      changes; **Save** → that key toggles talk.
- [ ] **OSK-2 Rows stay independent** — the dialog opened from the *Hotkey*
      row highlights the **PTT** key and picking changes only that row; a
      modifier (Left Shift) picked for the toggle shows the modifier
      warning; Win keys are greyed on both rows; nothing persists before
      **Save**.

### Input-stall diagnostics — DIAG-1…DIAG-6

- [ ] **DIAG-1 Startup line** — after launch the log (%LOCALAPPDATA%\ptt-tool\logs\ptt.log) contains "Windows LowLevelHooksTimeout = …" (a value or "not set").
- [ ] **DIAG-2 Quiet in normal use** — ordinary typing/mouse use (non-gaming) produces no new WARN lines; if any appear, they are findings — send the log lines verbatim.
- [ ] **DIAG-3 Incident capture** — if input stalls again: note the exact time, keep the app running, then send the log tail (WARN lines) and check Windows Event Viewer → Windows Logs → Application for "Application Error" (Event 1000) around that time.
- [ ] **DIAG-4 Panic visibility** — a forced panic (see M3) also writes a "panic at …" line into ptt.log.
- [ ] **DIAG-5 Reading a stall WARN** — a lone `HookThreadStalled` line while input was actually flowing (or immediately after resume from sleep) is likely `WM_TIMER` starvation under load, not a real stall — correlate it with whether input really froze; `HooksSilent` only fires after the hooks have delivered at least one event. Send any WARN lines verbatim either way.
- [ ] **DIAG-6 Reading a stall** — each `HookThreadStalled` WARN is followed by two probe lines: `wait-state probe: …` (executing / blocked / starved, with the raw CPU and idle percentages) and `hook-thread phase: …` (which call the thread was standing in — our shared mutex or `CallNextHookEx`). For a stall send both logs, `ptt.log` and the durable `findings.log`, from launch onward, verbatim.

## Known limitations (confirm and record, plan §7)

- [ ] Elevated (admin) window focused: hotkey does **not** fire (UIPI) — as documented.
- [ ] Bound key / games with anti-cheat: record results per game.
- [ ] Drivers/apps that reset mute on their own: record which.
