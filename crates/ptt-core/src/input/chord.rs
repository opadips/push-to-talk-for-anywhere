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
}
