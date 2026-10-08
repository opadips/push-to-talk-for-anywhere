//! The running hold-to-talk session (plan §7, §9 M2/M4): the engine plus its
//! input source, pumped by one worker thread that also takes orders from
//! whoever owns the handle — the CLI, the tray menu, the settings window.
//!
//! The microphone is touched on that worker thread only, never on the hook
//! callback (plan §7), and it is handed back on the way out however the
//! session ends (plan §6.2).

use crate::audio::MicController;
use crate::config::{Config, OnExit};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::failsafe::{self, StateFile};
use crate::input::{Binding, InputEvent, InputSource};
use crate::state::State;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// How often the worker wakes while nothing is pending; it doubles as the
/// interval in which it notices orders from the tray or the settings window.
const POLL: Duration = Duration::from_millis(50);

/// The three states the tray icon can show (plan §9 M4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// The machine is disarmed: the binding does nothing (plan §5).
    Disabled,
    /// Live but quiet: the microphone is muted, waiting for the binding.
    Muted,
    /// The microphone is open — also while the release delay runs, because
    /// the mic only really closes when the machine reaches `Muted` again.
    Talking,
}

/// What the owner renders: the tray icon's state plus the last failure from
/// the engine (plan §9 M4: "any error from the engine, e.g. device
/// unavailable").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub state: SessionState,
    pub error: Option<String>,
}

/// Orders the owner can give a running session (plan §9 M4).
pub enum Command {
    /// Tray menu / settings window: arm or disarm the machine.
    SetEnabled(bool),
    /// The settings window saved a new binding (plan §4).
    Rebind { binding: Binding, swallow: bool },
    /// "Press any key": the next press answers on this channel.
    Capture(Sender<Binding>),
    /// The window found the key itself (or went away): disarm the capture.
    CancelCapture,
    /// Stop: take the hooks down and hand the microphone back.
    Quit,
}

/// A running session: the worker thread plus the two channels its owner
/// needs — one to give orders, one it publishes its status into.
pub struct SessionHandle {
    commands: Sender<Command>,
    status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl SessionHandle {
    /// Bring a session up (plan §6.1, §7): record the microphone's own mute
    /// state, mute it, install the hooks and start pumping on a worker
    /// thread. `enabled` decides whether the machine comes up armed (plan
    /// §8's `enabled`; the CLI forces it on — running `ptt ptt` *is* the
    /// enable).
    pub fn start<C, S>(
        config: Config,
        controller: C,
        mut source: S,
        state_path: PathBuf,
        enabled: bool,
    ) -> Result<SessionHandle>
    where
        C: MicController + 'static,
        S: InputSource + Send + 'static,
    {
        // Plan §6.1: remember the user's own mute state *before* touching it.
        let original_muted = controller.get_mute()?;
        failsafe::record(&state_path, &config.audio.device_id, original_muted)?;

        let binding = config.binding();
        let swallow = config.binding.swallow;
        let on_exit = config.audio.on_exit;
        let device_id = config.audio.device_id.clone();

        let mut engine = Engine::new(controller, config);
        engine.enable(Instant::now())?;
        if !enabled {
            // Muted either way — `Disabled` *is* "muted, not listening"
            // (plan §5) — only the binding goes inert.
            engine.disable(Instant::now())?;
        }

        let (events_tx, events_rx) = channel();
        if let Err(error) = source.start(binding, swallow, events_tx) {
            // Nothing has been held yet: undo and clear the record (§6.6).
            engine.mic().set_mute(original_muted)?;
            failsafe::mark_clean(&state_path)?;
            return Err(error);
        }

        let status = Arc::new(Mutex::new(Status {
            state: session_state(engine.state()),
            error: None,
        }));
        let (commands_tx, commands_rx) = channel();
        let worker_status = Arc::clone(&status);
        let thread = std::thread::Builder::new()
            .name("ptt-session".into())
            .spawn(move || {
                worker(
                    engine,
                    source,
                    events_rx,
                    commands_rx,
                    worker_status,
                    state_path,
                    original_muted,
                    device_id,
                    on_exit,
                )
            })?;

        Ok(SessionHandle {
            commands: commands_tx,
            status,
            thread: Some(thread),
        })
    }

    /// Tray menu / settings window: arm or disarm the machine (plan §9 M4).
    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.commands.send(Command::SetEnabled(enabled));
    }

    /// The settings window saved a new binding: it reaches the running hook
    /// without a restart (plan §4).
    pub fn rebind(&self, binding: Binding, swallow: bool) {
        let _ = self.commands.send(Command::Rebind { binding, swallow });
    }

    /// "Press any key" (plan §9 M4): answers with the next press, skipping
    /// bare modifiers (they cannot be held to talk).
    pub fn capture(&self) -> Receiver<Binding> {
        let (tx, rx) = channel();
        let _ = self.commands.send(Command::Capture(tx));
        rx
    }

    /// Disarm an outstanding [`SessionHandle::capture`]. Its receiver then
    /// disconnects instead of answering.
    pub fn cancel_capture(&self) {
        let _ = self.commands.send(Command::CancelCapture);
    }

    /// What the tray icon and the settings window render right now.
    pub fn status(&self) -> Status {
        let mut status = self.status.lock().unwrap().clone();
        if self.finished() && status.error.is_none() {
            // The worker is gone without having said why (dead hook, a panic
            // the fail-safe already handled): never show a live state for a
            // session that is not running.
            status.error = Some("the session is not running".to_string());
        }
        status
    }

    /// Has the worker stopped on its own? The owner restarts the session
    /// after an engine failure (plan §9 M4).
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// Take the hooks down, hand the microphone back the way `[audio]
    /// on_exit` asks for and clear the record (plan §6.2, §6.6), then join
    /// the worker and report whatever it found.
    pub fn stop(mut self) -> Result<()> {
        let _ = self.commands.send(Command::Quit);
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| Error::Worker("the session thread panicked".to_string()))?,
            None => Ok(()),
        }
    }
}

/// A session must never outlive its owner: dropping the handle stops the
/// worker and lets the exit path hand the microphone back.
impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The worker thread: feed the engine from the hook, take orders from the
/// owner, and when that ends take the hooks down and hand the microphone
/// back — whatever happened (plan §6.2).
#[allow(clippy::too_many_arguments)]
fn worker<C: MicController, S: InputSource>(
    mut engine: Engine<C>,
    mut source: S,
    events: Receiver<InputEvent>,
    commands: Receiver<Command>,
    status: Arc<Mutex<Status>>,
    state_path: PathBuf,
    original_muted: bool,
    device_id: String,
    on_exit: OnExit,
) -> Result<()> {
    let outcome = pump(&mut engine, &mut source, &events, &commands, &status);

    source.stop();
    let shutdown = engine.shutdown(Instant::now()).map(|_| ());
    let restored = match on_exit {
        OnExit::Restore => failsafe::restore(
            engine.mic(),
            &StateFile {
                device_id,
                original_muted,
                dirty: true,
                pid: std::process::id(),
            },
            &state_path,
        ),
        OnExit::Unmute => engine
            .mic()
            .set_mute(false)
            .and_then(|()| failsafe::mark_clean(&state_path)),
    };

    // Published before the thread ends, so `finished()` implies the owner
    // can already read why (plan §9 M4: the UI must show engine errors).
    let outcome = outcome.and(shutdown).and(restored);
    publish(
        &status,
        engine.state(),
        outcome.as_ref().err().map(ToString::to_string),
    );
    outcome
}

/// Run one transition and apply its actions until the source goes away, an
/// order says quit, or the engine fails (plan §7).
fn pump<C: MicController, S: InputSource>(
    engine: &mut Engine<C>,
    source: &mut S,
    events: &Receiver<InputEvent>,
    commands: &Receiver<Command>,
    status: &Arc<Mutex<Status>>,
) -> Result<()> {
    let mut deadline: Option<Instant> = None;
    // An outstanding "press any key" capture (plan §9 M4).
    let mut capture: Option<(Receiver<Binding>, Sender<Binding>)> = None;

    loop {
        let now = Instant::now();
        let timeout = deadline
            .map(|at| at.saturating_duration_since(now))
            .unwrap_or(POLL)
            .min(POLL);

        match events.recv_timeout(timeout) {
            Ok(event) => {
                let _ = engine.input(event, Instant::now())?;
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                if deadline.is_some_and(|at| now >= at) {
                    let _ = engine.tick(now)?;
                }
            }
            // The input source went away: nothing can be said any more.
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Orders from the tray and the settings window (plan §9 M4); none
        // may wait behind a burst of keystrokes, so drain them all.
        loop {
            match commands.try_recv() {
                Ok(Command::SetEnabled(enabled)) => {
                    let now = Instant::now();
                    let _ = if enabled {
                        engine.enable(now)
                    } else {
                        engine.disable(now)
                    }?;
                }
                Ok(Command::Rebind { binding, swallow }) => source.set_binding(binding, swallow),
                Ok(Command::Capture(reply)) => {
                    info!("capture armed: the next key press becomes the binding");
                    capture = Some((source.capture_next(), reply));
                }
                Ok(Command::CancelCapture) => {
                    // Dropping `reply` disconnects whoever waits for it.
                    if capture.take().is_some() {
                        info!("capture cancelled by the owner");
                        source.cancel_capture();
                    }
                }
                Ok(Command::Quit) => return Ok(()),
                Err(TryRecvError::Empty) => break,
                // The owner dropped its handle; `Drop` also stops us, this
                // only covers the moment in between.
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // A captured press becomes the binding — except a bare modifier,
        // which cannot be held to talk, so the capture starts over.
        if let Some((received, reply)) = &mut capture {
            match received.try_recv() {
                Ok(binding) if binding.is_modifier() => {
                    info!("{binding:?} alone cannot be held to talk — capture starts over");
                    *received = source.capture_next();
                }
                Ok(binding) => {
                    info!("captured {binding:?}");
                    let _ = reply.send(binding);
                    capture = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    warn!("the capture listener went away");
                    capture = None;
                }
            }
        }

        deadline = match engine.state() {
            State::ReleasePending { deadline } => Some(*deadline),
            _ => None,
        };
        publish(status, engine.state(), None);
    }

    Ok(())
}

/// Publish what the owner renders. An error sticks: a session that failed
/// keeps saying so until a new one is started.
fn publish(status: &Arc<Mutex<Status>>, state: &State, error: Option<String>) {
    let mut published = status.lock().unwrap();
    published.state = session_state(state);
    if error.is_some() {
        published.error = error;
    }
}

/// Map the state machine onto the three states the tray shows (plan §5,
/// §9 M4).
fn session_state(state: &State) -> SessionState {
    match state {
        State::Disabled => SessionState::Disabled,
        State::Muted => SessionState::Muted,
        State::Talking | State::ReleasePending { .. } => SessionState::Talking,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{DeviceInfo, MicController};
    use crate::config::{AudioConfig, Config};
    use crate::error::Result as CoreResult;
    use crate::input::{Binding, InputEvent, InputSource, MouseButton};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Cloneable fake microphone: every clone sees the same call log, which
    /// the worker thread writes to (plan §10's fake `MicController`).
    #[derive(Clone, Default)]
    struct SharedMic {
        calls: Arc<Mutex<Vec<bool>>>,
    }

    impl SharedMic {
        fn calls(&self) -> Vec<bool> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MicController for SharedMic {
        fn list_capture_devices(&self) -> CoreResult<Vec<DeviceInfo>> {
            Ok(Vec::new())
        }

        fn get_mute(&self) -> CoreResult<bool> {
            Ok(*self.calls.lock().unwrap().last().unwrap_or(&true))
        }

        fn set_mute(&self, muted: bool) -> CoreResult<()> {
            self.calls.lock().unwrap().push(muted);
            Ok(())
        }
    }

    /// Microphone whose `set_mute` starts failing on demand — plan §9 M4
    /// wants the engine's error surfaced to the owner.
    #[derive(Clone, Default)]
    struct FlakyMic {
        calls: Arc<Mutex<Vec<bool>>>,
        failing: Arc<AtomicBool>,
    }

    impl FlakyMic {
        fn fail_from_now_on(&self) {
            self.failing.store(true, Ordering::Relaxed);
        }
    }

    impl MicController for FlakyMic {
        fn list_capture_devices(&self) -> CoreResult<Vec<DeviceInfo>> {
            Ok(Vec::new())
        }

        fn get_mute(&self) -> CoreResult<bool> {
            Ok(*self.calls.lock().unwrap().last().unwrap_or(&false))
        }

        fn set_mute(&self, muted: bool) -> CoreResult<()> {
            if self.failing.load(Ordering::Relaxed) {
                return Err(crate::error::Error::DeviceUnavailable(
                    "the test microphone went away".into(),
                ));
            }
            self.calls.lock().unwrap().push(muted);
            Ok(())
        }
    }

    /// Fake input source: hands the test the event sender instead of a hook,
    /// and can answer a "press any key" capture (plan §9 M4).
    #[derive(Clone, Default)]
    struct FakeSource {
        events: Arc<Mutex<Option<Sender<InputEvent>>>>,
        binding: Arc<Mutex<Option<(Binding, bool)>>>,
        pending_capture: Arc<Mutex<Option<Sender<Binding>>>>,
        stopped: Arc<AtomicBool>,
    }

    impl FakeSource {
        fn press(&self, event: InputEvent) {
            let sender = self.events.lock().unwrap();
            sender
                .as_ref()
                .expect("the session started the source")
                .send(event)
                .unwrap();
        }

        fn binding(&self) -> Option<(Binding, bool)> {
            *self.binding.lock().unwrap()
        }

        fn capturing(&self) -> bool {
            self.pending_capture.lock().unwrap().is_some()
        }

        fn answer_capture(&self, binding: Binding) {
            let answered = self.pending_capture.lock().unwrap().take();
            answered
                .expect("a capture is pending")
                .send(binding)
                .unwrap();
        }

        fn stopped(&self) -> bool {
            self.stopped.load(Ordering::Relaxed)
        }
    }

    impl InputSource for FakeSource {
        fn start(
            &mut self,
            binding: Binding,
            swallow: bool,
            tx: Sender<InputEvent>,
        ) -> CoreResult<()> {
            *self.binding.lock().unwrap() = Some((binding, swallow));
            *self.events.lock().unwrap() = Some(tx);
            Ok(())
        }

        fn set_binding(&mut self, binding: Binding, swallow: bool) {
            *self.binding.lock().unwrap() = Some((binding, swallow));
        }

        fn capture_next(&mut self) -> Receiver<Binding> {
            let (tx, rx) = channel();
            *self.pending_capture.lock().unwrap() = Some(tx);
            rx
        }

        fn cancel_capture(&mut self) {
            *self.pending_capture.lock().unwrap() = None;
        }

        fn stop(&mut self) {
            self.stopped.store(true, Ordering::Relaxed);
        }
    }

    fn config(release_delay_ms: u64) -> Config {
        Config {
            audio: AudioConfig {
                release_delay_ms,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
    }

    /// A state file per test so the parallel tests cannot share one.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ptt-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.json"));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Poll `condition` until it holds or ~3 s pass (the worker runs on its
    /// own thread and wakes at most every `POLL`).
    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        for _ in 0..300 {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn events_from_the_source_drive_the_microphone() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            mic.clone(),
            source.clone(),
            scratch("drive"),
            true,
        )
        .unwrap();

        source.press(InputEvent::BindingDown);
        assert!(
            wait_until(|| session.status().state == SessionState::Talking),
            "the tray sees that the microphone is open"
        );

        source.press(InputEvent::BindingUp);
        assert!(wait_until(|| mic.calls() == vec![true, false, true]));
        assert!(wait_until(|| session.status().state == SessionState::Muted));

        session.stop().unwrap();
    }

    #[test]
    fn stop_hands_the_microphone_back_and_clears_the_record() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let path = scratch("stop");
        let session =
            SessionHandle::start(config(0), mic.clone(), source.clone(), path.clone(), true)
                .unwrap();

        source.press(InputEvent::BindingDown);
        assert!(wait_until(|| mic.calls() == vec![true, false]));

        session.stop().unwrap();

        assert_eq!(
            mic.calls(),
            vec![true, false, true],
            "back to the recorded mute state"
        );
        assert!(source.stopped(), "the hooks are gone");
        match crate::failsafe::read(&path) {
            crate::failsafe::Snapshot::Found(state) => {
                assert!(!state.dirty, "the record is cleared (plan §6.6)")
            }
            other => panic!("the record was left behind: {other:?}"),
        }
    }

    #[test]
    fn starting_disabled_keeps_the_microphone_muted_and_the_hotkey_inert() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            mic.clone(),
            source.clone(),
            scratch("disabled"),
            false,
        )
        .unwrap();

        assert!(wait_until(
            || session.status().state == SessionState::Disabled
        ));
        assert_eq!(mic.calls(), vec![true], "muted, never unmuted");

        source.press(InputEvent::BindingDown);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            mic.calls(),
            vec![true],
            "a disabled session never opens the microphone"
        );

        session.stop().unwrap();
    }

    #[test]
    fn the_tray_can_disarm_and_rearm_a_running_session() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            mic.clone(),
            source.clone(),
            scratch("toggle"),
            true,
        )
        .unwrap();

        session.set_enabled(false);
        assert!(wait_until(
            || session.status().state == SessionState::Disabled
        ));
        assert!(
            mic.calls().last() == Some(&true),
            "the microphone stays muted"
        );

        session.set_enabled(true);
        assert!(wait_until(|| session.status().state == SessionState::Muted));

        source.press(InputEvent::BindingDown);
        assert!(wait_until(
            || session.status().state == SessionState::Talking
        ));
        session.stop().unwrap();
    }

    #[test]
    fn a_rebind_reaches_the_running_input_source() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            mic.clone(),
            source.clone(),
            scratch("rebind"),
            true,
        )
        .unwrap();

        session.rebind(Binding::Mouse(MouseButton::X1), true);
        assert!(wait_until(|| {
            source.binding() == Some((Binding::Mouse(MouseButton::X1), true))
        }));

        session.stop().unwrap();
    }

    #[test]
    fn capturing_answers_with_the_next_press() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            mic.clone(),
            source.clone(),
            scratch("capture"),
            true,
        )
        .unwrap();

        let captured = session.capture();
        assert!(
            wait_until(|| source.capturing()),
            "the hook waits for a key"
        );
        source.answer_capture(Binding::Key { vk: 0x14, scan: 0 });

        assert_eq!(
            captured.recv_timeout(Duration::from_secs(2)).unwrap(),
            Binding::Key { vk: 0x14, scan: 0 }
        );
        session.stop().unwrap();
    }

    #[test]
    fn cancelling_a_capture_disarms_the_source_and_frees_the_waiter() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            mic.clone(),
            source.clone(),
            scratch("cancel"),
            true,
        )
        .unwrap();

        let captured = session.capture();
        assert!(
            wait_until(|| source.capturing()),
            "the hook waits for a key"
        );

        session.cancel_capture();
        assert!(
            wait_until(|| !source.capturing()),
            "nothing keeps waiting to swallow the user's next key"
        );
        assert!(
            matches!(
                captured.recv_timeout(Duration::from_secs(2)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
            ),
            "the window's pending request ends instead of hanging"
        );

        // A later capture still works.
        let again = session.capture();
        assert!(wait_until(|| source.capturing()));
        source.answer_capture(Binding::Key { vk: 0x41, scan: 0 });
        assert_eq!(
            again.recv_timeout(Duration::from_secs(2)).unwrap(),
            Binding::Key { vk: 0x41, scan: 0 }
        );
        session.stop().unwrap();
    }

    #[test]
    fn cancelling_without_a_capture_is_harmless() {
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            SharedMic::default(),
            source.clone(),
            scratch("cancel-idle"),
            true,
        )
        .unwrap();
        session.cancel_capture();
        std::thread::sleep(Duration::from_millis(120));
        assert!(!session.finished());
        session.stop().unwrap();
    }

    #[test]
    fn a_bare_modifier_is_captured_again_instead_of_becoming_the_binding() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(0),
            mic.clone(),
            source.clone(),
            scratch("modifier"),
            true,
        )
        .unwrap();

        let captured = session.capture();
        assert!(wait_until(|| source.capturing()));
        source.answer_capture(Binding::Key { vk: 0x11, scan: 0 }); // Ctrl
        assert!(
            wait_until(|| source.capturing()),
            "a bare modifier cannot be held to talk: capture again"
        );
        source.answer_capture(Binding::Key { vk: 0x41, scan: 0 }); // A

        assert_eq!(
            captured.recv_timeout(Duration::from_secs(2)).unwrap(),
            Binding::Key { vk: 0x41, scan: 0 }
        );
        session.stop().unwrap();
    }

    #[test]
    fn the_release_delay_elapses_before_the_microphone_mutes() {
        let mic = SharedMic::default();
        let source = FakeSource::default();
        let session = SessionHandle::start(
            config(120),
            mic.clone(),
            source.clone(),
            scratch("delay"),
            true,
        )
        .unwrap();

        source.press(InputEvent::BindingDown);
        assert!(wait_until(|| mic.calls() == vec![true, false]));
        source.press(InputEvent::BindingUp);
        let released_at = Instant::now();

        // Several POLL wake-ups happen before the deadline; they must not
        // drop it (that would leave the microphone open forever).
        let mut muted_at = None;
        for _ in 0..200 {
            if mic.calls() == vec![true, false, true] {
                muted_at = Some(Instant::now());
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        session.stop().unwrap();

        let muted_at = muted_at.expect("the microphone muted after the delay");
        let waited = muted_at.duration_since(released_at);
        assert!(
            waited >= Duration::from_millis(120),
            "muted before the release delay: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(2),
            "muted far too late: {waited:?}"
        );
    }

    #[test]
    fn an_engine_failure_reaches_the_owner_and_ends_the_session() {
        let mic = FlakyMic::default();
        let source = FakeSource::default();
        let path = scratch("failure");
        let session =
            SessionHandle::start(config(0), mic.clone(), source.clone(), path.clone(), true)
                .unwrap();

        mic.fail_from_now_on();
        source.press(InputEvent::BindingDown);

        assert!(wait_until(|| session.finished()), "the session gave up");
        let status = session.status();
        assert!(status.error.is_some(), "the error is reported: {status:?}");
        assert!(session.stop().is_err(), "the caller sees the failure too");
        match crate::failsafe::read(&path) {
            // Never claimed a clean hand-back it could not perform (§6.6).
            crate::failsafe::Snapshot::Found(state) => assert!(state.dirty),
            other => panic!("the record was lost: {other:?}"),
        }
    }
}
