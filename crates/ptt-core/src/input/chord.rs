//! Key combinations ("chords"): a set of modifier roles plus at most one
//! final key or mouse button (spec §3), including modifier-only chords such
//! as `Ctrl+Shift`.
//!
//! Everything here is platform-independent on purpose: the low-level hook is
//! `#[cfg(windows)]` glue, so the pure types and rules live here where the
//! host test suite can cover them.

use super::{button_label, key_name, MouseButton};

/// Which side of a modifier a chord asks for (spec §3).
///
/// `Off` is the default so a `Modifiers` built field-by-field never demands a
/// modifier the chord does not use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Nothing of this role may be held.
    #[default]
    Off,
    /// Either side; both sides together also counts.
    Any,
    Left,
    Right,
}

impl Side {
    /// The `config.toml` / CLI spelling, the lowercase word itself.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Any => "any",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// The four modifier roles, each independently pinnable (spec §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Ctrl,
    Shift,
    Alt,
    Win,
}

impl Role {
    /// The bare display name; [`Side::Left`]/[`Side::Right`] prefix it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Shift => "Shift",
            Self::Alt => "Alt",
            Self::Win => "Win",
        }
    }
}

/// The four modifier roles a chord can ask for, all defaulting to `Off`
/// (spec §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Modifiers {
    pub ctrl: Side,
    pub shift: Side,
    pub alt: Side,
    pub win: Side,
}

impl Modifiers {
    /// The roles paired with their requested side, always in the order
    /// Ctrl, Shift, Alt, Win — the order labels, config fields and the
    /// settings UI use.
    pub fn roles(self) -> [(Role, Side); 4] {
        [
            (Role::Ctrl, self.ctrl),
            (Role::Shift, self.shift),
            (Role::Alt, self.alt),
            (Role::Win, self.win),
        ]
    }
}

/// One of the eight physical modifier keys (spec §4): a role plus the side it
/// was pressed on. `Copy`, like everything else in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    LeftCtrl,
    RightCtrl,
    LeftShift,
    RightShift,
    LeftAlt,
    RightAlt,
    LeftWin,
    RightWin,
}

impl Slot {
    /// The bit this slot occupies in [`HeldModifiers`] (spec §4):
    /// `L_CTRL=0, R_CTRL=1, L_SHIFT=2, R_SHIFT=3, L_ALT=4, R_ALT=5,
    /// L_WIN=6, R_WIN=7`.
    fn bit(self) -> u8 {
        match self {
            Self::LeftCtrl => 1 << 0,
            Self::RightCtrl => 1 << 1,
            Self::LeftShift => 1 << 2,
            Self::RightShift => 1 << 3,
            Self::LeftAlt => 1 << 4,
            Self::RightAlt => 1 << 5,
            Self::LeftWin => 1 << 6,
            Self::RightWin => 1 << 7,
        }
    }
}

/// The two slots making up `role`, left first.
fn role_slots(role: Role) -> [Slot; 2] {
    match role {
        Role::Ctrl => [Slot::LeftCtrl, Slot::RightCtrl],
        Role::Shift => [Slot::LeftShift, Slot::RightShift],
        Role::Alt => [Slot::LeftAlt, Slot::RightAlt],
        Role::Win => [Slot::LeftWin, Slot::RightWin],
    }
}

/// Left Shift's make code as `KBDLLHOOKSTRUCT.scanCode` reports it, the side
/// signal for the generic Shift vk (`VK_SHIFT`, `0x10`) alongside [`SCAN_RSHIFT`].
const SCAN_LSHIFT: u16 = 0x2A;

/// Right Shift's make code, likewise a side signal for the generic Shift vk.
const SCAN_RSHIFT: u16 = 0x36;

/// Which physical slot a keyboard event belongs to (spec §4), or `None` for
/// an ordinary, non-modifier key.
///
/// `WH_KEYBOARD_LL` reports the *generic* vk for Ctrl, Alt and Shift, so the
/// side is recovered from `KBDLLHOOKSTRUCT`: `extended` (`LLKHF_EXTENDED`)
/// for Ctrl/Alt, `scan` for Shift, and the vk itself for the two Win keys.
///
/// The side-specific vks `VK_LSHIFT`…`VK_RMENU` (`0xA0..=0xA5`) are accepted
/// as-is: injected events (`SendInput` without a scancode, key remappers, the
/// on-screen keyboard) carry exactly the vk the caller supplies rather than a
/// scan-derived one, and dropping them would make a genuinely held modifier
/// invisible to chord matching.
///
/// A Shift event with any other scan code is recorded as the left side —
/// keeping it tracked beats losing it, since a lost Shift would let a
/// chord bound with `shift = "off"` fire while Shift is held.
pub fn modifier_slot(vk: u16, scan: u16, extended: bool) -> Option<Slot> {
    Some(match vk {
        0x10 => {
            if scan == SCAN_RSHIFT {
                Slot::RightShift
            } else if scan == SCAN_LSHIFT {
                Slot::LeftShift
            } else {
                // Unrecognised scan: track as the left side rather than
                // dropping the event (see the doc comment above).
                Slot::LeftShift
            }
        }
        0x11 => {
            if extended {
                Slot::RightCtrl
            } else {
                Slot::LeftCtrl
            }
        }
        0x12 => {
            if extended {
                Slot::RightAlt
            } else {
                Slot::LeftAlt
            }
        }
        0x5B => Slot::LeftWin,
        0x5C => Slot::RightWin,
        // Side-specific vks: the vk itself already names the side, so neither
        // the scan code nor the extended flag is consulted.
        0xA0 => Slot::LeftShift,
        0xA1 => Slot::RightShift,
        0xA2 => Slot::LeftCtrl,
        0xA3 => Slot::RightCtrl,
        0xA4 => Slot::LeftAlt,
        0xA5 => Slot::RightAlt,
        _ => return None,
    })
}

/// The eight physical modifier slots as a bitfield (spec §4). `Default` is
/// "nothing held"; `Copy` because the matcher passes it by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeldModifiers(u8);

impl HeldModifiers {
    /// Record `slot` as physically down.
    pub fn insert(&mut self, slot: Slot) {
        self.0 |= slot.bit();
    }

    /// Record `slot` as released.
    pub fn remove(&mut self, slot: Slot) {
        self.0 &= !slot.bit();
    }

    /// Whether `slot` is currently down.
    pub fn contains(self, slot: Slot) -> bool {
        self.0 & slot.bit() != 0
    }

    /// Drop both sides of `role`, e.g. when a resync re-derives the generic
    /// role from `GetAsyncKeyState` (spec §2).
    pub fn clear_role(&mut self, role: Role) {
        for slot in role_slots(role) {
            self.remove(slot);
        }
    }
}

/// Exact `Side` matching against the held slots (spec §4): `Off` ⇒ neither
/// side down, `Any` ⇒ at least one (both counts), `Left`/`Right` ⇒ that side
/// down **and** the other not. Hence `Ctrl+Shift+N` never fires a `Ctrl+N`
/// binding — they are different shortcuts.
pub fn role_held(role: Role, side: Side, held: HeldModifiers) -> bool {
    let [left, right] = role_slots(role);
    let left = held.contains(left);
    let right = held.contains(right);
    match side {
        Side::Off => !left && !right,
        Side::Any => left || right,
        Side::Left => left && !right,
        Side::Right => right && !left,
    }
}

/// The non-modifier member of a chord, if the chord has one (spec §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChordKey {
    Key { vk: u16, scan: u16 },
    Mouse(MouseButton),
}

/// A key combination: modifiers plus at most one final key or mouse button
/// (spec §3). `Copy`, because [`super::Binding`] flows through `Option`,
/// `Sender` and exhaustive `match`es.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Chord {
    pub modifiers: Modifiers,
    /// `None` ⇒ a modifier-only chord such as `Ctrl+Shift`.
    pub key: Option<ChordKey>,
}

impl Chord {
    /// A bare single modifier is not a binding — today "press any key"
    /// rejects it ("cannot be held to talk") and it stays rejected (spec §3).
    /// Valid therefore means: there is a final key or mouse button, or at
    /// least two of the four roles are requested.
    pub fn is_valid(&self) -> bool {
        self.key.is_some()
            || self
                .modifiers
                .roles()
                .iter()
                .filter(|(_, side)| *side != Side::Off)
                .count()
                >= 2
    }

    /// What the settings window shows: members joined with `+`, bare role
    /// names for unpinned sides, a `Left `/`Right ` prefix when pinned
    /// (spec §9).
    pub fn label(&self) -> String {
        let mut members: Vec<String> = self
            .modifiers
            .roles()
            .iter()
            .filter_map(|(role, side)| match side {
                Side::Off => None,
                Side::Any => Some(role.name().to_string()),
                Side::Left => Some(format!("Left {}", role.name())),
                Side::Right => Some(format!("Right {}", role.name())),
            })
            .collect();
        if let Some(key) = self.key {
            members.push(match key {
                ChordKey::Key { vk, .. } => key_name(vk),
                ChordKey::Mouse(button) => button_label(button),
            });
        }
        members.join("+")
    }

    /// The terse, field-by-field form the CLI prints, matching
    /// [`super::Binding::describe`].
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = vec!["chord".to_string()];
        for (role, side) in self.modifiers.roles() {
            parts.push(format!(
                "{}={}",
                role.name().to_ascii_lowercase(),
                side.name()
            ));
        }
        match self.key {
            Some(ChordKey::Key { vk, .. }) => parts.push(format!("key={vk:#04x}")),
            Some(ChordKey::Mouse(button)) => parts.push(format!("mouse={}", button.name())),
            None => {}
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- sides (the `config.toml` spelling) -------------------------------

    #[test]
    fn sides_deserialize_to_the_lowercase_config_spelling() {
        for (text, expected) in [
            ("off", Side::Off),
            ("any", Side::Any),
            ("left", Side::Left),
            ("right", Side::Right),
        ] {
            let side: Side = serde_json::from_str(&format!("\"{text}\""))
                .unwrap_or_else(|error| panic!("\"{text}\" parses: {error}"));
            assert_eq!(side, expected, "\"{text}\" means {expected:?}");
            assert_eq!(
                serde_json::to_string(&side).unwrap(),
                format!("\"{text}\""),
                "{side:?} round-trips as \"{text}\""
            );
        }
    }

    // --- validity (spec §3: a bare single modifier is still not a binding) -

    #[test]
    fn a_chord_with_no_members_and_no_modifiers_is_invalid() {
        let chord = Chord {
            modifiers: Modifiers::default(),
            key: None,
        };
        assert!(!chord.is_valid(), "an empty chord binds nothing");
    }

    #[test]
    fn a_single_bare_modifier_is_invalid() {
        let chord = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: None,
        };
        assert!(
            !chord.is_valid(),
            "Ctrl alone is the bare modifier today rejects"
        );
    }

    #[test]
    fn two_modifiers_with_no_key_form_a_valid_modifier_only_chord() {
        let chord = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                shift: Side::Any,
                ..Default::default()
            },
            key: None,
        };
        assert!(chord.is_valid(), "Ctrl+Shift needs no final key");
    }

    #[test]
    fn one_modifier_with_a_key_is_valid() {
        let chord = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        };
        assert!(chord.is_valid());
    }

    // --- labels (what the settings window shows) --------------------------

    #[test]
    fn a_label_shows_bare_role_names_when_no_side_is_pinned() {
        let chord = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        };
        assert_eq!(chord.label(), "Ctrl+N");
    }

    #[test]
    fn a_label_prefixes_a_pinned_side() {
        let right_ctrl = Chord {
            modifiers: Modifiers {
                ctrl: Side::Right,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        };
        assert_eq!(right_ctrl.label(), "Right Ctrl+N");

        let mixed = Chord {
            modifiers: Modifiers {
                ctrl: Side::Left,
                shift: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4D, scan: 48 }),
        };
        assert_eq!(mixed.label(), "Left Ctrl+Shift+M");
    }

    #[test]
    fn a_modifier_only_chord_labels_without_a_key() {
        let chord = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                shift: Side::Any,
                ..Default::default()
            },
            key: None,
        };
        assert_eq!(chord.label(), "Ctrl+Shift");
    }

    // --- modifier slots: left/right out of a generic vk (spec §4) --------

    #[test]
    fn left_and_right_modifiers_are_distinguished_from_a_generic_vk() {
        // Ctrl and Alt carry the side in `LLKHF_EXTENDED`.
        assert_eq!(modifier_slot(0x11, 0, false), Some(Slot::LeftCtrl));
        assert_eq!(modifier_slot(0x11, 0, true), Some(Slot::RightCtrl));
        assert_eq!(modifier_slot(0x12, 0, false), Some(Slot::LeftAlt));
        assert_eq!(modifier_slot(0x12, 0, true), Some(Slot::RightAlt));
        // Shift carries it in the scan code; the extended flag is irrelevant.
        assert_eq!(
            modifier_slot(0x10, SCAN_LSHIFT, false),
            Some(Slot::LeftShift)
        );
        assert_eq!(
            modifier_slot(0x10, SCAN_LSHIFT, true),
            Some(Slot::LeftShift)
        );
        assert_eq!(
            modifier_slot(0x10, SCAN_RSHIFT, false),
            Some(Slot::RightShift)
        );
        assert_eq!(
            modifier_slot(0x10, SCAN_RSHIFT, true),
            Some(Slot::RightShift)
        );
        // The Win keys have distinct vks; RWin also sets the extended flag.
        assert_eq!(modifier_slot(0x5B, 0, false), Some(Slot::LeftWin));
        assert_eq!(modifier_slot(0x5B, 0, true), Some(Slot::LeftWin));
        assert_eq!(modifier_slot(0x5C, 0, false), Some(Slot::RightWin));
        assert_eq!(modifier_slot(0x5C, 0, true), Some(Slot::RightWin));
    }

    #[test]
    fn an_ordinary_key_is_not_a_modifier() {
        assert_eq!(modifier_slot(0x4E, 49, false), None, "N is not a modifier");
        assert_eq!(modifier_slot(0x4E, 49, true), None);
    }

    // --- side-specific modifier vks (injected / remapped / on-screen) ----

    #[test]
    fn side_specific_modifier_vks_map_to_the_same_slot_as_the_generic_form() {
        // `SendInput` without a scancode, key remappers and the on-screen
        // keyboard carry `0xA0..=0xA5` instead of a generic vk plus a
        // scan/extended signal; they must land in the identical slot or the
        // chord never engages.
        for (side_vk, slot, generic) in [
            (0xA0, Slot::LeftShift, (0x10, SCAN_LSHIFT, false)),
            (0xA1, Slot::RightShift, (0x10, SCAN_RSHIFT, false)),
            (0xA2, Slot::LeftCtrl, (0x11, 0, false)),
            (0xA3, Slot::RightCtrl, (0x11, 0, true)),
            (0xA4, Slot::LeftAlt, (0x12, 0, false)),
            (0xA5, Slot::RightAlt, (0x12, 0, true)),
        ] {
            assert_eq!(
                modifier_slot(side_vk, 0, false),
                Some(slot),
                "vk {side_vk:#04x} is {slot:?}"
            );
            assert_eq!(
                modifier_slot(side_vk, 0, true),
                Some(slot),
                "the extended flag never changes a side-specific vk"
            );
            assert_eq!(
                modifier_slot(side_vk, 0, false),
                modifier_slot(generic.0, generic.1, generic.2),
                "vk {side_vk:#04x} agrees with its generic-vk equivalent"
            );
        }
    }

    #[test]
    fn a_side_specific_vk_is_not_overridden_by_a_stray_scan_code() {
        // The vk names the side outright, so a scan code from an unrelated
        // source (or none at all) must not move it.
        assert_eq!(
            modifier_slot(0xA0, SCAN_RSHIFT, false),
            Some(Slot::LeftShift)
        );
        assert_eq!(
            modifier_slot(0xA1, SCAN_LSHIFT, false),
            Some(Slot::RightShift)
        );
    }

    // --- the tracked-Shift fallback (safety, see `modifier_slot`) --------

    #[test]
    fn a_shift_with_an_unrecognised_scan_is_still_tracked_as_left_shift() {
        // Deliberate: dropping such an event would make Shift invisible to
        // chord matching, letting a `shift = "off"` chord fire while Shift is
        // genuinely held. Pinned here so a refactor cannot silently lose it.
        for scan in [0, 0x1D, 0x38, SCAN_LSHIFT + 1, u16::MAX] {
            assert_eq!(
                modifier_slot(0x10, scan, false),
                Some(Slot::LeftShift),
                "scan {scan:#04x} still tracks Shift"
            );
            assert_eq!(
                modifier_slot(0x10, scan, true),
                Some(Slot::LeftShift),
                "the extended flag does not change the fallback"
            );
        }
        // Only the right-hand scan code escapes the fallback.
        assert_eq!(
            modifier_slot(0x10, SCAN_RSHIFT, false),
            Some(Slot::RightShift)
        );
    }

    // --- exact side matching against held slots (spec §4) ----------------

    #[test]
    fn off_is_satisfied_only_when_nothing_of_that_role_is_held() {
        let mut held = HeldModifiers::default();
        assert!(role_held(Role::Ctrl, Side::Off, held), "nothing held");

        held.insert(Slot::LeftCtrl);
        assert!(!role_held(Role::Ctrl, Side::Off, held), "left Ctrl down");
        held.insert(Slot::RightCtrl);
        assert!(!role_held(Role::Ctrl, Side::Off, held), "both down");
        held.remove(Slot::LeftCtrl);
        assert!(!role_held(Role::Ctrl, Side::Off, held), "right still down");
        held.remove(Slot::RightCtrl);
        assert!(role_held(Role::Ctrl, Side::Off, held), "released again");

        // Other roles never satisfy or spoil this one.
        held.insert(Slot::LeftShift);
        assert!(role_held(Role::Ctrl, Side::Off, held), "Shift is not Ctrl");
        assert!(
            !role_held(Role::Shift, Side::Off, held),
            "but Shift is held"
        );
        held.clear_role(Role::Shift);
        assert!(
            !held.contains(Slot::LeftShift),
            "clear_role empties the role"
        );
        assert!(
            role_held(Role::Shift, Side::Off, held),
            "Shift is clear again"
        );
    }

    #[test]
    fn any_is_satisfied_by_either_side_or_both() {
        let mut held = HeldModifiers::default();
        assert!(!role_held(Role::Alt, Side::Any, held), "nothing held");

        held.insert(Slot::LeftAlt);
        assert!(role_held(Role::Alt, Side::Any, held), "left Alt down");
        held.remove(Slot::LeftAlt);
        held.insert(Slot::RightAlt);
        assert!(role_held(Role::Alt, Side::Any, held), "right Alt down");
        held.insert(Slot::LeftAlt);
        assert!(role_held(Role::Alt, Side::Any, held), "both sides together");
    }

    #[test]
    fn a_pinned_side_is_satisfied_by_that_side_alone() {
        let mut held = HeldModifiers::default();
        assert!(!role_held(Role::Ctrl, Side::Left, held), "no Ctrl held");

        held.insert(Slot::LeftCtrl);
        assert!(role_held(Role::Ctrl, Side::Left, held), "left Ctrl alone");
        assert!(
            !role_held(Role::Ctrl, Side::Right, held),
            "left is not right"
        );
        held.insert(Slot::RightShift);
        assert!(
            role_held(Role::Ctrl, Side::Left, held),
            "another role's side is irrelevant"
        );

        let mut held = HeldModifiers::default();
        held.insert(Slot::RightCtrl);
        assert!(role_held(Role::Ctrl, Side::Right, held), "right Ctrl alone");
        assert!(
            !role_held(Role::Ctrl, Side::Left, held),
            "right is not left"
        );
    }

    #[test]
    fn a_pinned_side_is_not_satisfied_when_the_other_side_is_also_held() {
        let mut held = HeldModifiers::default();
        held.insert(Slot::LeftCtrl);
        held.insert(Slot::RightCtrl);
        assert!(
            !role_held(Role::Ctrl, Side::Left, held),
            "both Ctrl slots down is not a left pin"
        );
        assert!(
            !role_held(Role::Ctrl, Side::Right, held),
            "both Ctrl slots down is not a right pin"
        );
        assert!(
            role_held(Role::Ctrl, Side::Any, held),
            "Any still accepts it"
        );

        let mut held = HeldModifiers::default();
        held.insert(Slot::LeftShift);
        held.insert(Slot::RightShift);
        assert!(!role_held(Role::Shift, Side::Left, held), "same for Shift");
        assert!(!role_held(Role::Shift, Side::Right, held), "same for Shift");
    }
}
