//! Single-instance guard and process liveness (plan §9 M3; plan §6.4's
//! "was the recorded pid still running?").

/// Take the single-instance lock for the lifetime of this process.
///
/// The handle is deliberately never closed: holding it *is* the guard, and
/// Windows releases the mutex when the process ends — so a second launch
/// quits in favour of the first (plan §9 M3).
#[cfg(any(windows, test))]
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
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

/// Is `pid` still running? Plan §6.4 needs this to decide whether the
/// microphone recorded in `state.json` was abandoned by a dead process.
#[cfg(any(windows, test))]
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
    #[cfg(not(windows))]
    {
        // The product runs on Windows (plan §1); this branch exists so the
        // decision logic can be unit-tested on the development host.
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

    #[test]
    fn the_single_instance_lock_is_taken_exactly_once() {
        // `CreateMutexW` reports "already exists" even for the same process,
        // so this must stay the only test that takes the lock (plan §9 M3:
        // a second launch quits in favour of the first).
        acquire().expect("the first acquisition succeeds");
    }
}
