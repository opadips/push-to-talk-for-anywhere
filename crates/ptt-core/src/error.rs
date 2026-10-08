//! Library error type (thiserror, plan §2: `thiserror` in libraries,
//! `anyhow` in the app).

/// Everything that can go wrong while talking to the audio stack.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no default capture device available")]
    NoDefaultDevice,

    #[error("capture device not found: {0}")]
    DeviceNotFound(String),

    #[error("capture device unavailable: {0}")]
    DeviceUnavailable(String),

    #[error("device does not support endpoint mute: {0}")]
    MuteUnsupported(String),

    #[error("invalid mouse button {0:?} (valid: left, right, middle, x1, x2)")]
    InvalidMouseButton(String),

    #[error("input hook failed: {0}")]
    InputHook(String),

    /// Windows-only: carries the `HRESULT`/message from windows-rs.
    #[cfg(windows)]
    #[error("Windows audio API error: {0}")]
    Windows(#[from] windows::core::Error),
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn device_not_found_message_names_the_id() {
        let err = Error::DeviceNotFound("{0.0.0.00000000}.abc".into());
        assert_eq!(
            err.to_string(),
            "capture device not found: {0.0.0.00000000}.abc"
        );
    }

    #[test]
    fn no_default_device_message_is_actionable() {
        let err = Error::NoDefaultDevice;
        assert_eq!(err.to_string(), "no default capture device available");
    }

    #[test]
    fn input_hook_message_names_the_failure() {
        let err = Error::InputHook("the hook thread exited".into());
        let message = err.to_string();
        assert!(message.contains("input hook"), "{message}");
        assert!(message.contains("the hook thread exited"), "{message}");
    }

    #[test]
    fn mute_unsupported_message_names_the_device() {
        let err = Error::MuteUnsupported("USB Mic".into());
        assert!(err.to_string().contains("USB Mic"));
        assert!(err.to_string().contains("mute"));
    }
}
