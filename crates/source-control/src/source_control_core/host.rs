//! Port of upstream/packages/source-control-core/src/server/SourceControlHost.ts
//!
//! What a source control provider may ask of the app it runs in. The app provides one
//! implementation (upstream `ServerSourceControlHost.layer`, which wires `ServerSettings`,
//! `VcsProcess`, `GitVcsDriver` and `VcsDriverRegistry`); everything a provider needs from its
//! environment (settings, the process runner, git) goes through it.
//!
//! Deviation: `git.remotes` upstream is `Effect<Effect<VcsListRemotesResult>>` (outer resolves the
//! driver, inner lists); in Rust it is one async call that does both.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::git::{GitCommandError, VcsSwitchRefInput, VcsSwitchRefResult};
use turin_contracts::settings::{ServerSettings, ServerSettingsError};
use turin_contracts::vcs::{VcsError, VcsListRemotesResult};

/// What happens past `max_output_bytes`: fail the run, or keep the first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Error,
    Truncate,
}

/// Called with each stdout chunk as it arrives.
pub type StdoutChunkHandler = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// One CLI invocation. The app bounds concurrency, output, and time for every run.
#[derive(Clone, Default)]
pub struct SourceControlProcessInput {
    pub operation: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub spawn_cwd: Option<String>,
    pub stdin: Option<String>,
    pub on_stdout_chunk: Option<StdoutChunkHandler>,
    /// Variables layered over the inherited environment; an empty value is passed as empty.
    pub env: Option<HashMap<String, String>>,
    pub allow_non_zero_exit: bool,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub output_mode: Option<OutputMode>,
    pub append_truncation_marker: bool,
}

impl std::fmt::Debug for SourceControlProcessInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceControlProcessInput")
            .field("operation", &self.operation)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceControlProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// Present on real process output; optional so narrow test doubles remain lightweight.
    pub stdout_invalid_utf8: Option<bool>,
    pub stderr_invalid_utf8: Option<bool>,
}

/// `Omit<SourceControlProcessOutput, "stdoutInvalidUtf8" | "stderrInvalidUtf8">`.
#[derive(Debug, Clone, Default)]
pub struct SourceControlGitOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone)]
pub struct SourceControlGitExecuteInput {
    pub operation: String,
    pub cwd: String,
    pub args: Vec<String>,
    pub max_output_bytes: Option<usize>,
    pub append_truncation_marker: bool,
}

#[derive(Debug, Clone)]
pub struct SourceControlGitRemoteBranch {
    pub cwd: String,
    pub remote_name: String,
    pub remote_branch: String,
}

#[async_trait]
pub trait SourceControlHost: Send + Sync {
    /// Read fresh on each call, so a credential saved in Settings applies without a restart.
    async fn settings_get(&self) -> Result<ServerSettings, ServerSettingsError>;

    async fn process_run(
        &self,
        input: SourceControlProcessInput,
    ) -> Result<SourceControlProcessOutput, VcsError>;

    /// Runs `git -C cwd …args` through the app's git driver, with its limits and metrics.
    async fn git_execute(
        &self,
        input: SourceControlGitExecuteInput,
    ) -> Result<SourceControlGitOutput, GitCommandError>;

    /// Resolves `revision` to a commit sha.
    async fn git_resolve_commit(
        &self,
        cwd: &str,
        revision: &str,
    ) -> Result<String, GitCommandError>;

    /// Lists the remotes of the repository at `cwd`; fails when the app cannot drive one there.
    async fn git_remotes(&self, cwd: &str) -> Result<VcsListRemotesResult, VcsError>;

    async fn git_read_config_value(
        &self,
        cwd: &str,
        key: &str,
    ) -> Result<Option<String>, GitCommandError>;

    async fn git_resolve_primary_remote_name(&self, cwd: &str) -> Result<String, GitCommandError>;

    /// Adds a remote for `url` unless one exists, and returns the name it is under.
    async fn git_ensure_remote(
        &self,
        cwd: &str,
        preferred_name: &str,
        url: &str,
    ) -> Result<String, GitCommandError>;

    async fn git_list_local_branch_names(&self, cwd: &str) -> Result<Vec<String>, GitCommandError>;

    async fn git_fetch_remote_branch(
        &self,
        input: &SourceControlGitRemoteBranch,
        local_branch: &str,
    ) -> Result<(), GitCommandError>;

    async fn git_fetch_remote_tracking_branch(
        &self,
        input: &SourceControlGitRemoteBranch,
    ) -> Result<(), GitCommandError>;

    async fn git_set_branch_upstream(
        &self,
        input: &SourceControlGitRemoteBranch,
        branch: &str,
    ) -> Result<(), GitCommandError>;

    async fn git_switch_ref(
        &self,
        input: &VcsSwitchRefInput,
    ) -> Result<VcsSwitchRefResult, GitCommandError>;
}

/// Whether a failed spawn means the executable is not installed (upstream: a `VcsProcessSpawnError`
/// whose cause is a `PlatformError` with reason `NotFound`). The git/process layer reports a
/// missing executable as a spawn error whose `cause` carries `{"reason":{"_tag":"NotFound"}}` or an
/// OS "No such file or directory" / "not found" message; both are recognised here.
pub fn is_command_not_found(error: &VcsError) -> bool {
    let VcsError::Spawn(spawn) = error else {
        return false;
    };
    let turin_contracts::vcs::VcsProcessSpawnError::VcsProcessSpawnError { cause, .. } = spawn;
    if cause.pointer("/reason/_tag").and_then(|v| v.as_str()) == Some("NotFound") {
        return true;
    }
    let text = match cause {
        serde_json::Value::String(s) => s.to_lowercase(),
        other => other.to_string().to_lowercase(),
    };
    text.contains("no such file or directory")
        || text.contains("os error 2")
        || text.contains("notfound")
        || text.contains("not found")
}
