// The talk-state badge page (overlay design spec): one pill, driven by the
// tray poller's `status` event — no framework, no session access.
// The window itself is created, sized and positioned by
// `src-tauri/src/overlay.rs` and stays on screen for as long as the overlay
// is enabled; this file only decides whether the pill is visible. (Hiding
// the window instead made WebView2 stop painting, so the first frame after
// showing it could be stale.)

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// The Rust side serializes SessionState lowercase: {"state":"talking"}.
type UiStatus = {
  state: "disabled" | "muted" | "talking";
  enabled: boolean;
  running: boolean;
  error: string | null;
};

type OverlaySettings = { mode: "talk-only" | "always" };

const badge = document.getElementById("badge") as HTMLDivElement;

// The window is rebuilt whenever the mode changes, so the mode is read once
// per load (spec).
let mode: OverlaySettings["mode"] = "talk-only";

// Map one engine state to the pill (the spec's behavior table).
function apply(state: UiStatus["state"]) {
  if (state === "talking") {
    // ReleasePending serializes as "talking" too — the mic really is
    // open, so the badge stays lit until it mutes.
    badge.className = "talking";
  } else if (state === "muted" && mode === "always") {
    badge.className = "dim";
  } else {
    badge.className = "";
  }
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
