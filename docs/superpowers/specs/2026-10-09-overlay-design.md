# On-screen overlay (talk-state badge) — design

Date: 2026-10-09 · Status: approved in chat, awaiting spec review
Plan deviation, recorded: the overlay is M6 stretch in IMPLEMENTATION_PLAN.md
§9; the partner chose to pull it forward. This spec supersedes the
"On-screen overlay" entry in docs/IDEAS.md.

## Purpose

While gaming or in a fullscreen call the tray icon is invisible, so there is
no way to tell whether the mic is open. The overlay is a small on-screen
badge that shows the talk state where the user can see it: a pulsing dot
while talking, nothing (or a dim dot) otherwise. It is a pure display —
clicks, keys and focus pass through to whatever is underneath, so a game
never loses input to it.

## Decisions (partner, 2026-10-09)

- Purpose: talk-state badge only (no key label, no device name, no meter).
- Visibility: configurable — talk-only or always-on (settings).
- Position: settings picker (corner/center + distance), not draggable.
- Click-through: always; no setting to disable it.
- Look: soft pulsing dot while talking; no text.
- Built as a second Tauri window (transparent WebView2), not a native
  Win32 layered window. Trade-off accepted: ~40–80 MB RAM while enabled —
  the overlay is opt-in (`enabled = false` by default), so the §12 idle
  budget is unchanged unless the partner turns it on.

## Behavior

The badge maps the engine's session state (already broadcast to every
window as the `status` event by the tray poller, `tray.rs`) like this:

| SessionState     | talk-only         | always            |
|------------------|-------------------|-------------------|
| Talking          | bright pulsing dot | bright pulsing dot |
| ReleasePending   | bright pulsing dot | bright pulsing dot |
| Muted            | window hidden     | dim static dot    |
| Disabled         | window hidden     | window hidden     |

- ReleasePending counts as "talking" visually: the microphone really is
  open until the release delay expires.
- `enabled = false` (the whole overlay off): no window exists at all.
- Engine errors do not change the badge; the settings window and tray
  tooltip remain the error surface.
- talk-only fades the dot out (CSS transition ≈300 ms) when the state
  leaves Talking/ReleasePending, then hides the window after the fade so
  nothing paints while muted.
- The overlay never takes focus and never appears in the taskbar or
  Alt-Tab.

## Configuration (`config.toml`, new section)

```toml
[overlay]
enabled = false                       # opt-in
mode = "talk-only"                    # "talk-only" | "always"
position = "bottom-right"             # top/bottom × left/center/right
distance = 24                         # pixels from the edges (clamped 0–200)
```

Rust side: new `OverlayConfig` in ptt-core's `Config`, serde defaults as
above; unknown `mode`/`position` values fall back to the defaults with a
warning (same posture as the existing config validation). `distance`
clamped to 0–200.

Settings UI: a new "Overlay" card — enable toggle, mode select, position
select (6 options), distance slider — saved through the existing
`save_settings` flow; no restart needed.

## The overlay window

- Second Tauri window, label `overlay`, created in code only while
  `overlay.enabled` and **destroyed** when it turns off or a settings save
  changes it (a hidden WebView2 would still cost RAM): transparent,
  frameless, no shadow, always-on-top, skip-taskbar, unfocusable,
  ignore-cursor-events (click-through), sized 64×64 px (the dot plus glow
  padding), positioned per config on the primary monitor (multi-monitor
  is explicitly out of scope, v2+).
- Page: dedicated `ui/overlay.html` (second Vite input) — one dot div,
  CSS pulse (talking) and fade (transitions). No framework needed; plain
  TS/JS reading its mode once via `invoke("get_settings")`, listening to
  the `status` event, plus one `invoke("get_status")` on load for the
  initial render. In talk-only mode it calls its own window
  `hide()`/`show()`.
- Focus: created with `focused(false)`; implementation must verify on
  Windows that talking never activates the window. Fallback if it does:
  raw `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` via `SetWindowLongPtrW`
  (precedent: raw Win32 already lives in ptt-core's hook.rs).
- Packaging: `overlay.html` ships inside the same embedded asset map;
  no release-workflow changes.

## Plumbing

- New `app::overlay::sync_overlay(app: &AppHandle)` (reads the current
  config from `AppState`) called from `save_settings` (and from setup when
  the app starts): creates, repositions, shows or destroys the window per
  the current config.
- State: nothing new — the poller's `emit("status")` already reaches
  every window; the overlay is a consumer only.
- The overlay window does not touch the session, the config mutex, or any
  command other than `get_settings` (its own mode, once on load) and
  `get_status` (initial state).

## Testing

Automated (ptt-core):
- Config tests: `[overlay]` defaults; serde round-trip; unknown
  mode/position falls back to defaults; distance clamped.

Manual (new "Overlay" section in docs/MANUAL_TESTS.md):
- Badge appears on key-down and disappears after release (talk-only).
- Always mode: dim while muted, bright while talking, hidden while
  disabled.
- Release delay keeps the badge bright until the mic actually mutes.
- All 6 positions render correctly with the distance slider respected.
- Click-through proof: clicking through the badge hits the window
  beneath it; typing focus is never stolen while talking.
- No taskbar entry, no Alt-Tab entry, never activated by the badge.
- Turning the overlay off in settings removes the window immediately.
- Real-game smoke test (fullscreen window focused).

## Non-goals (v2+, kept out on purpose)

Multi-monitor placement, voice-reactive/level ring, drag-to-move,
"LIVE" text or any label, per-application rules, overlay in the CLI,
native Win32 rendering.
