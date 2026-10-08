//! Microphone control abstraction (plan §4) plus its Windows Core Audio
//! implementation (plan §7).
//!
//! The [`MicController`] trait and [`DeviceInfo`] arrive in milestone M1,
//! together with `wasapi.rs`; everything else in the workspace depends only
//! on the trait, never on the Windows types.
