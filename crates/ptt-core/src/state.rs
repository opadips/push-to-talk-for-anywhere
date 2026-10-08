//! Pure state machine (plan §5): `step(state, event, cfg, now) -> (state, actions)`.
//!
//! Deterministic and fully unit-testable with a fake clock: time only enters
//! through `now` and `Event::Tick`, never through `Instant::now()` inside
//! `step`.

use crate::config::Config;
use std::time::{Duration, Instant};

/// Where the app is right now (plan §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Off: not intercepting input, engine restores the original mic state.
    Disabled,
    /// On, mic muted, waiting for the bound input.
    Muted,
    /// Bound input held: mic unmuted.
    Talking,
    /// Bound input released, waiting for the release delay to expire.
    ReleasePending { deadline: Instant },
}

/// Everything that can move the state machine (plan §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Enable,
    Disable,
    PttDown,
    PttUp,
    /// Fired by the timer armed with [`Action::ScheduleTick`].
    Tick(Instant),
    Shutdown,
}

/// What the engine must do after a transition (plan §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Mute,
    Unmute,
    /// Arm a timer for this instant; the engine sends [`Event::Tick`] with it.
    ScheduleTick(Instant),
    PlayStartSound,
    PlayStopSound,
    /// Explicit no-op; `step` returns an empty `Vec` instead.
    None,
}

/// Advance the state machine by one event (plan §5).
///
/// Sound actions are always produced when sounds are enabled in `cfg`;
/// muting/unmuting is emitted unconditionally so the engine stays dumb.
pub fn step(state: State, event: Event, cfg: &Config, now: Instant) -> (State, Vec<Action>) {
    match (state, event) {
        // Plan §5: Disable and Shutdown always win; restoring the user's
        // original mute state is the engine's job via the fail-safe (§6).
        (_, Event::Disable | Event::Shutdown) => (State::Disabled, Vec::new()),

        // Plan §1: the product keeps the microphone muted by default, so
        // enabling ends in Muted, not in "mic open".
        (State::Disabled, Event::Enable) => (State::Muted, vec![Action::Mute]),
        (state, Event::Enable) => (state, Vec::new()),

        // Plan §5: down from Muted starts talking.
        (State::Muted, Event::PttDown) => (
            State::Talking,
            vec![Action::Unmute, sound(Action::PlayStartSound, cfg)],
        ),
        // Plan §5: down while a release is pending cancels it — no start
        // sound, because the user never stopped talking.
        (State::ReleasePending { .. }, Event::PttDown) => (State::Talking, Vec::new()),
        // Plan §5: auto-repeat while already talking is ignored.
        (state, Event::PttDown) => (state, Vec::new()),

        // Plan §5: up starts the release delay (0 = mute immediately).
        (State::Talking, Event::PttUp) => {
            let delay = Duration::from_millis(cfg.audio.release_delay_ms);
            if delay.is_zero() {
                return (
                    State::Muted,
                    vec![Action::Mute, sound(Action::PlayStopSound, cfg)],
                );
            }
            let deadline = now + delay;
            (
                State::ReleasePending { deadline },
                vec![Action::ScheduleTick(deadline)],
            )
        }
        (state, Event::PttUp) => (state, Vec::new()),

        // Plan §5: only a tick at or past the deadline mutes; late or stale
        // ticks (a re-press cancelled the release) are ignored so the mic
        // never chatters under a held key.
        (State::ReleasePending { deadline }, Event::Tick(at)) if at >= deadline => (
            State::Muted,
            vec![Action::Mute, sound(Action::PlayStopSound, cfg)],
        ),
        (state, Event::Tick(_)) => (state, Vec::new()),
    }
}

/// Emit a sound action only when sounds are enabled (plan §8).
fn sound(action: Action, cfg: &Config) -> Action {
    debug_assert!(
        matches!(action, Action::PlayStartSound | Action::PlayStopSound),
        "sound() is only for sound actions"
    );
    if cfg.sounds.enabled {
        action
    } else {
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudioConfig, Config};
    use std::time::{Duration, Instant};

    fn cfg() -> Config {
        Config {
            audio: AudioConfig {
                release_delay_ms: 200,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
    }

    fn step(state: State, event: Event, now: Instant) -> (State, Vec<Action>) {
        super::step(state, event, &cfg(), now)
    }

    #[test]
    fn muted_and_ptt_down_talks_and_plays_the_start_sound() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Muted, Event::PttDown, t0);
        assert_eq!(state, State::Talking);
        assert_eq!(actions, vec![Action::Unmute, Action::PlayStartSound]);
    }

    #[test]
    fn talking_and_ptt_up_schedules_the_release_delay() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Talking, Event::PttUp, t0);
        assert_eq!(
            state,
            State::ReleasePending {
                deadline: t0 + Duration::from_millis(200),
            }
        );
        assert_eq!(
            actions,
            vec![Action::ScheduleTick(t0 + Duration::from_millis(200))]
        );
    }

    #[test]
    fn talking_and_ptt_up_with_zero_delay_mutes_immediately() {
        let t0 = Instant::now();
        let (state, actions) = super::step(
            State::Talking,
            Event::PttUp,
            &Config {
                audio: AudioConfig {
                    release_delay_ms: 0,
                    ..AudioConfig::default()
                },
                ..Config::default()
            },
            t0,
        );
        assert_eq!(state, State::Muted);
        assert_eq!(actions, vec![Action::Mute, Action::PlayStopSound]);
    }

    #[test]
    fn release_pending_and_ptt_down_resumes_talking_without_a_start_sound() {
        let t0 = Instant::now();
        let pending = State::ReleasePending {
            deadline: t0 + Duration::from_millis(200),
        };
        let (state, actions) = step(pending, Event::PttDown, t0);
        assert_eq!(state, State::Talking);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn release_pending_tick_before_the_deadline_changes_nothing() {
        let t0 = Instant::now();
        let pending = State::ReleasePending {
            deadline: t0 + Duration::from_millis(200),
        };
        let (state, actions) = step(
            pending.clone(),
            Event::Tick(t0 + Duration::from_millis(50)),
            t0,
        );
        assert_eq!(state, pending);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn release_pending_tick_at_the_deadline_mutes_and_plays_the_stop_sound() {
        let t0 = Instant::now();
        let pending = State::ReleasePending {
            deadline: t0 + Duration::from_millis(200),
        };
        let (state, actions) = step(pending, Event::Tick(t0 + Duration::from_millis(200)), t0);
        assert_eq!(state, State::Muted);
        assert_eq!(actions, vec![Action::Mute, Action::PlayStopSound]);
    }

    #[test]
    fn auto_repeat_ptt_down_while_talking_is_ignored() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Talking, Event::PttDown, t0);
        assert_eq!(state, State::Talking);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn ptt_up_while_muted_is_ignored() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Muted, Event::PttUp, t0);
        assert_eq!(state, State::Muted);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn a_stale_tick_after_re_pressing_does_not_mute() {
        let t0 = Instant::now();
        // armed for release, then pressed again -> Talking, then the old
        // timer fires: it must not mute under the held key.
        let (state, actions) = step(State::Talking, Event::Tick(t0 + Duration::from_secs(5)), t0);
        assert_eq!(state, State::Talking);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn disable_while_talking_enters_disabled_without_actions() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Talking, Event::Disable, t0);
        assert_eq!(state, State::Disabled);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn disable_while_release_pending_enters_disabled() {
        let t0 = Instant::now();
        let pending = State::ReleasePending {
            deadline: t0 + Duration::from_millis(200),
        };
        let (state, actions) = step(pending, Event::Disable, t0);
        assert_eq!(state, State::Disabled);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn enable_from_disabled_mutes_the_microphone() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Disabled, Event::Enable, t0);
        assert_eq!(state, State::Muted);
        assert_eq!(actions, vec![Action::Mute]);
    }

    #[test]
    fn enable_while_already_muted_is_idempotent() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Muted, Event::Enable, t0);
        assert_eq!(state, State::Muted);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn input_events_while_disabled_are_ignored() {
        let t0 = Instant::now();
        for event in [Event::PttDown, Event::PttUp, Event::Tick(t0)] {
            let (state, actions) = step(State::Disabled, event, t0);
            assert_eq!(state, State::Disabled);
            assert_eq!(actions, Vec::<Action>::new());
        }
    }

    #[test]
    fn shutdown_enters_disabled() {
        let t0 = Instant::now();
        for start in [State::Muted, State::Talking, State::Disabled] {
            let (state, _) = step(start, Event::Shutdown, t0);
            assert_eq!(state, State::Disabled);
        }
    }

    #[test]
    fn a_tick_in_muted_is_ignored() {
        let t0 = Instant::now();
        let (state, actions) = step(State::Muted, Event::Tick(t0), t0);
        assert_eq!(state, State::Muted);
        assert_eq!(actions, Vec::<Action>::new());
    }

    /// Property test (plan §5): for any sequence of events, while enabled the
    /// microphone is unmuted only in states that have a path back to `Muted`.
    ///
    /// The mute flag is simulated by applying every `Mute`/`Unmute` action,
    /// exactly as the engine will.
    #[test]
    fn any_event_sequence_keeps_the_microphone_on_a_path_back_to_muted() {
        let events = [Event::Enable, Event::Disable, Event::PttDown, Event::PttUp];
        let mut seed: u64 = 0x5eed;
        let mut next = move || {
            // tiny deterministic LCG: no dependency, no flakiness
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };

        let mut state = State::Disabled;
        let mut muted = true; // the app keeps the mic muted by default
        let t0 = Instant::now();

        for _ in 0..5000 {
            let event = match next() % 5 {
                0..=2 => events[next() % events.len()].clone(),
                // a tick at a random offset around the release deadline
                _ => Event::Tick(t0 + Duration::from_millis((next() % 400) as u64)),
            };

            let (new_state, actions) = super::step(state.clone(), event, &cfg(), t0);
            for action in &actions {
                match action {
                    Action::Mute => muted = true,
                    Action::Unmute => muted = false,
                    _ => {}
                }
            }
            state = new_state;

            match state {
                State::Muted => assert!(muted, "Muted must mean the mic is muted"),
                State::Talking | State::ReleasePending { .. } => {
                    assert!(!muted, "unmuted only while talking or pending release");
                }
                // the engine restores the original state here (plan §5/§6)
                State::Disabled => {}
            }
        }
    }
}
