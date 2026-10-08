# Push-to-Talk for Windows

A small tray app that keeps your microphone muted and only unmutes it while you
hold a key (or a mouse button). Works in whatever has focus — Discord, a browser
call, a fullscreen game. Mic goes back to muted when you let go.

Windows 10/11, x64. No account, no installer nonsense, no network access.

## Getting it

Download `ptt-tool-vX.Y.Z-windows-x64.zip` (or the plain `.exe`, it's the same
program) from the [Releases](https://github.com/opadips/push-to-talk-for-anywhere/releases)
page. Unzip, run `ptt-tool.exe`. That's it.

There's a `SHA256SUMS.txt` next to the files if you want to check them.

Heads up: the app installs a global keyboard hook, which some antivirus tools
and anti-cheat systems find suspicious. Nothing shady is going on, see Privacy
below, but expect the occasional false positive on an unsigned build.

## Using it

The app sits in the tray. Three icon states: **Disabled**, **Muted**,
**Talking**.

- Left click the icon → settings window
- Right click → menu (Enable/Disable, Open settings, Quit)
- Closing the window just hides it; the session keeps running

In settings:

- **Hotkey** — press *Change*, then press any key or mouse button. Or use the
  on-screen keyboard if you'd rather click. "Also block the key" stops the bound
  key from reaching other apps (handy for Caps Lock, a bad idea for Shift/Ctrl/Alt
  unless you mean it).
- **Microphone** — pick a device or follow the system default.
- **Release delay** — how long the mic stays open after you let go (default
  200 ms, so you don't clip the end of words). 0–2000 ms.
- **On exit** — hand the mic back the way you found it, or always leave it
  unmuted.
- **Sounds** — a short cue when you start and stop talking, with a volume
  slider.
- **App** — start with Windows, start hidden in the tray.

Only one hold-to-talk session at a time. If `ptt.exe ptt` is already running,
the tray app says so instead of fighting it.

## Command line (optional)

There's a `ptt.exe` if you prefer a terminal. Not required for normal use.

```text
ptt devices                       list mics
ptt status                        prints 'muted' or 'unmuted'
ptt mute  /  ptt unmute

ptt ptt                           hold-to-talk using config.toml
ptt ptt --key 0x14                Caps Lock
ptt ptt --mouse x1                a side button
ptt ptt --release-delay 0 --no-swallow --device <id>
```

Enter or Ctrl+C quits a session and restores the mute state it found at start-up.

## Files it keeps

| What | Where |
|---|---|
| Config | `%APPDATA%\ptt-tool\config.toml` |
| Log | `%LOCALAPPDATA%\ptt-tool\logs\ptt.log` |
| Recovery state | `%LOCALAPPDATA%\ptt-tool\state.json` |

A corrupt config falls back to defaults and gets renamed `config.toml.bad`
rather than taking the app down with it.

## Building it

You need Rust and Node.

```text
npm ci --prefix ui
npm run build --prefix ui
cargo build --release
```

`ui/dist` is generated, so build the frontend before Cargo — the settings window
lives in there. Output: `target\release\ptt-tool.exe` and `ptt.exe`.

Before pushing, the usual:

```text
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Manual test checklist is in [docs/MANUAL_TESTS.md](docs/MANUAL_TESTS.md).
Cutting a release: [docs/RELEASING.md](docs/RELEASING.md) (GitHub Actions does
the work).

## Limitations

- **Elevated windows.** A normal process can't see input while an admin window
  has focus, so the hotkey won't fire there. That's Windows (UIPI), not a bug.
- **Anti-cheat.** Global hooks upset some of them. Test your games and report
  what happens.
- **Mute resets.** A few drivers/apps stomp the mute state on their own. Not
  defended against yet.

## Fail-safe

The worst outcome would be leaving your mic stuck muted (or stuck open), so the
app records the original mute state on start and restores it on quit, disable,
panic, or Ctrl+C. If it gets killed, the next launch notices and fixes it.

## Privacy

No network access anywhere. No telemetry. The hook compares your input against
the one bound key/button and that's all — nothing else is captured, logged, or
stored.

## License

MIT OR Apache-2.0, as declared in `Cargo.toml`.
