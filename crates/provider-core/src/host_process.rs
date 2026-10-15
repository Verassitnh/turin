//! Process-environment facts upstream reads through `@t3tools/shared/HostProcess`
//! (`Platform`, `HomeDirectory`, `Environment`). Upstream exposes them as
//! `Context.Reference`s so tests can override them; here they are one plain
//! value that callers pass down.
//!
//! Local shim: swap for `turin_shared::host_process` when `turin-shared`
//! provides it.

use std::{collections::HashMap, fmt};

/// A process environment (`NodeJS.ProcessEnv`).
pub type ProcessEnv = HashMap<String, String>;

/// `Debug` for an environment that prints variable names and no values. Values
/// can hold provider API keys and device auth tokens.
pub(crate) struct RedactedEnvironment<'a>(pub &'a ProcessEnv);

impl fmt::Debug for RedactedEnvironment<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut keys: Vec<&String> = self.0.keys().collect();
        keys.sort();
        f.debug_map()
            .entries(keys.into_iter().map(|key| (key, "[REDACTED]")))
            .finish()
    }
}

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
#[derive(Clone)]
pub struct HostProcess {
    pub platform: Platform,
    pub home_directory: String,
    pub environment: ProcessEnv,
}

impl fmt::Debug for HostProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostProcess")
            .field("platform", &self.platform)
            .field("home_directory", &self.home_directory)
            .field("environment", &RedactedEnvironment(&self.environment))
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_names_environment_variables_without_their_values() {
        let host = HostProcess {
            platform: Platform::Linux,
            home_directory: "/home/fixture".to_owned(),
            environment: HashMap::from([("OPENAI_API_KEY".to_owned(), "fixture-key".to_owned())]),
        };
        let debug = format!("{host:?}");
        assert!(debug.contains("OPENAI_API_KEY"), "{debug}");
        assert!(!debug.contains("fixture-key"), "{debug}");
    }
}
