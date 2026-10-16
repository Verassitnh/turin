//! Port of upstream/packages/provider-core/src/server/processGroup.ts (signal only).

use std::io;

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(unix)]
const ESRCH: i32 = 3;

/// Signal only a process group created for a spawned child. `pid <= 1` must
/// never reach `kill(-pid, ...)`: zero targets our group and -1 is global.
#[cfg(unix)]
pub fn signal_process_group(pid: i64, signal: i32) -> io::Result<()> {
    if pid <= 1 || pid > i32::MAX as i64 {
        return Err(io::Error::from_raw_os_error(ESRCH));
    }
    // SAFETY: a checked positive child group id becomes negative only here.
    let rc = unsafe { kill(-(pid as i32), signal) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub fn signal_process_group(_pid: i64, _signal: i32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process groups are unix-only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn rejects_unsafe_group_ids() {
        for pid in [i64::MIN, -1, 0, 1, i64::MAX] {
            assert_eq!(
                signal_process_group(pid, 0).unwrap_err().raw_os_error(),
                Some(ESRCH)
            );
        }
    }
}
