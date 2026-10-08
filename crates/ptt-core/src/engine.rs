//! Engine wiring: input events -> state machine -> audio (plan §7 and §9).
//!
//! M2 pulls this forward from M3: hold-to-talk cannot be accepted without it,
//! and plan §10 wants engine tests against a fake `MicController`. Threading
//! (hook thread, timer thread, audio worker) lives in the caller — the engine
//! itself is synchronous and clock-injected, so it is fully testable.
//!
//! Sound actions are dropped here until a player exists (M5); the state
//! machine still emits them.

use crate::audio::MicController;
use crate::config::Config;
use crate::error::Result;
use crate::input::InputEvent;
use crate::state::{step, Action, Event, State};
use std::time::Instant;

/// Ties bound input to the state machine and the microphone (plan §7).
pub struct Engine<C> {
    controller: C,
    config: Config,
    state: State,
}

impl<C: MicController> Engine<C> {
    /// Starts disabled; call [`Engine::enable`] to arm the machine.
    pub fn new(controller: C, config: Config) -> Self {
        Self {
            controller,
            config,
            state: State::Disabled,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Direct access for callers that need the microphone (e.g. to read the
    /// current state before taking over).
    pub fn mic(&self) -> &C {
        &self.controller
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn enable(&mut self, now: Instant) -> Result<Option<Instant>> {
        self.fire(Event::Enable, now)
    }

    pub fn disable(&mut self, now: Instant) -> Result<Option<Instant>> {
        self.fire(Event::Disable, now)
    }

    pub fn shutdown(&mut self, now: Instant) -> Result<Option<Instant>> {
        self.fire(Event::Shutdown, now)
    }

    /// A press or release of the bound input.
    ///
    /// Returns the instant a release timer should be armed for, if any. The
    /// caller does not have to cancel stale timers: [`Engine::tick`] ignores
    /// ticks that arrive in the wrong state (plan §5).
    pub fn input(&mut self, event: InputEvent, now: Instant) -> Result<Option<Instant>> {
        let event = match event {
            InputEvent::BindingDown => Event::PttDown,
            InputEvent::BindingUp => Event::PttUp,
        };
        self.fire(event, now)
    }

    /// The release timer fired.
    pub fn tick(&mut self, now: Instant) -> Result<Option<Instant>> {
        self.fire(Event::Tick(now), now)
    }

    /// Run one transition and apply its actions to the microphone.
    ///
    /// The microphone is touched **on the calling thread**, which must not be
    /// the hook thread (plan §7: never do audio work inside the hook
    /// callback).
    fn fire(&mut self, event: Event, now: Instant) -> Result<Option<Instant>> {
        let (state, actions) = step(self.state.clone(), event, &self.config, now);
        self.state = state;

        let mut armed = None;
        for action in actions {
            match action {
                Action::Mute => self.controller.set_mute(true)?,
                Action::Unmute => self.controller.set_mute(false)?,
                Action::ScheduleTick(at) => armed = Some(at),
                // No sound player exists yet (plan §9 M5); the state machine
                // still emits these and the runner may log them.
                Action::PlayStartSound | Action::PlayStopSound | Action::None => {}
            }
        }
        Ok(armed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{DeviceInfo, MicController};
    use crate::config::{AudioConfig, Config};
    use crate::error::Result;
    use crate::input::InputEvent;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Records every mute call — the plan's fake `MicController` (§10).
    struct FakeMic {
        calls: Mutex<Vec<bool>>,
    }

    impl FakeMic {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<bool> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MicController for FakeMic {
        fn list_capture_devices(&self) -> Result<Vec<DeviceInfo>> {
            Ok(Vec::new())
        }

        fn get_mute(&self) -> Result<bool> {
            Ok(*self.calls.lock().unwrap().last().unwrap_or(&true))
        }

        fn set_mute(&self, muted: bool) -> Result<()> {
            self.calls.lock().unwrap().push(muted);
            Ok(())
        }
    }

    fn config(delay_ms: u64) -> Config {
        Config {
            audio: AudioConfig {
                release_delay_ms: delay_ms,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
    }

    fn engine(delay_ms: u64) -> Engine<FakeMic> {
        Engine::new(FakeMic::new(), config(delay_ms))
    }

    #[test]
    fn enabling_mutes_the_microphone() {
        let mut eng = engine(200);
        eng.enable(Instant::now()).unwrap();
        assert_eq!(eng.mic().calls(), vec![true]);
    }

    #[test]
    fn hold_unmutes_and_the_delayed_release_mutes() {
        let mut eng = engine(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();

        eng.input(InputEvent::BindingDown, t0).unwrap();
        assert_eq!(eng.mic().calls(), vec![true, false], "held = unmuted");

        let armed = eng.input(InputEvent::BindingUp, t0).unwrap();
        assert_eq!(
            armed,
            Some(t0 + Duration::from_millis(200)),
            "a tick is armed"
        );
        assert_eq!(eng.mic().calls(), vec![true, false], "not muted yet");

        let next = eng.tick(t0 + Duration::from_millis(200)).unwrap();
        assert_eq!(next, None, "nothing left to arm");
        assert_eq!(
            eng.mic().calls(),
            vec![true, false, true],
            "exactly three mute calls in order"
        );
    }

    #[test]
    fn zero_release_delay_mutes_immediately_on_release() {
        let mut eng = engine(0);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        let armed = eng.input(InputEvent::BindingUp, t0).unwrap();

        assert_eq!(armed, None, "no timer is needed");
        assert_eq!(eng.mic().calls(), vec![true, false, true]);
    }

    #[test]
    fn a_stale_tick_after_re_pressing_leaves_the_mic_open() {
        let mut eng = engine(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        let deadline = eng.input(InputEvent::BindingUp, t0).unwrap().unwrap();
        // key pressed again before the deadline
        eng.input(InputEvent::BindingDown, t0 + Duration::from_millis(50))
            .unwrap();
        // the already-armed timer still fires and must be ignored
        eng.tick(deadline).unwrap();

        assert_eq!(
            eng.mic().calls(),
            vec![true, false],
            "no extra mute while the key is held"
        );
        assert_eq!(eng.state(), &State::Talking);
    }

    #[test]
    fn input_while_disabled_does_nothing() {
        let mut eng = engine(200);
        let t0 = Instant::now();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        eng.input(InputEvent::BindingUp, t0).unwrap();
        assert_eq!(eng.mic().calls(), Vec::<bool>::new());
        assert_eq!(eng.state(), &State::Disabled);
    }

    #[test]
    fn disable_stops_the_engine_from_touching_the_microphone() {
        let mut eng = engine(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        eng.disable(t0).unwrap();

        assert_eq!(eng.state(), &State::Disabled);
        let after = eng.mic().calls().len();
        // further input must not reach the microphone at all
        eng.input(InputEvent::BindingDown, t0).unwrap();
        eng.tick(t0 + Duration::from_secs(1)).unwrap();
        assert_eq!(eng.mic().calls().len(), after);
    }
}
