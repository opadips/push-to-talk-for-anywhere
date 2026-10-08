# Manual tests (Windows)

Run on a clean Windows 10/11 machine before every release. Tick nothing that
you did not actually observe.

## Environment

- OS version / build:
- App version / commit:
- Audio device:
- Date / tester:

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
- [ ] Quit from tray: mic restored to original state.
- [ ] Panic (forced): mic restored.
- [ ] Corrupt `config.toml`: falls back to defaults, logs, no crash.
- [ ] Two instances: second launch focuses/quits in favour of the first.

### Sleep / lock (M2-M3)
- [ ] Sleep + resume: hotkey still works.
- [ ] Lock + unlock: hotkey still works.

### Tray & settings (M4)
- [ ] Tray icon shows three distinct states: Disabled / Muted / Talking.
- [ ] Settings persist across restart.
- [ ] "Press any key" rebinding works for keyboard and mouse buttons.
- [ ] Closing the settings window hides to tray (does not quit).
- [ ] Start with Windows works; `start_hidden` starts hidden.
- [ ] Second launch focuses the existing instance.

### Polish & release (M5)
- [ ] Sound cues play on talk start/stop; volume setting respected; mute setting silences them.
- [ ] Settings UI is fully keyboard accessible; contrast passes WCAG AA.
- [ ] Clean VM: install → configure → use → uninstall.
- [ ] After uninstall, the mic is in a sane state (not stuck muted).

## Known limitations (confirm and record, plan §7)

- [ ] Elevated (admin) window focused: hotkey does **not** fire (UIPI) — as documented.
- [ ] Bound key / games with anti-cheat: record results per game.
- [ ] Drivers/apps that reset mute on their own: record which.
