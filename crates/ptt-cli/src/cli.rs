//! Argument parsing and command execution for the `ptt` binary.
//!
//! Kept as pure functions over `&dyn MicController` so every command can be
//! tested without a microphone or a keyboard (plan §0).

use anyhow::{bail, Result};
use ptt_core::audio::{pick_device, DeviceInfo, MicController, DEFAULT_DEVICE};

pub const USAGE: &str = "\
usage: ptt <command> [--device <id>]

commands:
  devices           list capture devices (marks the system default)
  mute              mute the microphone
  unmute            unmute the microphone
  status            print 'muted' or 'unmuted'
  help              show this help

options:
  --device <id>     use this endpoint id instead of the system default
                    (run 'ptt devices' to see ids; 'default' follows the system)";

/// A parsed invocation of `ptt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Devices,
    Mute { device: Option<String> },
    Unmute { device: Option<String> },
    Status { device: Option<String> },
    Help,
}

/// Parse command-line arguments (everything after argv[0]).
pub fn parse(args: &[String]) -> Result<Command> {
    let Some(name) = args.first() else {
        return Ok(Command::Help);
    };
    match name.as_str() {
        "devices" => Ok(Command::Devices),
        "mute" | "unmute" | "status" => {
            let device = parse_device_flag(&args[1..])?;
            Ok(match name.as_str() {
                "mute" => Command::Mute { device },
                "unmute" => Command::Unmute { device },
                _ => Command::Status { device },
            })
        }
        "help" | "-h" | "--help" => Ok(Command::Help),
        other => bail!("unknown command: {other}\n\n{USAGE}"),
    }
}

fn parse_device_flag(rest: &[String]) -> Result<Option<String>> {
    match rest {
        [] => Ok(None),
        [flag] if flag == "--device" => bail!("--device requires a value (an endpoint id)"),
        [flag, value] if flag == "--device" => Ok(Some(value.clone())),
        [flag, ..] if flag == "--device" => bail!("--device takes exactly one value"),
        other => bail!("unexpected arguments: {}", other.join(" ")),
    }
}

/// Validate a device selection and return the device it resolves to.
pub fn resolve_selection(
    ctl: &dyn MicController,
    requested: Option<&str>,
) -> ptt_core::error::Result<DeviceInfo> {
    let devices = ctl.list_capture_devices()?;
    let chosen = pick_device(&devices, requested.unwrap_or(DEFAULT_DEVICE))?;
    Ok(chosen.clone())
}

/// Run a parsed command against a controller and return its output.
pub fn execute(cmd: &Command, ctl: &dyn MicController) -> Result<String> {
    Ok(match cmd {
        Command::Help => USAGE.to_string(),
        Command::Devices => format_devices(&ctl.list_capture_devices()?),
        Command::Mute { device } => {
            resolve_selection(ctl, device.as_deref())?;
            ctl.set_mute(true)?;
            "microphone muted".to_string()
        }
        Command::Unmute { device } => {
            resolve_selection(ctl, device.as_deref())?;
            ctl.set_mute(false)?;
            "microphone unmuted".to_string()
        }
        Command::Status { device } => {
            resolve_selection(ctl, device.as_deref())?;
            if ctl.get_mute()? { "muted" } else { "unmuted" }.to_string()
        }
    })
}

fn format_devices(devices: &[DeviceInfo]) -> String {
    if devices.is_empty() {
        return "no capture devices found".to_string();
    }
    devices
        .iter()
        .map(|d| {
            if d.is_default {
                format!("{} - {} (default)", d.name, d.id)
            } else {
                format!("{} - {}", d.name, d.id)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptt_core::audio::DeviceInfo;
    use std::sync::Mutex;

    /// Records mute calls and answers queries — the plan's "fake
    /// MicController" (§10).
    struct FakeController {
        devices: Vec<DeviceInfo>,
        muted: Mutex<bool>,
        calls: Mutex<Vec<bool>>,
    }

    impl FakeController {
        fn new(devices: Vec<DeviceInfo>, muted: bool) -> Self {
            Self {
                devices,
                muted: Mutex::new(muted),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<bool> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ptt_core::audio::MicController for FakeController {
        fn list_capture_devices(&self) -> ptt_core::error::Result<Vec<DeviceInfo>> {
            Ok(self.devices.clone())
        }

        fn get_mute(&self) -> ptt_core::error::Result<bool> {
            Ok(*self.muted.lock().unwrap())
        }

        fn set_mute(&self, muted: bool) -> ptt_core::error::Result<()> {
            *self.muted.lock().unwrap() = muted;
            self.calls.lock().unwrap().push(muted);
            Ok(())
        }
    }

    fn devices() -> Vec<DeviceInfo> {
        vec![
            DeviceInfo {
                id: "{0.0.0.00000000}.aaa".into(),
                name: "USB Headset".into(),
                is_default: false,
            },
            DeviceInfo {
                id: "{0.0.0.00000000}.bbb".into(),
                name: "Laptop Microphone".into(),
                is_default: true,
            },
        ]
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // --- parsing ---------------------------------------------------------

    #[test]
    fn parse_recognises_the_devices_command() {
        assert!(matches!(parse(&args(&["devices"])), Ok(Command::Devices)));
    }

    #[test]
    fn parse_reads_mute_and_unmute_with_an_optional_device() {
        assert!(matches!(
            parse(&args(&["mute"])),
            Ok(Command::Mute { device: None })
        ));
        assert!(matches!(
            parse(&args(&["unmute", "--device", "{id}"])),
            Ok(Command::Unmute { device: Some(id) }) if id == "{id}"
        ));
    }

    #[test]
    fn parse_reads_status_with_a_device() {
        assert!(matches!(
            parse(&args(&["status", "--device", "abc"])),
            Ok(Command::Status { device: Some(id) }) if id == "abc"
        ));
    }

    #[test]
    fn parse_rejects_a_device_flag_without_a_value() {
        let err = parse(&args(&["status", "--device"])).expect_err("missing value");
        assert!(err.to_string().contains("--device"), "usage hints the flag");
    }

    #[test]
    fn parse_rejects_an_unknown_command() {
        let err = parse(&args(&["frobnicate"])).expect_err("unknown command");
        assert!(err.to_string().to_lowercase().contains("usage"));
    }

    #[test]
    fn parse_treats_no_args_and_help_flags_as_help() {
        assert!(matches!(parse(&[]), Ok(Command::Help)));
        assert!(matches!(parse(&args(&["--help"])), Ok(Command::Help)));
        assert!(matches!(parse(&args(&["-h"])), Ok(Command::Help)));
    }

    // --- device selection ------------------------------------------------

    #[test]
    fn selection_falls_back_to_the_default_device() {
        let ctl = FakeController::new(devices(), true);
        let chosen = resolve_selection(&ctl, None).expect("default exists");
        assert_eq!(chosen.name, "Laptop Microphone");
    }

    #[test]
    fn selection_accepts_an_explicit_id() {
        let ctl = FakeController::new(devices(), true);
        let chosen = resolve_selection(&ctl, Some("{0.0.0.00000000}.aaa")).expect("known id");
        assert_eq!(chosen.name, "USB Headset");
    }

    #[test]
    fn selection_rejects_an_unknown_id() {
        let ctl = FakeController::new(devices(), true);
        let err = resolve_selection(&ctl, Some("typo")).expect_err("unknown id");
        assert!(err.to_string().contains("typo"));
    }

    // --- execution -------------------------------------------------------

    #[test]
    fn mute_command_mutes_the_microphone_once() {
        let ctl = FakeController::new(devices(), false);
        let out = execute(&Command::Mute { device: None }, &ctl).expect("mute works");
        assert_eq!(ctl.calls(), vec![true]);
        assert_eq!(out, "microphone muted");
    }

    #[test]
    fn unmute_command_unmutes_the_microphone_once() {
        let ctl = FakeController::new(devices(), true);
        let out = execute(&Command::Unmute { device: None }, &ctl).expect("unmute works");
        assert_eq!(ctl.calls(), vec![false]);
        assert_eq!(out, "microphone unmuted");
    }

    #[test]
    fn status_reports_whether_the_microphone_is_muted() {
        let muted = FakeController::new(devices(), true);
        let unmuted = FakeController::new(devices(), false);
        assert_eq!(
            execute(&Command::Status { device: None }, &muted).unwrap(),
            "muted"
        );
        assert_eq!(
            execute(&Command::Status { device: None }, &unmuted).unwrap(),
            "unmuted"
        );
        // status must never touch the mute state
        assert!(muted.calls().is_empty());
        assert!(unmuted.calls().is_empty());
    }

    #[test]
    fn devices_lists_names_ids_and_the_default_flag() {
        let ctl = FakeController::new(devices(), true);
        let out = execute(&Command::Devices, &ctl).expect("list works");
        assert!(out.contains("USB Headset"));
        assert!(out.contains("{0.0.0.00000000}.bbb"));
        assert_eq!(out.matches("(default)").count(), 1);
        let default_line = out.lines().find(|l| l.contains("(default)")).unwrap();
        assert!(default_line.contains("Laptop Microphone"));
    }
}
