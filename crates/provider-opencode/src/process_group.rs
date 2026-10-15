//! Port of `upstream/packages/provider-core/src/server/processGroup.ts`.
//!
//! `kill(0)` signals this process's own group and `kill(-1)` every process the
//! user owns. A pid of 0 or 1 fails with ESRCH, and callers ignore that.

use std::io;

pub const SIGTERM: i32 = 15;
pub const SIGKILL: i32 = 9;

pub fn signal_process_group(pid: u32, signal: i32) -> io::Result<()> {
    if pid <= 1 {
        return Err(io::Error::from_raw_os_error(esrch()));
    }
    #[cfg(unix)]
    {
        let rc = unsafe { kill(-(pid as i32), signal) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process groups are not used on Windows; the child is killed directly",
        ))
    }
}

fn esrch() -> i32 {
    3
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
