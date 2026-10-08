//! Keyboard/mouse input abstraction (plan §4) plus the low-level hook
//! implementation (plan §7).
//!
//! [`InputSource`], [`Binding`] and [`InputEvent`] arrive in milestone M2,
//! together with `hook.rs`. The hook only ever compares against the one bound
//! input — it never records or forwards other keystrokes (plan §11).
