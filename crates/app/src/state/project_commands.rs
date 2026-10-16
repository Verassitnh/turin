//! Port of upstream/packages/client-runtime/src/state/projectCommands.ts
//!
//! The atom family (`createProjectEnvironmentAtoms`) is RPC plumbing: one
//! process, so the reads and writes are direct calls and the file metadata
//! cache is `state/file_metadata.rs`. What stays is the logic the atoms
//! carried: the command serialization keys, the paths a write invalidates,
//! the optimistic-file key, and `openScratch`'s wait for the created project
//! to reach the client store (10 s).

use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexSet;
use tokio::sync::watch;
use turin_shared::path::resolve_workspace_file_path;

/// `OptimisticProjectFileTarget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimisticProjectFileTarget {
    pub environment_id: String,
    pub cwd: String,
    pub relative_path: String,
}

fn json_key(parts: &[&str]) -> String {
    serde_json::to_string(parts).expect("string arrays serialize")
}

/// `optimisticProjectFileKey`.
pub fn optimistic_project_file_key(target: &OptimisticProjectFileTarget) -> String {
    json_key(&[&target.environment_id, &target.cwd, &target.relative_path])
}

/// `projectConcurrency.key`: project commands run one at a time per project.
pub fn project_command_key(environment_id: &str, project_id: &str) -> String {
    json_key(&[environment_id, project_id])
}

/// The key for `openScratch` and `createNew`: serial per environment.
pub fn environment_command_key(environment_id: &str) -> String {
    environment_id.to_owned()
}

/// `writeFile`'s key: serial per file.
pub fn file_write_key(environment_id: &str, cwd: &str, relative_path: &str) -> String {
    json_key(&[environment_id, cwd, relative_path])
}

/// `writtenFilePaths`: the canonical path the server reported and the one the
/// caller asked for, resolved against the workspace, deduplicated in order.
/// Both are invalidated and refreshed, since either may be what a reader holds.
pub fn written_file_paths(
    cwd: &str,
    input_relative_path: &str,
    result_relative_path: &str,
) -> Vec<String> {
    [result_relative_path, input_relative_path]
        .into_iter()
        .map(|path| resolve_workspace_file_path(path, cwd))
        .collect::<IndexSet<_>>()
        .into_iter()
        .collect()
}

/// The Scratch project was created, but its event never reached this client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The folder for threads without a project has not reached this device yet. Try again.")]
pub struct ScratchProjectNotLoadedError {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenScratchError {
    /// `projectsEnsureScratch` failed; carries its message.
    #[error("{0}")]
    Ensure(String),
    #[error(transparent)]
    NotLoaded(#[from] ScratchProjectNotLoadedError),
}

/// `request(WS_METHODS.projectsEnsureScratch, ...)`.
#[async_trait]
pub trait EnsureScratchProject: Send + Sync {
    /// Finds or creates the environment's Scratch project and returns its id.
    async fn ensure_scratch(&self) -> Result<String, String>;
}

/// `options.projectAtom({ environmentId, projectId })`: the client store's
/// project, `None` until its event arrives.
pub trait ProjectArrivals<P>: Send + Sync {
    fn watch_project(&self, project_id: &str) -> watch::Receiver<Option<P>>;
}

/// `openScratch`'s wait: `Effect.timeoutOption("10 seconds")`.
pub const OPEN_SCRATCH_TIMEOUT: Duration = Duration::from_secs(10);

/// `openScratch`: finds or creates the environment's Scratch project and
/// resolves once the project is in the client store, since drafts key off its
/// stored path.
pub async fn open_scratch<P: Clone + Send + Sync>(
    ensure: &dyn EnsureScratchProject,
    store: &dyn ProjectArrivals<P>,
) -> Result<P, OpenScratchError> {
    open_scratch_within(ensure, store, OPEN_SCRATCH_TIMEOUT).await
}

pub async fn open_scratch_within<P: Clone + Send + Sync>(
    ensure: &dyn EnsureScratchProject,
    store: &dyn ProjectArrivals<P>,
    timeout: Duration,
) -> Result<P, OpenScratchError> {
    let project_id = ensure
        .ensure_scratch()
        .await
        .map_err(OpenScratchError::Ensure)?;
    let mut project = store.watch_project(&project_id);
    let arrived =
        tokio::time::timeout(timeout, project.wait_for(|project| project.is_some())).await;
    match arrived {
        Ok(Ok(project)) => Ok(project.clone().expect("waited for Some")),
        // Timed out, or the store dropped the project's feed.
        _ => Err(ScratchProjectNotLoadedError { project_id }.into()),
    }
}

#[cfg(test)]
#[path = "project_commands_tests.rs"]
mod tests;
