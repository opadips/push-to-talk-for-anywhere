<script lang="ts">
  // The on-screen keyboard for the hotkey: click the key you want instead of
  // pressing it ("press any key" stays the way to bind a mouse button, or a
  // key the keyboard on hand does not have). An ordinary key reports its
  // virtual-key code through `onpick` and the dialog closes; a modifier key
  // (Shift, Ctrl, Alt, Win) arms its role instead, and the next ordinary key
  // completes a combination (`Ctrl+N`) — or, with two or more modifiers
  // armed, "Use this combination" binds them on their own (`Ctrl+Shift`).
  // One click reports one complete binding: the picker takes no part in the
  // settings page's press-and-hold capture. Nothing is saved until the
  // settings window's Save.
  import { onMount } from "svelte";
  import {
    MAIN_ROWS,
    MAIN_UNITS,
    NUMPAD,
    isBlockedVk,
    isSpacer,
    modifierRole,
    type ChordRole,
    type ChordSide,
    type KeyDef,
    type PickedBinding,
  } from "./keyboardLayout";

  let {
    selectedVk = null,
    currentLabel = "",
    target = "ptt",
    onpick,
    onclose,
  }: {
    /// The bound key, highlighted on the board. `null` when a mouse button
    /// (or nothing) is bound.
    selectedVk?: number | null;
    /// What the binding is called today, for the footer.
    currentLabel?: string;
    /// Which row opened the picker: the toggle-key copy names its binding.
    target?: "ptt" | "toggle";
    onpick: (picked: PickedBinding) => void;
    onclose: () => void;
  } = $props();

  let tab = $state<"keyboard" | "numpad">("keyboard");
  let hovered = $state<KeyDef | null>(null);
  let dialog: HTMLDivElement | undefined;

  // Captions and join order of the four chord modifiers — the same order
  // and spelling `Chord::label` on the Rust side joins them with `+`.
  const ROLE_NAMES: Record<ChordRole, string> = {
    ctrl: "Ctrl",
    shift: "Shift",
    alt: "Alt",
    win: "Win",
  };
  const ROLE_ORDER: ChordRole[] = ["ctrl", "shift", "alt", "win"];

  // Armed modifiers: clicking a modifier key toggles its role here, always
  // as `Any` — which *side* must be held is the settings row's three-way
  // selector, not this dialog's job. Arming is local to the dialog and is
  // discarded with it; a chord is reported complete in one action (a
  // completing key click, or "Use this combination").
  let armed = $state<Record<ChordRole, boolean>>({
    ctrl: false,
    shift: false,
    alt: false,
    win: false,
  });
  let armedRoles = $derived(ROLE_ORDER.filter((role) => armed[role]));
  let armedNames = $derived(armedRoles.map((role) => ROLE_NAMES[role]));
  let armedLabel = $derived(armedNames.join("+"));

  // The grid works in quarter units, so every width in the layout (1.25,
  // 1.75, 2.25 … 6.25) is a whole number of columns and all rows line up.
  const COLS = MAIN_UNITS * 4;
  const span = (units: number) => Math.round(units * 4);

  function pick(key: KeyDef) {
    const role = modifierRole(key.vk);
    // A modifier arms (disarms) its own role and is never the key of the
    // binding, so the engine can never receive a chord whose final member
    // is a modifier — which `Chord::is_valid` rejects anyway.
    if (role) {
      armed[role] = !armed[role];
      return;
    }
    // Defence in depth: the only blocked keys are the Windows keys, and
    // those are modifiers caught above — arming Win is allowed, binding a
    // Windows key on its own never is.
    if (isBlockedVk(key.vk)) return;
    commit(key.vk);
  }

  // One click, one report: the chord the armed modifiers plus this key
  // make — or the plain key alone when nothing is armed, exactly today's
  // behaviour. The page has no scan code (stored but never matched on).
  function commit(vk: number) {
    onpick(
      armedRoles.length > 0
        ? { Chord: { modifiers: chordModifiers(), key: { Key: { vk, scan: 0 } } } }
        : { Key: { vk, scan: 0 } },
    );
  }

  // A modifier-only chord: at least two armed modifiers and no key — the
  // engine engages it when the last of them goes down and releases it on
  // the first one let go.
  function commitModifierOnly() {
    onpick({ Chord: { modifiers: chordModifiers(), key: null } });
  }

  // Armed is `Any` — the combination fires whichever side of the modifier
  // is held. Unarmed is `Off` — the role must not be held at all.
  function chordModifiers(): Record<ChordRole, ChordSide> {
    return {
      ctrl: armed.ctrl ? "any" : "off",
      shift: armed.shift ? "any" : "off",
      alt: armed.alt ? "any" : "off",
      win: armed.win ? "any" : "off",
    };
  }

  function switchTab(next: "keyboard" | "numpad") {
    tab = next;
    hovered = null; // the key under the pointer is gone with the old tab
  }

  function nameOf(key: KeyDef): string {
    return key.name ?? key.label;
  }

  function describe(key: KeyDef): string {
    const role = modifierRole(key.vk);
    // Both physical sides describe the shared role: arming binds either
    // side, so the tooltip must not promise a side pin this dialog does not
    // set. The cap's `aria-label` still names the physical cap ("Left Ctrl")
    // so a screen-reader user can tell the two apart, as a sighted user does
    // by position.
    if (role) {
      return armed[role]
        ? `${ROLE_NAMES[role]} is armed — click it again to disarm, or click a key to complete the combination`
        : `${ROLE_NAMES[role]} — click to arm it for a combination`;
    }
    return isBlockedVk(key.vk)
      ? `${nameOf(key)} — the Windows keys cannot be used`
      : nameOf(key);
  }

  // Esc closes; Tab stays inside the dialog (it is modal).
  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onclose();
      return;
    }
    if (event.key !== "Tab" || !dialog) return;
    const focusable = [...dialog.querySelectorAll<HTMLElement>("button:not(:disabled)")];
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (event.shiftKey && (active === first || !dialog.contains(active))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (active === last || !dialog.contains(active))) {
      event.preventDefault();
      first.focus();
    }
  }

  onMount(() => {
    // Land on the bound key when it is on the board, else on the first key.
    if (selectedVk !== null && NUMPAD.some((key) => key.vk === selectedVk)) {
      const onMain = MAIN_ROWS.some((row) =>
        row.some((cell) => !isSpacer(cell) && cell.vk === selectedVk),
      );
      if (!onMain) tab = "numpad";
    }
    queueMicrotask(() => {
      const target =
        dialog?.querySelector<HTMLElement>("button.key.selected") ??
        dialog?.querySelector<HTMLElement>("button.key:not(:disabled)");
      target?.focus();
    });
  });
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="backdrop" onclick={(event) => event.target === event.currentTarget && onclose()}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="vk-title"
    bind:this={dialog}
  >
    <header>
      <h2 id="vk-title">{target === "toggle" ? "Choose the toggle key" : "Choose the hotkey"}</h2>
      <button class="close" onclick={onclose} aria-label="Close without choosing">✕</button>
    </header>

    <div class="tabs" role="tablist" aria-label="Keyboard area">
      <button
        role="tab"
        aria-selected={tab === "keyboard"}
        class:active={tab === "keyboard"}
        onclick={() => switchTab("keyboard")}>Keyboard</button
      >
      <button
        role="tab"
        aria-selected={tab === "numpad"}
        class:active={tab === "numpad"}
        onclick={() => switchTab("numpad")}>Numpad</button
      >
    </div>

    <div class="stage">
    {#if tab === "keyboard"}
      <div class="board" role="group" aria-label="Keyboard">
        {#each MAIN_ROWS as row, r (r)}
          <div class="row" style:grid-template-columns="repeat({COLS}, 1fr)">
            {#each row as cell, c (c)}
              {#if isSpacer(cell)}
                <span style:grid-column="span {span(cell.gap)}" aria-hidden="true"></span>
              {:else}
                {@render keycap(cell, span(cell.w ?? 1))}
              {/if}
            {/each}
          </div>
        {/each}
      </div>
    {:else}
      <div class="pad" role="group" aria-label="Numeric keypad">
        {#each NUMPAD as key (key.vk + ":" + key.label)}
          <div
            class="cell"
            style:grid-column="{key.col} / span {key.colSpan ?? 1}"
            style:grid-row="{key.row} / span {key.rowSpan ?? 1}"
          >
            {@render keycap(key, 0)}
          </div>
        {/each}
      </div>
      <p class="note">
        With Num Lock off these keys act as Home, End, arrows and so on — pick those on the
        Keyboard tab instead.
      </p>
    {/if}
    </div>

    {#if armed.win}
      <p class="warn">
        Windows and the shell own many Win+ shortcuts and may act on the combination too —
        we cannot stop that.
      </p>
    {/if}

    <footer>
      <span class="status" aria-live="polite">
        {#if hovered}
          {describe(hovered)}
        {:else if armedNames.length > 0}
          {armedLabel} armed — click a key to bind it with {armedLabel}, or click a selected
          modifier again to disarm.
        {:else}
          Click a key to use it as the {target === "toggle" ? "toggle" : "push-to-talk"} key.
        {/if}
      </span>
      {#if armedRoles.length >= 2}
        <button
          class="commit"
          title="Bind {armedLabel} on its own, with no key"
          onclick={commitModifierOnly}
        >
          Use this combination
        </button>
      {/if}
      <span class="current">Now: <strong>{currentLabel || "—"}</strong></span>
    </footer>
    <p class="note">
      Click Shift, Ctrl, Alt or Win to arm it, then click the key that completes the
      combination (Ctrl then N binds “Ctrl+N”); two or more armed modifiers can be bound on
      their own with “Use this combination”. Armed modifiers match either side — pin Left or
      Right afterwards in the hotkey row's selectors. No modifier can be bound on its own from
      here: it arms a combination instead (the Windows keys cannot be bound at all).
      Punctuation keys are drawn for a US keyboard; on other layouts they follow your layout.
      For a mouse button use “Change”.
    </p>
  </div>
</div>

{#snippet keycap(key: KeyDef, columns: number)}
  {@const role = modifierRole(key.vk)}
  <!-- A blocked key is one barred from being the key of a binding — the
       Windows keys. They are chord modifiers, so they arm instead and are
       never disabled; `blocked` only bites for a future key that is neither. -->
  {@const blocked = isBlockedVk(key.vk) && role === null}
  {@const lit = role !== null && armed[role]}
  <div class="slot" style:grid-column={columns ? `span ${columns}` : undefined}>
    <button
      class="key"
      class:selected={key.vk === selectedVk || lit}
      class:blocked
      class:wide={key.label.length > 1}
      disabled={blocked}
      aria-label={nameOf(key)}
      aria-pressed={key.vk === selectedVk || lit}
      title={describe(key)}
      onclick={() => pick(key)}
      onmouseenter={() => (hovered = key)}
      onmouseleave={() => (hovered = null)}
      onfocus={() => (hovered = key)}
      onblur={() => (hovered = null)}
    >
      {key.label}
    </button>
  </div>
{/snippet}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0.75rem;
    background: rgba(8, 9, 11, 0.72);
  }
  .dialog {
    width: 100%;
    max-width: 44rem;
    max-height: 100%;
    overflow: auto;
    box-sizing: border-box;
    background: #191c21;
    border: 1px solid #2c3036;
    border-radius: 12px;
    padding: 0.8rem 0.9rem 0.7rem;
    box-shadow: 0 18px 50px rgba(0, 0, 0, 0.55);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.6rem;
  }
  h2 {
    margin: 0;
    font-size: 1rem;
    font-weight: 600;
    color: #e8eaed;
  }
  .close {
    padding: 0.15rem 0.5rem;
    line-height: 1.2;
  }
  .tabs {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    margin-bottom: 0.65rem;
    background: #14161a;
    border: 1px solid #2c3036;
    border-radius: 8px;
  }
  .tabs button {
    background: transparent;
    border: 0;
    padding: 0.28rem 0.8rem;
    color: #9aa0a6;
  }
  .tabs button.active {
    background: #2b3037;
    color: #e8eaed;
  }

  /* Both tabs get the same height so the dialog does not jump. */
  .stage {
    min-height: 15.5rem;
    display: flex;
    flex-direction: column;
    justify-content: center;
  }

  /* --- the keyboard ---------------------------------------------------- */
  .board {
    container-type: inline-size;
    display: flex;
    flex-direction: column;
  }
  .row {
    display: grid;
    /* One key unit, taken from the width the board actually has. */
    height: calc(100cqw / 18.5);
  }
  .slot {
    padding: 2px;
    min-width: 0;
    display: flex;
  }

  .pad {
    container-type: inline-size;
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    grid-auto-rows: 2.6rem;
    /* Size containment gives the box no intrinsic width, so inside the
       flex `.stage` it must ask for the width explicitly. */
    width: 100%;
    max-width: 18rem;
    margin: 0 auto;
  }
  .pad .cell {
    display: flex;
    min-width: 0;
  }
  .pad .slot {
    flex: 1;
  }

  .key {
    flex: 1;
    min-width: 0;
    padding: 0;
    font: inherit;
    font-size: clamp(0.5rem, 2.6cqw, 0.8rem);
    line-height: 1;
    color: #e8eaed;
    background: #23272d;
    border: 1px solid #343a41;
    border-bottom-width: 2px;
    border-radius: 5px;
    overflow: hidden;
    white-space: nowrap;
    cursor: pointer;
  }
  .pad .key {
    font-size: 1rem;
  }
  .key.wide {
    font-size: clamp(0.4rem, 1.65cqw, 0.7rem);
  }
  .pad .key.wide {
    font-size: 0.8rem;
  }
  .key:hover:not(:disabled) {
    background: #2f353d;
    border-color: #4a525c;
  }
  .key:focus-visible,
  .tabs button:focus-visible,
  .close:focus-visible,
  .commit:focus-visible {
    outline: 2px solid #81c995;
    outline-offset: 1px;
  }
  .key.selected {
    background: #2f6b45;
    border-color: #3d8a59;
    color: #fff;
  }
  .key.blocked {
    color: #5f6368;
    background: #1a1d22;
    border-color: #262a30;
    cursor: not-allowed;
    opacity: 1;
  }

  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin-top: 0.7rem;
    font-size: 0.85rem;
    color: #c9cdd2;
  }
  .status {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .current {
    flex: 0 0 auto;
    color: #9aa0a6;
  }
  .current strong {
    color: #e8eaed;
  }
  .commit {
    flex: 0 0 auto;
    background: #2f6b45;
    border: 1px solid #3d8a59;
    border-radius: 7px;
    padding: 0.3rem 0.7rem;
    font: inherit;
    font-size: 0.82rem;
    color: #fff;
    cursor: pointer;
  }
  .commit:hover {
    background: #377c51;
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
  .note {
    margin: 0.45rem 0 0;
    color: #767c83;
    font-size: 0.75rem;
    line-height: 1.35;
  }
</style>
