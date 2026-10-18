//! Single-instance guard and process liveness (plan §9 M3; plan §6.4's
//! "was the recorded pid still running?").

/// Take the single-instance lock for the lifetime of this process.
///
/// The handle is deliberately never closed: holding it *is* the guard, and
/// Windows releases the mutex when the process ends — so a second launch
/// quits in favour of the first (plan §9 M3). Unix takes a non-blocking
/// exclusive `flock` on the lock file instead; the OS drops it when the
/// process ends, exactly the way the named mutex is released. Callers must
/// take it at most once per process: `CreateMutexW` reports "already
/// exists" for a second handle in the *same* process too, and on Unix a
/// second `flock` — through a freshly opened file — would conflict with the
/// process's own held lock the same way.
pub fn acquire() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;

        let name: Vec<u16> = "Local\\ptt-tool\0".encode_utf16().collect();
        // SAFETY: `name` outlives the call and is NUL-terminated.
        let _mutex = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .map_err(|error| anyhow::anyhow!("cannot create the single-instance lock: {error}"))?;
        // A valid handle *and* this error means someone else holds the name.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            anyhow::bail!("ptt is already running — quit that session before starting another");
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        // Spec §7: an XDG runtime lock — `$XDG_RUNTIME_DIR` is per-login
        // fresh and private to the user, `/tmp` covers sessions without
        // one. The file itself stays empty; the `flock` on it is the guard.
        let path = if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
            .and_then(|value| (!value.is_empty()).then_some(value))
        {
            std::path::PathBuf::from(dir).join("ptt-tool.lock")
        } else {
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("ptt-tool-{uid}.lock"))
        };

        let file = std::fs::File::create(&path)
            .map_err(|error| anyhow::anyhow!("cannot create the single-instance lock: {error}"))?;
        match file.try_lock() {
            Ok(()) => {}
            // `TryLockError::WouldBlock` is the other instance holding it.
            Err(std::fs::TryLockError::WouldBlock) => {
                anyhow::bail!("ptt is already running — quit that session before starting another");
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(anyhow::anyhow!(
                    "cannot take the single-instance lock: {error}"
                ));
            }
        }
        // Keep the handle for the life of the process: dropping it would
        // release the lock (the Windows arm keeps its mutex for the same
        // reason). One file descriptor, leaked once per process.
        std::mem::forget(file);
        Ok(())
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok(())
    }
}

/// Is `pid` still running? Plan §6.4 needs this to decide whether the
/// microphone recorded in `state.json` was abandoned by a dead process.
pub fn process_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };

        // SAFETY: a pid nobody owns simply yields no handle.
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
            return false;
        };
        // Signalled means the process object was triggered: it has exited.
        let exited = unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
        let _ = unsafe { CloseHandle(handle) };
        !exited
    }
    #[cfg(unix)]
    {
        // A pid wider than `pid_t` cannot exist; `kill(-1, 0)` would mean
        // "any process", which is not what a caller with `u32::MAX` means.
        if pid > libc::pid_t::MAX as u32 {
            return false;
        }
        // `kill(pid, 0)` does the whole lookup and permission check but
        // sends no signal: `0` means alive, `EPERM` means "alive but owned
        // by someone else" — still running, from the failsafe's point of
        // view — and only `ESRCH` means gone.
        // SAFETY: `pid` is in range and `0` is the no-signal sentinel.
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        alive || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(any(windows, unix)))]
    {
        // The product runs on Windows, Linux and macOS (design §1); this
        // branch exists so the decision logic stays compilable everywhere.
        std::path::Path::new("/proc").join(pid.to_string()).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_process_is_running_and_a_bogus_pid_is_not() {
        assert!(process_running(std::process::id()));
        assert!(!process_running(u32::MAX), "no process owns that id");
    }

    #[cfg(unix)]
    #[test]
    fn a_second_flock_on_the_same_file_is_refused() {
        // The mechanism `acquire()` relies on, exercised on a throwaway
        // file (`FileExt::try_lock` is non-blocking *exclusive*): a second
        // open file description cannot take the lock while the first one
        // holds it (plan §9 M3: a second launch, or even a second call in
        // one process, quits in favour of the first).
        let path = std::env::temp_dir().join(format!("ptt-tool.lock-test-{}", std::process::id()));
        let first = std::fs::File::create(&path).expect("create the lock file");
        first
            .try_lock()
            .expect("the first exclusive lock is taken, blocking would mean a lock bug");
        let second = std::fs::File::create(&path).expect("reopening the file works");
        let outcome = second.try_lock().expect_err("held elsewhere");
        assert!(
            matches!(outcome, std::fs::TryLockError::WouldBlock),
            "the second lock conflicts with the first: {outcome:?}"
        );
        drop(second);
        drop(first);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_single_instance_lock_is_taken_exactly_once() {
        // `CreateMutexW` reports "already exists" even for the same process
        // (and a second `flock` would conflict with the process's own lock),
        // so this must stay the only test that takes the lock (plan §9 M3:
        // a second launch quits in favour of the first).
        acquire().expect("the first acquisition succeeds");
    }
}
