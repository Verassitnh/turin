//! Shared types and traits for POSIX process ownership and Linux cgroups.

use crate::session_runtime::error::AcpProcessGroupTerminationError;

/// Port of upstream `AcpPosixProcessIdentity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpPosixProcessIdentity {
    pub executable: Option<String>,
    pub pgid: i32,
    pub pid: i32,
    pub ppid: i32,
    pub sid: i32,
    pub state: Option<String>,
    pub start_time: String,
}

/// Port of upstream `AcpOwnedPosixProcess`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpOwnedPosixProcess {
    pub executable: Option<String>,
    pub pgid: i32,
    pub pid: i32,
    pub ppid: i32,
    pub sid: i32,
    pub state: Option<String>,
    pub start_time: String,
    pub parent_executable: Option<String>,
    pub parent_start_time: String,
}

impl From<AcpPosixProcessIdentity> for AcpOwnedPosixProcess {
    fn from(value: AcpPosixProcessIdentity) -> Self {
        Self {
            executable: value.executable,
            pgid: value.pgid,
            pid: value.pid,
            ppid: value.ppid,
            sid: value.sid,
            state: value.state,
            start_time: value.start_time,
            parent_executable: None,
            parent_start_time: String::new(),
        }
    }
}

/// Mutable root capture state for POSIX ownership tracking.
#[derive(Debug, Default)]
pub struct AcpPosixOwnershipRoot {
    pub capture_attempted: bool,
    pub value: Option<AcpOwnedPosixProcess>,
}

/// Port of upstream `AcpPosixProcessTreeController`.
pub trait AcpPosixProcessTreeController: Send + Sync {
    fn child_pids_of(&self, pid: i32) -> Vec<i32>;
    fn children_of(&self, pid: i32) -> Vec<AcpPosixProcessIdentity>;
    fn identity(&self, pid: i32) -> Option<AcpPosixProcessIdentity>;
    fn snapshot(&self) -> Result<Vec<AcpPosixProcessIdentity>, AcpProcessGroupTerminationError>;
    fn signal_process(&self, pid: i32, signal: i32);
}

/// Port of upstream `AcpLinuxCgroupLease`.
pub trait AcpLinuxCgroupLease: Send + Sync {
    fn contains(&self, pid: i32) -> bool;
    fn exists(&self) -> bool;
    fn path(&self) -> &str;
    fn relative_path(&self) -> &str;
    fn kill(&self) -> Result<(), std::io::Error>;
    fn populated(&self) -> Result<bool, std::io::Error>;
    fn remove(&self) -> Result<(), std::io::Error>;
}

/// Port of upstream `AcpLinuxCgroupController`.
pub trait AcpLinuxCgroupController: Send + Sync {
    fn create(&self) -> Option<Box<dyn AcpLinuxCgroupLease>>;
}
