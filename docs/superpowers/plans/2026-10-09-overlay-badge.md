# On-screen Overlay (Talk-State Badge) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An opt-in on-screen pulsing-dot badge that shows the talk state where a game or fullscreen call can't hide it — click-through, unfocusable, configured from the settings window.

**Architecture:** A second Tauri window (`overlay`, plain HTML/CSS/TS, no framework) is created only while `overlay.enabled` in `config.toml`. It listens to the `status` event the tray poller already broadcasts every 100 ms and maps `SessionState` to a CSS dot. Rust owns window lifecycle and position math; the page owns presentation and hide/show.

**Tech Stack:** Existing stack only — Rust (ptt-core config, Tauri v2 windows), Svelte 5 settings UI, Vite multi-page build. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-09-overlay-design.md`

## Global Constraints

- Config defaults exactly: `enabled = false`, `mode = "talk-only"`, `position = "bottom-right"`, `distance = 24`; `distance` clamped to 0–200; unknown `mode`/`position` fall back to defaults with a warning (never an error).
- Window: label `overlay`, 64×64 logical px, transparent, frameless, no shadow, always-on-top, skip-taskbar, unfocusable, `set_ignore_cursor_events(true)`; primary monitor only.
- Badge mapping: `Talking`/release-delay → bright pulsing dot; `Muted` → talk-only: window hidden, always: dim static dot; `Disabled` → hidden in both modes. (The UI enum has no ReleasePending variant — `UiStatus.state` is `SessionState::{Disabled, Muted, Talking}` and the release delay already maps to `Talking`.)
- talk-only fades out ≈300 ms, then calls `window.hide()`.
- Overlay window is **destroyed** (not hidden) when disabled or when settings change it — a hidden WebView2 would still cost RAM; §12's idle budget must be untouched by default.
- The overlay page reads its mode once via `invoke("get_settings")` on load; mode/position changes take effect because `sync_overlay` destroys and recreates the window (fresh page load). No live reconfiguration channel.
- The overlay uses no command except `get_status`/`get_settings`, touches no session state, and adds no network access (plan §11).
- Gate before every commit: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`, `cargo test`, `cargo build`, `npm run build --prefix ui`.

## Review Focus

Failure modes the spec implies but no single test proves — each owned by the task listed:

1. **Talking steals focus from the game** — expected: typing and mouse stay in the game while the badge pulses. Owned by Task 2 (create with `focused(false)`; if Windows still activates, apply `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` via `SetWindowLongPtrW`, `cfg(windows)`); pinned by manual check M-4.
2. **Badge swallows a click** — expected: a click over the badge reaches the window beneath. Owned by Task 2 (`set_ignore_cursor_events(true)`); pinned by manual check M-3.
3. **Transparent window renders a white box or rectangular shadow** — expected: only the dot is visible. Owned by Task 3 (transparent window + `html,body{background:transparent}` + dot-only paint); pinned by manual check M-1.
4. **Saving overlay settings does nothing until restart** — expected: Save immediately repositions/removes the badge. Owned by Task 2 (`sync_overlay` called from `save_settings` and `setup`); pinned by manual check M-6.
5. **Configs or payloads from before this feature break** — expected: `[overlay]` absent → defaults, no error. Owned by Task 1 (`#[serde(default)]` + round-trip test T1-D).
6. **talk-only leaves a visible window while muted** — expected: after the fade the window is hidden (no paint, no hit-test surface). Owned by Task 3 (hide-after-fade in `overlay.ts`); pinned by manual check M-2.

---

### Task 1: `OverlayConfig` in ptt-core

**Files:**
- Modify: `crates/ptt-core/src/config.rs` (structs + `Config` field + `validate`)
- Test: same file, `#[cfg(test)] mod tests` (existing pattern, lines ~392+)

**Interfaces:**
- Produces: `pub struct OverlayConfig { pub enabled: bool, pub mode: String, pub position: String, pub distance: u32 }` with `#[serde(default)]`; `Config.overlay: OverlayConfig`; `Default` = spec defaults. Later tasks read `config.overlay` (Rust) and `settings.overlay` (JSON via `get_settings`/`save_settings`).

- [ ] **Step 1: Write the failing tests** (in `config.rs` test module)

```rust
#[test]
fn overlay_defaults_match_the_spec() {
    let overlay = OverlayConfig::default();
    assert_eq!(overlay.enabled, false);
    assert_eq!(overlay.mode, "talk-only");
    assert_eq!(overlay.position, "bottom-right");
    assert_eq!(overlay.distance, 24);
}

#[test]
fn overlay_section_is_optional_and_round_trips() {
    // A config file written before the feature: no [overlay] at all.
    let legacy = "version = 1\nenabled = true\n";
    let parsed: Config = toml::from_str(legacy).expect("legacy config parses");
    assert_eq!(parsed.overlay, OverlayConfig::default());

    let overlay = OverlayConfig { enabled: true, mode: "always".into(), position: "top-left".into(), distance: 80 };
    let mut config = Config::default();
    config.overlay = overlay.clone();
    let text = toml::to_string(&config).unwrap();
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.overlay, overlay);
}

#[test]
fn overlay_validate_falls_back_and_clamps() {
    let mut config = Config::default();
    config.overlay = OverlayConfig { enabled: true, mode: "sideways".into(), position: "middle".into(), distance: 9999 };
    let warnings = config.validate();
    assert_eq!(config.overlay.mode, "talk-only");
    assert_eq!(config.overlay.position, "bottom-right");
    assert_eq!(config.overlay.distance, 200);
    assert_eq!(warnings.len(), 3);
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p ptt-core config::tests::overlay` — Expected: FAIL (no `OverlayConfig`).

- [ ] **Step 3: Implement**

`OverlayConfig` with `#[serde(default)]`, `Serialize/Deserialize/Clone/Debug/PartialEq/Eq`, `Default` per spec; add `#[serde(default)] pub overlay: OverlayConfig` to `Config` + its `Default`; extend `validate()` to normalize `mode` ∈ {`talk-only`,`always`}, `position` ∈ {`top-left`,`top-center`,`top-right`,`bottom-left`,`bottom-center`,`bottom-right`}, clamp `distance` 0..=200 — each with an `info!`-style warning message pushed to the returned `Vec<String>` (mirror the existing volume clamp).

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p ptt-core` — Expected: all pass (existing + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/ptt-core/src/config.rs
git commit -m "feat: overlay config section with safe defaults"
```

### Task 2: Overlay window lifecycle + position math (src-tauri)

**Files:**
- Create: `src-tauri/src/overlay.rs`
- Modify: `src-tauri/src/main.rs` (add `mod overlay;`)
- Modify: `src-tauri/src/commands.rs:39-49` (`save_settings`)
- Modify: `src-tauri/src/app.rs:368-389` (`setup`)
- Test: `src-tauri/src/overlay.rs`, `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `Config.overlay` (Task 1), `app.state::<AppState>().config`.
- Produces: `pub fn sync_overlay(app: &tauri::AppHandle)` — idempotent; destroys any `overlay` window, then recreates it iff `overlay.enabled`, positioned per config. Called by Task 2's own hooks in `save_settings`/`setup`; nothing else calls it.

- [ ] **Step 1: Write the failing position-math tests**

```rust
#[test]
fn positions_place_the_badge_on_the_requested_edge() {
    // 64x64 window, 1920x1080 screen, distance 24.
    assert_eq!(overlay_position("bottom-right", 24, 1920, 1080), (1832.0, 992.0));
    assert_eq!(overlay_position("bottom-left", 24, 1920, 1080), (24.0, 992.0));
    assert_eq!(overlay_position("top-left", 24, 1920, 1080), (24.0, 24.0));
    assert_eq!(overlay_position("top-right", 24, 1920, 1080), (1832.0, 24.0));
    assert_eq!(overlay_position("top-center", 24, 1920, 1080), (928.0, 24.0));
    assert_eq!(overlay_position("bottom-center", 24, 1920, 1080), (928.0, 992.0));
}
```

- [ ] **Step 2: Run, verify failure** — `cargo test -p ptt-tool overlay` — Expected: FAIL (missing fn).

- [ ] **Step 3: Implement `overlay.rs`**

- `fn overlay_position(position: &str, distance: u32, screen_w: u32, screen_h: u32) -> (f64, f64)` — pure; window size constant `const OVERLAY_SIZE: f64 = 64.0`; x/y computed from the edge and `distance as f64`; center variants use `(screen_w - 64) / 2`.
- `pub fn sync_overlay(app: &tauri::AppHandle)`: run via `app.run_on_main_thread` (called from a blocking command thread). Read `config.overlay`; if a window labeled `overlay` exists, `destroy()` it. If `enabled`, build a `WebviewWindowBuilder` with `WebviewUrl::App("overlay.html".into())`: `.transparent(true).decorations(false).shadow(false).always_on_top(true).skip_taskbar(true).focused(false).inner_size(64.0, 64.0)` then `.set_ignore_cursor_events(true)`; position from `app.primary_monitor()` (fallback: current window's monitor) via `overlay_position`. Log a warning and do nothing on any window-creation error (badge is cosmetic).
- Hooks: in `save_settings` after `app::remember(...)` success, call `overlay::sync_overlay(&app)`; in `setup` after `show_settings(...)`, call it too. `main.rs`: `mod overlay;`.

- [ ] **Step 4: Run tests + full Rust gate** — `cargo test` and both clippys — Expected: PASS/clean.

- [ ] **Step 5: Commit** — `git add src-tauri && git commit -m "feat: overlay window lifecycle driven by config"`

### Task 3: The overlay page (ui/)

**Files:**
- Create: `ui/overlay.html`, `ui/src/overlay.ts`, `ui/src/overlay.css`
- Modify: `ui/vite.config.ts` (multi-page input)
- Test: `npm run build --prefix ui` produces `dist/overlay.html`

**Interfaces:**
- Consumes: `status` event payload `{ state: "Disabled"|"Muted"|"Talking", ... }`; `invoke("get_settings")` → `Config` with `overlay.mode`.
- Produces: the served page for Task 2's window (no code interface).

- [ ] **Step 1: Multi-page build config** — add `build.rollupOptions.input = { main: "index.html", overlay: "overlay.html" }` (paths relative to `ui/`).

- [ ] **Step 2: Write the page** — `overlay.html`: minimal shell, `<div id="dot">`, links `overlay.css` + `overlay.ts` module. `overlay.css`: `html,body{background:transparent;margin:0;height:100%}`; `#dot` centered 20 px circle; classes `.talking` (red `#ef4444`, glow `box-shadow`, `animation: pulse 1.2s ease-in-out infinite`) and `.dim` (gray, no animation); `#dot{transition:opacity 300ms}` for the fade. `overlay.ts`: on load `invoke<Config>("get_settings")` for the mode; `invoke("get_status")` once for the initial state; `listen("status", ...)`; state machine: `Talking` → `show()` + `.talking`; `Muted` → always-mode: `show()` + `.dim`, talk-only: add fade class, then `setTimeout(350)` → `hide()`; `Disabled` → `hide()`. Use `getCurrentWindow()` from `@tauri-apps/api/window` and `listen` from `@tauri-apps/api/event` (same imports the settings app already uses).

- [ ] **Step 3: Build check** — `npm run build --prefix ui` — Expected: `dist/overlay.html` exists alongside `dist/index.html`; `cargo build` embeds both (verify asset map contains `/overlay.html` via `strings target/debug/ptt-tool | grep overlay`).

- [ ] **Step 4: Commit** — `git add ui && git commit -m "feat: overlay page with pulsing talk-state dot"`

### Task 4: Settings UI — Overlay card

**Files:**
- Modify: `ui/src/App.svelte` (Config interface + new card near the existing toggles)

**Interfaces:**
- Consumes: `settings.overlay` from `get_settings` (Task 1's serde shape: `{ enabled, mode, position, distance }`).
- Produces: nothing new — Save already round-trips the whole `Config`.

- [ ] **Step 1: Extend the TS `Config` interface** with `overlay: { enabled: boolean; mode: "talk-only" | "always"; position: string; distance: number }` and make the draft/save payload carry it (the `clone()`-based payload already sends the whole object once the field exists).

- [ ] **Step 2: Add the "Overlay" card** — enable checkbox, mode select (Talk only / Always), position select (the 6 values, human labels), distance slider 0–200 (same slider pattern as release delay). No new commands.

- [ ] **Step 3: Build check** — `npm run build --prefix ui` — Expected: clean.

- [ ] **Step 4: Commit** — `git add ui/src/App.svelte && git commit -m "feat: overlay card in the settings window"`

### Task 5: Docs — manual checklist, ideas, README

**Files:**
- Modify: `docs/MANUAL_TESTS.md` (new "Overlay" section before "Known limitations")
- Modify: `docs/IDEAS.md` (remove the "On-screen overlay" line)
- Modify: `README.md` ("Using it": one short paragraph on the optional overlay)

- [ ] **Step 1: Add the manual checklist** — the spec's Manual list as `- [ ]` items, labeled M-1..M-7 (M-1 no white box/shadow, M-2 talk-only hides after fade, M-3 click-through proof, M-4 focus never stolen, M-5 all 6 positions + distance, M-6 live apply on Save incl. disable-removes, M-7 game smoke test).

- [ ] **Step 2: IDEAS.md + README edits** — remove the overlay idea line (it is now built); README gets 2–3 sentences: optional overlay, off by default, where to turn it on.

- [ ] **Step 3: Commit** — `git add docs README.md && git commit -m "docs: overlay manual checklist and readme note"`

### Task 6: Full gate, push, CI, handoff

- [ ] **Step 1: Full gate** — every command in Global Constraints — Expected: all clean.
- [ ] **Step 2: Ledger** — append the overlay design/plan/decisions to `.superpowers/sdd/IMPLEMENTATION_PLAN/progress.md` (deviation: M6 stretch pulled forward, spec path).
- [ ] **Step 3: Push + CI** — `git push origin main`; watch the CI run to green.
- [ ] **Step 4: Hand off** — tell the partner to `git pull`, rebuild both steps (UI is embedded), and run the Overlay checklist M-1..M-7; focus/click-through (M-3/M-4) are the ones to scrutinize, with the `WS_EX_NOACTIVATE` fallback ready if M-4 fails.
