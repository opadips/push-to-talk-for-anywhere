# Push-to-Talk for Anywhere

A small tray app that keeps your microphone muted and only unmutes it while you
hold a key (or a mouse button). Whatever you're in — a browser tab, a game, a
stream — the mic opens when you press and closes when you let go.

**Why you'd want it:**

- **Mic off by default.** No more hot-mic moments — the mic is muted until your
  finger is on the key, and back to muted the instant you're done.
- **One key works everywhere.** Same hold-to-talk in every app, even ones that
  have no push-to-talk setting at all.
- **Doesn't clip your words.** A short release delay keeps the last syllable
  from getting cut off (default 200 ms, adjustable).
- **Your key, your call.** Any keyboard key or mouse button, including side
  buttons. Optionally swallow it so it never reaches other apps.
- **You can hear it.** Optional start/stop sound cues with a volume slider, so
  you always know when you're live.
- **Leaves things as it found them.** Mute state is handed back on quit.

## What people use it for

- **Web-based meetings** — Google Meet, Teams and Zoom in a browser tab rarely
  give you a push-to-talk key. Now you have one, no extension needed.
- **Games without built-in push-to-talk** — plenty of co-op and multiplayer
  games only offer "mic always on". Hold a key instead of muting your system mic
  by hand.
- **Streamers on OBS** — cut the mic between takes, during coughs or keyboard
  pauses, without touching OBS. Works alongside your existing OBS mic settings.
- **Screen sharing** — kill the mic instantly while sharing your screen, without
  hunting for the right mute button in the right app.
- **Quiet rooms** — kids, pets, keyboard clack: mute the noise between
  sentences instead of staying muted the whole call.
- **Apps with no mute** — old VOIP clients and web apps that expose no mic
  control at all. The system mic is fair game, so anything can get push-to-talk.

## Getting it

Download `ptt-tool-vX.Y.Z-windows-x64.zip` (or the plain `.exe`, it's the same
program) from the [Releases](https://github.com/opadips/push-to-talk-for-anywhere/releases)
page. Unzip, run `ptt-tool.exe`. That's it.

There's a `SHA256SUMS.txt` next to the files if you want to check them.

If the window you're talking into runs with **higher privileges** (an elevated /
administrator app), run `ptt-tool.exe` as administrator too — otherwise Windows
blocks the hotkey and it won't respond while that window has focus.

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

## Privacy

No network access anywhere. No telemetry. The hook compares your input against
the one bound key/button and that's all — nothing else is captured, logged, or
stored.

## License

MIT. See [LICENSE](LICENSE).
