<script lang="ts">
  // The settings window (plan §9 M4): what the plan lists as bind
  // key/button ("press any key"), device, release delay, swallow, sounds,
  // start-with-Windows, on-exit — plus the live state and any error the
  // engine reports. Everything is applied by Save; Enable/Disable acts
  // immediately, like the tray menu.
  import { onDestroy, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import VirtualKeyboard from "./VirtualKeyboard.svelte";
  import { isShiftCtrlAltVk } from "./keyboardLayout";

  // --- shapes the Rust side serializes (plan §8, §9 M4) -------------------
  type BindingConfig = {
    // `""` is the toggle's unbound state (its neutral state before the
    // feature existed); the PTT binding is always "key" or "mouse".
    kind: "" | "key" | "mouse";
    vk: number;
    scan: number;
    mouse_button: string;
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
  // serde's external tagging: {"Key": {...}} or {"Mouse": "X1"}
  type Captured = { Key: { vk: number; scan: number } } | { Mouse: string };
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
    window.removeEventListener("keydown", onCaptureKey, true);
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
  // Two listeners race, and the first answer wins:
  //
  //  * KEYS are read by this page. While a capture is running the settings
  //    window is the one that has the keyboard focus, so a plain DOM
  //    `keydown` always sees the press. The global low-level hook cannot be
  //    relied on for this: it was observed not to deliver key presses while
  //    WebView2 owned the focus, and the raw-input fallback added for that
  //    rejected every key report (see hook.rs).
  //  * MOUSE BUTTONS come from the backend (`capture_binding`): the global
  //    hook sees them anywhere on screen, including the side buttons.
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

  function endCapture(cancelBackend: boolean) {
    captureId++;
    capturing = false;
    window.removeEventListener("keydown", onCaptureKey, true);
    // The backend capture is still armed: tell it to stand down, or the hook
    // would swallow the next key / mouse button the user presses anywhere.
    if (cancelBackend) invoke("cancel_capture").catch(() => {});
  }

  async function applyCaptured(captured: Captured, target: CaptureTarget) {
    if (!form) return;
    const bound = target === "toggle" ? form.toggle : form.binding;
    if ("Key" in captured) {
      bound.kind = "key";
      bound.vk = captured.Key.vk;
      bound.scan = captured.Key.scan;
      bound.mouse_button = "";
    } else {
      bound.kind = "mouse";
      bound.mouse_button = String(captured.Mouse).toLowerCase();
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
    form.toggle = { kind: "", vk: 0, scan: 0, mouse_button: "", swallow: true };
    toggleLabel = "Not set";
  }

  function onCaptureKey(event: KeyboardEvent) {
    // While capturing, every key belongs to the capture — not to the page
    // (Tab must not move focus, Enter/Space must not click, F5 must not reload).
    event.preventDefault();
    event.stopPropagation();
    // The press that clicked "Change" may still be auto-repeating.
    if (event.repeat || event.isComposing) return;
    const vk = event.keyCode;
    // A bare modifier is skipped here (and by the core's capture): the page
    // cannot tell left from right, and a chord must not bind its first key.
    // Shift/Ctrl/Alt are bound from the on-screen keyboard instead.
    if (!vk || vk === VK_IME || MODIFIER_VKS.has(vk)) return;
    endCapture(true);
    // The page has no scan code. It is stored but never matched on — the
    // hook compares `vk` only — and 0 passes the config validation.
    void applyCaptured({ Key: { vk, scan: 0 } }, captureTarget);
  }

  async function capture(target: CaptureTarget) {
    if (!form || capturing) return;
    const id = ++captureId;
    captureTarget = target;
    capturing = true;
    window.addEventListener("keydown", onCaptureKey, true);
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
  // The same result as pressing the key, reached by clicking it: the picker
  // hands back a virtual-key code and the draft binding changes exactly as it
  // does after "press any key". (There is no scan code to give — it is stored
  // but never matched on, see `onCaptureKey`.)
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

  async function pickKey(vk: number) {
    closePicker();
    await applyCaptured({ Key: { vk, scan: 0 } }, pickerTarget);
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
    <h1>Push-to-Talk</h1>
    <div class="head">
      <span class="chip {status?.running ? status.state : "stopped"}">
        <span class="dot"></span>{stateLabel}
      </span>
      {#if status}
        <button class="toggle" onclick={toggleEnabled}>
          {status.enabled ? "Disable" : "Enable"}
        </button>
      {/if}
    </div>
  </header>

  {#if status?.error}
    <div class="banner error">{status.error}</div>
  {/if}
  {#if notice}
    <div class="banner">{notice}</div>
  {/if}

  {#if form}
    <section>
      <h2>Hotkey</h2>
      <div class="row">
        <span class="binding">{label}</span>
        <span class="actions">
          <button onclick={() => capture("ptt")} disabled={capturing || busy}>
            {capturing && captureTarget === "ptt" ? "Press any key or button…" : "Change"}
          </button>
          <button
            onclick={(e) => openPicker("ptt", e)}
            disabled={capturing || busy}
            title="Click the key on an on-screen keyboard instead of pressing it"
          >
            On-screen keyboard
          </button>
        </span>
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={form.binding.swallow} />
        Also block the key from other applications
      </label>
      {#if blocksModifier}
        <p class="warn">
          This key is Shift, Ctrl or Alt: blocking it stops it working in every program. Untick
          the box above unless that is what you want.
        </p>
      {/if}
      <div class="row toggle-row">
        <span class="binding">
          <span class="toggle-caption">Toggle key (press once to talk)</span>
          {toggleLabel || "Not set"}
        </span>
        <span class="actions">
          <button onclick={() => capture("toggle")} disabled={capturing || busy}>
            {capturing && captureTarget === "toggle" ? "Press any key or button…" : "Change"}
          </button>
          <button
            onclick={(e) => openPicker("toggle", e)}
            disabled={capturing || busy}
            title="Click the key on an on-screen keyboard instead of pressing it"
          >
            On-screen keyboard
          </button>
          <button onclick={clearToggle} disabled={capturing || busy}>Clear</button>
        </span>
      </div>
      <label class="check">
        <input
          type="checkbox"
          bind:checked={form.toggle.swallow}
          disabled={form.toggle.kind === ""}
        />
        Also block the toggle key from other applications
      </label>
      {#if blocksToggleModifier}
        <p class="warn">
          This key is Shift, Ctrl or Alt: blocking it stops it working in every program. Untick
          the box above unless that is what you want.
        </p>
      {/if}
    </section>

    <section>
      <h2>Microphone</h2>
      <label class="field">
        <span>Device</span>
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
        <span>Release delay</span>
        <span class="inline">
          <input
            type="range"
            min="0"
            max="2000"
            step="10"
            bind:value={form.audio.release_delay_ms}
          />
          <span class="value">{form.audio.release_delay_ms} ms</span>
        </span>
      </label>
      <label class="field">
        <span>On exit</span>
        <select bind:value={form.audio.on_exit}>
          <option value="restore">Hand the microphone back as it was found</option>
          <option value="unmute">Always leave it unmuted</option>
        </select>
      </label>
    </section>

    <section>
      <h2>Sounds</h2>
      <label class="check">
        <input type="checkbox" bind:checked={form.sounds.enabled} />
        Play the talk / release sounds
      </label>
      <label class="field">
        <span>Volume</span>
        <span class="inline">
          <input
            type="range"
            min="0"
            max="1"
            step="0.05"
            bind:value={form.sounds.volume}
            disabled={!form.sounds.enabled}
          />
          <span class="value">{Math.round(form.sounds.volume * 100)}%</span>
        </span>
      </label>
    </section>

    <section>
      <h2>Overlay</h2>
      <label class="check">
        <input type="checkbox" bind:checked={form.overlay.enabled} />
        Show an on-screen dot while talking (clicks pass through it)
      </label>
      <label class="field">
        <span>Show</span>
        <select bind:value={form.overlay.mode} disabled={!form.overlay.enabled}>
          <option value="talk-only">Only while talking</option>
          <option value="always">Always (dim while muted)</option>
        </select>
      </label>
      <label class="field">
        <span>Position</span>
        <select bind:value={form.overlay.position} disabled={!form.overlay.enabled}>
          <option value="top-left">Top left</option>
          <option value="top-center">Top center</option>
          <option value="top-right">Top right</option>
          <option value="bottom-left">Bottom left</option>
          <option value="bottom-center">Bottom center</option>
          <option value="bottom-right">Bottom right</option>
        </select>
      </label>
      <label class="field">
        <span>Distance from edge</span>
        <span class="inline">
          <input
            type="range"
            min="0"
            max="200"
            step="4"
            bind:value={form.overlay.distance}
            disabled={!form.overlay.enabled}
          />
          <span class="value">{form.overlay.distance} px</span>
        </span>
      </label>
    </section>

    <section>
      <h2>App</h2>
      <label class="check">
        <input type="checkbox" bind:checked={form.app.start_with_windows} />
        Start with Windows
      </label>
      <label class="check">
        <input type="checkbox" bind:checked={form.app.start_hidden} />
        Start hidden in the tray
      </label>
    </section>
  {:else}
    <p class="placeholder">Loading settings…</p>
  {/if}

  {#if pickerOpen && form}
    <VirtualKeyboard
      selectedVk={pickerTarget === "toggle"
        ? (form.toggle.kind === "key" ? form.toggle.vk : null)
        : (form.binding.kind === "key" ? form.binding.vk : null)}
      currentLabel={pickerTarget === "toggle" ? (toggleLabel || "Not set") : label}
      onpick={pickKey}
      onclose={closePicker}
    />
  {/if}

  <footer>
    <span class="hint">Closing this window hides it to the tray.</span>
    <button class="primary" onclick={save} disabled={!dirty || busy || capturing}>
      {busy ? "Saving…" : "Save"}
    </button>
  </footer>
</main>

<style>
  :global(body) {
    margin: 0;
    background: #14161a;
    color: #e8eaed;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  main {
    padding: 1.25rem 1.5rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  h1 {
    font-size: 1.2rem;
    margin: 0;
  }
  h2 {
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: #9aa0a6;
    margin: 0 0 0.6rem;
    font-weight: 600;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.25rem 0.6rem;
    border-radius: 999px;
    background: #1e2126;
    border: 1px solid #2c3036;
    font-size: 0.85rem;
  }
  .dot {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background: #5f6368;
  }
  .chip.talking .dot {
    background: #81c995;
  }
  .chip.muted .dot {
    background: #f28b82;
  }
  .chip.disabled .dot {
    background: #5f6368;
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
  section {
    background: #191c21;
    border: 1px solid #24282e;
    border-radius: 10px;
    padding: 0.9rem 1rem;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  .binding {
    font-size: 1.05rem;
    font-weight: 600;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .warn {
    margin: 0.6rem 0 0;
    padding: 0.5rem 0.65rem;
    border-radius: 8px;
    background: #2b2616;
    border: 1px solid #5d4e1f;
    color: #f0dca0;
    font-size: 0.82rem;
    line-height: 1.35;
  }
  .actions {
    display: flex;
    flex: 0 0 auto;
    gap: 0.5rem;
  }
  .toggle-row {
    margin-top: 0.85rem;
    padding-top: 0.85rem;
    border-top: 1px solid #24282e;
  }
  .toggle-caption {
    display: block;
    font-size: 0.78rem;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #9aa0a6;
    margin-bottom: 0.15rem;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.9rem;
    color: #c9cdd2;
    margin-top: 0.7rem;
  }
  .field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    font-size: 0.9rem;
    color: #c9cdd2;
    margin-top: 0.55rem;
  }
  .field > span:first-child {
    flex: 0 0 auto;
  }
  .inline {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex: 1;
    justify-content: flex-end;
  }
  .inline input[type="range"] {
    flex: 1;
    max-width: 12rem;
  }
  .value {
    min-width: 4.2rem;
    text-align: right;
    color: #e8eaed;
    font-variant-numeric: tabular-nums;
  }
  select {
    flex: 1;
    max-width: 20rem;
    background: #14161a;
    color: #e8eaed;
    border: 1px solid #2c3036;
    border-radius: 6px;
    padding: 0.4rem 0.5rem;
    font: inherit;
  }
  input[type="range"] {
    accent-color: #81c995;
  }
  button {
    background: #23272d;
    color: #e8eaed;
    border: 1px solid #343a41;
    border-radius: 7px;
    padding: 0.42rem 0.85rem;
    font: inherit;
    font-size: 0.88rem;
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    background: #2b3037;
  }
  button:disabled {
    opacity: 0.55;
    cursor: default;
  }
  button.primary {
    background: #2f6b45;
    border-color: #3d8a59;
  }
  button.primary:hover:not(:disabled) {
    background: #377c51;
  }
  .toggle {
    font-size: 0.82rem;
    padding: 0.3rem 0.7rem;
  }
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin-top: 0.15rem;
  }
  .hint {
    color: #767c83;
    font-size: 0.8rem;
  }
  .placeholder {
    color: #9aa0a6;
  }
</style>
