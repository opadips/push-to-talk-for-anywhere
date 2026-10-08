//! Microphone control abstraction (plan §4) plus its Windows Core Audio
//! implementation (plan §7).
//!
//! Everything outside this module depends only on [`MicController`], never on
//! a Windows type, so the engine can be tested with a fake (plan §0).

use crate::error::{Error, Result};

#[cfg(windows)]
pub mod wasapi;

/// A capture endpoint as the rest of the app sees it.
///
/// `id` is the endpoint ID string (`IMMDevice::GetId`), never a list index —
/// indices change whenever devices are plugged in (plan §7).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// The microphone operations the app needs (plan §4).
pub trait MicController: Send {
    fn list_capture_devices(&self) -> Result<Vec<DeviceInfo>>;
    fn get_mute(&self) -> Result<bool>;
    fn set_mute(&self, muted: bool) -> Result<()>;
}

/// Device id that means "follow the system default endpoint" (plan §7).
pub const DEFAULT_DEVICE: &str = "default";

/// Resolve a requested device id against the live device list.
///
/// [`DEFAULT_DEVICE`] selects the endpoint the system reports as default;
/// anything else must match an endpoint id exactly, so a typo produces a
/// readable error instead of a silent fallback.
pub fn pick_device<'a>(devices: &'a [DeviceInfo], requested: &str) -> Result<&'a DeviceInfo> {
    if requested == DEFAULT_DEVICE {
        return devices
            .iter()
            .find(|d| d.is_default)
            .ok_or(Error::NoDefaultDevice);
    }
    devices
        .iter()
        .find(|d| d.id == requested)
        .ok_or_else(|| Error::DeviceNotFound(requested.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{pick_device, DeviceInfo, DEFAULT_DEVICE};
    use crate::error::Error;

    fn device(id: &str, name: &str, is_default: bool) -> DeviceInfo {
        DeviceInfo {
            id: id.to_string(),
            name: name.to_string(),
            is_default,
        }
    }

    fn two_devices() -> Vec<DeviceInfo> {
        vec![
            device("{0.0.0.00000000}.aaa", "USB Headset", false),
            device("{0.0.0.00000000}.bbb", "Laptop Microphone", true),
        ]
    }

    #[test]
    fn pick_device_returns_the_default_when_requested() {
        let devices = two_devices();
        let picked = pick_device(&devices, DEFAULT_DEVICE).expect("a default exists");
        assert_eq!(picked.id, "{0.0.0.00000000}.bbb");
        assert!(picked.is_default);
    }

    #[test]
    fn pick_device_returns_a_concrete_device_by_id() {
        let devices = two_devices();
        let picked = pick_device(&devices, "{0.0.0.00000000}.aaa").expect("known id");
        assert_eq!(picked.name, "USB Headset");
        assert!(!picked.is_default);
    }

    #[test]
    fn pick_device_rejects_an_unknown_id_and_names_it() {
        let devices = two_devices();
        let err = pick_device(&devices, "not-a-device").expect_err("unknown id");
        assert!(matches!(err, Error::DeviceNotFound(_)));
        assert!(err.to_string().contains("not-a-device"));
    }

    #[test]
    fn pick_device_reports_a_missing_default() {
        let devices = vec![device("{0.0.0.00000000}.aaa", "USB Headset", false)];
        let err = pick_device(&devices, DEFAULT_DEVICE).expect_err("no default");
        assert!(matches!(err, Error::NoDefaultDevice));
    }

    #[test]
    fn pick_device_reports_a_missing_default_on_an_empty_list() {
        let err = pick_device(&[], DEFAULT_DEVICE).expect_err("empty list");
        assert!(matches!(err, Error::NoDefaultDevice));
    }
}
