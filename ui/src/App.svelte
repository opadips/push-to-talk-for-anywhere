<script lang="ts">
  // The settings window (plan §9 M4): what the plan lists as bind
  // key/button ("press any key"), device, release delay, swallow, sounds,
  // start-with-Windows, on-exit — plus the live state and any error the
  // engine reports. Everything is applied by Save; Enable/Disable acts
  // immediately, like the tray menu.
  import { onDestroy, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  // --- shapes the Rust side serializes (plan §8, §9 M4) -------------------
  type BindingConfig = {
    kind: "key" | "mouse";
    vk: number;
    scan: number;
    mouse_button: string;
    swallow: boolean;
  };
  type Config = {
    version: number;
    enabled: boolean;
    binding: BindingConfig;
    audio: { device_id: string; release_delay_ms: number; on_exit: "restore" | "unmute" };
    sounds: { enabled: boolean; volume: number };
    app: { start_with_windows: boolean; start_hidden: boolean };
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

  let settings = $state<Config | null>(null); // what is on disk
  let form = $state<Config | null>(null); // what the user is editing
  let devices = $state<DeviceInfo[]>([]);
  let status = $state<UiStatus | null>(null);
  let label = $state("");
  let capturing = $state(false);
  let busy = $state(false);
  let notice = $state<string | null>(null);

  let dirty = $derived(
    !!form && !!settings && JSON.stringify(form) !== JSON.stringify(settings),
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

  onDestroy(() => unlisten?.());

  async function labelFor(config: Config): Promise<string> {
    return invoke<string>("binding_label", { settings: clone(config) }).catch(() => "…");
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
      settings = await invoke<Config>("save_settings", { settings: payload });
      form = clone(settings);
      label = await labelFor(form);
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

  /// "Press any key" (plan §9 M4): the hook answers with whatever is
  /// pressed next; the button is disabled meanwhile so only one capture
  /// can be outstanding.
  async function capture() {
    if (!form) return;
    capturing = true;
    try {
      const captured = await invoke<Captured>("capture_binding");
      if ("Key" in captured) {
        form.binding.kind = "key";
        form.binding.vk = captured.Key.vk;
        form.binding.scan = captured.Key.scan;
        form.binding.mouse_button = "";
      } else {
        form.binding.kind = "mouse";
        form.binding.mouse_button = String(captured.Mouse).toLowerCase();
      }
      label = await labelFor(form);
    } catch (error) {
      flash(String(error));
    } finally {
      capturing = false;
    }
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
  {:else if notice}
    <div class="banner">{notice}</div>
  {/if}

  {#if form}
    <section>
      <h2>Hotkey</h2>
      <div class="row">
        <span class="binding">{label}</span>
        <button onclick={capture} disabled={capturing || busy}>
          {capturing ? "Press any key or button…" : "Change"}
        </button>
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={form.binding.swallow} />
        Also block the key from other applications
      </label>
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
