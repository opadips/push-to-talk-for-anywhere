//! Linux (and anything speaking the PulseAudio native protocol — PipeWire's
//! `pipewire-pulse` included) implementation of [`MicController`].
//!
//! Every capture endpoint on a PulseAudio server is a *source*; muting is
//! the server-side source-mute flag: it applies system-wide, to whatever is
//! capturing through that source, exactly like the endpoint mute the Windows
//! WASAPI backend drives. The design mirrors [`crate::audio::wasapi`]: the
//! endpoint is stored as an id string — here the PulseAudio source *name*,
//! an id, never a list index — and [`super::DEFAULT_DEVICE`] means "whatever
//! the server reports as the default source right now", re-resolved on every
//! call so unplugging a device or switching the default is picked up on the
//! next call instead of caching a dead name.
//!
//! Structure: libpulse is callback-based and its mainloop type is `!Send`
//! (it is `Rc`-based), so one dedicated worker thread owns every mainloop,
//! context and operation. The controller itself is just a channel: each of
//! the three trait methods ships one command over and blocks on a one-shot
//! reply, and the worker performs the whole round-trip synchronously —
//! *connect freshly, per call* (a restart of the sound server or a device
//! change is picked up by the next call, same contract as WASAPI's
//! re-create-per-call), run the operation, answer. A five-second deadline
//! bounds everything: a hung or wedged sound server turns into
//! [`Error::Backend`] after five seconds instead of stalling the session
//! worker forever.
//!
//! The parts that are pure without an audio server — the decision of which
//! PulseAudio sources count as microphones and how the requested id decides
//! between them — are extracted as [`microphone_from_source`] and
//! [`find_source`] and unit-tested below, since this dev environment has no
//! sound server to enumerate.

use super::{DeviceInfo, MicController, DEFAULT_DEVICE};
use crate::error::{Error, Result};
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State as ContextState};
use libpulse_binding::error::PAErr;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::operation::{Operation, State as OperationState};
use libpulse_binding::proplist::{properties, Proplist};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// Overall deadline for one command, connect and operation together.
const ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling interval while waiting for the server: `iterate` runs without
/// blocking (after the last event of an operation there is nothing left to
/// wake it), so sleeping between polls stops the wait from busy-spinning.
const TICK: Duration = Duration::from_millis(5);

/// The name the mixer and the server display for this client.
const CLIENT_NAME: &str = "push-to-talk-for-anywhere";

/// One user request, executed by the worker thread. The device id travels in
/// the envelope because the worker outlives the controller object that was
/// constructed with it.
enum Command {
    List(Sender<Result<Vec<DeviceInfo>>>),
    GetMute(String, Sender<Result<bool>>),
    SetMute(String, bool, Sender<Result<()>>),
}

/// The PulseAudio [`MicController`].
///
/// `new` takes the device id to talk to: [`super::DEFAULT_DEVICE`] follows
/// the server's current default source, anything else is a source *name*.
pub struct PulseController {
    device_id: String,
    commands: Sender<Command>,
}

impl PulseController {
    pub fn new(device_id: impl Into<String>) -> Self {
        let (commands, queue) = channel::<Command>();
        let spawned = std::thread::Builder::new()
            .name("ptt-pulse".into())
            .spawn(move || worker(queue));
        if let Err(error) = spawned {
            // Only ever OOM/panic; every call then fails with the channel
            // send below instead of hanging the session worker.
            tracing::warn!("cannot start the PulseAudio worker thread: {error}");
        }
        Self {
            device_id: device_id.into(),
            commands,
        }
    }

    fn ask<T>(&self, make: impl FnOnce(&str, Sender<Result<T>>) -> Command) -> Result<T> {
        let (reply_tx, reply_rx) = channel();
        // A failed send — or a dropped reply channel — means the worker
        // thread is gone: no audio control is possible any more, the same
        // outcome a dead COM gives the Windows path.
        self.commands
            .send(make(&self.device_id, reply_tx))
            .map_err(|_| Error::Backend("the PulseAudio worker stopped".into()))?;
        reply_rx
            .recv()
            .map_err(|_| Error::Backend("the PulseAudio worker stopped before replying".into()))?
    }
}

impl MicController for PulseController {
    fn list_capture_devices(&self) -> Result<Vec<DeviceInfo>> {
        self.ask(|_, reply| Command::List(reply))
    }

    fn get_mute(&self) -> Result<bool> {
        self.ask(|device_id, reply| Command::GetMute(device_id.to_owned(), reply))
    }

    fn set_mute(&self, muted: bool) -> Result<()> {
        self.ask(|device_id, reply| Command::SetMute(device_id.to_owned(), muted, reply))
    }
}

// --- worker thread ---------------------------------------------------------

fn worker(queue: Receiver<Command>) {
    // Ends when the controller is dropped: the queue closes and the
    // mainloop/context state lives only within one command's scope.
    while let Ok(command) = queue.recv() {
        match command {
            Command::List(reply) => {
                let _ = reply.send(with_context(list_devices));
            }
            Command::GetMute(device_id, reply) => {
                let _ = reply.send(with_context(|session, deadline| {
                    read_mute(session, &device_id, deadline)
                }));
            }
            Command::SetMute(device_id, muted, reply) => {
                let _ = reply.send(with_context(|session, deadline| {
                    write_mute(session, &device_id, muted, deadline)
                }));
            }
        }
    }
}

// --- one connected session (owned by the worker, per command) ----------------

/// A connected context *and* the mainloop driving it, as one unit: every
/// operation needs both — the context to issue, the mainloop to pump the
/// events that run libpulse's callbacks. Dropped together after one
/// command's work is done; `context` is declared first on purpose, so it
/// drops before `mainloop` it borrows from.
struct Session {
    context: Context,
    mainloop: Mainloop,
}

impl Session {
    /// Connect (freshly — see the module doc) within the deadline.
    fn connect(deadline: Instant) -> Result<Self> {
        let mut mainloop = Mainloop::new()
            .ok_or_else(|| Error::Backend("cannot create the PulseAudio mainloop".into()))?;
        let mut proplist = Proplist::new()
            .ok_or_else(|| Error::Backend("cannot create a PulseAudio proplist".into()))?;
        if proplist
            .set_str(properties::APPLICATION_NAME, CLIENT_NAME)
            .is_err()
        {
            // Cosmetic only: the client still works, just shows unnamed.
            tracing::debug!("cannot set the PulseAudio client name property");
        }
        let mut context = Context::new_with_proplist(&mainloop, CLIENT_NAME, &proplist)
            .ok_or_else(|| Error::Backend("cannot create the PulseAudio context".into()))?;
        // NOFAIL | NOAUTOSPAWN: never *spawn* a daemon merely to mute
        // something — a machine without a sound server has nothing to mute.
        context
            .connect(None, FlagSet::NOFAIL | FlagSet::NOAUTOSPAWN, None)
            .map_err(|PAErr(code)| {
                Error::Backend(format!("cannot connect to the PulseAudio server ({code})"))
            })?;
        loop {
            match context.get_state() {
                ContextState::Ready => break,
                ContextState::Failed | ContextState::Terminated => {
                    return Err(Error::Backend(format!(
                        "the PulseAudio connection failed: {:?}",
                        context.get_state()
                    )));
                }
                _ => {}
            }
            within(deadline)?;
            session_pump(&mut mainloop)?;
            std::thread::sleep(TICK);
        }
        Ok(Self { context, mainloop })
    }

    /// Run one asynchronous operation to completion (or the deadline).
    ///
    /// The operation object must stay alive while it is running — dropping
    /// it unrefs the underlying C operation, which stops its callbacks — so
    /// this drives by iterating the mainloop, not by watching a flag alone.
    fn drive<T: ?Sized>(&mut self, operation: Operation<T>, deadline: Instant) -> Result<()> {
        loop {
            match operation.get_state() {
                OperationState::Done | OperationState::Cancelled => return Ok(()),
                OperationState::Running => {}
            }
            within(deadline)?;
            session_pump(&mut self.mainloop)?;
            std::thread::sleep(TICK);
        }
    }
}

/// One idle-`iterate` round. The mainloop is run without blocking (`false`)
/// and we sleep between rounds; Quit is reported like Success because this
/// code only ever runs a mainloop it never asks to quit.
fn session_pump(mainloop: &mut Mainloop) -> Result<()> {
    match mainloop.iterate(false) {
        IterateResult::Success(_) => Ok(()),
        IterateResult::Quit(_) => Ok(()),
        IterateResult::Err(PAErr(code)) => Err(Error::Backend(format!(
            "the PulseAudio mainloop failed: {code}"
        ))),
    }
}

/// The overall deadline for a command: a wedged server is an error after
/// five seconds, exactly like a silent Windows COM call.
fn within(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(Error::Backend(
            "the sound server did not answer within 5 s".into(),
        ));
    }
    Ok(())
}

/// Whole lifetime of one command: connect late, run `work` immediately, and
/// let everything drop before the next command starts (so a restarted or
/// dead server is simply a failed *this* command, not a broken worker). The
/// deadline covers connect *and* the work — one command, five seconds, and
/// `work` receives it because libpulse operations eat whatever is left of
/// it.
fn with_context<T>(work: impl FnOnce(&mut Session, Instant) -> Result<T>) -> Result<T> {
    let deadline = Instant::now() + ROUND_TRIP_TIMEOUT;
    let mut session = Session::connect(deadline)?;
    work(&mut session, deadline)
}

// --- enumeration -------------------------------------------------------------

/// One PulseAudio source, reduced to what the mute question needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceRow {
    /// The source's *name* — the id this backend addresses it by.
    id: String,
    /// The human-readable description ("USB Microphone") to show.
    description: Option<String>,
    /// `Some` when the source is the monitor loopback of a sink's *playback*
    /// stream — never something a user could physically talk into.
    monitor_of_sink: Option<u32>,
    /// Whether the server currently routes the default capture into it.
    is_default: bool,
    mute: bool,
}

/// The source list as the app sees it: only physically real microphones
/// with a default flag attached where the server says so.
pub(crate) fn microphones(sources: &[SourceRow]) -> Vec<DeviceInfo> {
    sources
        .iter()
        .filter_map(microphone_from_source)
        .collect()
}

/// Which sources count as microphones (`None` when one does not).
///
/// The one rule: *monitor* sources — the loopbacks PulseAudio installs on
/// every sink by default, named `…Sink.monitor`, discovered here by the
/// `monitor_of_sink` back-reference rather than a name suffix — are
/// playback-mirrors, not microphones, and would otherwise flood "device"
/// lists with dozens of unusable entries. Everything else is captured and
/// marked default when it is the server's default capture source.
pub(crate) fn microphone_from_source(source: &SourceRow) -> Option<DeviceInfo> {
    if source.monitor_of_sink.is_some() {
        return None;
    }
    Some(DeviceInfo {
        id: source.id.clone(),
        name: source
            .description
            .clone()
            .unwrap_or_else(|| source.id.clone()),
        // The *default* too — even though the very shape of a default capture
        // device makes a monitor audit least likely, being explicit here
        // costs nothing.
        is_default: source.is_default,
    })
}

/// Resolve the id this controller was configured for against a gathered
/// source list: [`DEFAULT_DEVICE`] is the row the server marked default;
/// anything else must match a source *name* exactly, so a typo in
/// `config.toml` produces a readable error instead of a silent fallback —
/// the same contract [`super::pick_device`] enforces on Windows for
/// endpoint ids.
pub(crate) fn find_source<'a>(sources: &'a [SourceRow], requested: &str) -> Result<&'a SourceRow> {
    if requested == DEFAULT_DEVICE {
        return sources
            .iter()
            .find(|source| source.is_default)
            .ok_or(Error::NoDefaultDevice);
    }
    sources
        .iter()
        .find(|source| source.id == requested)
        .ok_or_else(|| Error::DeviceNotFound(requested.to_string()))
}

/// One source-enumeration round trip: lists the sources and, at the same
/// time, asks the server which one capture defaults to (both callbacks fill
/// the same shared cell; the default comes as a one-shot server-info
/// callback after the source list completes).
fn enumerate(session: &mut Session, deadline: Instant) -> Result<Vec<SourceRow>> {
    let shared = Rc::new(RefCell::new(Enumerated {
        sources: Vec::new(),
        failed: false,
        default_source: None,
    }));

    let source_shared = Rc::clone(&shared);
    let source_operation = session
        .context
        .introspect()
        .get_source_info_list(move |result| match result {
            ListResult::Item(info) => source_shared.borrow_mut().push_source(info),
            ListResult::End => {}
            ListResult::Error => source_shared.borrow_mut().failed = true,
        });
    session.drive(source_operation, deadline)?;

    let default_shared = Rc::clone(&shared);
    let server_operation = session.context.introspect().get_server_info(move |info| {
        default_shared.borrow_mut().default_source = info
            .default_source_name
            .as_ref()
            .map(|name| name.to_string());
    });
    session.drive(server_operation, deadline)?;

    let gathered = shared.borrow();
    if gathered.failed {
        tracing::debug!("the PulseAudio source list ended in an error");
        return Err(Error::Backend(
            "listing the PulseAudio sources failed".into(),
        ));
    }
    Ok(gathered.sources.clone())
}

#[derive(Debug, Default)]
struct Enumerated {
    sources: Vec<SourceRow>,
    failed: bool,
    default_source: Option<String>,
}

impl Enumerated {
    fn push_source(&mut self, info: &libpulse_binding::context::introspect::SourceInfo) {
        let name = info.name.as_ref().map(Cow::as_ref);
        self.sources.push(SourceRow {
            id: name.unwrap_or_default().to_owned(),
            description: info.description.as_ref().map(|d| d.to_string()),
            monitor_of_sink: info.monitor_of_sink,
            is_default: self.default_source.as_deref() == name,
            mute: info.mute,
        });
    }
}

// --- the three trait operations, in the worker's terms -----------------------

fn list_devices(session: &mut Session, deadline: Instant) -> Result<Vec<DeviceInfo>> {
    let sources = enumerate(session, deadline)?;
    Ok(microphones(&sources))
}

fn read_mute(session: &mut Session, device_id: &str, deadline: Instant) -> Result<bool> {
    let sources = enumerate(session, deadline)?;
    Ok(find_source(&sources, device_id)?.mute)
}

/// Flip one source's server-side mute flag. The callback's bool is the
/// server's answer about whether the flip was applied; the operation can
/// also reach `Done` without the callback ever meaning "worked", so the
/// flag — not just the operation state — decides success.
fn write_mute(
    session: &mut Session,
    device_id: &str,
    muted: bool,
    deadline: Instant,
) -> Result<()> {
    let sources = enumerate(session, deadline)?;
    let target = find_source(&sources, device_id)?;

    let succeeded = Rc::new(Cell::new(false));
    let answer = Rc::clone(&succeeded);
    let mut introspector = session.context.introspect();
    let operation = introspector.set_source_mute_by_name(
        &target.id,
        muted,
        Some(Box::new(move |ok| answer.set(ok))),
    );
    session.drive(operation, deadline)?;
    if succeeded.get() {
        Ok(())
    } else {
        Err(Error::Backend(format!(
            "the sound server refused to {} the mute on '{device_id}'",
            if muted { "set" } else { "clear" }
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{find_source, microphone_from_source, microphones, SourceRow};
    use crate::audio::DEFAULT_DEVICE;
    use crate::error::Error;

    fn row(
        id: &str,
        description: Option<&str>,
        monitor_of: Option<u32>,
        default: bool,
    ) -> SourceRow {
        SourceRow {
            id: id.to_owned(),
            description: description.map(str::to_owned),
            monitor_of_sink: monitor_of,
            is_default: default,
            mute: false,
        }
    }

    fn plausible_server() -> Vec<SourceRow> {
        vec![
            row(
                "alsa_output.pci.monitor",
                Some("Built-in Audio Analog Stereo"),
                Some(0),
                false,
            ),
            row(
                "alsa_input.pci.mic",
                Some("Built-in Audio Analog Microphone"),
                None,
                false,
            ),
            row(
                "bluez_input headset",
                Some("USB Headset Microphone"),
                None,
                true,
            ),
            // A source PulseAudio may expose with no description yet.
            row("odd.source", None, None, false),
        ]
    }

    #[test]
    fn monitors_of_sinks_are_not_microphones() {
        let devices = microphones(&plausible_server());
        assert!(devices.iter().all(|d| d.id != "alsa_output.pci.monitor"));
        assert_eq!(devices.len(), 3);
    }

    #[test]
    fn the_default_flag_travels_through() {
        let devices = microphones(&plausible_server());
        let defaults: Vec<_> = devices.iter().filter(|d| d.is_default).collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].id, "bluez_input headset");
    }

    #[test]
    fn a_missing_description_falls_back_to_the_id() {
        let devices = microphones(&plausible_server());
        let named = devices
            .iter()
            .find(|d| d.id == "odd.source")
            .expect("present");
        assert_eq!(named.name, "odd.source");
    }

    #[test]
    fn a_described_source_shows_its_description() {
        let devices = microphones(&plausible_server());
        let named = devices
            .iter()
            .find(|d| d.id == "alsa_input.pci.mic")
            .expect("present");
        assert_eq!(named.name, "Built-in Audio Analog Microphone");
        assert!(!named.is_default);
    }

    #[test]
    fn default_device_resolves_to_the_row_the_server_marked() {
        let sources = plausible_server();
        let picked = find_source(&sources, DEFAULT_DEVICE).expect("a default exists");
        assert_eq!(picked.id, "bluez_input headset");
    }

    #[test]
    fn default_device_reports_a_missing_default() {
        let sources = vec![row("alsa_input.pci.mic", Some("Mic"), None, false)];
        let error = find_source(&sources, DEFAULT_DEVICE).expect_err("no default");
        assert!(matches!(error, Error::NoDefaultDevice));
    }

    #[test]
    fn default_device_reports_a_missing_default_on_an_empty_list() {
        let error = find_source(&[], DEFAULT_DEVICE).expect_err("no sources at all");
        assert!(matches!(error, Error::NoDefaultDevice));
    }

    #[test]
    fn a_concrete_source_name_matches_exactly() {
        let sources = plausible_server();
        let picked = find_source(&sources, "alsa_input.pci.mic").expect("present");
        assert_eq!(picked.id, "alsa_input.pci.mic");
    }

    #[test]
    fn an_unknown_source_name_is_rejected_and_named() {
        let error = find_source(&plausible_server(), "typo.mic").expect_err("no such source");
        assert!(matches!(error, Error::DeviceNotFound(_)));
        assert!(error.to_string().contains("typo.mic"));
    }

    #[test]
    fn only_an_exact_name_match_wins_not_a_substring() {
        let error = find_source(&plausible_server(), "alsa_input.pci")
            .expect_err("substring must not match");
        assert!(matches!(error, Error::DeviceNotFound(_)));
    }

    #[test]
    fn the_monitor_filter_runs_on_one_row_alone() {
        let monitor = row("sink.monitor", Some("Sink monitor"), Some(7), false);
        assert!(microphone_from_source(&monitor).is_none());
        let physical = row("mic", None, None, false);
        assert!(microphone_from_source(&physical).is_some());
    }
}
