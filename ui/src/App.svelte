<script lang="ts">
  // The settings window (plan §9 M4): what the plan lists as bind
  // key/button ("press any key"), device, release delay, swallow, sounds,
  // start-at-login, on-exit — plus the live state and any error the
  // engine reports. Everything is applied by Save; Enable/Disable acts
  // immediately, like the tray menu.
  import { onDestroy, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import VirtualKeyboard from "./VirtualKeyboard.svelte";
  import { isShiftCtrlAltVk, type PickedBinding } from "./keyboardLayout";

  // --- shapes the Rust side serializes (plan §8, §9 M4) -------------------
  // The four side words a chord stores for each modifier (spec §8): which
  // side of that role must be held for the combination to fire.
  type Side = "off" | "any" | "left" | "right";
  type SideRole = "ctrl" | "shift" | "alt" | "win";
  type BindingConfig = {
    // `""` is the toggle's unbound state (its neutral state before the
    // feature existed); the PTT binding is always "key", "mouse" or "chord".
    kind: "" | "key" | "mouse" | "chord";
    vk: number;
    scan: number;
    mouse_button: string;
    // Chord members — inert unless `kind === "chord"`, additive like every
    // other new config field: a file from before chords existed loads with
    // all four "off" (spec §8).
    ctrl: Side;
    shift: Side;
    alt: Side;
    win: Side;
    swallow: boolean;
  };
  type Config = {
    version: number;
    enabled: boolean;
    binding: BindingConfig;
    toggle: BindingConfig;
    audio: { device_id: string; release_delay_ms: number; on_exit: "restore" | "unmute" };
    sounds: { enabled: boolean; volume: number };
    app: { start_with_windows: boolean; start_hidden: boolean };
    overlay: {
      enabled: boolean;
      mode: "talk-only" | "always";
      position: string;
      distance: number;
    };
  };
  type UiStatus = {
    state: "disabled" | "muted" | "talking";
    enabled: boolean;
    running: boolean;
    error: string | null;
  };
  type DeviceInfo = { id: string; name: string; is_default: boolean };
  // serde's external tagging: {"Key": {...}} or {"Mouse": "X1"} — or, since
  // chords exist, {"Chord": {…}} where `key` is `null` for a modifier-only
  // chord such as `Ctrl+Shift`, and each modifier records the exact side
  // that was pressed (capture never softens it to "any", spec §7).
  type Captured =
    | { Key: { vk: number; scan: number } }
    | { Mouse: string }
    | {
        Chord: {
          modifiers: Record<SideRole, Side>;
          key: { Key: { vk: number; scan: number } } | { Mouse: string } | null;
        };
      };
  // Which binding a running capture — or the on-screen keyboard picker —
  // will write to: the PTT key or the toggle key.
  type CaptureTarget = "ptt" | "toggle";

  let settings = $state<Config | null>(null); // what is on disk
  let form = $state<Config | null>(null); // what the user is editing
  let devices = $state<DeviceInfo[]>([]);
  let status = $state<UiStatus | null>(null);
  let label = $state("");
  let toggleLabel = $state("");
  let capturing = $state(false);
  let captureTarget = $state<CaptureTarget>("ptt");
  let pickerOpen = $state(false);
  let pickerTarget = $state<CaptureTarget>("ptt");
  // The button that opened the dialog — focus goes back to it on close.
  let pickerOpener = $state<HTMLButtonElement | undefined>();
  let busy = $state(false);
  let notice = $state<string | null>(null);

  let dirty = $derived(
    !!form && !!settings && JSON.stringify(form) !== JSON.stringify(settings),
  );

  // Blocking Shift/Ctrl/Alt from other applications would break them
  // everywhere (Ctrl+C, capital letters …), so say so next to the checkbox.
  let blocksModifier = $derived(
    !!form &&
      form.binding.kind === "key" &&
      form.binding.swallow &&
      isShiftCtrlAltVk(form.binding.vk),
  );

  // The same warning for the toggle key: blocking Shift/Ctrl/Alt with the
  // toggle binding would break them everywhere just the same.
  let blocksToggleModifier = $derived(
    !!form &&
      form.toggle.kind === "key" &&
      form.toggle.swallow &&
      isShiftCtrlAltVk(form.toggle.vk),
  );

  // What the swallow checkbox blocks (spec §9). For a plain key or button it
  // is that key; for a chord only its *completing* member, and the wording
  // has to follow the shape because the rest of the sentence is true of only
  // some of them:
  //
  //  * a keyed chord (`Ctrl+N`): the final key's press, the repeats while it
  //    is held and its release are blocked, while the modifiers keep reaching
  //    other programs — `Ctrl+C` still works;
  //  * a mouse-ending chord: the same, with the mouse button in place of the
  //    key;
  //  * a modifier-only chord (`Ctrl+Shift`): there is no separate final
  //    member, so the LAST modifier pressed completes the chord and is the
  //    one that gets blocked. The earlier modifier's press and release are
  //    still forwarded — claiming the whole combination is blocked would be
  //    false.
  function chordSwallowCaption(
    bound: BindingConfig,
    who: "the" | "the toggle's",
  ): string {
    if (bound.vk === 0 && bound.mouse_button === "") {
      return (
        `Blocks the last modifier pressed in ${who} combination; the earlier ones still ` +
        "reach other applications."
      );
    }
    const finalMember =
      bound.mouse_button !== "" ? "final mouse button" : "final key";
    return `Blocks ${who} ${finalMember}; the modifiers still reach other applications.`;
  }

  let swallowCaption = $derived.by(() => {
    if (!form) return "Also block the key from other applications";
    return form.binding.kind === "chord"
      ? chordSwallowCaption(form.binding, "the")
      : "Also block the key from other applications";
  });
  let toggleSwallowCaption = $derived.by(() => {
    if (!form) return "Also block the toggle key from other applications";
    return form.toggle.kind === "chord"
      ? chordSwallowCaption(form.toggle, "the toggle's")
      : "Also block the toggle key from other applications";
  });

  let stateLabel = $derived(
    !status || !status.running
      ? "Not running"
      : status.state === "talking"
        ? "Talking"
        : status.state === "muted"
          ? "Muted"
          : "Disabled",
  );

  let unlisten: (() => void) | null = null;

  /// Copy the config for the draft form. `$state` hands back a reactive
  /// Proxy, and `structuredClone` refuses to clone proxies
  /// (`DataCloneError: #<Object> could not be cloned` — verified against V8,
  /// the WebView2 engine) while JSON is exactly the shape these values
  /// have. Used everywhere a config object is duplicated or sent.
  function clone<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T;
  }

  onMount(async () => {
    try {
      settings = await invoke<Config>("get_settings");
      form = clone(settings);
      devices = await invoke<DeviceInfo[]>("get_devices").catch(() => []);
      status = await invoke<UiStatus>("get_status");
      label = await labelFor(form);
      toggleLabel = await toggleLabelFor(form);
      // The tray pushes state changes; the chip and the banner follow them.
      unlisten = await listen<UiStatus>("status", (event) => {
        status = event.payload;
        // The tray menu (and the Enable button above) can flip `enabled`
        // behind our back — keep the draft in step so Save cannot silently
        // undo it (plan §9 M4).
        if (form) form.enabled = event.payload.enabled;
        if (settings) settings.enabled = event.payload.enabled;
      });
    } catch (error) {
      notice = String(error);
    }
  });

  onDestroy(() => {
    unlisten?.();
    stopCaptureListeners();
  });

  async function labelFor(config: Config): Promise<string> {
    return invoke<string>("binding_label", { settings: clone(config) }).catch(() => "…");
  }

  /// The display name of the toggle binding — "Not set" while it is
  /// unbound. Mirrors `labelFor`: refreshed on mount and after a save
  /// (the Clear button updates `toggleLabel` directly instead).
  async function toggleLabelFor(config: Config): Promise<string> {
    return invoke<string>("toggle_binding_label", { settings: clone(config) }).catch(
      () => "…",
    );
  }

  async function save() {
    if (!form) return;
    busy = true;
    try {
      // Sliders are typed as numbers, but a `<select>`/`<input>` hand-back
      // can arrive as a string — send numbers for sure, the config is
      // strict about types (plan §8). `clone` also turns the `$state`
      // proxies into plain objects on the way out.
      const payload = clone(form);
      payload.audio.release_delay_ms = Number(payload.audio.release_delay_ms);
      payload.sounds.volume = Number(payload.sounds.volume);
      payload.overlay.distance = Number(payload.overlay.distance);
      settings = await invoke<Config>("save_settings", { settings: payload });
      form = clone(settings);
      label = await labelFor(form);
      toggleLabel = await toggleLabelFor(form);
      flash("Saved");
    } catch (error) {
      flash(String(error));
    } finally {
      busy = false;
    }
  }

  async function toggleEnabled() {
    if (!status) return;
    try {
      await invoke("set_enabled", { enabled: !status.enabled });
    } catch (error) {
      flash(String(error));
    }
  }

  // --- "press any key" (plan §9 M4) --------------------------------------
  //
  // Two listeners can answer, and the first one to do so wins:
  //
  //  * PLAIN KEYS are read by this page. While a capture is running the
  //    settings window is the one that has the keyboard focus, so a plain DOM
  //    `keydown` always sees the press. The global low-level hook cannot be
  //    relied on for this: it was observed not to deliver key presses while
  //    WebView2 owned the focus, and the raw-input fallback added for that
  //    rejected every key report (see hook.rs).
  //  * A MODIFIER-INITIATED SEQUENCE belongs to the backend alone
  //    (`capture_binding`): it is the only side that sees which side of the
  //    modifier moved, and its answer is a chord — or the correct discard of
  //    a lone modifier. The page therefore stands down whenever a modifier
  //    is held (see `heldModifiers`), leaving the backend to answer it.
  //  * MOUSE BUTTONS also come from the backend: the global hook sees them
  //    anywhere on screen, including the side buttons.
  //
  // WebView2 is Chromium on Windows, where `KeyboardEvent.keyCode` is the
  // Windows virtual-key code — the very number the hook reports as `vkCode`
  // and `config.toml` stores as `vk`. (`keyCode` is deprecated in the spec but
  // it is the only way a page can get a VK.)
  const MODIFIER_VKS = new Set([16, 17, 18, 91, 92]); // Shift, Ctrl, Alt, Win
  const VK_IME = 229; // "a key went to the input method" — not a real key

  // Bumped whenever a capture ends, so a late answer from the other
  // listener (or a request we cancelled) is recognised and ignored.
  let captureId = 0;

  // --- modifier state while a capture runs ---------------------------------
  //
  // `onCaptureKey` only ever answers with a *plain key*, so as long as a
  // modifier is down the backend owns the sequence: it alone sees which side
  // of the modifier moved, and it answers with a chord — or correctly
  // discards a lone modifier. Answering here instead would turn every
  // `Ctrl+N` into a bare `N`, and that is exactly what would happen on the
  // machine the raw-input fallback exists for (key presses that never reach
  // the hook): there the page answers without any thread wakeup while the
  // backend still has to run hook thread → channel → session loop → command
  // → IPC, so the page always wins the race.
  //
  // The keys the page sees itself, so a modifier pressed before "Change" was
  // clicked (which produced no `keydown` we could have seen) is caught by
  // `KeyboardEvent.getModifierState` instead — see `onCaptureKey`.
  const heldModifiers = new Set<number>();
  // The reactive half of `heldModifiers` — only the capture hint reads it.
  let modifierHeld = $state(false);

  function startCaptureListeners() {
    heldModifiers.clear();
    modifierHeld = false;
    window.addEventListener("keydown", onCaptureKey, true);
    window.addEventListener("keyup", onCaptureKeyUp, true);
    // A modifier released while the window is not focused never sends its
    // `keyup` here; a stale entry would keep the page from ever answering
    // again, so the set is dropped wholesale instead.
    window.addEventListener("blur", onCaptureBlur);
  }

  function stopCaptureListeners() {
    window.removeEventListener("keydown", onCaptureKey, true);
    window.removeEventListener("keyup", onCaptureKeyUp, true);
    window.removeEventListener("blur", onCaptureBlur);
    heldModifiers.clear();
    modifierHeld = false;
  }

  function endCapture(cancelBackend: boolean) {
    captureId++;
    capturing = false;
    stopCaptureListeners();
    // The backend capture is still armed: tell it to stand down, or the hook
    // would swallow the next key / mouse button the user presses anywhere.
    if (cancelBackend) invoke("cancel_capture").catch(() => {});
  }

  async function applyCaptured(captured: Captured, target: CaptureTarget) {
    if (!form) return;
    const bound = target === "toggle" ? form.toggle : form.binding;
    if ("Chord" in captured) {
      // A press-and-hold capture: `kind = "chord"` plus the four side
      // fields, written exactly as `config.toml` stores them (spec §8).
      const { modifiers, key } = captured.Chord;
      bound.kind = "chord";
      bound.ctrl = modifiers.ctrl;
      bound.shift = modifiers.shift;
      bound.alt = modifiers.alt;
      bound.win = modifiers.win;
      if (!key) {
        // A modifier-only chord (`Ctrl+Shift`) has no final member: `vk`
        // and `mouse_button` both stay empty (spec §8).
        bound.vk = 0;
        bound.scan = 0;
        bound.mouse_button = "";
      } else if ("Key" in key) {
        bound.vk = key.Key.vk;
        bound.scan = key.Key.scan;
        bound.mouse_button = "";
      } else {
        // A final mouse button lives in `mouse_button`, with `vk` zeroed.
        bound.mouse_button = String(key.Mouse).toLowerCase();
        bound.vk = 0;
        bound.scan = 0;
      }
    } else if ("Key" in captured) {
      bound.kind = "key";
      bound.vk = captured.Key.vk;
      bound.scan = captured.Key.scan;
      bound.mouse_button = "";
      clearSides(bound);
    } else {
      bound.kind = "mouse";
      bound.mouse_button = String(captured.Mouse).toLowerCase();
      clearSides(bound);
    }
    if (target === "toggle") {
      toggleLabel = await toggleLabelFor(form);
    } else {
      label = await labelFor(form);
    }
  }

  // Reset the toggle binding to its neutral state — unbound — and say so
  // right away (the backend label comes back the next time it is asked,
  // on mount or after a save).
  function clearToggle() {
    if (!form) return;
    form.toggle = {
      kind: "",
      vk: 0,
      scan: 0,
      mouse_button: "",
      ctrl: "off",
      shift: "off",
      alt: "off",
      win: "off",
      swallow: true,
    };
    toggleLabel = "Not set";
  }

  // --- chord side selectors (spec §9) --------------------------------------
  //
  // Capture records the exact side a modifier was pressed on, so a binding
  // captured with the right Ctrl (`Right Ctrl+N`) does not fire on the left
  // one. These four selectors are the one place the user can loosen a pin
  // back to `Any` (or drop a modifier entirely) without rebinding.

  const SIDE_ROLES: { role: SideRole; caption: string }[] = [
    { role: "ctrl", caption: "Ctrl" },
    { role: "shift", caption: "Shift" },
    { role: "alt", caption: "Alt" },
    { role: "win", caption: "Win" },
  ];

  // Drop a binding back to a plain key or button: clear the chord side
  // fields too, exactly as `Config::set_binding` does (`clear_chord_fields`)
  // — otherwise a later chord would silently inherit stale sides.
  function clearSides(bound: BindingConfig) {
    bound.ctrl = "off";
    bound.shift = "off";
    bound.alt = "off";
    bound.win = "off";
  }

  // One selector changed: write the side into the draft, then refetch the
  // label — it comes from Rust (`Binding::label`) and is never rebuilt here.
  async function setSide(target: CaptureTarget, role: SideRole, value: string) {
    if (!form) return;
    const bound = target === "toggle" ? form.toggle : form.binding;
    bound[role] = value as Side;
    if (target === "toggle") {
      toggleLabel = await toggleLabelFor(form);
    } else {
      label = await labelFor(form);
    }
  }

  // `Chord::is_valid` on the draft: a chord with no final member needs at
  // least two modifiers, and the engine falls back to the default binding
  // the moment it is saved — say so while the user can still fix it.
  function chordIsUnusable(bound: BindingConfig): boolean {
    const modifiers = [bound.ctrl, bound.shift, bound.alt, bound.win].filter(
      (side) => side !== "off",
    ).length;
    return (
      bound.kind === "chord" &&
      bound.vk === 0 &&
      bound.mouse_button === "" &&
      modifiers < 2
    );
  }

  function onCaptureKey(event: KeyboardEvent) {
    // While capturing, every key belongs to the capture — not to the page
    // (Tab must not move focus, Enter/Space must not click, F5 must not reload).
    event.preventDefault();
    event.stopPropagation();
    // The press that clicked "Change" may still be auto-repeating.
    if (event.repeat || event.isComposing) return;
    const vk = event.keyCode;
    if (MODIFIER_VKS.has(vk)) {
      heldModifiers.add(vk);
      modifierHeld = true;
      return;
    }
    if (!vk || vk === VK_IME) return;
    // A modifier-initiated sequence is the backend's, not this page's: only
    // the hook sees which side of a modifier moved, and its accumulator
    // answers with a chord — or correctly discards a lone modifier. This
    // page can only ever answer with a plain key (spec §7), so answering
    // while a modifier is down would silently downgrade every `Ctrl+N` to a
    // bare `N` — and on the machine the raw-input fallback exists for the
    // page *always* wins that race (see `heldModifiers`). `getModifierState`
    // covers a modifier held before "Change" was clicked, whose `keydown`
    // no listener here ever saw.
    if (heldModifiers.size > 0 || modifierHeldOn(event)) return;
    endCapture(true);
    // The page has no scan code. It is stored but never matched on — the
    // hook compares `vk` only — and 0 passes the config validation.
    void applyCaptured({ Key: { vk, scan: 0 } }, captureTarget);
  }

  // Whether a modifier is currently down, as the browser itself reports it
  // on this event — the state our own listeners can only see from the moment
  // the capture started.
  function modifierHeldOn(event: KeyboardEvent): boolean {
    return (
      event.getModifierState("Control") ||
      event.getModifierState("Shift") ||
      event.getModifierState("Alt") ||
      event.getModifierState("Meta")
    );
  }

  // Releases matter as much as presses: without them a Ctrl released after
  // the capture ended would leave the set stuck and the page never answering
  // again (see `stopCaptureListeners`).
  function onCaptureKeyUp(event: KeyboardEvent) {
    const vk = event.keyCode;
    if (!MODIFIER_VKS.has(vk)) return;
    heldModifiers.delete(vk);
    modifierHeld = heldModifiers.size > 0;
  }

  function onCaptureBlur() {
    heldModifiers.clear();
    modifierHeld = false;
  }

  async function capture(target: CaptureTarget) {
    if (!form || capturing) return;
    const id = ++captureId;
    captureTarget = target;
    capturing = true;
    startCaptureListeners();
    try {
      const captured = await invoke<Captured>("capture_binding");
      if (id !== captureId) return; // the page already answered (or cancelled)
      endCapture(false);
      await applyCaptured(captured, target);
    } catch (error) {
      // A request we cancelled ourselves ends with an error: not news.
      if (id === captureId) {
        endCapture(false);
        flash(String(error));
      }
    }
  }

  // --- the on-screen keyboard ---------------------------------------------
  //
  // The same result as pressing the key (or holding a modifier and pressing
  // it), reached by clicking: the picker hands back either a plain key or a
  // complete chord — the modifiers armed on the board plus the key that
  // completes them — and the draft binding changes exactly as it does after
  // "press any key" or a captured combination. (There is no scan code to
  // give — it is stored but never matched on, see `onCaptureKey`.)
  function openPicker(target: CaptureTarget, event: MouseEvent) {
    if (!form || capturing) return;
    pickerTarget = target;
    pickerOpener = event.currentTarget as HTMLButtonElement;
    pickerOpen = true;
  }

  function closePicker() {
    pickerOpen = false;
    // Hand the focus back to the button that opened the dialog.
    queueMicrotask(() => pickerOpener?.focus());
  }

  async function pickKey(picked: PickedBinding) {
    closePicker();
    await applyCaptured(picked, pickerTarget);
  }

  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  function flash(message: string) {
    notice = message;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => (notice = null), 3000);
  }
</script>

<main>
  <header>
    <div class="brand">
      <svg class="brand-icon" viewBox="0 0 24 24" aria-hidden="true">
        <rect x="9" y="3" width="6" height="11" rx="3" />
        <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
      </svg>
      <div class="brand-text">
        <h1>Push-to-Talk <span class="sep">//</span> Settings</h1>
        <p class="hint">Closing this window hides it to the tray.</p>
      </div>
    </div>
    <div class="head">
      <span class="chip {status?.running ? status.state : "stopped"}">
        <span class="dot"></span>{stateLabel}
      </span>
      {#if status}
        <button class="toggle" onclick={toggleEnabled}>
          {status.enabled ? "Disable" : "Enable"}
        </button>
      {/if}
      <button class="primary" onclick={save} disabled={!dirty || busy || capturing}>
        {busy ? "Saving…" : "Save"}
      </button>
    </div>
  </header>

  {#if status?.error}
    <div class="banner error">{status.error}</div>
  {/if}
  {#if notice}
    <div class="banner">{notice}</div>
  {/if}

  {#snippet holdHint()}
    <p class="hold-hint">
      A modifier is held — press the rest of your combination, or release it on its own to
      bind that modifier by itself.
    </p>
  {/snippet}

  {#snippet sideSelectors(bound: BindingConfig, target: CaptureTarget)}
    <div
      class="sides"
      title="Which side of each modifier must be held for this combination to fire"
    >
      {#each SIDE_ROLES as entry (entry.role)}
        <label class="side">
          <span>{entry.caption}</span>
          <select
            value={bound[entry.role]}
            onchange={(event) => setSide(target, entry.role, event.currentTarget.value)}
          >
            <option value="off">Off</option>
            <option value="any">Any</option>
            <option value="left">Left</option>
            <option value="right">Right</option>
          </select>
        </label>
      {/each}
    </div>
  {/snippet}

  {#if form}
    <div class="grid">
      <section class="hotkeys">
        <h2>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <rect x="2.5" y="6" width="19" height="12" rx="2" />
            <path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10" />
          </svg>
          <span class="num">01</span> // Hotkeys
        </h2>
        <div class="bindings">
          <div class="binding-card">
            <h3>Hold to talk</h3>
            <span class="keycap">{label}</span>
            <div class="bind-row">
              <button onclick={() => capture("ptt")} disabled={capturing || busy}>
                {capturing && captureTarget === "ptt" ? "Press a key…" : "Change"}
              </button>
              <button
                onclick={(e) => openPicker("ptt", e)}
                disabled={capturing || busy}
                title="Click the key on an on-screen keyboard instead of pressing it"
              >
                On-screen
              </button>
            </div>
            {#if capturing && captureTarget === "ptt" && modifierHeld}
              {@render holdHint()}
            {/if}
            {#if form.binding.kind === "chord"}
              {@render sideSelectors(form.binding, "ptt")}
              {#if chordIsUnusable(form.binding)}
                <p class="warn">
                  This combination has no final key and fewer than two modifiers, so it is not a
                  valid binding — the hotkey falls back to the default key when you save.
                </p>
              {/if}
            {/if}
            <label class="switch-row" title={swallowCaption}>
              <span>Block key from other apps</span>
              <input type="checkbox" class="switch" bind:checked={form.binding.swallow} />
            </label>
            {#if form.binding.kind === "chord"}
              <p class="note">{swallowCaption}</p>
            {/if}
            {#if blocksModifier}
              <p class="warn">
                This key is Shift, Ctrl or Alt: blocking it stops it working in every program.
                Turn the switch off unless that is what you want.
              </p>
            {/if}
          </div>

          <div class="binding-card">
            <h3>Toggle <span class="dim">- press once to talk</span></h3>
            <span class="keycap" class:unset={form.toggle.kind === ""}>
              {toggleLabel || "Not set"}
            </span>
            <div class="bind-row">
              <button onclick={() => capture("toggle")} disabled={capturing || busy}>
                {capturing && captureTarget === "toggle" ? "Press a key…" : "Change"}
              </button>
              <button
                onclick={(e) => openPicker("toggle", e)}
                disabled={capturing || busy}
                title="Click the key on an on-screen keyboard instead of pressing it"
              >
                On-screen
              </button>
              <button class="link danger" onclick={clearToggle} disabled={capturing || busy}>
                Clear
              </button>
            </div>
            {#if capturing && captureTarget === "toggle" && modifierHeld}
              {@render holdHint()}
            {/if}
            {#if form.toggle.kind === "chord"}
              {@render sideSelectors(form.toggle, "toggle")}
              {#if chordIsUnusable(form.toggle)}
                <p class="warn">
                  This combination has no final key and fewer than two modifiers, so it is not a
                  valid binding — the toggle is left unbound when you save.
                </p>
              {/if}
            {/if}
            <label class="switch-row" title={toggleSwallowCaption}>
              <span>Block toggle key from other apps</span>
              <input
                type="checkbox"
                class="switch"
                bind:checked={form.toggle.swallow}
                disabled={form.toggle.kind === ""}
              />
            </label>
            {#if form.toggle.kind === "chord"}
              <p class="note">{toggleSwallowCaption}</p>
            {/if}
            {#if blocksToggleModifier}
              <p class="warn">
                This key is Shift, Ctrl or Alt: blocking it stops it working in every program.
                Turn the switch off unless that is what you want.
              </p>
            {/if}
          </div>
        </div>
      </section>

      <section class="mic">
        <h2>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <rect x="9" y="3" width="6" height="11" rx="3" />
            <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
          </svg>
          <span class="num">02</span> // Microphone
        </h2>
        <label class="field">
          <span class="label">Device</span>
          <select bind:value={form.audio.device_id}>
            <option value="default">System default</option>
            {#each devices as device (device.id)}
              <option value={device.id}>
                {device.name}{device.is_default ? " (system default)" : ""}
              </option>
            {/each}
            {#if form.audio.device_id !== "default" && !devices.some((d) => d.id === form.audio.device_id)}
              <option value={form.audio.device_id}>(not connected) {form.audio.device_id}</option>
            {/if}
          </select>
        </label>
        <label class="field">
          <span class="label">
            Release delay <span class="value">{form.audio.release_delay_ms} ms</span>
          </span>
          <input
            type="range"
            min="0"
            max="2000"
            step="10"
            bind:value={form.audio.release_delay_ms}
          />
        </label>
        <label class="field">
          <span class="label">On exit</span>
          <select bind:value={form.audio.on_exit}>
            <option value="restore">Hand the microphone back as it was found</option>
            <option value="unmute">Always leave it unmuted</option>
          </select>
        </label>
      </section>

      <section class="sounds">
        <h2>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 9.5v5h3.5L12 18.5v-13L7.5 9.5zM16 9a4.5 4.5 0 0 1 0 6" />
          </svg>
          <span class="num">03</span> // Sounds
        </h2>
        <label class="switch-row">
          <span>Play the talk / release sounds</span>
          <input type="checkbox" class="switch" bind:checked={form.sounds.enabled} />
        </label>
        <label class="field">
          <span class="label">
            Volume <span class="value">{Math.round(form.sounds.volume * 100)}%</span>
          </span>
          <input
            type="range"
            min="0"
            max="1"
            step="0.05"
            bind:value={form.sounds.volume}
            disabled={!form.sounds.enabled}
          />
        </label>
      </section>

      <section class="overlay">
        <h2>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <rect x="3" y="4" width="18" height="13" rx="2" />
            <path d="M9 21h6M12 17v4" />
          </svg>
          <span class="num">04</span> // Overlay
        </h2>
        <label class="switch-row" title="Clicks pass through the dot">
          <span>On-screen indicator while talking</span>
          <input type="checkbox" class="switch" bind:checked={form.overlay.enabled} />
        </label>
        <div class="pair">
          <label class="field">
            <span class="label">Show</span>
            <select bind:value={form.overlay.mode} disabled={!form.overlay.enabled}>
              <option value="talk-only">Only while talking</option>
              <option value="always">Always (dim while muted)</option>
            </select>
          </label>
          <label class="field">
            <span class="label">Position</span>
            <select bind:value={form.overlay.position} disabled={!form.overlay.enabled}>
              <option value="top-left">Top left</option>
              <option value="top-center">Top center</option>
              <option value="top-right">Top right</option>
              <option value="bottom-left">Bottom left</option>
              <option value="bottom-center">Bottom center</option>
              <option value="bottom-right">Bottom right</option>
            </select>
          </label>
        </div>
        <label class="field">
          <span class="label">
            Distance from edge <span class="value">{form.overlay.distance} px</span>
          </span>
          <input
            type="range"
            min="0"
            max="200"
            step="4"
            bind:value={form.overlay.distance}
            disabled={!form.overlay.enabled}
          />
        </label>
      </section>

      <section class="general">
        <h2>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 8h13M14 5l3 3-3 3M20 16H7M10 13l-3 3 3 3" />
          </svg>
          <span class="num">05</span> // General
        </h2>
        <label class="switch-row">
          <span>Start automatically at login</span>
          <input type="checkbox" class="switch" bind:checked={form.app.start_with_windows} />
        </label>
        <label class="switch-row">
          <span>Start hidden in the tray</span>
          <input type="checkbox" class="switch" bind:checked={form.app.start_hidden} />
        </label>
      </section>
    </div>
  {:else}
    <p class="placeholder">Loading settings…</p>
  {/if}

  {#if pickerOpen && form}
    <VirtualKeyboard
      selectedVk={pickerTarget === "toggle"
        ? (form.toggle.kind === "key" ? form.toggle.vk : null)
        : (form.binding.kind === "key" ? form.binding.vk : null)}
      currentLabel={pickerTarget === "toggle" ? (toggleLabel || "Not set") : label}
      target={pickerTarget}
      onpick={pickKey}
      onclose={closePicker}
    />
  {/if}
</main>

<style>
  :global(html) {
    font-size: 13px;
  }
  :global(body) {
    margin: 0;
    background: #0e1013;
    color: #e8eaed;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
    --accent: #6fd39a;
    --accent-ink: #0a1d12;
    --card: #14171b;
    --inset: #0e1013;
    --line: #23272d;
    --line-strong: #33383f;
    --muted: #9aa0a6;
    --dim: #868c94;
  }
  :global(::selection) {
    background: #2f6b45;
    color: #fff;
  }
  main {
    padding: 1rem 1.25rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    min-height: 100vh;
    box-sizing: border-box;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
    padding-bottom: 0.3rem;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    min-width: 0;
  }
  .brand-icon {
    width: 1.5rem;
    height: 1.5rem;
    flex: none;
  }
  svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  h1 {
    font-size: 1.05rem;
    margin: 0;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-weight: 700;
  }
  .sep {
    font-weight: 400;
    color: var(--muted);
  }
  .hint {
    margin: 0.1rem 0 0;
    color: var(--dim);
    font-size: 0.78rem;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  h2 {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.74rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--muted);
    margin: 0 0 0.9rem;
    font-weight: 600;
  }
  h2 svg {
    width: 0.95rem;
    height: 0.95rem;
    flex: none;
    margin-right: 0.15rem;
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  h3 {
    margin: 0 0 0.6rem;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-weight: 600;
    color: var(--muted);
  }
  .dim {
    color: var(--dim);
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.3rem 0.8rem;
    border-radius: 999px;
    background: var(--card);
    border: 1px solid var(--line-strong);
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: #5f6368;
  }
  .chip.talking .dot {
    background: var(--accent);
  }
  .chip.muted .dot {
    background: #f28b82;
  }
  .banner {
    padding: 0.6rem 0.75rem;
    border-radius: 8px;
    background: #1b2b1f;
    border: 1px solid #2f5137;
    color: #c8e6c9;
    font-size: 0.88rem;
    word-break: break-word;
  }
  .banner.error {
    background: #2c1a19;
    border-color: #6e3128;
    color: #f5c9c4;
  }

  /* --- card grid ------------------------------------------------------- */
  .grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 0.9rem;
    align-items: stretch;
  }
  section {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 0.95rem 1.05rem 1.05rem;
  }
  .hotkeys {
    grid-column: span 2;
  }
  .bindings {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.75rem;
  }
  .binding-card {
    background: var(--inset);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 0.75rem 0.8rem 0.85rem;
    min-width: 0;
  }
  .bind-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.45rem;
    margin-bottom: 0.8rem;
  }
  .keycap {
    display: inline-flex;
    align-items: center;
    min-height: 2.1rem;
    max-width: 100%;
    box-sizing: border-box;
    padding: 0.2rem 0.85rem;
    margin-bottom: 0.6rem;
    background: #1a1d22;
    border: 1px solid var(--line-strong);
    border-bottom-width: 2px;
    border-radius: 7px;
    font-size: 1rem;
    font-weight: 700;
    overflow-wrap: anywhere;
  }
  .keycap.unset {
    color: var(--dim);
    font-weight: 500;
    border-style: dashed;
  }
  .switch-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    font-size: 0.88rem;
    color: #d3d7dc;
    padding: 0.35rem 0;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    font-size: 0.88rem;
    margin-top: 0.8rem;
    min-width: 0;
  }
  .field:first-of-type {
    margin-top: 0;
  }
  .label {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    font-size: 0.78rem;
    color: var(--muted);
  }
  .value {
    color: #e8eaed;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .pair {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.6rem;
    margin-top: 0.8rem;
  }
  .pair .field {
    margin-top: 0;
  }
  .warn,
  .hold-hint,
  .note {
    margin: 0.5rem 0 0;
    font-size: 0.78rem;
    line-height: 1.4;
  }
  .note {
    color: var(--dim);
  }
  .warn,
  .hold-hint {
    padding: 0.5rem 0.65rem;
    border-radius: 8px;
  }
  .warn {
    background: #2b2616;
    border: 1px solid #5d4e1f;
    color: #f0dca0;
  }
  .hold-hint {
    margin: 0 0 0.8rem;
    background: #16202b;
    border: 1px solid #2c3d51;
    color: #c4d6e8;
  }
  .sides {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.5rem 0.9rem;
    margin-bottom: 0.7rem;
  }
  .side {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.4rem;
  }
  .side > span {
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .side select {
    width: 5.5rem;
    padding: 0.25rem 0.4rem;
    font-size: 0.82rem;
  }

  /* --- controls -------------------------------------------------------- */
  select {
    width: 100%;
    box-sizing: border-box;
    background: var(--inset);
    color: #e8eaed;
    border: 1px solid var(--line-strong);
    border-radius: 7px;
    padding: 0.5rem 0.6rem;
    font: inherit;
    font-size: 0.88rem;
  }
  select:disabled,
  input:disabled {
    opacity: 0.45;
  }
  input[type="range"] {
    appearance: none;
    width: 100%;
    height: 1.1rem;
    margin: 0;
    background: transparent;
    cursor: pointer;
  }
  input[type="range"]::-webkit-slider-runnable-track {
    height: 4px;
    border-radius: 2px;
    background: var(--line-strong);
  }
  input[type="range"]::-webkit-slider-thumb {
    appearance: none;
    width: 0.95rem;
    height: 0.95rem;
    margin-top: calc(2px - 0.475rem);
    border-radius: 50%;
    background: #e8eaed;
    border: 0;
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.5);
  }
  input[type="range"]:hover:not(:disabled)::-webkit-slider-thumb {
    background: var(--accent);
  }
  .switch {
    appearance: none;
    flex: none;
    width: 2.4rem;
    height: 1.3rem;
    margin: 0;
    border-radius: 999px;
    background: #2a2e34;
    border: 1px solid var(--line-strong);
    position: relative;
    cursor: pointer;
    transition: background 0.18s cubic-bezier(0.22, 1, 0.36, 1);
  }
  .switch::after {
    content: "";
    position: absolute;
    top: 50%;
    left: 0.17rem;
    width: 0.9rem;
    height: 0.9rem;
    border-radius: 50%;
    background: #c9cdd2;
    transform: translateY(-50%);
    transition:
      left 0.18s cubic-bezier(0.22, 1, 0.36, 1),
      background 0.18s;
  }
  .switch:checked {
    background: #2f8f5b;
    border-color: #3fa86e;
  }
  .switch:checked::after {
    left: calc(100% - 1.07rem);
    background: #fff;
  }
  .switch:disabled {
    cursor: default;
  }
  button {
    background: #1c2025;
    color: #e8eaed;
    border: 1px solid var(--line-strong);
    border-radius: 7px;
    padding: 0.42rem 0.85rem;
    font: inherit;
    font-size: 0.84rem;
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    background: #262b32;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-ink);
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    padding: 0.55rem 1.8rem;
  }
  button.primary:hover:not(:disabled) {
    background: #86e0ac;
  }
  button.primary:disabled {
    opacity: 0.8;
  }
  button.link {
    background: none;
    border-color: transparent;
    padding-inline: 0.4rem;
  }
  button.danger {
    color: #f28b82;
  }
  button.danger:hover:not(:disabled) {
    background: #2c1a19;
  }
  .toggle {
    font-weight: 600;
  }
  :global(:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .placeholder {
    color: var(--muted);
  }

  @media (max-width: 860px) {
    .grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .hotkeys {
      grid-column: span 2;
    }
  }
  @media (max-width: 560px) {
    .grid,
    .bindings,
    .pair {
      grid-template-columns: minmax(0, 1fr);
    }
    .hotkeys {
      grid-column: auto;
    }
  }
</style>
