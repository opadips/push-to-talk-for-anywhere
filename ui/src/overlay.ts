// The talk-state badge page (overlay design spec): one dot, driven by
// the tray poller's `status` event — no framework, no session access.
// The window itself is created and positioned by `src-tauri/src/overlay.rs`;
// this file only paints the dot and, in talk-only mode, hides the window
// while muted so nothing stays on screen.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

// The Rust side serializes SessionState lowercase: {"state":"talking"}.
type UiStatus = {
  state: "disabled" | "muted" | "talking";
  enabled: boolean;
  running: boolean;
  error: string | null;
};

type OverlaySettings = { mode: "talk-only" | "always" };

const dot = document.getElementById("dot") as HTMLDivElement;
const win = getCurrentWindow();

// The window is created visible with an invisible dot (no flash) and is
// destroyed/recreated whenever the mode changes, so "talk-only" is the
// only state this page ever has to learn (spec: mode read once on load).
let mode: OverlaySettings["mode"] = "talk-only";
let hideTimer: number | null = null;
let shown = true;

// Idempotent show/hide: only the actual hidden→visible edges may call the
// window — on Windows a show() re-attempts activation (see the plan's M-4
// execution note), so redundant calls are not just chatter.
function show() {
  if (!shown) {
    shown = true;
    void win.show();
  }
}

function hide() {
  if (shown) {
    shown = false;
    void win.hide();
  }
}

// The dot breathes out over the CSS 300ms transition; the window is
// hidden once that has finished. A repeated "goes away" state never
// restarts the clock (the poller re-emits when e.g. `error` changes
// alongside the state).
function startFade() {
  if (hideTimer !== null) return;
  hideTimer = window.setTimeout(() => {
    hideTimer = null;
    dot.className = "";
    hide();
  }, 350);
}

function cancelFade() {
  if (hideTimer !== null) {
    clearTimeout(hideTimer);
    hideTimer = null;
  }
}

// Map one engine state to dot + window (the spec's behavior table).
function apply(state: UiStatus["state"]) {
  if (state === "talking") {
    // ReleasePending serializes as "talking" too — the mic really is
    // open, so the badge stays bright until it mutes.
    cancelFade();
    dot.className = "talking";
    show();
    return;
  }

  if (state === "muted" && mode === "always") {
    cancelFade();
    dot.className = "dim";
    show();
    return;
  }

  // Everything else takes the badge away. A bright dot (talk-only muted,
  // or disabled mid-talk) fades out first — the spec fades the dot
  // whenever the state leaves Talking/ReleasePending.
  if (dot.className === "talking") {
    startFade();
    return;
  }
  cancelFade();
  dot.className = "";
  hide();
}

async function start() {
  try {
    const settings = await invoke<{ overlay: OverlaySettings }>("get_settings");
    mode = settings.overlay.mode === "always" ? "always" : "talk-only";
  } catch {
    // "talk-only" is the spec's default and the safe one.
  }

  // Listen before taking the snapshot: the poller only emits on change, so
  // a transition landing between get_status and listen would otherwise
  // leave the badge stale until the *next* change.
  let sawEvent = false;
  await listen<UiStatus>("status", (event) => {
    sawEvent = true;
    apply(event.payload.state);
  });
  if (sawEvent) return;

  try {
    const status = await invoke<UiStatus>("get_status");
    if (!sawEvent) apply(status.state);
  } catch {
    // No snapshot and no event yet: the next change corrects us.
  }
}

void start();
