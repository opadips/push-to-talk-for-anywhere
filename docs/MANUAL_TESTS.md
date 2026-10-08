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
- [ ] Sounds toggle + volume are persisted (audio itself is checked in M5).
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

## Known limitations (confirm and record, plan §7)

- [ ] Elevated (admin) window focused: hotkey does **not** fire (UIPI) — as documented.
- [ ] Bound key / games with anti-cheat: record results per game.
- [ ] Drivers/apps that reset mute on their own: record which.
