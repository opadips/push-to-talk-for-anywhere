// The keys the on-screen keyboard offers (hotkey picker, see
// VirtualKeyboard.svelte).
//
// Every key carries its Windows virtual-key code — the number the low-level
// hook reports as `vkCode` and `config.toml` stores as `vk` (the same value
// `KeyboardEvent.keyCode` gives the "press any key" capture). Picking a key
// here therefore produces exactly the binding that pressing it would.
//
// Letters and digits are identified by VK, so on a non-US layout the caption
// still names the key that *types* that letter. Punctuation keys are the
// exception: their VK is layout dependent (`VK_OEM_1` … `VK_OEM_7`), so their
// captions are the US-layout glyphs and the picker says so.

export type KeyDef = {
  /// Windows virtual-key code.
  vk: number;
  /// What is printed on the key cap (short).
  label: string;
  /// Spoken / tooltip name when the cap alone is not enough.
  name?: string;
  /// Width in key units (1 = a letter key). Defaults to 1.
  w?: number;
};

/// Empty space between keys, in key units.
export type Spacer = { gap: number };

export type Cell = KeyDef | Spacer;

export function isSpacer(cell: Cell): cell is Spacer {
  return "gap" in cell;
}

/// Keys the picker shows but does not let you pick: the two Windows keys. A
/// tap on them opens the Start menu and they carry system shortcuts.
///
/// Shift, Ctrl and Alt *can* be picked here — always as the left/right
/// variant (0xA0–0xA5), which is what the low-level hook reports; the generic
/// 0x10–0x12 codes never reach it, so a binding to one would never fire.
/// ("Press any key" still skips all modifiers, so a shortcut chord cannot
/// bind one by accident.)
export function isBlockedVk(vk: number): boolean {
  return vk === 0x5b || vk === 0x5c;
}

/// Left/right Shift, Ctrl and Alt. Bound with "Also block the key from other
/// applications" on, such a key would stop working in every program.
export function isShiftCtrlAltVk(vk: number): boolean {
  return vk >= 0xa0 && vk <= 0xa5;
}

const k = (vk: number, label: string, w = 1, name?: string): KeyDef => ({ vk, label, w, name });
const gap = (width: number): Spacer => ({ gap: width });
/// Consecutive virtual-key codes starting at `from`, one per caption.
const run = (from: number, labels: string[]): KeyDef[] =>
  labels.map((label, i) => k(from + i, label));

const letters = (text: string): KeyDef[] =>
  [...text].map((ch) => k(ch.charCodeAt(0), ch));

/// Rows of the main block plus the navigation cluster — an ANSI
/// tenkeyless board, 18.5 units wide. Every row sums to the same width so
/// the keys line up in a grid (see the layout in VirtualKeyboard.svelte).
export const MAIN_ROWS: Cell[][] = [
  [
    k(0x1b, "Esc", 1, "Escape"),
    gap(1),
    ...run(0x70, ["F1", "F2", "F3", "F4"]),
    gap(0.5),
    ...run(0x74, ["F5", "F6", "F7", "F8"]),
    gap(0.5),
    ...run(0x78, ["F9", "F10", "F11", "F12"]),
    gap(0.5),
    k(0x2c, "Prt", 1, "Print Screen"),
    k(0x91, "Scr", 1, "Scroll Lock"),
    k(0x13, "Pse", 1, "Pause"),
  ],
  [
    k(0xc0, "`", 1, "OEM 3 (` on a US keyboard)"),
    ...letters("1234567890"),
    k(0xbd, "-", 1, "Minus"),
    k(0xbb, "=", 1, "Equals"),
    k(0x08, "Backspace", 2),
    gap(0.5),
    k(0x2d, "Ins", 1, "Insert"),
    k(0x24, "Home"),
    k(0x21, "PgUp", 1, "Page Up"),
  ],
  [
    k(0x09, "Tab", 1.5),
    ...letters("QWERTYUIOP"),
    k(0xdb, "[", 1, "OEM 4 ([ on a US keyboard)"),
    k(0xdd, "]", 1, "OEM 6 (] on a US keyboard)"),
    k(0xdc, "\\", 1.5, "OEM 5 (\\ on a US keyboard)"),
    gap(0.5),
    k(0x2e, "Del", 1, "Delete"),
    k(0x23, "End"),
    k(0x22, "PgDn", 1, "Page Down"),
  ],
  [
    k(0x14, "Caps", 1.75, "Caps Lock"),
    ...letters("ASDFGHJKL"),
    k(0xba, ";", 1, "OEM 1 (; on a US keyboard)"),
    k(0xde, "'", 1, "OEM 7 (' on a US keyboard)"),
    k(0x0d, "Enter", 2.25),
  ],
  [
    k(0xa0, "Shift", 2.25, "Left Shift"),
    ...letters("ZXCVBNM"),
    k(0xbc, ",", 1, "Comma"),
    k(0xbe, ".", 1, "Period"),
    k(0xbf, "/", 1, "OEM 2 (/ on a US keyboard)"),
    k(0xa1, "Shift", 2.75, "Right Shift"),
    gap(1.5),
    k(0x26, "↑", 1, "Up Arrow"),
  ],
  [
    k(0xa2, "Ctrl", 1.25, "Left Ctrl"),
    k(0x5b, "Win", 1.25, "Left Windows"),
    k(0xa4, "Alt", 1.25, "Left Alt"),
    k(0x20, "Space", 6.25),
    k(0xa5, "Alt", 1.25, "Right Alt"),
    k(0x5c, "Win", 1.25, "Right Windows"),
    k(0x5d, "Menu", 1.25, "Context Menu"),
    k(0xa3, "Ctrl", 1.25, "Right Ctrl"),
    gap(0.5),
    k(0x25, "←", 1, "Left Arrow"),
    k(0x28, "↓", 1, "Down Arrow"),
    k(0x27, "→", 1, "Right Arrow"),
  ],
];

/// Width of every row of [`MAIN_ROWS`], in key units.
export const MAIN_UNITS = 18.5;

/// A numpad key and where it sits on the 4-column numpad grid.
export type PadKey = KeyDef & { col: number; row: number; colSpan?: number; rowSpan?: number };

const pad = (
  vk: number,
  label: string,
  col: number,
  row: number,
  extra: Partial<PadKey> = {},
): PadKey => ({ vk, label, col, row, ...extra });

export const NUMPAD: PadKey[] = [
  pad(0x90, "Num", 1, 1, { name: "Num Lock" }),
  pad(0x6f, "/", 2, 1, { name: "Numpad /" }),
  pad(0x6a, "*", 3, 1, { name: "Numpad *" }),
  pad(0x6d, "-", 4, 1, { name: "Numpad -" }),
  pad(0x67, "7", 1, 2, { name: "Numpad 7" }),
  pad(0x68, "8", 2, 2, { name: "Numpad 8" }),
  pad(0x69, "9", 3, 2, { name: "Numpad 9" }),
  pad(0x6b, "+", 4, 2, { name: "Numpad +", rowSpan: 2 }),
  pad(0x64, "4", 1, 3, { name: "Numpad 4" }),
  pad(0x65, "5", 2, 3, { name: "Numpad 5" }),
  pad(0x66, "6", 3, 3, { name: "Numpad 6" }),
  pad(0x61, "1", 1, 4, { name: "Numpad 1" }),
  pad(0x62, "2", 2, 4, { name: "Numpad 2" }),
  pad(0x63, "3", 3, 4, { name: "Numpad 3" }),
  // The numpad's Enter reports the same virtual-key code as the main one:
  // binding either binds both.
  pad(0x0d, "Enter", 4, 4, { name: "Enter (same key code as the main Enter)", rowSpan: 2 }),
  pad(0x60, "0", 1, 5, { name: "Numpad 0", colSpan: 2 }),
  pad(0x6e, ".", 3, 5, { name: "Numpad ." }),
];
