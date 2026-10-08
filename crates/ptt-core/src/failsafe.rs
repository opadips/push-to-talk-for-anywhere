//! Fail-safe: record the original mute state, restore it on exit, panic, kill
//! or crash (plan §6). This is a core requirement, not polish. Lands in M3.
