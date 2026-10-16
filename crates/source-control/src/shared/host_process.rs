//! Port of upstream/packages/shared/src/HostProcess.ts (`Environment`, `WorkingDirectory`).
//!
//! The process environment behind a trait so tests can supply their own variables.

use std::collections::HashMap;
use std::sync::Arc;

pub trait Environment: Send + Sync + 'static {
    fn get(&self, name: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// A fixed set of variables, for tests.
#[derive(Debug, Default, Clone)]
pub struct MapEnvironment(pub HashMap<String, String>);

impl MapEnvironment {
    pub fn new<I, K, V>(vars: I) -> Arc<Self>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Arc::new(Self(
            vars.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        ))
    }
}

impl Environment for MapEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

pub fn process_environment() -> Arc<dyn Environment> {
    Arc::new(ProcessEnvironment)
}

/// `HostProcess.WorkingDirectory`: the process's current directory.
pub fn working_directory() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into())
}
