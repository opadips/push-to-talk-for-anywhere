//! Rolling file logging (plan §2: `tracing` + `tracing-appender` writing to
//! `%LOCALAPPDATA%\ptt-tool\logs`).

use std::path::PathBuf;

/// Where the log files live (plan §2: `%LOCALAPPDATA%\ptt-tool\logs`).
pub fn log_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("ptt-tool")
        .join("logs")
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
        assert!(log_dir().ends_with(std::path::Path::new("ptt-tool").join("logs")));
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
