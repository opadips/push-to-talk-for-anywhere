//! Rolling file logging (plan §2: `tracing` + `tracing-appender` into the
//! per-OS log directory `ptt_core::paths` decides).

use std::path::PathBuf;

/// Where the log files live (plan §2: `%LOCALAPPDATA%\ptt-tool\logs` on
/// Windows, the state dir on Linux, `~/Library/Logs` on macOS — in
/// `ptt_core::paths`).
pub fn log_dir() -> PathBuf {
    ptt_core::paths::log_dir()
}

/// Start logging at `info` into `logs/ptt.log` (rotated daily).
///
/// Returns the worker guard: it must be held for as long as the process
/// logs, and dropping it flushes the file. A second call is harmless — only
/// the first installs a subscriber.
pub fn init() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let appender = tracing_appender::rolling::daily(log_dir(), "ptt.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    // try_init, never init: a second subscriber must not panic (plan §8).
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .try_init();
    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_directory_lives_under_ptt_tool() {
        let log = log_dir();
        // Per-platform expected leaf: Windows/Linux have `ptt-tool/logs`,
        // macOS uses `~/Library/Logs/ptt-tool/` (no `logs` leaf).
        #[cfg(target_os = "macos")]
        let expected = std::path::Path::new("ptt-tool").to_path_buf();
        #[cfg(not(target_os = "macos"))]
        let expected = std::path::Path::new("ptt-tool").join("logs");
        assert!(
            log.ends_with(&expected),
            "log dir {} does not end with {}",
            log.display(),
            expected.display()
        );
    }

    #[test]
    fn init_is_safe_to_call_more_than_once() {
        // `try_init`, not `init`: a second subscriber must not panic —
        // plan §8's "never crash" applies to the whole app.
        let first = init();
        let second = init();
        assert!(first.is_some());
        assert!(second.is_some());
    }
}
