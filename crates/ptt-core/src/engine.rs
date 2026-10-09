//! Engine wiring: input events -> state machine -> audio (plan §7 and §9).
//!
//! M2 pulls this forward from M3: hold-to-talk cannot be accepted without it,
//! and plan §10 wants engine tests against a fake `MicController`. Threading
//! (hook thread, timer thread, audio worker) lives in the caller — the engine
//! itself is synchronous and clock-injected, so it is fully testable.
//!
//! Sound actions go to the [`SoundPlayer`] when one is attached (M5);
//! without one — the tests, a machine with no sound — they are dropped.

use crate::audio::MicController;
use crate::config::{Config, SoundsConfig};
use crate::error::Result;
use crate::input::InputEvent;
use crate::sound::{Cue, SoundPlayer};
use crate::state::{step, Action, Event, State};
use std::time::{Duration, Instant};

/// Two toggle presses closer together than this are one press: the second
/// is dropped, so key bounce and auto-repeat cannot flip the latch twice.
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(100);

/// Ties bound input to the state machine and the microphone (plan §7).
pub struct Engine<C> {
    controller: C,
    config: Config,
    state: State,
    sound: Option<Box<dyn SoundPlayer>>,
    /// Set while a toggle press holds the mic open: the machine is parked
    /// in [`State::Talking`] and PTT input never reaches it.
    latched: bool,
    /// When the last accepted toggle press happened (debounce).
    last_toggle: Option<Instant>,
}

impl<C: MicController> Engine<C> {
    /// Starts disabled; call [`Engine::enable`] to arm the machine.
    pub fn new(controller: C, config: Config) -> Self {
        Self {
            controller,
            config,
            state: State::Disabled,
            sound: None,
            latched: false,
            last_toggle: None,
        }
    }

    /// Attach the player the talk / release cues go to.
    pub fn with_sound(mut self, player: Box<dyn SoundPlayer>) -> Self {
        self.sound = Some(player);
        self
    }

    /// The settings window saved new `[sounds]` values: they apply from the
    /// next cue on, without restarting the session. (Whether a cue is due at
    /// all is decided by the state machine from `enabled`.)
    pub fn set_sounds(&mut self, sounds: SoundsConfig) {
        self.config.sounds = sounds;
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
        self.latched = false;
        self.fire(Event::Disable, now)
    }

    pub fn shutdown(&mut self, now: Instant) -> Result<Option<Instant>> {
        self.latched = false;
        self.fire(Event::Shutdown, now)
    }

    /// A press or release of the bound input.
    ///
    /// Returns the instant a release timer should be armed for, if any. The
    /// caller does not have to cancel stale timers: [`Engine::tick`] ignores
    /// ticks that arrive in the wrong state (plan §5).
    pub fn input(&mut self, event: InputEvent, now: Instant) -> Result<Option<Instant>> {
        match event {
            // While latched the machine is parked in Talking: the bound
            // input's press and release never reach the state machine.
            InputEvent::BindingDown | InputEvent::BindingUp if self.latched => Ok(None),
            InputEvent::BindingDown => self.fire(Event::PttDown, now),
            InputEvent::BindingUp => self.fire(Event::PttUp, now),
            InputEvent::ToggleDown => self.toggle(now),
        }
    }

    /// The release timer fired.
    pub fn tick(&mut self, now: Instant) -> Result<Option<Instant>> {
        // A stale timer from before the latch: while latched no tick
        // reaches the machine either — the mic must stay open.
        if self.latched {
            return Ok(None);
        }
        self.fire(Event::Tick(now), now)
    }

    /// One toggle press: flip between latched (mic open until the next
    /// press) and the normal release path.
    ///
    /// Returns the instant a release timer should be armed for, if any —
    /// unlatching runs through the usual release delay.
    fn toggle(&mut self, now: Instant) -> Result<Option<Instant>> {
        if self
            .last_toggle
            .is_some_and(|last| now.duration_since(last) < TOGGLE_DEBOUNCE)
        {
            return Ok(None); // bounce / auto-repeat: the press is dropped
        }
        self.last_toggle = Some(now);

        if self.latched {
            self.latched = false;
            return self.fire(Event::PttUp, now);
        }

        if self.state == State::Disabled {
            self.fire(Event::Enable, now)?; // a toggle press also starts the session
        }
        self.fire(Event::PttDown, now)?; // moves to Talking (unmute + start cue)
        self.latched = true; // only after the unmute actually succeeded
        Ok(None)
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
                Action::PlayStartSound => self.play(Cue::Start),
                Action::PlayStopSound => self.play(Cue::Stop),
                Action::None => {}
            }
        }
        Ok(armed)
    }

    /// Hand a cue to the player — it returns at once (see [`SoundPlayer`]).
    fn play(&self, cue: Cue) {
        if let Some(player) = &self.sound {
            player.play(cue, self.config.sounds.volume);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{DeviceInfo, MicController};
    use crate::config::{AudioConfig, Config};
    use crate::error::Result;
    use crate::input::InputEvent;
    use std::sync::{Arc, Mutex};
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

    // --- sound cues (plan §9 M5) -----------------------------------------

    /// Records every cue with the volume it was asked for.
    #[derive(Clone, Default)]
    struct FakePlayer {
        played: Arc<Mutex<Vec<(Cue, f32)>>>,
    }

    impl FakePlayer {
        fn played(&self) -> Vec<(Cue, f32)> {
            self.played.lock().unwrap().clone()
        }
    }

    impl SoundPlayer for FakePlayer {
        fn play(&self, cue: Cue, volume: f32) {
            self.played.lock().unwrap().push((cue, volume));
        }
    }

    fn engine_with_sound(delay_ms: u64) -> (Engine<FakeMic>, FakePlayer) {
        let player = FakePlayer::default();
        let mut config = config(delay_ms);
        config.sounds.volume = 0.25;
        let eng = Engine::new(FakeMic::new(), config).with_sound(Box::new(player.clone()));
        (eng, player)
    }

    #[test]
    fn pressing_plays_the_start_cue_and_the_delayed_release_the_stop_cue() {
        let (mut eng, player) = engine_with_sound(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        assert_eq!(player.played(), vec![], "enabling is silent");

        eng.input(InputEvent::BindingDown, t0).unwrap();
        assert_eq!(
            player.played(),
            vec![(Cue::Start, 0.25)],
            "volume passed on"
        );

        eng.input(InputEvent::BindingUp, t0).unwrap();
        assert_eq!(
            player.played().len(),
            1,
            "no stop cue while the mic is open"
        );

        eng.tick(t0 + Duration::from_millis(200)).unwrap();
        assert_eq!(
            player.played(),
            vec![(Cue::Start, 0.25), (Cue::Stop, 0.25)],
            "the stop cue comes with the mute"
        );
    }

    #[test]
    fn zero_release_delay_plays_the_stop_cue_on_release() {
        let (mut eng, player) = engine_with_sound(0);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        eng.input(InputEvent::BindingUp, t0).unwrap();
        assert_eq!(
            player
                .played()
                .iter()
                .map(|(cue, _)| *cue)
                .collect::<Vec<_>>(),
            vec![Cue::Start, Cue::Stop]
        );
    }

    #[test]
    fn pressing_again_during_the_release_delay_does_not_replay_the_start_cue() {
        let (mut eng, player) = engine_with_sound(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        eng.input(InputEvent::BindingUp, t0).unwrap();
        eng.input(InputEvent::BindingDown, t0 + Duration::from_millis(50))
            .unwrap();
        eng.tick(t0 + Duration::from_millis(200)).unwrap(); // stale tick
        assert_eq!(player.played(), vec![(Cue::Start, 0.25)]);
    }

    #[test]
    fn disabled_sounds_stay_silent_and_a_new_volume_applies_live() {
        let (mut eng, player) = engine_with_sound(0);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();

        eng.set_sounds(SoundsConfig {
            enabled: false,
            volume: 0.25,
        });
        eng.input(InputEvent::BindingDown, t0).unwrap();
        eng.input(InputEvent::BindingUp, t0).unwrap();
        assert_eq!(player.played(), vec![], "the checkbox is off");

        eng.set_sounds(SoundsConfig {
            enabled: true,
            volume: 0.9,
        });
        eng.input(InputEvent::BindingDown, t0).unwrap();
        assert_eq!(player.played(), vec![(Cue::Start, 0.9)]);
    }

    #[test]
    fn an_engine_without_a_player_just_skips_the_cues() {
        let mut eng = engine(0);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        eng.input(InputEvent::BindingUp, t0).unwrap();
        assert_eq!(eng.mic().calls(), vec![true, false, true]);
    }

    // --- toggle latch (plan §1: press-to-talk becomes press-to-toggle) ---

    /// Records every cue — the latch tests only care which ones fired.
    #[derive(Clone, Default)]
    struct FakeSound {
        cues: Arc<Mutex<Vec<Cue>>>,
    }

    impl FakeSound {
        fn cues(&self) -> Vec<Cue> {
            self.cues.lock().unwrap().clone()
        }
    }

    impl SoundPlayer for FakeSound {
        fn play(&self, cue: Cue, _volume: f32) {
            self.cues.lock().unwrap().push(cue);
        }
    }

    fn engine_with_sounds(delay_ms: u64) -> (Engine<FakeMic>, FakeSound) {
        let sound = FakeSound::default();
        let eng = Engine::new(FakeMic::new(), config(delay_ms)).with_sound(Box::new(sound.clone()));
        (eng, sound)
    }

    #[test]
    fn toggle_latches_the_mic_open_and_plays_the_start_cue() {
        let (mut eng, sound) = engine_with_sounds(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0).unwrap();

        assert_eq!(eng.mic().calls(), vec![true, false], "muted, then unmuted");
        assert_eq!(eng.state(), &State::Talking);
        assert_eq!(sound.cues(), vec![Cue::Start]);
    }

    #[test]
    fn ptt_events_and_ticks_are_ignored_while_latched() {
        let mut eng = engine(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0).unwrap(); // latch

        assert_eq!(eng.input(InputEvent::BindingDown, t0).unwrap(), None);
        assert_eq!(
            eng.input(InputEvent::BindingUp, t0).unwrap(),
            None,
            "no timer armed"
        );
        assert_eq!(eng.tick(t0 + Duration::from_secs(60)).unwrap(), None);

        assert_eq!(eng.mic().calls(), vec![true, false], "exactly one unmute");
        assert_eq!(eng.state(), &State::Talking);
    }

    #[test]
    fn unlatching_honors_the_release_delay() {
        let (mut eng, sound) = engine_with_sounds(200);
        let latch_at = Instant::now();
        eng.enable(latch_at).unwrap();
        eng.input(InputEvent::ToggleDown, latch_at).unwrap(); // latch

        // The next press must clear the toggle debounce window.
        let t0 = latch_at + Duration::from_millis(150);
        let armed = eng.input(InputEvent::ToggleDown, t0).unwrap();
        assert_eq!(armed, Some(t0 + Duration::from_millis(200)));
        assert_eq!(eng.mic().calls(), vec![true, false], "not muted yet");

        eng.tick(t0 + Duration::from_millis(200)).unwrap();
        assert_eq!(eng.mic().calls(), vec![true, false, true]);
        assert_eq!(sound.cues(), vec![Cue::Start, Cue::Stop]);
    }

    #[test]
    fn zero_release_delay_unlatches_immediately() {
        let mut eng = engine(0);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0).unwrap(); // latch

        let armed = eng
            .input(InputEvent::ToggleDown, t0 + Duration::from_millis(150))
            .unwrap();
        assert_eq!(armed, None, "no timer is needed");
        assert_eq!(eng.mic().calls(), vec![true, false, true]);
    }

    #[test]
    fn toggle_reenables_a_disabled_app_straight_into_the_latch() {
        let (mut eng, sound) = engine_with_sounds(200); // fresh: never enabled
        let t0 = Instant::now();
        eng.input(InputEvent::ToggleDown, t0).unwrap();

        assert_eq!(eng.mic().calls(), vec![true, false]);
        assert_eq!(eng.state(), &State::Talking);
        assert_eq!(sound.cues(), vec![Cue::Start]);
    }

    #[test]
    fn disable_clears_the_latch_and_ptt_works_normally_afterwards() {
        let mut eng = engine(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0).unwrap(); // latch
        eng.disable(t0).unwrap();

        assert_eq!(eng.state(), &State::Disabled);
        assert_eq!(
            eng.mic().calls(),
            vec![true, false],
            "disable never touches the microphone"
        );

        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        assert_eq!(
            eng.mic().calls(),
            vec![true, false, true, false],
            "PTT is no longer dropped"
        );
    }

    #[test]
    fn shutdown_clears_the_latch() {
        let mut eng = engine(200);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0).unwrap(); // latch
        eng.shutdown(t0).unwrap();

        assert_eq!(eng.state(), &State::Disabled);
        assert_eq!(
            eng.mic().calls(),
            vec![true, false],
            "shutdown never touches the microphone"
        );

        eng.enable(t0).unwrap();
        eng.input(InputEvent::BindingDown, t0).unwrap();
        assert_eq!(
            eng.mic().calls(),
            vec![true, false, true, false],
            "PTT is no longer dropped"
        );
    }

    #[test]
    fn a_duplicate_toggle_within_the_debounce_window_is_ignored() {
        let mut eng = engine(0);
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0 + Duration::from_millis(10))
            .unwrap();

        assert_eq!(eng.mic().calls(), vec![true, false], "still latched");
        assert_eq!(eng.state(), &State::Talking);

        eng.input(InputEvent::ToggleDown, t0 + Duration::from_millis(200))
            .unwrap();
        assert_eq!(eng.mic().calls(), vec![true, false, true], "unlatched");
    }

    #[test]
    fn cues_are_silent_when_sounds_are_disabled() {
        let (mut eng, sound) = engine_with_sounds(0);
        eng.set_sounds(SoundsConfig {
            enabled: false,
            volume: 0.25,
        });
        let t0 = Instant::now();
        eng.enable(t0).unwrap();
        eng.input(InputEvent::ToggleDown, t0).unwrap(); // latch
        eng.input(InputEvent::ToggleDown, t0 + Duration::from_millis(150))
            .unwrap();

        assert_eq!(sound.cues(), vec![]);
        assert_eq!(
            eng.mic().calls(),
            vec![true, false, true],
            "both transitions happened"
        );
    }
}
