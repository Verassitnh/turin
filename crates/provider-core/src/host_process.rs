//! Process-environment facts upstream reads through `@t3tools/shared/HostProcess`
//! (`Platform`, `HomeDirectory`, `Environment`). Upstream exposes them as
//! `Context.Reference`s so tests can override them; here they are one plain
//! value that callers pass down.
//!
//! Local shim: swap for `turin_shared::host_process` when `turin-shared`
//! provides it.

use std::collections::HashMap;

/// A process environment (`NodeJS.ProcessEnv`).
pub type ProcessEnv = HashMap<String, String>;

/// Node's `process.platform` names, which upstream branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Darwin,
    Linux,
    Win32,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "windows") {
            Self::Win32
        } else {
            Self::Other
        }
    }

    pub fn is_windows(self) -> bool {
        self == Self::Win32
    }
}

/// The facts about the host process that provider code branches on.
#[derive(Debug, Clone)]
pub struct HostProcess {
    pub platform: Platform,
    pub home_directory: String,
    pub environment: ProcessEnv,
}

impl HostProcess {
    /// Read from the running process.
    pub fn current() -> Self {
        Self {
            platform: Platform::current(),
            home_directory: dirs::home_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            environment: std::env::vars().collect(),
        }
    }
}
