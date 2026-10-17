//! macOS Core Audio implementation of [`MicController`] (plan §7, spec §4).
//!
//! Every capture endpoint on macOS is an audio *device* (service objects and
//! aggregates included); muting is the device's input-scope mute switch —
//! system-wide, to whatever is recording through that device, exactly like
//! the endpoint mute the Windows WASAPI backend drives. The design mirrors
//! [`crate::audio::wasapi`]: the device is stored as its user-facing id
//! string — here the Core Audio `DeviceUID` — never an index, and
//! [`super::DEFAULT_DEVICE`] means "whatever the system reports as the
//! default input device right now", re-resolved on every call so unplugging
//! a device or switching the default is picked up on the next call.
//!
//! ## Devices without a mute switch (spec §4)
//!
//! Some CoreAudio input devices expose no mute control at all — only a
//! volume. For those this backend mutes by driving the volume to zero and
//! unmutes by putting the recorded level back, which is what the
//! [`get_volume`]/[`set_volume`] half of the trait exists for: the failsafe
//! carries the true level through a crash in `state.json`. A device that has
//! *neither* a mute switch nor a volume answers mute requests with
//! [`Error::MuteUnsupported`] instead — there is no way to mute it.
//!
//! Everything outside this module still only sees the plain
//! [`MicController`] surface: `get_mute` on a volume-only device reports
//! "volume is zero" and `set_mute(true)` snapshots the level first, so no
//! caller has to know which rule a device follows.
//!
//! ## Threading
//!
//! CoreAudio hardware-property calls are thread-safe as used here (one
//! synchronous call, no callbacks), so — unlike the PulseAudio mainloop —
//! no worker thread is needed. Calls are re-created per command just like
//! WASAPI's COM calls, to the same contract.

use super::{DeviceInfo, MicController, DEFAULT_DEVICE};
use crate::error::{Error, Result};
use objc2_core_audio::{
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyMute, kAudioDevicePropertyStreams,
    kAudioDevicePropertyVolumeScalar, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioObjectSystemObject,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectHasProperty,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
};
use objc2_core_foundation::{CFRetained, CFString};
use std::ptr::NonNull;
use std::sync::Mutex;

/// The system object every hardware property hangs off.
const SYSTEM_OBJECT: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

// --- property plumbing ------------------------------------------------------

/// OSStatus → `Result`, describing what the call was for so the log line is
/// actionable.
fn status_of(status: i32, what: &str) -> Result<()> {
    (status == 0)
        .then_some(())
        .ok_or_else(|| Error::Backend(format!("CoreAudio error {status:#010x} while {what}")))
}

/// The three-part key every CoreAudio property lives behind.
fn address(selector: u32, scope: u32, element: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    }
}

/// `AudioObjectHasProperty` — does this object answer this key at all?
///
/// SAFETY: caller must pass a live object id; the address is read-only.
unsafe fn has_property(object: AudioObjectID, of: &AudioObjectPropertyAddress) -> bool {
    unsafe { AudioObjectHasProperty(object, NonNull::from(of)) }
}

/// `AudioObjectGetPropertyDataSize` — how many bytes the property needs.
///
/// SAFETY: caller must pass a live object id; the address is read-only.
unsafe fn property_size(object: AudioObjectID, of: &AudioObjectPropertyAddress) -> Result<u32> {
    let mut size: u32 = 0;
    // SAFETY: static lifetimes on the address; size is written by the call.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(of),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    status_of(status, "measuring a CoreAudio property")?;
    Ok(size)
}

/// Read one `Copy` scalar property (mute flags, scalars, object ids).
///
/// SAFETY: `T` must be the exact layout CoreAudio writes for this property
/// (all uses below use `u32` and `f32`, whose layouts the header fixes).
unsafe fn read<T: Copy>(
    object: AudioObjectID,
    of: &AudioObjectPropertyAddress,
    what: &str,
) -> Result<T> {
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut size = std::mem::size_of::<T>() as u32;
    // SAFETY: the pointer or buffer is the caller's own stack memory and the
    // exact size is announced; CoreAudio never writes more than announced.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(of),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(value.as_mut_ptr().cast()).expect("value pointer is never null"),
        )
    };
    status_of(status, what)?;
    // SAFETY: successfully read per `status`, size announced in/out.
    Ok(unsafe { value.assume_init() })
}

/// Write one `Copy` scalar property.
///
/// SAFETY: `T` must be the exact layout CoreAudio expects (see [`read`]).
unsafe fn write<T>(
    object: AudioObjectID,
    of: &AudioObjectPropertyAddress,
    value: &T,
    what: &str,
) -> Result<()> {
    let size = std::mem::size_of::<T>() as u32;
    // SAFETY: the value outlives the call; the address is read-only.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            NonNull::from(of),
            0,
            std::ptr::null(),
            size,
            NonNull::from(value).cast(),
        )
    };
    status_of(status, what)
}

/// Read a `CFString` property (device name, UID). The reference CoreAudio
/// hands back is owned (+1); [`CFRetained::from_raw`] adopts it so it is
/// released exactly once.
///
/// SAFETY: caller must pass a live object id; the address is read-only.
unsafe fn read_cfstring(object: AudioObjectID, of: &AudioObjectPropertyAddress) -> Option<String> {
    let mut reference: *mut CFString = std::ptr::null_mut();
    let mut size = std::mem::size_of::<*mut CFString>() as u32;
    // SAFETY: the pointer buffer is announced by `size` on entry; the write
    // lands in `reference`, a local, through a pointer to it.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(of),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut reference).cast(),
        )
    };
    if status != 0 || reference.is_null() {
        return None;
    }
    // SAFETY: on success the reference is +1 owned; adopting it releases it.
    let owned = unsafe { CFRetained::from_raw(NonNull::new(reference)?) };
    Some(owned.to_string())
}

/// One device as gathered from the hardware, before the final conversion.
///
/// The enum-less fields keep every decision *below* pure and unit-testable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeviceRow {
    /// The (internal) CoreAudio object id.
    id: AudioObjectID,
    /// The `DeviceUID` — the *stable* string SID callers address this device
    /// by; absent means "cannot be addressed at all" and the device is
    /// filtered out by [`microphones_from_devices`].
    uid: Option<String>,
    /// Human-readable name, absent when the device does not publish one.
    name: Option<String>,
    /// Whether the system routes default capture into this device *now*.
    is_default: bool,
}

impl DeviceRow {
    /// In-app shape. `None` when the row cannot be addressed: no UID means
    /// no way back to the device by id, so listing it would be a lie — the
    /// same rule that drops index-only devices on Windows.
    fn as_device_info(&self) -> Option<DeviceInfo> {
        let uid = self.uid.as_ref()?;
        Some(DeviceInfo {
            id: uid.clone(),
            name: self.name.clone().unwrap_or_else(|| uid.clone()),
            is_default: self.is_default,
        })
    }
}

/// Rows the user may pick and see, with the default flag where the hardware
/// says so.
pub(crate) fn microphones_from_devices(rows: &[DeviceRow]) -> Vec<DeviceInfo> {
    rows.iter().filter_map(DeviceRow::as_device_info).collect()
}

/// Resolve the id this controller was configured for against a gathered
/// device list: [`DEFAULT_DEVICE`] is the row the system marked default;
/// anything else must match a `DeviceUID` exactly, so a typo in
/// `config.toml` produces a readable error instead of a silent fallback —
/// the same contract [`super::pick_device`] enforces on Windows.
pub(crate) fn find_device<'a>(rows: &'a [DeviceRow], requested: &str) -> Result<&'a DeviceRow> {
    if requested == DEFAULT_DEVICE {
        return rows
            .iter()
            .find(|row| row.is_default)
            .ok_or(Error::NoDefaultDevice);
    }
    rows.iter()
        .find(|row| row.uid.as_deref() == Some(requested))
        .ok_or_else(|| Error::DeviceNotFound(requested.to_string()))
}

// --- enumeration ------------------------------------------------------------

/// Enumerate *capture* devices: everything with at least one input stream.
///
/// Input-ness is probed by size of the `kAudioDevicePropertyStreams` list
/// *in the input scope*: this is Apple's documented "does this device even
/// have capture?" test, and unlike a name check it cannot be fooled by
/// output-only devices that happen to have a name.
unsafe fn capture_devices() -> Result<Vec<DeviceRow>> {
    let devices = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let size = unsafe { property_size(SYSTEM_OBJECT, &devices) }?;
    let count = (size / std::mem::size_of::<AudioObjectID>() as u32) as usize;

    let mut ids = vec![AudioObjectID::default(); count];
    let mut buffer_size = size;
    if count > 0 {
        // SAFETY: the buffer's bytes are measured and announced exactly.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                NonNull::from(&devices),
                0,
                std::ptr::null(),
                NonNull::from(&mut buffer_size),
                NonNull::new(ids.as_mut_ptr().cast()).expect("nonempty slice is never null"),
            )
        };
        status_of(status, "listing CoreAudio devices")?;
    }

    // The system default, read once: "no default" is not an error for
    // listing, the flag simply stays unset (WASAPI plays the same game).
    let default = address(
        kAudioHardwarePropertyDefaultInputDevice,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let default_id: Option<AudioObjectID> =
        unsafe { read(SYSTEM_OBJECT, &default, "reading the default input device") }.ok();

    Ok(ids
        .into_iter()
        .filter(|id| is_capture_device(*id))
        .map(|id| DeviceRow {
            id,
            // A device without a UID survives the row list but not the
            // conversion — `DeviceRow::as_device_info` drops it.
            uid: unsafe {
                read_cfstring(
                    id,
                    &address(
                        kAudioDevicePropertyDeviceUID,
                        kAudioObjectPropertyScopeGlobal,
                        kAudioObjectPropertyElementMain,
                    ),
                )
            },
            name: unsafe {
                read_cfstring(
                    id,
                    &address(
                        kAudioObjectPropertyName,
                        kAudioObjectPropertyScopeGlobal,
                        kAudioObjectPropertyElementMain,
                    ),
                )
            },
            is_default: default_id == Some(id),
        })
        .collect())
}

/// Does this object have capture hardware? Devices that cannot be read (a
/// device can vanish mid-enumeration) simply do not count.
unsafe fn is_capture_device(id: AudioObjectID) -> bool {
    let streams = address(
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeInput,
        kAudioObjectPropertyElementMain,
    );
    // SAFETY: the id comes straight from the enumeration above.
    unsafe { has_property(id, &streams) && property_size(id, &streams).is_ok_and(|size| size > 0) }
}

// --- the mute/volume capability decision ------------------------------------

/// How (or whether) a device can be muted (spec §4): the decision every
/// method below branches on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Control {
    /// A real mute switch exists, on input-scope element `element`.
    Mute { element: u32 },
    /// No mute switch — this backend mutes by zeroing the volume at
    /// input-scope element `element` and handing the original back.
    VolumeOnly { element: u32 },
    /// Neither: the device physically cannot be silenced by this app.
    Neither,
}

/// Scan a device for its mute capability. `element` picks the first
/// input-scope element that answers: 0 = the device-wide "main" element,
/// 1.. = per-channel; either may be missing, so both are probed.
///
/// SAFETY: caller must pass a live device id.
unsafe fn capability(device: AudioObjectID) -> Control {
    let mute = |element: u32| {
        address(
            kAudioDevicePropertyMute,
            kAudioObjectPropertyScopeInput,
            element,
        )
    };
    for element in [kAudioObjectPropertyElementMain, 1] {
        // SAFETY: live device id, read-only probing.
        if unsafe { has_property(device, &mute(element)) } {
            return Control::Mute { element };
        }
    }
    let volume = |element: u32| {
        address(
            kAudioDevicePropertyVolumeScalar,
            kAudioObjectPropertyScopeInput,
            element,
        )
    };
    for element in [kAudioObjectPropertyElementMain, 1] {
        // SAFETY: live device id, read-only probing.
        if unsafe { has_property(device, &volume(element)) } {
            return Control::VolumeOnly { element };
        }
    }
    Control::Neither
}

/// Read the device volume on the given *input-scope* element (0.0–1.0).
///
/// SAFETY: caller must pass a live device id and an element the device
/// actually has a volume control on.
unsafe fn volume_scalar(device: AudioObjectID, element: u32) -> Result<f32> {
    let of = address(
        kAudioDevicePropertyVolumeScalar,
        kAudioObjectPropertyScopeInput,
        element,
    );
    unsafe { read::<f32>(device, &of, "reading the device volume") }
}

// --- the controller ----------------------------------------------------------

/// Microphone control over macOS Core Audio (plan §4, §7).
pub struct CoreAudioController {
    device_id: String,
    /// The level this controller zeroed when it muted a volume-only device,
    /// so `set_mute(false)` can hand the user's level back — the in-process
    /// half of the failsafe (`state.json` carries it across a crash).
    /// `Mutex` because [`MicController`] demands `Send`.
    volume_before_mute: Mutex<Option<f32>>,
}

impl CoreAudioController {
    /// `device_id` is a `DeviceUID` or the special value [`DEFAULT_DEVICE`].
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            volume_before_mute: Mutex::new(None),
        }
    }

    /// Resolve the controller's id against the live device list, per call —
    /// the same contract WASAPI runs.
    fn resolve(&self) -> Result<(AudioObjectID, Control)> {
        let rows = unsafe { capture_devices() }?;
        let row = find_device(&rows, &self.device_id)?;
        // SAFETY: the id is a live device from the enumeration above.
        let control = unsafe { capability(row.id) };
        Ok((row.id, control))
    }

    /// The volume-only half of `set_mute`: snapshot before zeroing, hand the
    /// level back before unmuting (`restore` keeps doing the last step when
    /// this controller was built fresh — its `set_volume` runs first by
    /// design).
    fn mute_by_volume(&self, device: AudioObjectID, element: u32, muted: bool) -> Result<()> {
        let mut remembered = self.volume_before_mute.lock().expect("volume mutex");
        if muted {
            // SAFETY: the element came from the capability scan.
            let current = unsafe { volume_scalar(device, element)? };
            if current > 0.0 {
                // The level the user actually had, not 0: only record a
                // *meaningful* level so unmute can never restore "0".
                *remembered = Some(current);
            }
            let of = address(
                kAudioDevicePropertyVolumeScalar,
                kAudioObjectPropertyScopeInput,
                element,
            );
            // SAFETY: a `Float32` scalar, the layout the header fixes.
            unsafe { write(device, &of, &0.0_f32, "zeroing the device volume") }
        } else {
            // Already audible: at some level, nothing to hand back.
            // SAFETY: the element came from the capability scan.
            if unsafe { volume_scalar(device, element)? } > 0.0 {
                return Ok(());
            }
            let target = remembered.unwrap_or_else(|| {
                // No snapshot (this controller never muted; the device came
                // muted from elsewhere — e.g. left zeroed by a dead run). The
                // failsafe's `set_volume` normally got here first with the real
                // level; when it did not, full volume is the only honest
                // "unmute" and the user can lower it again — silence is the one
                // state this code exists to avoid leaving behind.
                tracing::warn!(
                    "unmuting '{}' by volume with no recorded level: using full volume",
                    self.device_id
                );
                1.0
            });
            let of = address(
                kAudioDevicePropertyVolumeScalar,
                kAudioObjectPropertyScopeInput,
                element,
            );
            // SAFETY: a `Float32` scalar, the layout the header fixes.
            unsafe { write(device, &of, &target, "restoring the device volume") }
        }
    }
}

impl MicController for CoreAudioController {
    fn list_capture_devices(&self) -> Result<Vec<DeviceInfo>> {
        // SAFETY: enumeration touches only CoreAudio object ids.
        Ok(microphones_from_devices(&unsafe { capture_devices() }?))
    }

    fn get_mute(&self) -> Result<bool> {
        let (device, control) = self.resolve()?;
        match control {
            Control::Mute { element } => {
                let of = address(
                    kAudioDevicePropertyMute,
                    kAudioObjectPropertyScopeInput,
                    element,
                );
                // SAFETY: the mute property is a `UInt32` 0/1 flag.
                let muted = unsafe { read::<u32>(device, &of, "reading the mute switch")? };
                Ok(muted != 0)
            }
            Control::VolumeOnly { element } => {
                // SAFETY: the element came from the capability scan.
                Ok(unsafe { volume_scalar(device, element)? } <= 0.0)
            }
            Control::Neither => Ok(false),
        }
    }

    fn set_mute(&self, muted: bool) -> Result<()> {
        let (device, control) = self.resolve()?;
        match control {
            Control::Mute { element } => {
                let of = address(
                    kAudioDevicePropertyMute,
                    kAudioObjectPropertyScopeInput,
                    element,
                );
                // SAFETY: the mute property is a `UInt32` 0/1 flag.
                unsafe { write(device, &of, &u32::from(muted), "flipping the mute switch") }
            }
            Control::VolumeOnly { element } => self.mute_by_volume(device, element, muted),
            // Nothing can be done; say so instead of lying about it.
            Control::Neither => Err(Error::MuteUnsupported(format!(
                "'{}' has neither a mute switch nor a volume control",
                self.device_id
            ))),
        }
    }

    fn get_volume(&self) -> Result<Option<f32>> {
        let (device, control) = self.resolve()?;
        // Only a volume-muting backend reports a volume: the failsafe uses
        // it to carry the true level, and a mute-native device must answer
        // `None` so records stay in the mute-only shape.
        match control {
            Control::VolumeOnly { element } => {
                // SAFETY: the element came from the capability scan.
                unsafe { volume_scalar(device, element) }.map(Some)
            }
            Control::Mute { .. } | Control::Neither => Ok(None),
        }
    }

    fn set_volume(&self, volume: f32) -> Result<()> {
        let (device, control) = self.resolve()?;
        match control {
            Control::VolumeOnly { element } => {
                let of = address(
                    kAudioDevicePropertyVolumeScalar,
                    kAudioObjectPropertyScopeInput,
                    element,
                );
                // SAFETY: a `Float32` scalar, the layout the header fixes.
                unsafe { write(device, &of, &volume, "setting the device volume") }
            }
            // Which is exactly what the default trait implementation says.
            Control::Mute { .. } | Control::Neither => Err(Error::VolumeUnsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{find_device, microphones_from_devices, DeviceRow};
    use crate::audio::DEFAULT_DEVICE;
    use crate::error::Error;

    fn row(id: u32, uid: Option<&str>, name: Option<&str>, default: bool) -> DeviceRow {
        DeviceRow {
            id,
            uid: uid.map(str::to_owned),
            name: name.map(str::to_owned),
            is_default: default,
        }
    }

    fn plausible_machine() -> Vec<DeviceRow> {
        vec![
            row(
                11,
                Some("BuiltInMicrophoneDevice"),
                Some("MacBook Pro Microphone"),
                true,
            ),
            row(
                12,
                Some("U0x1391x1111"),
                Some("USB Headset Microphone"),
                false,
            ),
            row(13, None, Some("Unnamed capture device"), false),
        ]
    }

    #[test]
    fn rows_without_a_uid_are_not_listed() {
        let devices = microphones_from_devices(&plausible_machine());
        assert_eq!(devices.len(), 2);
        assert!(devices.iter().all(|d| !d.id.is_empty()));
    }

    #[test]
    fn the_default_flag_travels_through() {
        let devices = microphones_from_devices(&plausible_machine());
        let defaults: Vec<_> = devices.iter().filter(|d| d.is_default).collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].id, "BuiltInMicrophoneDevice");
    }

    #[test]
    fn a_missing_name_falls_back_to_the_uid() {
        let devices = microphones_from_devices(&[row(14, Some("uid-only"), None, false)]);
        assert_eq!(devices[0].name, "uid-only");
        assert_eq!(devices[0].id, "uid-only");
    }

    #[test]
    fn a_named_device_shows_its_name() {
        let devices = microphones_from_devices(&plausible_machine());
        let named = devices
            .iter()
            .find(|d| d.id == "U0x1391x1111")
            .expect("present");
        assert_eq!(named.name, "USB Headset Microphone");
        assert!(!named.is_default);
    }

    #[test]
    fn default_device_resolves_to_the_row_the_system_marked() {
        let rows = plausible_machine();
        let picked = find_device(&rows, DEFAULT_DEVICE).expect("a default exists");
        assert_eq!(picked.id, 11);
    }

    #[test]
    fn default_device_reports_a_missing_default() {
        let error = find_device(&vec![row(1, Some("uid"), None, false)], DEFAULT_DEVICE)
            .expect_err("no default");
        assert!(matches!(error, Error::NoDefaultDevice));
    }

    #[test]
    fn default_device_reports_a_missing_default_on_an_empty_list() {
        let error = find_device(&[], DEFAULT_DEVICE).expect_err("no devices at all");
        assert!(matches!(error, Error::NoDefaultDevice));
    }

    #[test]
    fn a_concrete_uid_matches_exactly() {
        let rows = plausible_machine();
        let picked = find_device(&rows, "U0x1391x1111").expect("present");
        assert_eq!(picked.id, 12);
    }

    #[test]
    fn an_unknown_uid_is_rejected_and_named() {
        let error = find_device(&plausible_machine(), "typo").expect_err("no such device");
        assert!(matches!(error, Error::DeviceNotFound(_)));
        assert!(error.to_string().contains("typo"));
    }

    #[test]
    fn only_an_exact_uid_match_wins_not_a_substring() {
        let error =
            find_device(&plausible_machine(), "BuiltIn").expect_err("substring must not match");
        assert!(matches!(error, Error::DeviceNotFound(_)));
    }

    #[test]
    fn rows_without_a_uid_never_match_a_requested_id() {
        let error = find_device(&plausible_machine(), "")
            .expect_err("uidless devices are unaddressable, not free ids");
        assert!(matches!(error, Error::DeviceNotFound(_)));
    }
}
