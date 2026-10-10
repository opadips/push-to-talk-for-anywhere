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

    /// The modifier role this slot belongs to.
    fn role(self) -> Role {
        match self {
            Self::LeftCtrl | Self::RightCtrl => Role::Ctrl,
            Self::LeftShift | Self::RightShift => Role::Shift,
            Self::LeftAlt | Self::RightAlt => Role::Alt,
            Self::LeftWin | Self::RightWin => Role::Win,
        }
    }

    /// Whether this slot is the role's left side.
    fn is_left(self) -> bool {
        matches!(
            self,
            Self::LeftCtrl | Self::LeftShift | Self::LeftAlt | Self::LeftWin
        )
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

/// Whether `vk` is one of the modifier virtual keys, generic and
/// side-specific alike — exactly the set [`modifier_slot`] maps to a
/// [`Slot`].
fn is_modifier_vk(vk: u16) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

impl Chord {
    /// A bare single modifier is not a binding — today "press any key"
    /// rejects it ("cannot be held to talk") and it stays rejected (spec §3).
    /// Valid therefore means: there is a final key or mouse button, or at
    /// least two of the four roles are requested.
    ///
    /// A final key that is *itself* a modifier vk (`Ctrl+Ctrl`) is rejected
    /// outright: such a chord would swallow both the press and the release of
    /// a modifier key, breaking `Ctrl+C` everywhere while it is bound. The
    /// check lives here so every producer — config, CLI, capture, the
    /// on-screen keyboard — inherits the guarantee for free.
    pub fn is_valid(&self) -> bool {
        if matches!(self.key, Some(ChordKey::Key { vk, .. }) if is_modifier_vk(vk)) {
            return false;
        }
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

/// A keyboard event as the matcher consumes it (spec §5). `extended` is the
/// `LLKHF_EXTENDED` flag; together with `vk` and `scan` it lets
/// [`modifier_slot`] recover which physical side of a modifier moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Down { vk: u16, scan: u16, extended: bool },
    Up { vk: u16, scan: u16, extended: bool },
}

/// What one event did to one chord binding (spec §5). An event that changes
/// nothing for the binding is not an outcome at all: [`ChordMatcher::step`]
/// returns `None` and the hook forwards the event untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The chord became active — the edge that emits `BindingDown` /
    /// `ToggleDown`. Auto-repeat repeats the *down*, never this edge, so
    /// one physical press yields exactly one engagement.
    Engaged { swallow: bool },
    /// The chord ended: a member was released.
    Released { swallow: bool },
}

/// The last event the matcher was told about, key or mouse (spec §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Note {
    Key {
        down: bool,
        vk: u16,
        /// The physical modifier slot this event belongs to, already
        /// derived by [`modifier_slot`] in [`ChordMatcher::note_key`].
        slot: Option<Slot>,
    },
    Mouse {
        down: bool,
        button: MouseButton,
    },
}

impl Note {
    fn is_down(self) -> bool {
        match self {
            Self::Key { down, .. } | Self::Mouse { down, .. } => down,
        }
    }
}

/// Per-binding activation state (spec §5): one instance per chord binding,
/// so a PTT chord and a toggle chord can be active side by side without
/// interfering — the matcher itself stays shared and holds only the held
/// modifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChordState {
    active: bool,
    /// A modifier-only chord remembers which modifier completed it: that
    /// one's press was swallowed, so its release must be swallowed too,
    /// while the other members' releases are forwarded (spec §5, "Swallow").
    completing: Option<Slot>,
    /// The chord ended while its final key or button was still held (a
    /// modifier was released first). Until that member is seen released,
    /// nothing re-engages the chord: neither the held key's auto-repeat nor
    /// a re-pressed modifier may resume talking mid-press (spec §5 rule 3).
    blocked: bool,
}

impl ChordState {
    /// Forget everything: no chord is active, blocked or mid-completion.
    ///
    /// This is the recovery path for a lost release event. Windows *does*
    /// drop low-level hooks, so the release that would have cleared
    /// [`ChordState::blocked`] may never arrive — the chord would then be
    /// dead until a restart or a rebind. It is meant to be called on session
    /// start and after rehooking, together with
    /// [`ChordMatcher::clear_last`] and a `GetAsyncKeyState` resync of the
    /// held modifiers.
    pub fn reset(&mut self) {
        self.active = false;
        self.blocked = false;
        self.completing = None;
    }
}

/// The pure chord state machine (spec §5), driven by the hook: the hook
/// calls [`ChordMatcher::note_key`] / [`ChordMatcher::note_mouse`] for every
/// input event — so modifier state is correct the moment a chord is bound —
/// and then [`ChordMatcher::step`] once per chord binding.
#[derive(Debug, Clone, Default)]
pub struct ChordMatcher {
    held: HeldModifiers,
    last: Option<Note>,
}

impl ChordMatcher {
    /// Record a keyboard event. Modifier slots are updated *before* the event
    /// is noted, so a [`ChordMatcher::step`] that follows sees the held state
    /// as it is after this press or release.
    pub fn note_key(&mut self, ev: KeyEvent) {
        let (down, vk, scan, extended) = match ev {
            KeyEvent::Down { vk, scan, extended } => (true, vk, scan, extended),
            KeyEvent::Up { vk, scan, extended } => (false, vk, scan, extended),
        };
        let slot = modifier_slot(vk, scan, extended);
        if let Some(slot) = slot {
            if down {
                self.held.insert(slot);
            } else {
                self.held.remove(slot);
            }
        }
        self.last = Some(Note::Key { down, vk, slot });
    }

    /// Record a mouse event. Modifier state only ever comes from keys.
    pub fn note_mouse(&mut self, button: MouseButton, down: bool) {
        self.last = Some(Note::Mouse { down, button });
    }

    /// Forget the last noted event, so a [`ChordMatcher::step`] issued after
    /// a rebind or a rehook cannot act on a note observed before it (a stale
    /// key-down could otherwise engage the fresh binding spuriously).
    ///
    /// Deliberately leaves the held modifiers alone: they are derived from
    /// real events and re-derived from `GetAsyncKeyState` by the resync
    /// path, never reset here.
    pub fn clear_last(&mut self) {
        self.last = None;
    }

    /// The four rules of spec §5 for one chord binding, against the event
    /// last noted:
    ///
    /// 1. While inactive, the chord engages only on the *final member's*
    ///    press with every role matched exactly (spec §4). A modifier press
    ///    never completes a chord that has a key, which is what makes
    ///    swallowing safe: no modifier is ever consumed for such a chord,
    ///    so `Ctrl+C` keeps working everywhere while `Ctrl+N` is bound.
    /// 2. While active, the release of any *member* ends the chord;
    ///    presses — even of members — neither drop it nor re-engage it.
    /// 3. `swallow` is reported only for the completing member; every other
    ///    member's press and release are forwarded.
    /// 4. Anything else is `None`: the event reaches other applications
    ///    untouched, and `state` is left alone.
    pub fn step(
        &mut self,
        chord: &Chord,
        swallow: bool,
        state: &mut ChordState,
    ) -> Option<Outcome> {
        let note = self.last?;

        if state.active {
            if !is_release_of_member(note, chord) {
                return None;
            }
            let completing = match state.completing {
                // Modifier-only: exactly the modifier that completed it.
                Some(slot) => {
                    matches!(note, Note::Key { down: false, slot: Some(n), .. } if n == slot)
                }
                // Any keyed or mouse chord: its final member completed it.
                None => is_release_of_final(note, chord),
            };
            let released_final = is_release_of_final(note, chord);
            state.active = false;
            state.completing = None;
            // A keyed chord that lost a modifier first still holds its final
            // key; block re-engagement until that key is released.
            state.blocked = !released_final && chord.key.is_some();
            return Some(Outcome::Released {
                swallow: swallow && completing,
            });
        }

        if state.blocked {
            if is_release_of_final(note, chord) {
                state.blocked = false;
            }
            return None;
        }

        if !note.is_down() || !self.modifiers_match(chord) {
            return None;
        }
        match chord.key {
            Some(ChordKey::Key { vk, .. }) => {
                if matches!(note, Note::Key { down: true, vk: noted, .. } if noted == vk) {
                    state.active = true;
                    return Some(Outcome::Engaged { swallow });
                }
            }
            Some(ChordKey::Mouse(button)) => {
                if matches!(note, Note::Mouse { down: true, button: noted } if noted == button) {
                    state.active = true;
                    return Some(Outcome::Engaged { swallow });
                }
            }
            None => {
                // A modifier-only chord completes on the last of its
                // members going down (spec §5 rule 2), in either order; the
                // exact-match check above already guarantees the pressed
                // modifier is one of the chord's own roles.
                if let Note::Key {
                    down: true,
                    slot: Some(slot),
                    ..
                } = note
                {
                    state.active = true;
                    state.completing = Some(slot);
                    return Some(Outcome::Engaged { swallow });
                }
            }
        }
        None
    }

    /// Every role of `chord` matches the held slots exactly (spec §4).
    fn modifiers_match(&self, chord: &Chord) -> bool {
        chord
            .modifiers
            .roles()
            .iter()
            .all(|&(role, side)| role_held(role, side, self.held))
    }
}

/// Whether `note` is the release of a member of `chord` (spec §5 rule 2):
/// one of its modifiers on the requested side, or its final key or button.
fn is_release_of_member(note: Note, chord: &Chord) -> bool {
    match (note, chord.key) {
        (
            Note::Key {
                down: false,
                slot: Some(slot),
                ..
            },
            _,
        ) => slot_is_member(slot, chord.modifiers),
        (
            Note::Key {
                down: false, vk, ..
            },
            Some(ChordKey::Key { vk: bound, .. }),
        ) => vk == bound,
        (
            Note::Mouse {
                down: false,
                button,
            },
            Some(ChordKey::Mouse(bound)),
        ) => button == bound,
        _ => false,
    }
}

/// Whether `note` is the release of the chord's final key or button — the
/// member that completes a keyed chord and is therefore the only one whose
/// press and release are ever swallowed (spec §5, "Swallow").
fn is_release_of_final(note: Note, chord: &Chord) -> bool {
    match (note, chord.key) {
        (
            Note::Key {
                down: false, vk, ..
            },
            Some(ChordKey::Key { vk: bound, .. }),
        ) => vk == bound,
        (
            Note::Mouse {
                down: false,
                button,
            },
            Some(ChordKey::Mouse(bound)),
        ) => button == bound,
        _ => false,
    }
}

/// Whether `slot` counts as a member of a chord asking for `modifiers` — the
/// exact matching of [`role_held`] applied to one slot: `Off` never, `Any`
/// always, `Left`/`Right` only for that side.
fn slot_is_member(slot: Slot, modifiers: Modifiers) -> bool {
    let wanted = match slot.role() {
        Role::Ctrl => modifiers.ctrl,
        Role::Shift => modifiers.shift,
        Role::Alt => modifiers.alt,
        Role::Win => modifiers.win,
    };
    match wanted {
        Side::Off => false,
        Side::Any => true,
        Side::Left => slot.is_left(),
        Side::Right => !slot.is_left(),
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

    #[test]
    fn a_final_key_that_is_a_modifier_vk_is_invalid() {
        // "Ctrl + Ctrl" would swallow Ctrl's press *and* its release —
        // breaking `Ctrl+C` everywhere while bound. Every producer inherits
        // this rejection from `is_valid`.
        for vk in [
            0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
        ] {
            let chord = Chord {
                modifiers: Modifiers {
                    ctrl: Side::Any,
                    ..Default::default()
                },
                key: Some(ChordKey::Key { vk, scan: 0 }),
            };
            assert!(!chord.is_valid(), "modifier vk {vk:#04x} as a final key");
            let bare = Chord {
                modifiers: Modifiers::default(),
                key: Some(ChordKey::Key { vk, scan: 0 }),
            };
            assert!(!bare.is_valid(), "modifier vk {vk:#04x} with no modifiers");
        }

        let ordinary = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        };
        assert!(ordinary.is_valid(), "an ordinary final key like N is fine");

        let modifier_only = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                shift: Side::Any,
                ..Default::default()
            },
            key: None,
        };
        assert!(
            modifier_only.is_valid(),
            "a modifier-only chord has no final key at all"
        );
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

    // --- the matcher state machine (spec §5) ------------------------------

    fn down(vk: u16, scan: u16) -> KeyEvent {
        KeyEvent::Down {
            vk,
            scan,
            extended: false,
        }
    }

    fn down_ext(vk: u16, scan: u16) -> KeyEvent {
        KeyEvent::Down {
            vk,
            scan,
            extended: true,
        }
    }

    fn up(vk: u16, scan: u16) -> KeyEvent {
        KeyEvent::Up {
            vk,
            scan,
            extended: false,
        }
    }

    fn ctrl_n_chord() -> Chord {
        Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        }
    }

    fn ctrl_shift_chord() -> Chord {
        Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                shift: Side::Any,
                ..Default::default()
            },
            key: None,
        }
    }

    // Engagement (spec §5 rule 1).

    #[test]
    fn a_key_chord_engages_only_when_the_modifiers_are_already_held() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0)); // Left Ctrl
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "a modifier press never completes a keyed chord"
        );
        matcher.note_key(down(0x4E, 49)); // N
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false }),
            "N with Ctrl already held completes the chord"
        );
    }

    #[test]
    fn a_key_chord_does_not_engage_when_the_key_came_first() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x4E, 49)); // N before any modifier
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "N alone is not the chord"
        );
        matcher.note_key(down(0x11, 0)); // Ctrl arrives afterwards
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "the modifier arriving last must not complete it: N's press was already forwarded"
        );
    }

    #[test]
    fn a_modifier_only_chord_engages_on_the_last_modifier_in_either_order() {
        let chord = ctrl_shift_chord();

        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down(0x11, 0)); // Ctrl
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "Ctrl alone is not Ctrl+Shift"
        );
        matcher.note_key(down(0x10, SCAN_LSHIFT)); // Left Shift
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false }),
            "the second modifier completes it"
        );

        // The other order, on a fresh matcher and state.
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down(0x10, SCAN_LSHIFT)); // Shift first
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "Shift alone is not Ctrl+Shift"
        );
        matcher.note_key(down(0x11, 0)); // Ctrl last
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false }),
            "completion is order-independent"
        );
    }

    #[test]
    fn a_pinned_side_does_not_engage_for_the_other_side() {
        let chord = Chord {
            modifiers: Modifiers {
                ctrl: Side::Right,
                ..Default::default()
            },
            key: Some(ChordKey::Key { vk: 0x4E, scan: 49 }),
        };

        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down(0x11, 0)); // Left Ctrl (not extended)
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "the left Ctrl must not fire a Right Ctrl+N chord"
        );

        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down_ext(0x11, 0)); // Right Ctrl (extended)
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false }),
            "the pinned side fires on its own side"
        );
    }

    #[test]
    fn an_extra_unrelated_modifier_prevents_engagement() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0)); // Ctrl
        matcher.note_key(down(0x10, SCAN_LSHIFT)); // Shift, whose side is Off in the chord
        matcher.note_key(down(0x4E, 49)); // N
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "exact match: Ctrl+Shift+N is a different shortcut and must not talk for Ctrl+N"
        );
    }

    // Release (spec §5 rules 2–4).

    #[test]
    fn releasing_the_modifier_releases_the_chord() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );

        matcher.note_key(up(0x11, 0)); // Ctrl let go, N still held
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: false }),
            "releasing the modifier ends the chord right away, but its release is forwarded"
        );
        matcher.note_key(up(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "the chord already ended, so this release is not an outcome"
        );
    }

    #[test]
    fn re_pressing_a_released_modifier_does_not_reengage_a_keyed_chord() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );

        matcher.note_key(up(0x11, 0)); // Ctrl let go while N is still held
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: false })
        );

        matcher.note_key(down(0x11, 0)); // Ctrl pressed again
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "talking must not resume while the final key is still down"
        );

        matcher.note_key(down(0x4E, 49)); // N's auto-repeat, no new physical press
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "the held key's auto-repeat must not resume talking either"
        );

        matcher.note_key(up(0x4E, 49)); // N finally released
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "clears the wait for the final key's release"
        );
        matcher.note_key(down(0x4E, 49)); // a genuine new press, Ctrl still held
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true }),
            "a fresh press of N talks again — the wait is not a permanent lockout"
        );
    }

    #[test]
    fn a_normal_release_re_arms_the_chord_for_the_next_press() {
        // The whole chord, modifier and final key, released in the natural
        // order. `blocked` must stay false here: an unconditional
        // `blocked = true` would swallow every second and later press.
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0)); // Ctrl
        matcher.note_key(down(0x4E, 49)); // N
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );

        matcher.note_key(up(0x4E, 49)); // N released first — the final key
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: true }),
            "the completing member's own release"
        );
        matcher.note_key(up(0x11, 0)); // Ctrl follows
        assert_eq!(matcher.step(&chord, true, &mut state), None);

        matcher.note_key(down(0x11, 0)); // Ctrl again
        matcher.note_key(down(0x4E, 49)); // N again
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true }),
            "a normally released chord re-engages on the next press"
        );
    }

    #[test]
    fn a_modifier_only_chord_re_engages_on_a_modifier_re_press() {
        // Unlike a keyed chord (whose held final key blocks re-engagement),
        // a modifier-only chord has no held key to misuse, and modifiers do
        // not auto-repeat — so a fresh press of a member while the other is
        // held is a new engagement (spec §5 rule 2).
        let chord = ctrl_shift_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0)); // Ctrl
        matcher.note_key(down(0x10, SCAN_LSHIFT)); // Shift completes it
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );

        matcher.note_key(up(0x11, 0)); // Ctrl let go, Shift still held
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: false })
        );

        matcher.note_key(down(0x11, 0)); // Ctrl pressed again, Shift still held
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true }),
            "modifiers never auto-repeat, so this press is a fresh engagement"
        );
    }

    #[test]
    fn state_reset_clears_active_blocked_and_completing() {
        // Lost-release recovery: a release that never arrives (Windows drops
        // low-level hooks) leaves `blocked` set with no way out; `reset` is
        // the way out.
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );
        matcher.note_key(up(0x11, 0)); // Ctrl goes first: N is still held
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: false })
        );
        // N's release never arrives (the hook was dropped).

        state.reset();
        assert_eq!(state, ChordState::default(), "everything cleared");

        matcher.note_key(down(0x11, 0)); // Ctrl pressed again, N never released
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true }),
            "after a reset the chord talks again instead of staying dead"
        );

        // And for a modifier-only chord the reset also forgets which member
        // completed it.
        let chord = ctrl_shift_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x10, SCAN_LSHIFT));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );
        state.reset();
        matcher.note_key(up(0x11, 0));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "a reset chord is inactive, so nothing is released and nothing swallowed"
        );
    }

    #[test]
    fn clear_last_forgets_a_pre_rebind_note() {
        // A matcher reused across a rebind must not engage the fresh binding
        // off a note observed before the rebind.
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0)); // Ctrl genuinely held
        matcher.note_key(down(0x4E, 49)); // a stale N press, noted pre-rebind
        matcher.clear_last();

        matcher.note_key(up(0x11, 0)); // the first real event after the rebind
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "the stale note cannot engage the rebound binding"
        );
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "and stepping again is still inert, not a replay"
        );
    }

    #[test]
    fn an_unrelated_key_pressed_while_active_does_not_release_it() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false })
        );

        matcher.note_key(down(0x4D, 48)); // M
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "an unrelated key passes through without dropping the chord"
        );
        matcher.note_key(down(0x10, SCAN_LSHIFT)); // even a member-role modifier
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "rule 4: presses never drop an active chord, only releases do"
        );

        matcher.note_key(up(0x4E, 49)); // the member's own release still ends it
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Released { swallow: false })
        );
    }

    // Emission (spec §5).

    #[test]
    fn auto_repeat_engages_only_once() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );
        matcher.note_key(down(0x4E, 49)); // auto-repeat
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "one BindingDown per physical press"
        );
        matcher.note_key(down(0x4E, 49));
        assert_eq!(matcher.step(&chord, true, &mut state), None);
        matcher.note_key(up(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: true })
        );
    }

    #[test]
    fn a_chord_ending_in_a_mouse_button_engages_and_releases() {
        let chord = Chord {
            modifiers: Modifiers {
                ctrl: Side::Any,
                ..Default::default()
            },
            key: Some(ChordKey::Mouse(MouseButton::Left)),
        };
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_mouse(MouseButton::Left, true);
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "the click before Ctrl is not the chord"
        );
        matcher.note_key(down(0x11, 0));
        matcher.note_mouse(MouseButton::Left, true);
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false }),
            "the click with Ctrl already held completes the chord"
        );

        matcher.note_mouse(MouseButton::Right, false); // another button let go
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "only the bound button is a member"
        );
        matcher.note_mouse(MouseButton::Left, false);
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Released { swallow: false })
        );
        matcher.note_key(up(0x11, 0));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            None,
            "the chord already ended"
        );
    }

    // Swallow (spec §5: only the completing member, never a modifier whose
    // press was forwarded — the worst outcome of this feature is a swallowed
    // Ctrl breaking `Ctrl+C` everywhere).

    #[test]
    fn a_firing_chord_swallows_only_the_completing_member() {
        // Key chord: N completes it, Ctrl never is swallowed.
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "the modifier's press is forwarded"
        );
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );
        matcher.note_key(up(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: true }),
            "the completing member's release is consumed along with its press"
        );
        matcher.note_key(up(0x11, 0));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "the modifier's release is forwarded"
        );

        // Modifier-only chord: the modifier that completed it is the only
        // one ever swallowed — releasing Ctrl first must forward its
        // release, or every other app would sit on a Ctrl that never went
        // down (`Ctrl+C` breaking everywhere).
        let chord = ctrl_shift_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down(0x11, 0));
        assert_eq!(matcher.step(&chord, true, &mut state), None);
        matcher.note_key(down(0x10, SCAN_LSHIFT)); // Shift completes it
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );
        matcher.note_key(up(0x11, 0)); // the non-completing modifier goes first
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: false }),
            "Ctrl's press was forwarded, so its release must be too"
        );
        matcher.note_key(up(0x10, SCAN_LSHIFT));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            None,
            "the chord already ended; the swallowed Shift's stray release passes through"
        );

        // And the completing modifier's own release is swallowed, in either
        // order of release.
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x10, SCAN_LSHIFT));
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Engaged { swallow: true })
        );
        matcher.note_key(up(0x10, SCAN_LSHIFT)); // the completing modifier goes first
        assert_eq!(
            matcher.step(&chord, true, &mut state),
            Some(Outcome::Released { swallow: true }),
            "the completing modifier's release is consumed like its press"
        );
        matcher.note_key(up(0x11, 0));
        assert_eq!(matcher.step(&chord, true, &mut state), None);
    }

    #[test]
    fn swallow_false_reports_no_swallowing() {
        let chord = ctrl_n_chord();
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();

        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false })
        );
        matcher.note_key(up(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Released { swallow: false }),
            "with swallowing off, neither edge consumes anything"
        );

        // Releasing the modifier first likewise swallows nothing.
        let mut matcher = ChordMatcher::default();
        let mut state = ChordState::default();
        matcher.note_key(down(0x11, 0));
        matcher.note_key(down(0x4E, 49));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Engaged { swallow: false })
        );
        matcher.note_key(up(0x11, 0));
        assert_eq!(
            matcher.step(&chord, false, &mut state),
            Some(Outcome::Released { swallow: false })
        );
    }
}
