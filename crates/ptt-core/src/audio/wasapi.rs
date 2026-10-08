//! Windows Core Audio implementation of [`MicController`] (plan §7).
//!
//! Every entry point initializes COM on the calling thread and resolves the
//! endpoint afresh, so unplugging a device or changing the system default is
//! picked up on the next call instead of caching a dead interface.
//!
//! The endpoint is stored as its ID string — never a list index — and the
//! special value [`DEFAULT_DEVICE`] means "whatever the system reports as
//! default right now" (plan §7).

use super::{DeviceInfo, MicController, DEFAULT_DEVICE};
use crate::error::{Error, Result};
use windows::core::{Error as WinError, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{
    PropVariantClear, PropVariantToString, PROPVARIANT,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

/// How many characters a device's friendly name may use (stack buffer, no
/// allocation, no free).
const NAME_BUFFER: usize = 256;

/// RAII guard: balances [`CoInitializeEx`] with [`CoUninitialize`] on drop.
///
/// `owned == false` means COM was already initialized in a different
/// apartment mode (STA): nothing to balance, and using COM is still legal.
struct ComGuard {
    owned: bool,
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

/// Initialize COM on the current thread (plan §7: every thread that touches
/// COM must do this).
fn com_init() -> Result<ComGuard> {
    // SAFETY: CoInitializeEx is called exactly once per guard; the guard
    // uninitializes only when the call succeeded.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_ok() {
        return Ok(ComGuard { owned: true });
    }
    if hr == RPC_E_CHANGED_MODE {
        // Already initialized as STA by the host; COM is usable, and this
        // failed call must NOT be balanced with CoUninitialize.
        return Ok(ComGuard { owned: false });
    }
    Err(Error::Windows(WinError::from(hr)))
}

/// Create the device enumerator for this thread.
fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: `MMDeviceEnumerator` is the documented CLSID for
    // IMMDeviceEnumerator; `None` = no aggregation.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(Error::Windows)
}

/// Resolve the endpoint this controller was configured with.
fn endpoint(device_id: &str) -> Result<IMMDevice> {
    let _com = com_init()?;
    let enum_ = enumerator()?;
    if device_id == DEFAULT_DEVICE {
        // SAFETY: no arguments besides the two enum values.
        return unsafe { enum_.GetDefaultAudioEndpoint(eCapture, eConsole) }
            .map_err(Error::Windows);
    }
    let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    let id = PCWSTR(wide.as_ptr());
    // SAFETY: `wide` outlives the call and is NUL-terminated.
    unsafe { enum_.GetDevice(id) }.map_err(Error::Windows)
}

/// Endpoint volume interface for a device — this is what carries mute.
fn endpoint_volume(device_id: &str) -> Result<IAudioEndpointVolume> {
    let device = endpoint(device_id)?;
    // SAFETY: no activation parameters are required for the volume
    // interface. Failure here means the endpoint has no mute capability.
    unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| Error::MuteUnsupported(format!("{device_id}: {e}")))
}

/// The endpoint ID string (`IMMDevice::GetId`).
fn endpoint_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: caller owns the returned PWSTR; read it before it goes out of
    // scope.
    let raw = unsafe { device.GetId() }?;
    unsafe { raw.to_string() }
        .map_err(|_| Error::DeviceUnavailable("endpoint id was not valid UTF-16".to_string()))
}

/// Human-readable name from the property store, if the device provides one.
fn friendly_name(device: &IMMDevice) -> Option<String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let mut value: PROPVARIANT = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;
    let mut buffer = [0u16; NAME_BUFFER];
    // SAFETY: buffer is valid memory; the variant is cleared on every path.
    let read = unsafe { PropVariantToString(&value, &mut buffer) }.is_ok();
    let cleared = unsafe { PropVariantClear(&mut value) }.is_ok();
    if !read || !cleared {
        return None;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let name = String::from_utf16_lossy(&buffer[..end]);
    (!name.is_empty()).then_some(name)
}

/// Microphone control over Windows Core Audio (plan §4 and §7).
pub struct WasapiController {
    device_id: String,
}

impl WasapiController {
    /// `device_id` is an endpoint ID or the special value [`DEFAULT_DEVICE`].
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
        }
    }
}

impl MicController for WasapiController {
    fn list_capture_devices(&self) -> Result<Vec<DeviceInfo>> {
        let _com = com_init()?;
        let enum_ = enumerator()?;

        // No default device is not an error for listing: the default flag
        // simply stays unset and `pick_device` reports it later.
        let default_id = unsafe { enum_.GetDefaultAudioEndpoint(eCapture, eConsole) }
            .ok()
            .map(|d| endpoint_id(&d))
            .transpose()?;

        // SAFETY: only active capture endpoints are requested.
        let collection = unsafe { enum_.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }?;
        let count = unsafe { collection.GetCount() }?;

        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = unsafe { collection.Item(index) }?;
            let id = endpoint_id(&device)?;
            let name = friendly_name(&device).unwrap_or_else(|| id.clone());
            devices.push(DeviceInfo {
                is_default: default_id.as_deref() == Some(id.as_str()),
                id,
                name,
            });
        }
        Ok(devices)
    }

    fn get_mute(&self) -> Result<bool> {
        let _com = com_init()?;
        let volume = endpoint_volume(&self.device_id)?;
        // SAFETY: `GetMute` takes no arguments.
        let muted = unsafe { volume.GetMute() }?;
        Ok(muted.as_bool())
    }

    fn set_mute(&self, muted: bool) -> Result<()> {
        let _com = com_init()?;
        let volume = endpoint_volume(&self.device_id)?;
        // SAFETY: null event-context GUID = "no event context"; documented
        // and valid for SetMute.
        unsafe { volume.SetMute(muted, std::ptr::null()) }?;
        Ok(())
    }
}
