//! Where the tool's files live on each platform (plan §2).
//!
//! One module for the four locations every layer needs — the config file, the
//! failsafe state file, the diagnostics log directory and the findings log —
//! so Windows, Linux and macOS disagree *here* and nowhere else:
//!
//! | path | Windows | Linux | macOS |
//! |---|---|---|---|
//! | config | `%APPDATA%\ptt-tool\config.toml` | `$XDG_CONFIG_HOME` or `~/.config/ptt-tool/config.toml` | `~/Library/Application Support/ptt-tool/config.toml` |
//! | state | `%LOCALAPPDATA%\ptt-tool\state.json` | `$XDG_STATE_HOME` or `~/.local/state/ptt-tool/state.json` | `~/Library/Application Support/ptt-tool/state.json` |
//! | logs | `%LOCALAPPDATA%\ptt-tool\logs` | `…/ptt-tool/logs` | `~/Library/Logs/ptt-tool` |
//!
//! The resolution itself lives in small *pure* helpers below that take the
//! environment they read as arguments (which platform, home directory, the
//! relevant data-directory variables). That is what makes them unit-testable:
//! `std::env` is process-global and mutating it in a test would race, so the
//! tests drive every platform's branch with invented values instead. The
//! public functions are thin wrappers that supply the real environment.
//!
//! Windows resolution is exactly what `config::default_path`,
//! `failsafe::default_path` and `diagnostics::findings_path` used to do
//! inline: an unset variable falls back to the temp directory, so the
//! existing call sites in [`crate::config`], [`crate::failsafe`] and
//! [`crate::diagnostics`] keep their behaviour byte-identical while they
//! delegate here. (One deliberate tightening: an *empty* variable used to
//! build a CWD-relative path like `ptt-tool/config.toml`; it now also falls
//! back to the temp directory, which is the only sensible reading of an
//! empty `%APPDATA%`.)
//!
//! On Linux and macOS the directory itself is created on demand
//! (`mkdir -p` semantics, failures ignored) so a first run on a bare system
//! does not depend on the directory already existing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `<config dir>/config.toml` (plan §2).
pub fn config_file() -> PathBuf {
    let dir = config_dir_for(
        platform(),
        &home_dir(),
        &env_var("XDG_CONFIG_HOME"),
        &env_var("APPDATA"),
    );
    ensure_dir(&dir);
    dir.join("config.toml")
}

/// `<state dir>/state.json` (plan §6.1).
pub fn state_file() -> PathBuf {
    let dir = state_dir_for(
        platform(),
        &home_dir(),
        &env_var("XDG_STATE_HOME"),
        &env_var("LOCALAPPDATA"),
    );
    ensure_dir(&dir);
    dir.join("state.json")
}

/// The directory log files go into (`ptt.log` and friends, plan §2).
pub fn log_dir() -> PathBuf {
    let dir = log_dir_for(
        platform(),
        &home_dir(),
        &env_var("XDG_STATE_HOME"),
        &env_var("LOCALAPPDATA"),
    );
    ensure_dir(&dir);
    dir
}

/// `<log dir>/findings.log` — the watchdog's synchronous evidence trail
/// (spec §4), reached the same way [`crate::diagnostics`] reaches it.
pub fn findings_file() -> PathBuf {
    log_dir().join("findings.log")
}

// --- pure resolution (unit-tested below) -----------------------------------------

/// Whose rules a pure helper applies: Windows, Linux or macOS. Exactly one
/// value per compiled target (`platform()`); the tests exercise the *other*
/// branches by calling the helpers directly with invented values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Windows,
    Linux,
    MacOs,
}

fn platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    }
}

/// The config directory, given the platform and its data-directory variable.
///
/// `home` is only consulted on Linux and macOS; on Windows it is ignored.
pub(crate) fn config_dir_for(
    platform: Platform,
    home: &Path,
    xdg_config_home: &Option<OsString>,
    windows_appdata: &Option<OsString>,
) -> PathBuf {
    match platform {
        // `%APPDATA%\ptt-tool` — `APPDATA` unset or empty falls back to the
        // temp directory, exactly as before this module existed (plan §2).
        Platform::Windows => base_dir(windows_appdata).join("ptt-tool"),
        // `~/Library/Application Support/ptt-tool`.
        Platform::MacOs => mac_support_dir(home).join("ptt-tool"),
        // `$XDG_CONFIG_HOME` when set (absolute, non-empty), else the XDG
        // base-directory spec's default `~/.config`, plus `ptt-tool`.
        Platform::Linux => {
            unix_leaf_dir(home, xdg_config_home, home.join(".config")).join("ptt-tool")
        }
    }
}

/// The failsafe state directory (plan §6.1).
pub(crate) fn state_dir_for(
    platform: Platform,
    home: &Path,
    xdg_state_home: &Option<OsString>,
    windows_localappdata: &Option<OsString>,
) -> PathBuf {
    match platform {
        // `%LOCALAPPDATA%\ptt-tool` (plan §2), same temp-dir fallback.
        Platform::Windows => base_dir(windows_localappdata).join("ptt-tool"),
        // State lives with the config on macOS (design §6).
        Platform::MacOs => mac_support_dir(home).join("ptt-tool"),
        // `$XDG_STATE_HOME` when set, else the spec default `~/.local/state`.
        Platform::Linux => {
            unix_leaf_dir(home, xdg_state_home, home.join(".local").join("state")).join("ptt-tool")
        }
    }
}

/// The log directory (plan §2). Linux keeps its logs beside the state file
/// (machine-local, non-essential data is `XDG_STATE_HOME`'s job); macOS uses
/// its separate `~/Library/Logs` tree; Windows keeps the `logs` leaf it
/// always had.
pub(crate) fn log_dir_for(
    platform: Platform,
    home: &Path,
    xdg_state_home: &Option<OsString>,
    windows_localappdata: &Option<OsString>,
) -> PathBuf {
    match platform {
        Platform::Windows => base_dir(windows_localappdata).join("ptt-tool").join("logs"),
        Platform::MacOs => mac_home(home).join("Logs").join("ptt-tool"),
        Platform::Linux => unix_leaf_dir(home, xdg_state_home, home.join(".local").join("state"))
            .join("ptt-tool")
            .join("logs"),
    }
}

// --- shared building blocks -------------------------------------------------------

/// The first component of a Windows app-data path: the variable's value, or
/// the temp directory when it is unset — the historical
/// `unwrap_or_else(std::env::temp_dir)` fallback. An *empty* value also
/// falls back: the pre-port code built a CWD-relative path for one, which
/// nothing could rely on.
fn base_dir(windows_variable: &Option<OsString>) -> PathBuf {
    windows_variable
        .as_ref()
        .map(PathBuf::from)
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir)
}

/// `$HOME`: unset or empty falls back to the temp directory rather than
/// building a path like `/ptt-tool/…` that nothing could write to. Windows
/// callers pass this too (the pure helpers keep one shape), but its Windows
/// arms never read it.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir)
}

fn env_var(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// A user base directory honouring an XDG variable: `$XDG_..._HOME` when it
/// is set, non-empty *and absolute*, else the conventional `~`-based default
/// passed as `default`. A relative variable is ignored — writing logs into
/// whichever directory the process started in would be worse.
///
/// "Absolute" is tested through [`std::path::Path::has_root`], not
/// [`std::path::Path::is_absolute`]: on Unix the two agree (`has_root` *is*
/// absolute there), but on a Windows host these helpers are unit-fed POSIX
/// paths (`/x/config`) that `is_absolute` rejects — a Windows path is only
/// absolute with a drive prefix, which would quietly send every such test
/// down the fallback arm. On a real Unix run such a variable always carries
/// a POSIX path, so the helper's semantics are unchanged.
fn unix_leaf_dir(home: &Path, xdg_variable: &Option<OsString>, default: PathBuf) -> PathBuf {
    let _ = home; // `default` is built from it by the caller
    xdg_variable
        .as_ref()
        .map(PathBuf::from)
        .filter(|value| !value.as_os_str().is_empty() && value.has_root())
        .unwrap_or(default)
}

/// `~/Library/Application Support` on macOS.
fn mac_support_dir(home: &Path) -> PathBuf {
    mac_home(home).join("Application Support")
}

/// `~/Library` on macOS, shared by the support and logs helpers.
fn mac_home(home: &Path) -> PathBuf {
    home.join("Library")
}

/// Create `dir` on demand (first run on a bare Linux/macOS system); ignore
/// failures — the callers that matter already report write errors of their
/// own, and the tests cannot create directories either.
fn ensure_dir(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/ada")
    }

    fn env(value: Option<&str>) -> Option<OsString> {
        value.map(OsString::from)
    }

    fn config(platform: Platform, xdg: Option<&str>, appdata: Option<&str>) -> PathBuf {
        config_dir_for(platform, &home(), &env(xdg), &env(appdata))
    }

    fn state(platform: Platform, xdg: Option<&str>, local: Option<&str>) -> PathBuf {
        state_dir_for(platform, &home(), &env(xdg), &env(local))
    }

    fn logs(platform: Platform, xdg: Option<&str>, local: Option<&str>) -> PathBuf {
        log_dir_for(platform, &home(), &env(xdg), &env(local))
    }

    // --- Windows: byte-identical with the pre-module behaviour ----------------

    #[test]
    fn windows_resolves_from_appdata_and_localappdata() {
        assert_eq!(
            config(
                Platform::Windows,
                None,
                Some("C:\\Users\\ada\\AppData\\Roaming")
            ),
            PathBuf::from("C:\\Users\\ada\\AppData\\Roaming").join("ptt-tool")
        );
        assert_eq!(
            config(Platform::Windows, None, None),
            std::env::temp_dir().join("ptt-tool"),
            "unset APPDATA falls back to the temp directory, as it always did"
        );
        assert_eq!(
            state(Platform::Windows, None, Some("C:\\LocalData")),
            PathBuf::from("C:\\LocalData").join("ptt-tool")
        );
        assert_eq!(
            logs(Platform::Windows, None, Some("C:\\LocalData")),
            PathBuf::from("C:\\LocalData").join("ptt-tool").join("logs")
        );
    }

    #[test]
    fn windows_falls_back_to_the_temp_dir_when_the_variable_is_missing_or_empty() {
        let temp = std::env::temp_dir();
        for variable in [None, Some("")] {
            assert_eq!(
                config(Platform::Windows, None, variable),
                temp.join("ptt-tool"),
                "APPDATA {variable:?}"
            );
            assert_eq!(
                state(Platform::Windows, None, variable),
                temp.join("ptt-tool"),
                "LOCALAPPDATA {variable:?}"
            );
            // The XDG variables are ignored on Windows.
            assert_eq!(
                config(Platform::Windows, Some("/x/config"), Some("C:\\Data")),
                PathBuf::from("C:\\Data").join("ptt-tool")
            );
        }
    }

    // --- Linux: XDG rules ------------------------------------------------------

    #[test]
    fn linux_follows_the_xdg_variables_when_they_are_set() {
        assert_eq!(
            config(Platform::Linux, Some("/x/config"), None),
            PathBuf::from("/x/config/ptt-tool")
        );
        assert_eq!(
            state(Platform::Linux, Some("/x/state"), None),
            PathBuf::from("/x/state/ptt-tool")
        );
        assert_eq!(
            logs(Platform::Linux, Some("/x/state"), None),
            PathBuf::from("/x/state/ptt-tool/logs")
        );
    }

    #[test]
    fn linux_defaults_are_the_spec_hidden_paths() {
        assert_eq!(
            config(Platform::Linux, None, None),
            PathBuf::from("/home/ada/.config/ptt-tool")
        );
        assert_eq!(
            state(Platform::Linux, None, None),
            PathBuf::from("/home/ada/.local/state/ptt-tool")
        );
        assert_eq!(
            logs(Platform::Linux, None, None),
            PathBuf::from("/home/ada/.local/state/ptt-tool/logs")
        );
    }

    #[test]
    fn linux_ignores_unset_empty_or_relative_xdg_variables() {
        for broken in [None, Some(""), Some("relative/dir")] {
            assert_eq!(
                config(Platform::Linux, broken, None),
                PathBuf::from("/home/ada/.config/ptt-tool"),
                "XDG_CONFIG_HOME {broken:?} must fall back to the spec default"
            );
            assert_eq!(
                state(Platform::Linux, broken, None),
                PathBuf::from("/home/ada/.local/state/ptt-tool"),
                "XDG_STATE_HOME {broken:?}"
            );
            assert_eq!(
                logs(Platform::Linux, broken, None),
                PathBuf::from("/home/ada/.local/state/ptt-tool/logs")
            );
        }
        // The Windows variables do not affect Linux.
        assert_eq!(
            config(Platform::Linux, None, Some("C:\\Data")),
            PathBuf::from("/home/ada/.config/ptt-tool")
        );
    }

    // --- macOS: Library trees ---------------------------------------------------

    #[test]
    fn macos_puts_config_and_state_in_application_support_and_logs_in_logs() {
        assert_eq!(
            config(Platform::MacOs, None, None),
            PathBuf::from("/home/ada/Library/Application Support/ptt-tool")
        );
        assert_eq!(
            state(Platform::MacOs, None, None),
            PathBuf::from("/home/ada/Library/Application Support/ptt-tool")
        );
        assert_eq!(
            logs(Platform::MacOs, None, None),
            PathBuf::from("/home/ada/Library/Logs/ptt-tool")
        );
        // Neither the XDG nor the Windows variables affect macOS.
        assert_eq!(
            config(Platform::MacOs, Some("/x/config"), Some("C:\\Data")),
            PathBuf::from("/home/ada/Library/Application Support/ptt-tool")
        );
        assert_eq!(
            logs(Platform::MacOs, Some("/x/state"), None),
            PathBuf::from("/home/ada/Library/Logs/ptt-tool")
        );
    }

    // --- the compiled platform's own files ---------------------------------------

    #[test]
    fn the_real_files_live_under_the_ptt_tool_directory() {
        let ptt_tool = Path::new("ptt-tool");
        // Windows/Linux: logs under ptt-tool/logs/
        // macOS: log dir is ~/Library/Logs/ptt-tool/ (no "logs" leaf),
        // findings is ~/Library/Logs/ptt-tool/findings.log
        #[cfg(target_os = "macos")]
        let under_ptt_tool_logs = ptt_tool.to_path_buf();
        #[cfg(not(target_os = "macos"))]
        let under_ptt_tool_logs = ptt_tool.join("logs");
        let log = log_dir();
        assert!(config_file().ends_with(ptt_tool.join("config.toml")));
        assert!(state_file().ends_with(ptt_tool.join("state.json")));
        assert!(log.ends_with(&under_ptt_tool_logs));
        assert_eq!(findings_file(), log.join("findings.log"));
        // Repeated asking is stable — the settings window, the session worker
        // and the diagnostics watchdog all resolve these more than once.
        assert_eq!(log_dir(), log);
        assert_eq!(config_file(), config_file());
        assert_eq!(state_file(), state_file());
    }
}
