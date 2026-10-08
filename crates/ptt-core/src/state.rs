//! Pure state machine (plan §5): `step(state, event, cfg, now) -> (state, actions)`.
//!
//! Fully deterministic and unit-testable with a fake clock. Lands in M2.
