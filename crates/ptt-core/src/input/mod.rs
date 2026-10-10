//! Keyboard/mouse input abstraction (plan §4) plus the low-level hook
//! implementation (plan §7).
//!
//! The hook only ever compares against the one bound input — it never records
//! or forwards other keystrokes (plan §11).

#[cfg(windows)]
pub mod hook;

pub mod chord;

use crate::error::{Error, Result};
use std::sync::mpsc::{Receiver, Sender};

/// A mouse button that can be bound (plan §1: including the side buttons).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl MouseButton {
    /// Parse the `config.toml` / CLI spelling of a button (plan §8).
    pub fn from_name(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "middle" => Ok(Self::Middle),
            "x1" => Ok(Self::X1),
            "x2" => Ok(Self::X2),
            _ => Err(Error::InvalidMouseButton(name.to_string())),
        }
    }

    /// The name `config.toml` stores (plan §8).
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
            Self::X1 => "x1",
            Self::X2 => "x2",
        }
    }
}

/// The one input the app listens for (plan §4).
///
/// `Serialize`/`Deserialize` are for the settings window: the UI both reads
/// the bound input and receives the one "press any key" captured (plan §9
/// M4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Binding {
    /// `vk` is the virtual-key code, `scan` the scan code (both are recorded
    /// so the binding survives layout details, plan §8).
    Key {
        vk: u16,
        scan: u16,
    },
    Mouse(MouseButton),
    /// A key combination: modifier roles plus at most one final key or mouse
    /// button, or two or more modifiers alone (`Ctrl+Shift`, spec §3).
    /// `Copy` like the rest, because [`Binding`] flows through `Option`,
    /// `Sender` and exhaustive `match`es.
    Chord(chord::Chord),
}

impl Binding {
    /// Keyboard hook comparison — **the only comparison the hook performs**
    /// (plan §11: nothing else is ever recorded or stored).
    ///
    /// Both sides of the comparison are normalised to the modifier *role*
    /// (`modifier_role_vk`), so the result does not depend on which form the
    /// hook reports: a bound left/right modifier (`0xA0..=0xA5`, what the
    /// on-screen keyboard offers) fires on the generic `0x10`/`0x11`/`0x12`
    /// press (spec §1, last row), and equally on its own side-specific code
    /// if the hook ever reports that; a generic `0x10`-`0x12` binding fires
    /// on either side. Two codes of the same role deliberately *do* match
    /// (`0xA2` Left Ctrl and `0xA3` Right Ctrl both normalise to `0x11`, so
    /// either side fires a Ctrl binding). Non-modifier codes pass through
    /// unchanged on both sides, and different roles still never match
    /// (`0xA2` Ctrl vs `0xA4` Alt normalise to `0x11` vs `0x12`).
    pub fn matches_key(&self, vk: u16) -> bool {
        match self {
            Self::Key { vk: bound, .. } => modifier_role_vk(*bound) == modifier_role_vk(vk),
            // A chord is a set of inputs, matched only by the chord
            // matcher — never by one key or button (spec §3).
            Self::Mouse(_) | Self::Chord(_) => false,
        }
    }

    /// Mouse hook comparison — likewise limited to the bound button.
    pub fn matches_mouse(&self, button: MouseButton) -> bool {
        match self {
            Self::Mouse(bound) => *bound == button,
            // A chord is matched only by the chord matcher (spec §3).
            Self::Key { .. } | Self::Chord(_) => false,
        }
    }

    /// Human-readable form for CLI output and the settings UI.
    pub fn describe(&self) -> String {
        match self {
            Self::Key { vk, .. } => format!("key {vk:#04x}"),
            Self::Mouse(button) => format!("mouse {}", button.name()),
            Self::Chord(chord) => chord.describe(),
        }
    }

    /// The name the settings window shows for what the user just pressed
    /// (plan §9 M4's "press any key"): `Caps Lock`, `Mouse button 4 (back)`.
    /// [`Binding::describe`] stays the terse form the CLI prints.
    pub fn label(&self) -> String {
        match self {
            Self::Key { vk, .. } => key_name(*vk),
            Self::Mouse(button) => button_label(*button),
            Self::Chord(chord) => chord.label(),
        }
    }

    /// `kind` field of `[binding]` in `config.toml` (plan §8).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Key { .. } => "key",
            Self::Mouse(_) => "mouse",
            Self::Chord(_) => "chord",
        }
    }

    /// `mouse_button` field of `[binding]` in `config.toml` (plan §8).
    pub fn button_name(&self) -> Option<&'static str> {
        match self {
            Self::Key { .. } => None,
            Self::Mouse(button) => Some(button.name()),
            Self::Chord(chord) => match chord.key {
                Some(chord::ChordKey::Mouse(button)) => Some(button.name()),
                _ => None,
            },
        }
    }

    /// `vk` field of `[binding]` in `config.toml` (plan §8).
    pub fn vk(&self) -> Option<u16> {
        match self {
            Self::Key { vk, .. } => Some(*vk),
            Self::Mouse(_) => None,
            Self::Chord(chord) => match chord.key {
                Some(chord::ChordKey::Key { vk, .. }) => Some(vk),
                _ => None,
            },
        }
    }

    /// A bare Shift/Ctrl/Alt/Win is skipped by "press any key" (plan §9 M4),
    /// so a shortcut chord cannot bind its first key by accident; it waits
    /// for a real key. It is only about *capturing*: the settings window's
    /// on-screen keyboard can still bind the left/right Shift, Ctrl and Alt
    /// keys (`0xA0..=0xA5`) on purpose. Which form the hook actually reports
    /// for them does not matter — [`Binding::matches_key`] normalises both
    /// sides to the modifier role. Mouse buttons are always bindable.
    pub fn is_modifier(&self) -> bool {
        match self {
            // Shift, Control, Menu/Alt, LWin, RWin and their extended forms.
            Self::Key { vk, .. } => matches!(*vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5),
            // A chord is a shortcut, not a modifier (spec §3).
            Self::Mouse(_) | Self::Chord(_) => false,
        }
    }
}

/// The modifier-role virtual-key code a left/right modifier key and its
/// generic form share: `VK_LSHIFT`…`VK_RALT` (`0xA0..=0xA5`) and the generic
/// `VK_SHIFT`/`VK_CONTROL`/`VK_MENU` (`0x10`/`0x11`/`0x12`) normalise to the
/// same value, so [`Binding::matches_key`] applies it to *both* the bound
/// code and the reported one and never has to guess which form the hook
/// sends. Anything else passes through untouched.
fn modifier_role_vk(vk: u16) -> u16 {
    match vk {
        0xA0 | 0xA1 => 0x10, // Left/Right Shift
        0xA2 | 0xA3 => 0x11, // Left/Right Ctrl
        0xA4 | 0xA5 => 0x12, // Left/Right Alt
        other => other,
    }
}

/// Virtual-key name for [`Binding::label`] (plan §9 M4 shows what the user
/// pressed). Unknown codes fall back to the hex code the CLI prints.
pub(crate) fn key_name(vk: u16) -> String {
    match vk {
        0x08 => "Backspace".to_string(),
        0x09 => "Tab".to_string(),
        0x0D => "Enter".to_string(),
        0x10 => "Shift".to_string(),
        0x11 => "Ctrl".to_string(),
        0x12 => "Alt".to_string(),
        0x13 => "Pause".to_string(),
        0x14 => "Caps Lock".to_string(),
        0x1B => "Escape".to_string(),
        0x20 => "Space".to_string(),
        0x21 => "Page Up".to_string(),
        0x22 => "Page Down".to_string(),
        0x23 => "End".to_string(),
        0x24 => "Home".to_string(),
        0x25 => "Left Arrow".to_string(),
        0x26 => "Up Arrow".to_string(),
        0x27 => "Right Arrow".to_string(),
        0x28 => "Down Arrow".to_string(),
        0x2C => "Print Screen".to_string(),
        0x2D => "Insert".to_string(),
        0x2E => "Delete".to_string(),
        0x5B | 0x5C => "Windows".to_string(),
        0x5D => "Context Menu".to_string(),
        0x90 => "Num Lock".to_string(),
        0x91 => "Scroll Lock".to_string(),
        // Left/right modifiers — bindable (`0xA0..=0xA5`, as the on-screen
        // keyboard offers); `matches_key` normalises both sides, so a binding
        // on either side fires whichever side is pressed. They have names.
        0xA0 => "Left Shift".to_string(),
        0xA1 => "Right Shift".to_string(),
        0xA2 => "Left Ctrl".to_string(),
        0xA3 => "Right Ctrl".to_string(),
        0xA4 => "Left Alt".to_string(),
        0xA5 => "Right Alt".to_string(),
        // Punctuation. `=` `,` `-` `.` are the same key on every layout; the
        // OEM 1–7 codes are *layout dependent*, so they are named by number
        // with the US-keyboard glyph as a hint, never by the glyph alone.
        0xBB => "Equals (=)".to_string(),
        0xBC => "Comma (,)".to_string(),
        0xBD => "Minus (-)".to_string(),
        0xBE => "Period (.)".to_string(),
        0xBA => "OEM 1 (; on US)".to_string(),
        0xBF => "OEM 2 (/ on US)".to_string(),
        0xC0 => "OEM 3 (` on US)".to_string(),
        0xDB => "OEM 4 ([ on US)".to_string(),
        0xDC => "OEM 5 (\\ on US)".to_string(),
        0xDD => "OEM 6 (] on US)".to_string(),
        0xDE => "OEM 7 (' on US)".to_string(),
        // Digits, letters, numpad digits and function keys.
        0x30..=0x39 => ((b'0' + (vk - 0x30) as u8) as char).to_string(),
        0x41..=0x5A => ((b'A' + (vk - 0x41) as u8) as char).to_string(),
        0x60..=0x69 => format!("Numpad {}", vk - 0x60),
        0x6A => "Numpad *".to_string(),
        0x6B => "Numpad +".to_string(),
        0x6D => "Numpad -".to_string(),
        0x6E => "Numpad .".to_string(),
        0x6F => "Numpad /".to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => format!("Key {vk:#04x}"),
    }
}

/// The settings-window name of a mouse button for [`Binding::label`] and
/// [`chord::Chord::label`] (plan §9 M4's "press any key": the wording stays
/// identical wherever a mouse button is named).
pub(crate) fn button_label(button: MouseButton) -> String {
    match button {
        MouseButton::Left => "Left mouse button".to_string(),
        MouseButton::Right => "Right mouse button".to_string(),
        MouseButton::Middle => "Middle mouse button".to_string(),
        MouseButton::X1 => "Mouse button 4 (back)".to_string(),
        MouseButton::X2 => "Mouse button 5 (forward)".to_string(),
    }
}

/// Press / release of the bound input (plan §4), plus a press of the
/// optional toggle input — a toggle has no `ToggleUp`: it works on the edge
/// of the press, and the debounce latch lives in the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    BindingDown,
    BindingUp,
    /// A press of the optional toggle binding (debounced: one per press).
    ToggleDown,
}

/// Where bound input comes from (plan §4).
///
/// Windows implements this with `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks
/// ([`hook`]); tests use a fake.
pub trait InputSource {
    /// Start listening; events go to `tx`. With `swallow`, the bound input is
    /// consumed so it never reaches other applications (plan §1). `toggle` is
    /// the optional second binding as `(binding, swallow)`: while it is
    /// `Some`, its presses are forwarded as [`InputEvent::ToggleDown`]
    /// alongside the bound input (its release is never forwarded).
    fn start(
        &mut self,
        binding: Binding,
        swallow: bool,
        toggle: Option<(Binding, bool)>,
        tx: Sender<InputEvent>,
    ) -> Result<()>;
    /// Point a running source at a new binding — and a new optional toggle —
    /// without a restart (plan §4).
    fn set_binding(&mut self, binding: Binding, swallow: bool, toggle: Option<(Binding, bool)>);
    /// "Press a key to bind" mode: yields the next input and does not forward
    /// it (plan §4).
    fn capture_next(&mut self) -> Receiver<Binding>;
    /// Abandon an outstanding [`InputSource::capture_next`] without waiting
    /// for a press — the settings window answered by itself, or was closed.
    /// Afterwards nothing may keep consuming the user's next key press.
    fn cancel_capture(&mut self) {}
    fn stop(&mut self);
}

#[cfg(test)]
mod tests {
    use super::chord::{Chord, ChordKey, Modifiers, Side};
    use super::*;

    fn caps_lock() -> Binding {
        Binding::Key {
            vk: 0x14,
            scan: 0x3a,
        }
    }

    fn ctrl_n() -> Binding {
        Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        })
    }

    fn ctrl_shift() -> Binding {
        Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                shift: Side::Any,
                ..Default::default()
            },
            key: None,
        })
    }

    // --- matching ---------------------------------------------------------

    #[test]
    fn a_key_binding_matches_only_its_own_vk_code() {
        let binding = caps_lock();
        assert!(binding.matches_key(0x14));
        assert!(!binding.matches_key(0x41), "any other key must not match");
        assert!(
            !binding.matches_mouse(MouseButton::X1),
            "mouse bindings ignore keys"
        );
    }

    #[test]
    fn a_mouse_binding_matches_only_its_own_button() {
        let binding = Binding::Mouse(MouseButton::X1);
        assert!(binding.matches_mouse(MouseButton::X1));
        assert!(!binding.matches_mouse(MouseButton::X2));
        assert!(!binding.matches_mouse(MouseButton::Left));
        assert!(
            !binding.matches_key(0x14),
            "key bindings ignore mouse buttons"
        );
    }

    #[test]
    fn a_left_or_right_modifier_key_matches_the_generic_press() {
        // The on-screen keyboard offers 0xA0..=0xA5 as single keys while the
        // hook may report the generic 0x10/0x11/0x12 for both sides (spec
        // §1's last row). Both sides of the comparison are normalised, so
        // every one of the six codes fires on its role's generic press *and*
        // on its own side-specific code.
        for (vk, generic) in [
            (0xA0, 0x10), // Left Shift
            (0xA1, 0x10), // Right Shift
            (0xA2, 0x11), // Left Ctrl
            (0xA3, 0x11), // Right Ctrl
            (0xA4, 0x12), // Left Alt
            (0xA5, 0x12), // Right Alt
        ] {
            let binding = Binding::Key { vk, scan: 0 };
            assert!(
                binding.matches_key(generic),
                "vk {vk:#x} fires on the generic {generic:#x} press"
            );
            assert!(
                binding.matches_key(vk),
                "vk {vk:#x} also fires if the hook reports its own side-specific code"
            );
            assert!(
                !binding.matches_key(0x41),
                "vk {vk:#x} never fires on a plain key"
            );
        }
    }

    #[test]
    fn a_left_or_right_modifier_key_never_matches_another_roles_press() {
        // Normalising both sides must not make every modifier look alike:
        // each of the six codes only fires on the two codes (its own
        // side-specific one and its role's generic one) of its own role.
        for (vk, own) in [
            (0xA0, 0x10),
            (0xA1, 0x10),
            (0xA2, 0x11),
            (0xA3, 0x11),
            (0xA4, 0x12),
            (0xA5, 0x12),
        ] {
            let binding = Binding::Key { vk, scan: 0 };
            for other in [0x10, 0x11, 0x12] {
                if other == own {
                    continue;
                }
                assert!(
                    !binding.matches_key(other),
                    "vk {vk:#x} must not fire on the other role {other:#x}"
                );
            }
            // The two sides of a *different* role stay different, too.
            let other_side = match vk {
                0xA0 | 0xA1 => 0xA2,
                0xA2 | 0xA3 => 0xA4,
                _ => 0xA0,
            };
            assert!(
                !binding.matches_key(other_side),
                "vk {vk:#x} must not fire on the other role's side-specific {other_side:#x}"
            );
        }
    }

    // --- chords (spec §3: matched by the matcher, never as one input) ------

    #[test]
    fn a_chord_never_matches_as_a_single_key_or_button() {
        let chord = ctrl_n();
        assert!(
            !chord.matches_key(0x4E),
            "the final key alone is not the chord"
        );
        assert!(
            !chord.matches_mouse(MouseButton::Left),
            "nor is any mouse button"
        );
    }

    #[test]
    fn a_chord_is_not_a_bare_modifier() {
        assert!(
            !ctrl_shift().is_modifier(),
            "a chord is a shortcut, not a modifier that cannot be held to talk"
        );
    }

    #[test]
    fn a_chord_reports_the_kind_and_final_vk_the_config_expects() {
        assert_eq!(ctrl_n().kind_name(), "chord");
        assert_eq!(ctrl_n().vk(), Some(0x4E));
        assert_eq!(ctrl_shift().vk(), None, "modifier-only has no final key");
        assert_eq!(
            Binding::Chord(Chord {
                modifiers: Modifiers {
                    ctrl: Side::Any,
                    ..Default::default()
                },
                key: Some(ChordKey::Mouse(MouseButton::Left)),
            })
            .vk(),
            None,
            "a mouse final member has no vk"
        );
    }

    #[test]
    fn a_chord_label_is_what_the_settings_window_shows() {
        assert_eq!(ctrl_n().label(), "Ctrl+N");
    }

    #[test]
    fn a_chord_describes_itself_field_by_field_for_the_cli() {
        assert_eq!(
            ctrl_n().describe(),
            "chord ctrl=any shift=off alt=off win=off key=0x4e"
        );
        assert_eq!(
            ctrl_shift().describe(),
            "chord ctrl=any shift=any alt=off win=off",
            "a modifier-only chord has no key field"
        );
    }

    #[test]
    fn a_chord_reports_a_mouse_button_only_when_it_is_the_final_member() {
        let ctrl_left_mouse = Binding::Chord(Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Mouse(MouseButton::Left)),
        });
        assert_eq!(ctrl_left_mouse.button_name(), Some("left"));
        assert_eq!(
            ctrl_left_mouse.label(),
            "Ctrl+Left mouse button",
            "labels join members with a bare +"
        );
        assert_eq!(
            ctrl_n().button_name(),
            None,
            "a keyed chord has no mouse button"
        );
        assert_eq!(
            ctrl_shift().button_name(),
            None,
            "nor has a modifier-only chord"
        );
    }

    // --- names (config.toml §8, CLI, settings UI) -------------------------

    #[test]
    fn mouse_buttons_parse_from_their_config_names() {
        for (name, expected) in [
            ("left", MouseButton::Left),
            ("right", MouseButton::Right),
            ("middle", MouseButton::Middle),
            ("x1", MouseButton::X1),
            ("x2", MouseButton::X2),
        ] {
            assert_eq!(MouseButton::from_name(name).unwrap(), expected);
        }
        assert_eq!(
            MouseButton::from_name("X1").unwrap(),
            MouseButton::X1,
            "case-insensitive"
        );
    }

    #[test]
    fn an_unknown_mouse_button_is_rejected_with_the_valid_names() {
        let err = MouseButton::from_name("thumb").expect_err("no such button");
        let message = err.to_string();
        for valid in ["left", "right", "middle", "x1", "x2"] {
            assert!(message.contains(valid), "message lists {valid}: {message}");
        }
        assert!(
            message.contains("thumb"),
            "message repeats what was typed: {message}"
        );
    }

    #[test]
    fn bindings_describe_themselves_for_cli_and_settings_output() {
        assert_eq!(caps_lock().describe(), "key 0x14");
        assert_eq!(Binding::Mouse(MouseButton::X2).describe(), "mouse x2");
    }

    // --- config round trip (§8 stores kind/vk/scan/mouse_button/swallow) --

    #[test]
    fn binding_reports_the_kind_and_button_name_the_config_expects() {
        assert_eq!(caps_lock().kind_name(), "key");
        assert_eq!(Binding::Mouse(MouseButton::X1).kind_name(), "mouse");
        assert_eq!(Binding::Mouse(MouseButton::X1).button_name(), Some("x1"));
        assert_eq!(caps_lock().button_name(), None);
        assert_eq!(caps_lock().vk(), Some(0x14));
        assert_eq!(Binding::Mouse(MouseButton::X1).vk(), None);
    }

    #[test]
    fn a_bare_modifier_is_never_a_hold_to_talk_binding() {
        for vk in [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA3, 0xA5] {
            assert!(
                Binding::Key { vk, scan: 0 }.is_modifier(),
                "vk {vk:#x} cannot be held to talk"
            );
        }
        assert!(
            !caps_lock().is_modifier(),
            "Caps Lock is plan §8's own example"
        );
        assert!(!Binding::Key { vk: 0x41, scan: 0 }.is_modifier(), "A");
        assert!(!Binding::Mouse(MouseButton::X1).is_modifier());
    }

    #[test]
    fn labels_are_what_the_settings_window_shows() {
        assert_eq!(caps_lock().label(), "Caps Lock");
        assert_eq!(Binding::Key { vk: 0x41, scan: 0 }.label(), "A");
        assert_eq!(Binding::Key { vk: 0x7B, scan: 0 }.label(), "F12");
        assert_eq!(Binding::Key { vk: 0x65, scan: 0 }.label(), "Numpad 5");
        assert_eq!(
            Binding::Mouse(MouseButton::X1).label(),
            "Mouse button 4 (back)"
        );
        // The on-screen keyboard (ui/src/keyboardLayout.ts) offers these.
        assert_eq!(
            Binding::Key { vk: 0xBF, scan: 0 }.label(),
            "OEM 2 (/ on US)"
        );
        assert_eq!(
            Binding::Key { vk: 0xDC, scan: 0 }.label(),
            "OEM 5 (\\ on US)"
        );
        assert_eq!(Binding::Key { vk: 0xBD, scan: 0 }.label(), "Minus (-)");
        assert_eq!(Binding::Key { vk: 0x6B, scan: 0 }.label(), "Numpad +");
        assert_eq!(Binding::Key { vk: 0xA1, scan: 0 }.label(), "Right Shift");
        // Something the table does not know still says what it is.
        assert_eq!(Binding::Key { vk: 0xE8, scan: 0 }.label(), "Key 0xe8");
    }
}
