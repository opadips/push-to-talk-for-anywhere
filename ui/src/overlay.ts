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
// destroyed/recreated whenever settings change, so "talk-only" is the
// only state this page ever has to learn (spec: mode read once on load).
let mode: OverlaySettings["mode"] = "talk-only";
let hideTimer: number | null = null;
let shown = true;

// Idempotent show/hide: the status event arrives every ~100ms, and a
// redundant show() is not just chatter — on Windows it can re-attempt
// activation (see the plan's M-4 execution note), so only the actual
// hidden→visible edges may call the window.
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

// Map one engine state to dot + window (the spec's behavior table).
function apply(state: UiStatus["state"]) {
  if (hideTimer !== null) {
    clearTimeout(hideTimer);
    hideTimer = null;
  }

  if (state === "talking") {
    // ReleasePending serializes as "talking" too — the mic really is
    // open, so the badge stays bright until it mutes.
    dot.className = "talking";
    show();
    return;
  }

  if (state === "muted" && mode === "always") {
    dot.className = "dim";
    show();
    return;
  }

  // Talk-only, muted: fade the dot out (CSS, 300ms) and then hide the
  // window so nothing paints while muted.
  if (mode === "talk-only" && state === "muted" && dot.className === "talking") {
    hideTimer = window.setTimeout(() => {
      hideTimer = null;
      dot.className = "";
      hide();
    }, 350);
    return;
  }

  // Disabled (both modes) and talk-only muted without a dot to fade.
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

  try {
    const status = await invoke<UiStatus>("get_status");
    apply(status.state);
  } catch {
    // The first status event (within ~100ms) corrects us.
  }

  await listen<UiStatus>("status", (event) => apply(event.payload.state));
}

void start();
