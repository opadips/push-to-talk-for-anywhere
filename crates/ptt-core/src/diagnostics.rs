//! Input-stall diagnostics (spec §4): when the hook thread's message loop
//! stalls, when the hooks go silent while input keeps arriving, and when the
//! session worker stalls. Every decision is a pure function of tick counts,
//! so all the rules below are unit-tested without Windows (spec §4).
//!
//! The rules the tests protect: every comparison is a `wrapping_sub` (the
//! tick source wraps every ~49.7 days), each finding kind is rate-limited to
//! one report per [`REPORT_GAP_MS`], and a condition that already exists
//! when the [`Watchdog`] is created reports on the very first check.

use std::sync::atomic::{AtomicU32, Ordering};

/// Wait this long for the hook to run and the stall warning may fire.
pub const STALL_WAIT_MS: u32 = 150;

/// Our own hook code may take this long and the stall warning may fire.
pub const STALL_EXEC_MS: u64 = 15;

/// At most one stall warning globally per 5 s (spec §4) — one gate for
/// every device, so a keyboard stall suppresses a mouse one within the gap.
pub const STALL_LOG_GAP_MS: u32 = 5_000;

/// The hook thread's message loop is stalled after this much silence.
pub const HOOK_THREAD_STALLED_MS: u32 = 5_000;

/// Callbacks this old count as silent, while system input keeps arriving.
pub const HOOKS_SILENT_AFTER_MS: u32 = 60_000;

/// System input within this window counts as active (the hooks-silent
/// finding means nothing when the machine is idle).
pub const SYSTEM_ACTIVE_WITHIN_MS: u32 = 10_000;

/// The session worker loop is stalled after this much silence.
pub const WORKER_STALLED_MS: u32 = 2_000;

/// At most one report per finding kind per minute (spec §4).
pub const REPORT_GAP_MS: u32 = 60_000;

/// Milliseconds since boot on Windows — the same cheap, system-wide clock
/// the hook thread and the session worker can both read, wrapping every
/// ~49.7 days (hence `wrapping_sub` in every comparison below).
#[cfg(windows)]
pub fn tick_ms() -> u32 {
    // SAFETY: GetTickCount takes no arguments and cannot fail.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

/// Off Windows: process-elapsed milliseconds — enough for the tests and a
/// non-Windows build; nothing shares this clock across processes.
#[cfg(not(windows))]
pub fn tick_ms() -> u32 {
    use std::sync::OnceLock;
    use std::time::Instant;

    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u32
}

/// One stall warning across all devices per [`STALL_LOG_GAP_MS`] (one
/// global gate, not one per device): the caller passes the last tick it
/// logged at, the caller stores `now` when this returns `Some` (the glue
/// below does both).
pub fn stall_warning(
    device: &str,
    wait_ms: u32,
    exec_ms: u64,
    now: u32,
    last_logged: u32,
) -> Option<String> {
    if wait_ms < STALL_WAIT_MS && exec_ms < STALL_EXEC_MS {
        return None;
    }
    if now.wrapping_sub(last_logged) < STALL_LOG_GAP_MS {
        return None;
    }
    Some(format!(
        "{device} hook: input waited {wait_ms} ms for the hook to run, our code took {exec_ms} ms"
    ))
}

/// The fresher of two tick beats, wrap-safe: the smaller wrapping distance
/// from `now` is the more recent one; a tie picks `b`.
pub fn most_recent(now: u32, a: u32, b: u32) -> u32 {
    if now.wrapping_sub(a) < now.wrapping_sub(b) {
        a
    } else {
        b
    }
}

/// Everything the [`Watchdog`] decides from, in ticks (spec §4). A beat of
/// `0` means the source was never seen — no finding can fire for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    /// Tick the snapshot was taken at.
    pub now: u32,
    /// Last time the hook thread's message loop beat (0 = never seen).
    pub hook_thread: u32,
    /// Last time either hook callback ran (0 = never seen).
    pub callback: u32,
    /// Last time the session worker loop beat (0 = gone / never seen).
    pub worker: u32,
    /// Last time Windows reported system-wide input (raw-input beat).
    pub system_input: u32,
}

/// What [`Watchdog::check`] found (spec §4). The `message` strings are
/// spec-fixed: they carry the recovery action, not just the symptom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    /// The hook thread's message loop stopped pumping.
    HookThreadStalled {
        /// Milliseconds since the last beat.
        ms: u32,
    },
    /// Input arrives but the hooks have not seen any for a minute.
    HooksSilent {
        /// Milliseconds between system input and the last callback.
        silent_for: u32,
    },
    /// The session worker loop stopped beating.
    WorkerStalled {
        /// Milliseconds since the last beat.
        ms: u32,
    },
}

impl Finding {
    /// The spec §4 log line — name what happened *and* what puts it right.
    pub fn message(&self) -> String {
        match self {
            Finding::HookThreadStalled { ms } => format!(
                "the input-hook thread's message loop stalled for {ms} ms — \
                 Windows can silently drop hooks that stall"
            ),
            Finding::HooksSilent { silent_for } => {
                let s = silent_for / 1_000;
                format!(
                    "input is arriving but our hooks have been silent for {s} s — \
                     Windows may have removed them; lock and unlock Windows to put them back"
                )
            }
            Finding::WorkerStalled { ms } => format!(
                "the session worker loop stalled for {ms} ms — \
                 a microphone change may not have been applied"
            ),
        }
    }
}

/// Rate-limited finding machine: one report per [`REPORT_GAP_MS`] per
/// finding kind, each kind gated by its own slot so a noisy kind cannot
/// starve the others (spec §4).
pub struct Watchdog {
    /// Tick of the last `HookThreadStalled` report.
    hook_thread: u32,
    /// Tick of the last `HooksSilent` report.
    hooks_silent: u32,
    /// Tick of the last `WorkerStalled` report.
    worker: u32,
}

impl Watchdog {
    /// Slots start one report gap in the past, so a condition that already
    /// exists is eligible to report on the very first [`Watchdog::check`].
    pub fn new(now: u32) -> Self {
        let gap = now.wrapping_sub(REPORT_GAP_MS);
        Self {
            hook_thread: gap,
            hooks_silent: gap,
            worker: gap,
        }
    }

    /// Fixed order — HookThreadStalled, HooksSilent, WorkerStalled. Each
    /// kind reports at most once per [`REPORT_GAP_MS`]: the gate stores the
    /// report tick only when it actually reports.
    pub fn check(&mut self, s: &Snapshot) -> Vec<Finding> {
        let mut findings = Vec::new();

        let hook_stalled =
            s.hook_thread != 0 && s.now.wrapping_sub(s.hook_thread) > HOOK_THREAD_STALLED_MS;
        if hook_stalled && s.now.wrapping_sub(self.hook_thread) >= REPORT_GAP_MS {
            self.hook_thread = s.now;
            findings.push(Finding::HookThreadStalled {
                ms: s.now.wrapping_sub(s.hook_thread),
            });
        }

        // The callback tick is recorded after the event tick, so in healthy
        // use `behind` underflows to a huge number — the `u32::MAX / 2`
        // guard rejects that wrapped region, leaving only genuine staleness.
        let behind = s.system_input.wrapping_sub(s.callback);
        let hooks_silent = s.callback != 0
            && s.now.wrapping_sub(s.system_input) <= SYSTEM_ACTIVE_WITHIN_MS
            && behind > HOOKS_SILENT_AFTER_MS
            && behind < u32::MAX / 2;
        if hooks_silent && s.now.wrapping_sub(self.hooks_silent) >= REPORT_GAP_MS {
            self.hooks_silent = s.now;
            findings.push(Finding::HooksSilent { silent_for: behind });
        }

        let worker_stalled = s.worker != 0 && s.now.wrapping_sub(s.worker) > WORKER_STALLED_MS;
        if worker_stalled && s.now.wrapping_sub(self.worker) >= REPORT_GAP_MS {
            self.worker = s.now;
            findings.push(Finding::WorkerStalled {
                ms: s.now.wrapping_sub(s.worker),
            });
        }

        findings
    }
}

// Thin glue: process-global beats recorded from the hook callbacks (hook
// thread), the hook-thread pump, and the session worker, read back as a
// [`Snapshot`]. No unit tests — statics are process-global and Rust tests
// run in parallel, so beat assertions would be flaky; every decision these
// feed lives in the pure code above.

/// Last keyboard hook callback beat.
static LAST_KEYBOARD_CB: AtomicU32 = AtomicU32::new(0);
/// Last mouse hook callback beat.
static LAST_MOUSE_CB: AtomicU32 = AtomicU32::new(0);
/// Last hook-thread message-loop beat.
static LAST_HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// Last session-worker beat (0 while the worker is gone).
static LAST_WORKER: AtomicU32 = AtomicU32::new(0);
/// Last stall-warning log tick — one global gate for all devices, via
/// [`maybe_stall_warning`].
static LAST_STALL_LOG: AtomicU32 = AtomicU32::new(0);

/// Record a keyboard hook callback beat.
pub fn note_keyboard_cb() {
    LAST_KEYBOARD_CB.store(tick_ms(), Ordering::Relaxed);
}

/// Record a mouse hook callback beat.
pub fn note_mouse_cb() {
    LAST_MOUSE_CB.store(tick_ms(), Ordering::Relaxed);
}

/// Record a hook-thread message-loop beat.
pub fn note_hook_thread() {
    LAST_HOOK_THREAD.store(tick_ms(), Ordering::Relaxed);
}

/// Record a session-worker beat.
pub fn note_worker() {
    LAST_WORKER.store(tick_ms(), Ordering::Relaxed);
}

/// The session worker is gone: clear its beat so it cannot "stall".
pub fn note_worker_gone() {
    LAST_WORKER.store(0, Ordering::Relaxed);
}

/// Zero every beat and the stall gate (spec §4).
pub fn reset() {
    LAST_KEYBOARD_CB.store(0, Ordering::Relaxed);
    LAST_MOUSE_CB.store(0, Ordering::Relaxed);
    LAST_HOOK_THREAD.store(0, Ordering::Relaxed);
    LAST_WORKER.store(0, Ordering::Relaxed);
    LAST_STALL_LOG.store(0, Ordering::Relaxed);
}

/// A [`Snapshot`] of the recorded beats, taken at [`tick_ms`].
pub fn snapshot(system_input: u32) -> Snapshot {
    let now = tick_ms();
    Snapshot {
        now,
        hook_thread: LAST_HOOK_THREAD.load(Ordering::Relaxed),
        callback: most_recent(
            now,
            LAST_KEYBOARD_CB.load(Ordering::Relaxed),
            LAST_MOUSE_CB.load(Ordering::Relaxed),
        ),
        worker: LAST_WORKER.load(Ordering::Relaxed),
        system_input,
    }
}

/// [`stall_warning`] against the recorded last-log tick, storing `now` when
/// it reports so the next call is rate-limited (spec §4).
pub fn maybe_stall_warning(device: &str, wait_ms: u32, exec_ms: u64) -> Option<String> {
    let now = tick_ms();
    let warning = stall_warning(
        device,
        wait_ms,
        exec_ms,
        now,
        LAST_STALL_LOG.load(Ordering::Relaxed),
    );
    if warning.is_some() {
        LAST_STALL_LOG.store(now, Ordering::Relaxed);
    }
    warning
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot where every beat is recent: nothing stalled.
    fn fresh(now: u32) -> Snapshot {
        Snapshot {
            now,
            hook_thread: now - 100,
            callback: now - 100,
            worker: now - 100,
            system_input: now - 100,
        }
    }

    #[test]
    fn stall_below_both_thresholds_reports_nothing() {
        assert_eq!(stall_warning("keyboard", 149, 14, 10_000, 0), None);
    }

    #[test]
    fn stall_reports_when_wait_exceeds_150ms() {
        let message = stall_warning("keyboard", 150, 0, 10_000, 4_000).expect("wait hit the bar");
        assert!(
            message.contains("keyboard hook: input waited 150 ms"),
            "{message}"
        );
        assert!(message.contains("our code took 0 ms"), "{message}");
    }

    #[test]
    fn stall_reports_when_exec_exceeds_15ms() {
        assert!(
            stall_warning("mouse", 0, 15, 10_000, 0).is_some(),
            "exec hit the bar"
        );
    }

    #[test]
    fn stall_reports_are_rate_limited_to_once_per_5s() {
        let reported = 100_000;
        assert!(stall_warning("keyboard", 150, 0, reported, reported - 5_000).is_some());
        assert_eq!(
            stall_warning("keyboard", 150, 0, reported + 4_999, reported),
            None,
            "the same call again — inside the 5 s gap"
        );
        assert!(
            stall_warning("keyboard", 150, 0, reported + 5_000, reported).is_some(),
            "5 s later"
        );
    }

    #[test]
    fn most_recent_callback_picks_the_newer_tick() {
        assert_eq!(most_recent(1_000, 900, 950), 950, "the newer beat wins");
        assert_eq!(most_recent(1_000, 950, 950), 950, "a tie picks b");
    }

    #[test]
    fn most_recent_handles_wraps() {
        assert_eq!(
            most_recent(10, u32::MAX - 5, 40),
            u32::MAX - 5,
            "the beat just before the wrap is the recent one"
        );
    }

    #[test]
    fn hook_thread_stall_fires_after_5s() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let stalled = Snapshot {
            hook_thread: now - 5_001,
            ..fresh(now)
        };
        assert_eq!(
            watchdog.check(&stalled),
            vec![Finding::HookThreadStalled { ms: 5_001 }]
        );
    }

    #[test]
    fn hook_thread_stall_is_quiet_when_fresh_or_absent() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let fresh_beat = Snapshot {
            hook_thread: now - 4_999,
            ..fresh(now)
        };
        assert!(watchdog.check(&fresh_beat).is_empty(), "still pumping");
        let absent = Snapshot {
            hook_thread: 0,
            ..fresh(now)
        };
        assert!(
            watchdog.check(&absent).is_empty(),
            "beat 0 means never seen"
        );
    }

    #[test]
    fn hooks_silent_fires_when_input_flows_but_callbacks_are_a_minute_old() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let system_input = now - 100;
        let silent = Snapshot {
            callback: system_input - 60_001,
            ..fresh(now)
        };
        assert_eq!(
            watchdog.check(&silent),
            vec![Finding::HooksSilent { silent_for: 60_001 }]
        );
    }

    #[test]
    fn hooks_silent_stays_quiet_when_callbacks_are_newer_than_system_input() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let system_input = now - 100;
        // Healthy use: the callback tick is recorded after the event tick,
        // so `system_input - callback` underflows — that must not read as
        // "the hooks have been silent for ~49 days".
        let healthy = Snapshot {
            callback: system_input.wrapping_add(3),
            ..fresh(now)
        };
        assert!(
            watchdog.check(&healthy).is_empty(),
            "a callback newer than the event tick is healthy, not silent"
        );
    }

    #[test]
    fn hooks_silent_stays_quiet_while_callbacks_are_fresh() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let system_input = now - 100;
        let fresh_callbacks = Snapshot {
            callback: system_input - 60_000,
            ..fresh(now)
        };
        assert!(
            watchdog.check(&fresh_callbacks).is_empty(),
            "exactly 60 s is not silent yet"
        );
    }

    #[test]
    fn hooks_silent_requires_active_system_input() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let system_input = now - 10_001;
        let idle = Snapshot {
            callback: system_input - 60_001,
            system_input,
            ..fresh(now)
        };
        assert!(
            watchdog.check(&idle).is_empty(),
            "no recent input: silence is normal"
        );
    }

    #[test]
    fn worker_check_is_quiet_when_absent_or_fresh() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let absent = Snapshot {
            worker: 0,
            ..fresh(now)
        };
        assert!(
            watchdog.check(&absent).is_empty(),
            "worker 0 means never seen"
        );
        let fresh_worker = Snapshot {
            worker: now - 1_999,
            ..fresh(now)
        };
        assert!(watchdog.check(&fresh_worker).is_empty(), "still ticking");
    }

    #[test]
    fn worker_stall_fires_after_2s() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let stalled = Snapshot {
            worker: now - 2_001,
            ..fresh(now)
        };
        assert_eq!(
            watchdog.check(&stalled),
            vec![Finding::WorkerStalled { ms: 2_001 }]
        );
    }

    #[test]
    fn findings_can_report_immediately_on_the_first_check() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let stalled = Snapshot {
            hook_thread: now - 5_001,
            ..fresh(now)
        };
        let findings = watchdog.check(&stalled);
        assert_eq!(
            findings.len(),
            1,
            "a pre-existing condition reports on the first check"
        );
    }

    #[test]
    fn findings_are_rate_limited_to_once_per_minute() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let stalled = Snapshot {
            worker: now - 2_001,
            ..fresh(now)
        };
        assert_eq!(
            watchdog.check(&stalled),
            vec![Finding::WorkerStalled { ms: 2_001 }]
        );
        assert!(
            watchdog.check(&stalled).is_empty(),
            "the same stall does not report twice"
        );
        let later = Snapshot {
            now: now + 60_000,
            worker: now + 60_000 - 2_001,
            ..fresh(now + 60_000)
        };
        assert_eq!(
            watchdog.check(&later),
            vec![Finding::WorkerStalled { ms: 2_001 }],
            "a full minute later it reports again"
        );
    }

    #[test]
    fn wrap_around_ticks_do_not_produce_bogus_findings() {
        let now = 1_000;
        let mut watchdog = Watchdog::new(now);
        let wrapped = Snapshot {
            now,
            hook_thread: u32::MAX - 1_000,
            callback: u32::MAX - 300,
            worker: u32::MAX - 100,
            system_input: now - 100,
        };
        assert!(
            watchdog.check(&wrapped).is_empty(),
            "the wrapping diffs are small and consistent"
        );
    }

    #[test]
    fn finding_messages_name_the_stall_and_worker_symptoms() {
        let hook = Finding::HookThreadStalled { ms: 4_242 }.message();
        assert!(hook.contains("stalled for 4242 ms"), "{hook}");
        let worker = Finding::WorkerStalled { ms: 2_500 }.message();
        assert!(
            worker.contains("session worker loop stalled for 2500 ms"),
            "{worker}"
        );
    }

    #[test]
    fn multiple_findings_come_out_in_fixed_order() {
        let now = 100_000;
        let mut watchdog = Watchdog::new(now);
        let system_input = now - 100;
        let everything = Snapshot {
            now,
            hook_thread: now - 5_001,
            callback: system_input - 60_001,
            worker: now - 2_001,
            system_input,
        };
        assert_eq!(
            watchdog.check(&everything),
            vec![
                Finding::HookThreadStalled { ms: 5_001 },
                Finding::HooksSilent { silent_for: 60_001 },
                Finding::WorkerStalled { ms: 2_001 },
            ],
            "all three at once, in the documented order"
        );
    }

    #[test]
    fn finding_messages_name_the_recovery_action() {
        let hooks_silent = Finding::HooksSilent { silent_for: 90_000 }.message();
        assert!(
            hooks_silent.contains("lock and unlock Windows"),
            "{hooks_silent}"
        );
        assert!(hooks_silent.contains("90 s"), "{hooks_silent}");
    }
}
