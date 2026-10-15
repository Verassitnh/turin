//! Port of upstream/packages/provider-core/src/server/processGroup.ts.

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

    #[test]
    fn signals_a_spawned_process_group() {
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        use std::process::{Command, Stdio};

        const SIGKILL: i32 = 9;
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 600 & wait"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = i64::from(child.id());
        // Signal 0 only probes.
        signal_process_group(pid, 0).unwrap();
        signal_process_group(pid, SIGKILL).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(SIGKILL));
    }
}
