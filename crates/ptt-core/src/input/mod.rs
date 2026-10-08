//! Keyboard/mouse input abstraction (plan §4) plus the low-level hook
//! implementation (plan §7).
//!
//! The hook only ever compares against the one bound input — it never records
//! or forwards other keystrokes (plan §11).

#[cfg(windows)]
pub mod hook;

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
}

impl Binding {
    /// Keyboard hook comparison — **the only comparison the hook performs**
    /// (plan §11: nothing else is ever recorded or stored).
    pub fn matches_key(&self, vk: u16) -> bool {
        matches!(self, Self::Key { vk: bound, .. } if *bound == vk)
    }

    /// Mouse hook comparison — likewise limited to the bound button.
    pub fn matches_mouse(&self, button: MouseButton) -> bool {
        matches!(self, Self::Mouse(bound) if *bound == button)
    }

    /// Human-readable form for CLI output and the settings UI.
    pub fn describe(&self) -> String {
        match self {
            Self::Key { vk, .. } => format!("key {vk:#04x}"),
            Self::Mouse(button) => format!("mouse {}", button.name()),
        }
    }

    /// The name the settings window shows for what the user just pressed
    /// (plan §9 M4's "press any key"): `Caps Lock`, `Mouse button 4 (back)`.
    /// [`Binding::describe`] stays the terse form the CLI prints.
    pub fn label(&self) -> String {
        match self {
            Self::Key { vk, .. } => key_name(*vk),
            Self::Mouse(button) => match button {
                MouseButton::Left => "Left mouse button".to_string(),
                MouseButton::Right => "Right mouse button".to_string(),
                MouseButton::Middle => "Middle mouse button".to_string(),
                MouseButton::X1 => "Mouse button 4 (back)".to_string(),
                MouseButton::X2 => "Mouse button 5 (forward)".to_string(),
            },
        }
    }

    /// `kind` field of `[binding]` in `config.toml` (plan §8).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Key { .. } => "key",
            Self::Mouse(_) => "mouse",
        }
    }

    /// `mouse_button` field of `[binding]` in `config.toml` (plan §8).
    pub fn button_name(&self) -> Option<&'static str> {
        match self {
            Self::Key { .. } => None,
            Self::Mouse(button) => Some(button.name()),
        }
    }

    /// `vk` field of `[binding]` in `config.toml` (plan §8).
    pub fn vk(&self) -> Option<u16> {
        match self {
            Self::Key { vk, .. } => Some(*vk),
            Self::Mouse(_) => None,
        }
    }

    /// A bare Shift/Ctrl/Alt/Win cannot be *held* to talk — you need those
    /// fingers for everything else — so "press any key" (plan §9 M4) skips it
    /// and waits for a real key. Mouse buttons are always bindable.
    pub fn is_modifier(&self) -> bool {
        match self {
            // Shift, Control, Menu/Alt, LWin, RWin and their extended forms.
            Self::Key { vk, .. } => matches!(*vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5),
            Self::Mouse(_) => false,
        }
    }
}

/// Virtual-key name for [`Binding::label`] (plan §9 M4 shows what the user
/// pressed). Unknown codes fall back to the hex code the CLI prints.
fn key_name(vk: u16) -> String {
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
        // Digits, letters, numpad digits and function keys.
        0x30..=0x39 => ((b'0' + (vk - 0x30) as u8) as char).to_string(),
        0x41..=0x5A => ((b'A' + (vk - 0x41) as u8) as char).to_string(),
        0x60..=0x69 => format!("Numpad {}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => format!("Key {vk:#04x}"),
    }
}

/// Press / release of the bound input (plan §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    BindingDown,
    BindingUp,
}

/// Where bound input comes from (plan §4).
///
/// Windows implements this with `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks
/// ([`hook`]); tests use a fake.
pub trait InputSource {
    /// Start listening; events go to `tx`. With `swallow`, the bound input is
    /// consumed so it never reaches other applications (plan §1).
    fn start(&mut self, binding: Binding, swallow: bool, tx: Sender<InputEvent>) -> Result<()>;
    fn set_binding(&mut self, binding: Binding, swallow: bool);
    /// "Press a key to bind" mode: yields the next input and does not forward
    /// it (plan §4).
    fn capture_next(&mut self) -> Receiver<Binding>;
    fn stop(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_lock() -> Binding {
        Binding::Key {
            vk: 0x14,
            scan: 0x3a,
        }
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
        // Something the table does not know still says what it is.
        assert_eq!(Binding::Key { vk: 0xE8, scan: 0 }.label(), "Key 0xe8");
    }
}
