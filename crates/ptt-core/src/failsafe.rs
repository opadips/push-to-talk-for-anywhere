//! Fail-safe: record the original mute state, restore it on exit, panic, kill
//! or crash (plan §6). This is a core requirement, not polish.
//!
//! The rule the tests below protect: whenever the app has touched the
//! microphone, `state.json` says so (`dirty`), and every way the process can
//! end leads back to the state the user started in.

use crate::audio::MicController;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `state.json` contents (plan §6.1).
///
/// `Eq` had to go when `volume` arrived — the record now can hold a float —
/// so the tests compare field by field or through [`PartialEq`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    /// Endpoint the record belongs to (plan §7: an id, never an index).
    pub device_id: String,
    /// The mute state the user had before this run touched the microphone.
    pub original_muted: bool,
    /// True from the moment the microphone is first touched until it has
    /// been handed back (plan §6).
    pub dirty: bool,
    /// Process that wrote the record, so a kill can be recognised (§6.4).
    pub pid: u32,
    /// The device volume the run started with, when the backend mutes by
    /// driving the volume to zero (spec §4: only such a backend reports
    /// one — Windows' WASAPI and PulseAudio never do). `None` in every v1
    /// record, via the serde default, and in every record from a
    /// mute-controlling backend.
    #[serde(default)]
    pub volume: Option<f32>,
}

/// What reading `state.json` produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Snapshot {
    /// No file: nothing was recorded.
    None,
    /// A readable record.
    Found(StateFile),
    /// Unreadable — reported, never acted on, never a crash.
    Unreadable(String),
}

impl Snapshot {
    /// Plan §6.4: recovery is needed only when a *dirty* record was left
    /// behind by a process that is no longer running. A live pid means the
    /// other instance still owns the microphone.
    pub fn needs_recovery(&self, is_running: impl Fn(u32) -> bool) -> Option<StateFile> {
        match self {
            Snapshot::Found(state) if state.dirty && !is_running(state.pid) => Some(state.clone()),
            _ => None,
        }
    }
}

/// `%LOCALAPPDATA%\ptt-tool\state.json` (plan §2); see [`crate::paths`].
pub fn default_path() -> PathBuf {
    crate::paths::state_file()
}

/// Write the record **before** the microphone is touched (plan §6.1).
///
/// The mute-only shape every Windows caller has used since v1: the volume
/// field stays `None` because WASAPI mutes natively and never reports a
/// volume. See `record_with_volume` for the volume-carrying form.
pub fn record(path: &Path, device_id: &str, original_muted: bool) -> Result<()> {
    record_state(
        path,
        &StateFile {
            device_id: device_id.to_string(),
            original_muted,
            dirty: true,
            pid: std::process::id(),
            volume: None,
        },
    )
}

/// [`record`] plus the device volume: for backends that mute by driving the
/// volume to zero (spec §4), so a crash can hand the original level back.
///
/// The volume is fetched from `controller` *here*, before anything is
/// written, and `None` when the controller does not report one — falls back
/// to [`record`], in other words. `original_muted` is the value the caller
/// already read (usually via `controller.get_mute()`); it is taken rather
/// than re-fetched because MuteUnsupported-style errors must abort the
/// caller, not this write.
pub fn record_with_volume(
    path: &Path,
    device_id: &str,
    original_muted: bool,
    controller: &dyn MicController,
) -> Result<()> {
    let volume = controller.get_volume().ok().flatten();
    record_state(
        path,
        &StateFile {
            device_id: device_id.to_string(),
            original_muted,
            dirty: true,
            pid: std::process::id(),
            volume,
        },
    )
}

/// Serialise a complete record — the single write every `record_*` ends in.
fn record_state(path: &Path, state: &StateFile) -> Result<()> {
    write_state(path, state)
}

/// Mark the run clean after the microphone has been handed back (plan §6.2).
pub fn mark_clean(path: &Path) -> Result<()> {
    let mut state = match read(path) {
        Snapshot::Found(state) => state,
        // Nothing recorded, or nothing trustworthy: nothing to clear.
        Snapshot::None | Snapshot::Unreadable(_) => return Ok(()),
    };
    state.dirty = false;
    write_state(path, &state)
}

/// Read `state.json`. Never fails: a missing file is [`Snapshot::None`] and
/// an unreadable one is reported as such.
pub fn read(path: &Path) -> Snapshot {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Snapshot::None,
        Err(error) => return Snapshot::Unreadable(error.to_string()),
    };
    match serde_json::from_str(&text) {
        Ok(state) => Snapshot::Found(state),
        Err(error) => Snapshot::Unreadable(error.to_string()),
    }
}

/// Hand the microphone back exactly as recorded, then clear the dirty flag
/// (plan §6.2). The controller must target [`StateFile::device_id`].
///
/// The recorded volume goes back *first* (spec §4 D6): the volume-muting
/// backends restore the level before the mute flip, so "unmute" can never
/// land on a still-zeroed volume. A backend that has no volume control —
/// Windows' WASAPI, PulseAudio — answers the volume restore with
/// [`Error::VolumeUnsupported`]; that is tolerated here rather than failing
/// the whole restore, because the mute hand-back is the authoritative
/// requirement and a v2 record being handed to a mute-native backend is not
/// an error worth muting over.
pub fn restore(ctl: &dyn MicController, state: &StateFile, path: &Path) -> Result<()> {
    if let Some(volume) = state.volume {
        let restored = ctl.set_volume(volume);
        if let Err(error) = restored {
            tracing::warn!("cannot restore the recorded volume: {error}");
        }
    }
    ctl.set_mute(state.original_muted)?;
    mark_clean(path)?;
    Ok(())
}

/// Spec §4 D6: one log line for a panic — where it happened and what
/// panicked, whatever the payload type.
pub fn panic_summary(
    payload: &(dyn std::any::Any + Send),
    location: Option<&std::panic::Location<'_>>,
) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    match location {
        Some(location) => format!("panic at {location}: {text}"),
        None => format!("panic: {text}"),
    }
}

/// Plan §6.3: attempt the restore before the process dies. The caller
/// supplies the restore logic because only it knows how to reach the
/// microphone from whichever thread is panicking.
pub fn install_panic_hook(restore: impl Fn() + Send + Sync + 'static) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{}", panic_summary(info.payload(), info.location()));
        restore();
        previous(info);
    }));
}

/// What start-up recovery decided (plan §6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Nothing recorded, or nothing worth restoring.
    Nothing,
    /// The recorded process is still alive: that instance owns the
    /// microphone and must not be disturbed.
    Busy { pid: u32 },
    /// The microphone was handed back after the previous run died.
    Restored {
        device_id: String,
        original_muted: bool,
    },
}

/// Plan §6.4: start-up recovery. When a *dirty* record was left behind by a
/// process that is no longer running, restore the recorded state and clear
/// the record. `make_controller` builds a controller for the recorded
/// device, so the caller decides how (plan §4 keeps that Windows-specific).
pub fn recover<C: MicController>(
    path: &Path,
    is_running: impl Fn(u32) -> bool,
    make_controller: impl Fn(&str) -> C,
) -> Result<Recovery> {
    let state = match read(path) {
        Snapshot::Found(state) if state.dirty => state,
        _ => return Ok(Recovery::Nothing),
    };
    if is_running(state.pid) {
        // A live pid means another instance is still running (§6.4).
        return Ok(Recovery::Busy { pid: state.pid });
    }
    let controller = make_controller(&state.device_id);
    restore(&controller, &state, path)?;
    Ok(Recovery::Restored {
        device_id: state.device_id,
        original_muted: state.original_muted,
    })
}

/// Plan §6.3: hand a *dirty* record back on the way out (panic, forced
/// shutdown). The pid check is skipped on purpose — the process asking is
/// the one that wrote the record. Returns whether anything was restored.
pub fn restore_dirty<C: MicController>(
    path: &Path,
    make_controller: impl Fn(&str) -> C,
) -> Result<bool> {
    let state = match read(path) {
        Snapshot::Found(state) if state.dirty => state,
        _ => return Ok(false),
    };
    let controller = make_controller(&state.device_id);
    restore(&controller, &state, path)?;
    Ok(true)
}

/// Write atomically: temp file + rename, like the config (plan §8), so a
/// torn record can never hide a dirty microphone.
fn write_state(path: &Path, state: &StateFile) -> Result<()> {
    let text =
        serde_json::to_string_pretty(state).map_err(|error| Error::Write(error.to_string()))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{DeviceInfo, MicController};
    use crate::error::Result as CoreResult;
    use std::any::Any;
    use std::panic::Location;
    use std::sync::{Arc, Mutex};

    /// Fake microphone: records mute calls and can be told to fail (plan §10).
    /// Clones share the call log, so a controller factory can build fresh
    /// ones and the test still sees everything.
    #[derive(Clone)]
    struct FakeMic {
        calls: Arc<Mutex<Vec<bool>>>,
        fail: bool,
    }

    impl FakeMic {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: true,
            }
        }

        fn calls(&self) -> Vec<bool> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MicController for FakeMic {
        fn list_capture_devices(&self) -> CoreResult<Vec<DeviceInfo>> {
            Ok(Vec::new())
        }

        fn get_mute(&self) -> CoreResult<bool> {
            Ok(*self.calls.lock().unwrap().last().unwrap_or(&true))
        }

        fn set_mute(&self, muted: bool) -> CoreResult<()> {
            if self.fail {
                return Err(crate::error::Error::DeviceUnavailable("gone".into()));
            }
            self.calls.lock().unwrap().push(muted);
            Ok(())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ptt-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    impl Snapshot {
        /// Test-only helper: the record we expect to be there.
        fn expect_found(self) -> StateFile {
            match self {
                Snapshot::Found(state) => state,
                other => panic!("expected a record, got {other:?}"),
            }
        }
    }

    #[test]
    fn default_path_lives_under_ptt_tool() {
        assert!(default_path().ends_with(std::path::Path::new("ptt-tool").join("state.json")));
    }

    #[test]
    fn record_writes_the_fields_plan_6_1_requires() {
        let path = scratch("record.json");
        record(&path, "{0.0.1.00000000}.abc", false).expect("record works");

        let snapshot = read(&path);
        let state = snapshot.expect_found();
        assert_eq!(state.device_id, "{0.0.1.00000000}.abc");
        assert!(!state.original_muted, "the mic was open at start-up");
        assert!(state.dirty, "a started session is dirty until restored");
        assert_eq!(state.pid, std::process::id());
    }

    #[test]
    fn mark_clean_keeps_the_record_but_clears_the_dirty_flag() {
        let path = scratch("clean.json");
        record(&path, "default", true).unwrap();
        mark_clean(&path).unwrap();

        let state = read(&path).expect_found();
        assert!(!state.dirty, "clean exit is recorded");
        assert!(state.original_muted, "the record is kept for debugging");
    }

    #[test]
    fn a_missing_record_means_nothing_to_recover() {
        let path = scratch("absent.json");
        let _ = std::fs::remove_file(&path);

        assert_eq!(read(&path), Snapshot::None);
        assert_eq!(
            read(&path).needs_recovery(|_| false),
            None,
            "nothing recorded, nothing to restore"
        );
    }

    #[test]
    fn an_unreadable_record_is_reported_not_crashed_on() {
        let path = scratch("garbage.json");
        std::fs::write(&path, "{ not json").unwrap();

        let snapshot = read(&path);
        assert!(
            matches!(snapshot, Snapshot::Unreadable(_)),
            "reported instead of crashing: {snapshot:?}"
        );
        assert_eq!(snapshot.needs_recovery(|_| false), None);
    }

    #[test]
    fn a_dirty_record_from_a_dead_process_needs_recovery() {
        let path = scratch("dead.json");
        record(&path, "default", true).unwrap();

        let needed = read(&path).needs_recovery(|_| false);
        assert_eq!(
            needed,
            Some(StateFile {
                device_id: "default".into(),
                original_muted: true,
                dirty: true,
                pid: std::process::id(),
                volume: None,
            }),
            "the mic must come back after a kill"
        );
    }

    #[test]
    fn a_clean_record_needs_no_recovery() {
        let path = scratch("clean-exit.json");
        record(&path, "default", true).unwrap();
        mark_clean(&path).unwrap();

        assert_eq!(read(&path).needs_recovery(|_| false), None);
    }

    #[test]
    fn a_live_process_keeps_its_own_microphone() {
        let path = scratch("live.json");
        record(&path, "default", false).unwrap();

        assert_eq!(
            read(&path).needs_recovery(|_| true),
            None,
            "another running instance owns the microphone"
        );
    }

    #[test]
    fn restore_hands_back_the_recorded_state_and_cleans_up() {
        let path = scratch("restore.json");
        record(&path, "default", false).unwrap();
        let state = read(&path).expect_found();

        let mic = FakeMic::new();
        restore(&mic, &state, &path).expect("restore works");

        assert_eq!(mic.calls(), vec![false], "original state handed back");
        assert!(!read(&path).expect_found().dirty, "the run is now clean");
    }

    // --- volume seam (spec §4: volume-muting backends must be recoverable) ---

    /// A fake microphone that also carries the volume seam: reports a
    /// programmable volume and records every call in order, so a test can
    /// assert the restore *order* as well as the values.
    #[derive(Clone)]
    struct VolumeMic {
        calls: Arc<Mutex<Vec<String>>>,
        report_volume: Option<f32>,
        fail: bool,
    }

    impl VolumeMic {
        fn reporting(volume: f32) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                report_volume: Some(volume),
                fail: false,
            }
        }

        /// Without a volume control at all — the WASAPI/PulseAudio shape.
        fn mute_only() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                report_volume: None,
                fail: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MicController for VolumeMic {
        fn list_capture_devices(&self) -> CoreResult<Vec<DeviceInfo>> {
            Ok(Vec::new())
        }

        fn get_mute(&self) -> CoreResult<bool> {
            Ok(self.calls().last().is_some_and(|call| *call == "mute true"))
        }

        fn set_mute(&self, muted: bool) -> CoreResult<()> {
            if self.fail {
                return Err(crate::error::Error::DeviceUnavailable("gone".into()));
            }
            self.calls.lock().unwrap().push(format!("mute {muted}"));
            Ok(())
        }

        fn get_volume(&self) -> CoreResult<Option<f32>> {
            Ok(self.report_volume)
        }

        fn set_volume(&self, volume: f32) -> CoreResult<()> {
            if self.report_volume.is_none() {
                return Err(crate::error::Error::VolumeUnsupported);
            }
            self.calls.lock().unwrap().push(format!("volume {volume}"));
            Ok(())
        }
    }

    #[test]
    fn record_stores_the_volume_the_controller_reports() {
        let path = scratch("volume.json");
        let mic = VolumeMic::reporting(0.5);
        record_with_volume(&path, "default", false, &mic).unwrap();

        let state = read(&path).expect_found();
        assert_eq!(state.volume, Some(0.5), "the start-up volume is stored");
        assert!(state.dirty);
        assert!(!state.original_muted);
    }

    #[test]
    fn record_with_a_mute_only_controller_stores_no_volume() {
        let path = scratch("volume-none.json");
        let mic = VolumeMic::mute_only();
        record_with_volume(&path, "default", true, &mic).unwrap();

        let state = read(&path).expect_found();
        assert_eq!(state.volume, None, "no volume control, no volume stored");
    }

    #[test]
    fn plain_record_keeps_the_volume_field_empty() {
        let path = scratch("volume-plain.json");
        record(&path, "default", true).unwrap();
        assert_eq!(read(&path).expect_found().volume, None);
    }

    #[test]
    fn a_v1_record_without_a_volume_field_still_reads_back() {
        // The shape the Windows release v1.2 writes — no `volume` key.
        let path = scratch("v1.json");
        std::fs::write(
            &path,
            r#"{
  "device_id": "default",
  "original_muted": true,
  "dirty": false,
  "pid": 4242
}"#,
        )
        .unwrap();

        let state = read(&path).expect_found();
        assert_eq!(state.volume, None, "v1 records default to no volume");
        assert_eq!(state.pid, 4242);
    }

    #[test]
    fn restore_applies_the_volume_before_the_mute() {
        let path = scratch("volume-restore.json");
        let mic = VolumeMic::reporting(0.8);
        record_with_volume(&path, "default", true, &mic).unwrap();
        let state = read(&path).expect_found();

        let mic = VolumeMic::reporting(0.8);
        restore(&mic, &state, &path).unwrap();

        // Volume first — "unmute" must not land on a still-zeroed volume —
        // then the recorded mute state, then nothing else.
        assert_eq!(
            mic.calls(),
            vec!["volume 0.8", "mute true"],
            "volume before mute, exactly as spec §4 orders it"
        );
        assert!(!read(&path).expect_found().dirty);
    }

    #[test]
    fn restore_tolerates_a_backend_without_volume_control() {
        // A v2 record handed to a mute-native backend (or recovered by a
        // different OS build): the mute hand-back is the authoritative
        // requirement, the volume is best-effort.
        let path = scratch("volume-tolerant.json");
        record_with_volume(&path, "default", true, &VolumeMic::reporting(0.6)).unwrap();
        let state = read(&path).expect_found();

        let mic = VolumeMic::mute_only();
        restore(&mic, &state, &path).unwrap();

        assert_eq!(mic.calls(), vec!["mute true"], "the mute still lands");
        assert!(!read(&path).expect_found().dirty, "the run is clean");
    }

    #[test]
    fn restore_without_a_recorded_volume_skips_the_volume_step_entirely() {
        let path = scratch("volume-absent.json");
        record(&path, "default", false).unwrap();
        let state = read(&path).expect_found();

        let mic = VolumeMic::mute_only();
        restore(&mic, &state, &path).unwrap();

        assert_eq!(mic.calls(), vec!["mute false"]);
    }

    #[test]
    fn restore_fails_loudly_when_the_microphone_cannot_be_reached() {
        let path = scratch("restore-fail.json");
        record(&path, "default", true).unwrap();
        let state = read(&path).expect_found();

        let mic = FakeMic::failing();
        let err = restore(&mic, &state, &path).expect_err("the device is gone");
        assert!(!err.to_string().is_empty());
        assert!(
            read(&path).expect_found().dirty,
            "still dirty: the next launch retries"
        );
    }

    /// Records which device the app asked a controller for.
    fn recorder() -> (Arc<Mutex<Vec<String>>>, impl Fn(&str) -> FakeMic) {
        let devices = Arc::new(Mutex::new(Vec::<String>::new()));
        let out = Arc::clone(&devices);
        (devices, move |device: &str| {
            out.lock().unwrap().push(device.to_string());
            FakeMic::new()
        })
    }

    fn dirty(path: &Path) -> bool {
        match read(path) {
            Snapshot::Found(state) => state.dirty,
            other => panic!("expected a record, got {other:?}"),
        }
    }

    #[test]
    fn a_kill_mid_talk_is_recovered_on_the_next_launch() {
        let path = scratch("recover.json");
        record(&path, "{0.0.1.00000000}.abc", false).unwrap();

        let (devices, factory) = recorder();
        let outcome = recover(&path, |_| false, factory).expect("recovery works");

        assert_eq!(
            outcome,
            Recovery::Restored {
                device_id: "{0.0.1.00000000}.abc".into(),
                original_muted: false
            }
        );
        assert_eq!(*devices.lock().unwrap(), vec!["{0.0.1.00000000}.abc"]);
        assert!(!dirty(&path), "the record is clean again");
    }

    #[test]
    fn a_running_instance_is_left_alone() {
        let path = scratch("busy.json");
        record(&path, "default", true).unwrap();

        let (devices, factory) = recorder();
        let outcome = recover(&path, |_| true, factory).expect("no work");

        assert_eq!(
            outcome,
            Recovery::Busy {
                pid: std::process::id()
            }
        );
        assert!(devices.lock().unwrap().is_empty(), "no controller built");
    }

    #[test]
    fn a_clean_exit_needs_no_recovery() -> Result<()> {
        let path = scratch("clean-recover.json");
        record(&path, "default", true).unwrap();
        mark_clean(&path).unwrap();

        let (devices, factory) = recorder();
        assert_eq!(recover(&path, |_| false, factory)?, Recovery::Nothing);
        assert!(devices.lock().unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn an_unreadable_record_needs_no_recovery() -> Result<()> {
        let path = scratch("bad-recover.json");
        std::fs::write(&path, "} not json").unwrap();

        let (devices, factory) = recorder();
        assert_eq!(recover(&path, |_| false, factory)?, Recovery::Nothing);
        assert!(devices.lock().unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn restore_dirty_hands_the_microphone_back_before_the_process_dies() -> Result<()> {
        let path = scratch("panic.json");
        record(&path, "default", true).unwrap();

        let (devices, factory) = recorder();
        assert!(restore_dirty(&path, factory)?, "the record was dirty");

        assert_eq!(*devices.lock().unwrap(), vec!["default"]);
        assert!(!dirty(&path));
        Ok(())
    }

    #[test]
    fn restore_dirty_does_nothing_when_the_run_was_clean() -> Result<()> {
        let path = scratch("panic-clean.json");
        record(&path, "default", true).unwrap();
        mark_clean(&path).unwrap();

        let (devices, factory) = recorder();
        assert!(!restore_dirty(&path, factory)?);
        assert!(devices.lock().unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn the_panic_hook_attempts_the_restore() {
        let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let previous = std::panic::take_hook();

        let hook_calls = Arc::clone(&calls);
        install_panic_hook(move || hook_calls.lock().unwrap().push("restore"));

        let _ = std::panic::catch_unwind(|| panic!("boom"));
        std::panic::set_hook(previous);

        assert_eq!(*calls.lock().unwrap(), vec!["restore"]);
    }

    #[test]
    fn panic_summaries_use_a_string_payload() {
        // `&"boom"`, not `"boom"`: unsized `str` cannot cast to the trait
        // object (rustc's own fix); the payload is `&str` either way.
        let text: &(dyn Any + Send) = &"boom";
        assert_eq!(panic_summary(text, None), "panic: boom");
        let owned: &(dyn Any + Send) = &String::from("boom");
        assert_eq!(panic_summary(owned, None), "panic: boom");
    }

    #[test]
    fn panic_summaries_name_a_location_when_there_is_one() {
        fn here() -> &'static Location<'static> {
            Location::caller()
        }
        let message = panic_summary(&"boom", Some(here()));
        assert!(message.starts_with("panic at "), "{message}");
        assert!(message.contains("failsafe.rs"), "{message}");
        assert!(message.ends_with(": boom"), "{message}");
    }

    #[test]
    fn panic_summaries_tolerate_a_non_string_payload() {
        let other: &(dyn Any + Send) = &5u8;
        assert_eq!(
            panic_summary(other, None),
            "panic: non-string panic payload"
        );
    }
}
