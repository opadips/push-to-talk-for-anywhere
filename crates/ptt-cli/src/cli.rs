//! Argument parsing and command execution for the `ptt` binary.
//!
//! Kept as pure functions over `&dyn MicController` so every command can be
//! tested without a microphone or a keyboard (plan §0).

use anyhow::{bail, Result};
use ptt_core::audio::{pick_device, DeviceInfo, MicController, DEFAULT_DEVICE};
use ptt_core::input::{Binding, MouseButton};

pub const USAGE: &str = "\
usage: ptt <command> [options]

commands:
  devices           list capture devices (marks the system default)
  mute              mute the microphone
  unmute            unmute the microphone
  status            print 'muted' or 'unmuted'
  ptt               hold-to-talk: unmute only while the bound input is held
  help              show this help

options:
  --device <id>     use this endpoint id instead of the system default
                    (run 'ptt devices' to see ids; 'default' follows the system)

'ptt' command options:
  --key <vk>            bind a keyboard key by virtual-key code, e.g. 0x14 (Caps Lock)
  --mouse <button>      bind a mouse button: left, right, middle, x1 or x2
  --release-delay <ms>  mute delay after release, 0-2000 (default 200)
  --no-swallow          also let the bound input reach other windows";

/// A parsed invocation of `ptt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Devices,
    Mute {
        device: Option<String>,
    },
    Unmute {
        device: Option<String>,
    },
    Status {
        device: Option<String>,
    },
    /// Interactive hold-to-talk session (plan §9, M2).
    Ptt(PttSpec),
    Help,
}

/// Arguments of the `ptt ptt` hold-to-talk command.
///
/// Everything here is an *override*: `None` means "take it from
/// `config.toml`", so the file stays the single source of truth (plan §8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PttSpec {
    pub device: Option<String>,
    pub binding: Option<Binding>,
    pub release_delay_ms: Option<u64>,
    /// `Some(false)` when `--no-swallow` was given: let the bound input
    /// reach other windows too (plan §1).
    pub swallow: Option<bool>,
}

/// Plan §8 range for `release_delay_ms`.
const MAX_RELEASE_DELAY_MS: u64 = 2000;

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
        "ptt" => parse_ptt(&args[1..]),
        "help" | "-h" | "--help" => Ok(Command::Help),
        other => bail!("unknown command: {other}\n\n{USAGE}"),
    }
}

/// Parse the hold-to-talk command: one binding plus its options.
fn parse_ptt(rest: &[String]) -> Result<Command> {
    let mut device = None;
    let mut key: Option<u16> = None;
    let mut mouse: Option<String> = None;
    let mut release_delay_ms: Option<u64> = None;
    let mut swallow: Option<bool> = None;

    let mut index = 0;
    while index < rest.len() {
        let flag = rest[index].as_str();
        if flag == "--no-swallow" {
            swallow = Some(false);
            index += 1;
            continue;
        }
        let Some(value) = rest.get(index + 1) else {
            bail!("{flag} requires a value\n\n{USAGE}");
        };
        if value.starts_with("--") {
            bail!("{flag} requires a value\n\n{USAGE}");
        }
        match flag {
            "--device" => device = Some(value.clone()),
            "--key" => key = Some(parse_vk(value)?),
            "--mouse" => mouse = Some(value.clone()),
            "--release-delay" => release_delay_ms = Some(parse_release_delay(value)?),
            other => bail!("unexpected argument for 'ptt': {other}\n\n{USAGE}"),
        }
        index += 2;
    }

    let binding = match (key, mouse) {
        (Some(_), Some(_)) => bail!("'ptt' takes either --key or --mouse, not both"),
        (Some(vk), None) => Some(Binding::Key { vk, scan: 0 }),
        (None, Some(name)) => Some(Binding::Mouse(MouseButton::from_name(&name)?)),
        // No flag: `config.toml` decides (plan §8).
        (None, None) => None,
    };

    Ok(Command::Ptt(PttSpec {
        device,
        binding,
        release_delay_ms,
        swallow,
    }))
}

/// Accept decimal (`20`) and hex (`0x14`) virtual-key codes; 0 is not a key.
fn parse_vk(value: &str) -> Result<u16> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"));
    let parsed = match hex {
        Some(hex) => u16::from_str_radix(hex, 16),
        None => value.parse::<u16>(),
    };
    let vk = parsed.map_err(|_| {
        anyhow::anyhow!("invalid key code {value:?}: use a number such as 20 or 0x14 (Caps Lock)")
    })?;
    if vk == 0 {
        bail!("key code 0 is not a key: use a number such as 20 or 0x14 (Caps Lock)");
    }
    Ok(vk)
}

/// Plan §8: 0–2000 ms.
fn parse_release_delay(value: &str) -> Result<u64> {
    let ms = value.parse::<u64>().map_err(|_| {
        anyhow::anyhow!("invalid release delay {value:?}: use milliseconds, e.g. 200")
    })?;
    if ms > MAX_RELEASE_DELAY_MS {
        bail!("release delay must be 0-{MAX_RELEASE_DELAY_MS} ms, got {ms}");
    }
    Ok(ms)
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
        Command::Ptt(_) => {
            bail!("the hold-to-talk command is interactive: run 'ptt ptt' on its own")
        }
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

    // --- the hold-to-talk command (plan §9 M2) ---------------------------

    fn spec(cmd: &Command) -> &PttSpec {
        match cmd {
            Command::Ptt(spec) => spec,
            other => panic!("expected the hold-to-talk command, got {other:?}"),
        }
    }

    #[test]
    fn ptt_without_flags_leaves_everything_to_the_config() {
        let cmd = parse(&args(&["ptt"])).expect("no flags means config defaults");
        let spec = spec(&cmd);
        assert_eq!(spec.binding, None, "binding comes from config.toml");
        assert_eq!(spec.device, None);
        assert_eq!(spec.release_delay_ms, None);
        assert_eq!(spec.swallow, None);
    }

    #[test]
    fn ptt_parses_a_hex_key() {
        let cmd = parse(&args(&["ptt", "--key", "0x14"])).expect("hex vk");
        assert_eq!(
            cmd,
            Command::Ptt(PttSpec {
                device: None,
                binding: Some(Binding::Key { vk: 0x14, scan: 0 }),
                release_delay_ms: None,
                swallow: None,
            })
        );
    }

    #[test]
    fn ptt_parses_a_decimal_key() {
        let cmd = parse(&args(&["ptt", "--key", "41"])).expect("decimal vk");
        assert_eq!(spec(&cmd).binding, Some(Binding::Key { vk: 41, scan: 0 }));
    }

    #[test]
    fn ptt_rejects_a_key_code_of_zero() {
        let err = parse(&args(&["ptt", "--key", "0"])).expect_err("0 is no key");
        assert!(err.to_string().contains("0x14"), "hints a real code: {err}");
    }

    #[test]
    fn ptt_rejects_a_malformed_key_code() {
        let err = parse(&args(&["ptt", "--key", "capslock"])).expect_err("not a number");
        assert!(err.to_string().contains("capslock"), "{err}");
    }

    #[test]
    fn ptt_parses_a_mouse_button() {
        let cmd = parse(&args(&["ptt", "--mouse", "x1"])).expect("mouse binding");
        assert_eq!(spec(&cmd).binding, Some(Binding::Mouse(MouseButton::X1)));
    }

    #[test]
    fn ptt_rejects_an_unknown_mouse_button() {
        let err = parse(&args(&["ptt", "--mouse", "thumb"])).expect_err("no such button");
        assert!(err.to_string().contains("x1"), "lists the buttons: {err}");
    }

    #[test]
    fn ptt_rejects_key_and_mouse_together() {
        let err = parse(&args(&["ptt", "--key", "0x14", "--mouse", "x1"]))
            .expect_err("ambiguous binding");
        assert!(err.to_string().contains("not both"), "{err}");
    }

    #[test]
    fn ptt_release_delay_is_configurable_within_the_plan_range() {
        let cmd = parse(&args(&["ptt", "--key", "0x14", "--release-delay", "0"]))
            .expect("zero delay is allowed");
        assert_eq!(spec(&cmd).release_delay_ms, Some(0));

        let err = parse(&args(&["ptt", "--key", "0x14", "--release-delay", "2001"]))
            .expect_err("over the maximum");
        assert!(err.to_string().contains("2000"), "{err}");
    }

    #[test]
    fn ptt_swallows_the_bound_input_unless_told_not_to() {
        let on = parse(&args(&["ptt", "--key", "0x14"])).expect("parses");
        assert_eq!(spec(&on).swallow, None, "config decides unless told");
        let off = parse(&args(&["ptt", "--key", "0x14", "--no-swallow"])).expect("parses");
        assert_eq!(spec(&off).swallow, Some(false));
    }

    #[test]
    fn ptt_accepts_a_device_and_rejects_a_valueless_flag() {
        let cmd = parse(&args(&["ptt", "--key", "0x14", "--device", "{id}"])).expect("parses");
        assert_eq!(spec(&cmd).device.as_deref(), Some("{id}"));

        let err = parse(&args(&["ptt", "--key"])).expect_err("missing value");
        assert!(err.to_string().contains("--key"), "{err}");
    }

    #[test]
    fn ptt_rejects_an_unknown_flag() {
        let err = parse(&args(&["ptt", "--loud", "yes"])).expect_err("unknown flag");
        assert!(err.to_string().contains("--loud"), "{err}");
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
    fn execute_refuses_the_interactive_command() {
        let cmd = parse(&args(&["ptt", "--key", "0x14"])).expect("parses");
        let ctl = FakeController::new(devices(), true);
        let err = execute(&cmd, &ctl).expect_err("interactive command");
        assert!(err.to_string().contains("hold-to-talk"), "{err}");
        assert!(ctl.calls().is_empty(), "it must not touch the microphone");
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
