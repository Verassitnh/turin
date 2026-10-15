//! Port of upstream/apps/server/src/vcs/VcsDriver.ts

use async_trait::async_trait;
use turin_contracts::base_schemas::CheckpointRef;
use turin_contracts::git::VcsInitInput;
use turin_contracts::vcs::{
    VcsDriverCapabilities, VcsError, VcsListRemotesResult, VcsListWorkspaceFilesResult,
    VcsRepositoryIdentity,
};

use crate::vcs_process::VcsProcessOutput;

/// `VcsError` is the contract union. Callers match the variant, so it stays unboxed.
#[allow(clippy::result_large_err)]
#[async_trait]
pub trait VcsDriver: Send + Sync {
    fn capabilities(&self) -> &VcsDriverCapabilities;
    async fn execute(&self, input: VcsExecuteInput) -> Result<VcsProcessOutput, VcsError>;
    async fn detect_repository(&self, cwd: &str)
    -> Result<Option<VcsRepositoryIdentity>, VcsError>;
    async fn is_inside_work_tree(&self, cwd: &str) -> Result<bool, VcsError>;
    async fn list_workspace_files(
        &self,
        cwd: &str,
    ) -> Result<VcsListWorkspaceFilesResult, VcsError>;
    async fn list_remotes(&self, cwd: &str) -> Result<VcsListRemotesResult, VcsError>;
    async fn filter_ignored_paths(
        &self,
        cwd: &str,
        relative_paths: &[String],
    ) -> Result<Vec<String>, VcsError>;
    async fn init_repository(&self, input: &VcsInitInput) -> Result<(), VcsError>;
    fn supports_checkpoints(&self) -> bool;
    async fn capture_checkpoint(&self, input: &VcsCaptureCheckpointInput) -> Result<(), VcsError>;
    async fn has_checkpoint_ref(&self, input: &VcsHasCheckpointInput) -> Result<bool, VcsError>;
    async fn restore_checkpoint(&self, input: &VcsRestoreCheckpointInput)
    -> Result<bool, VcsError>;
    async fn diff_checkpoints(&self, input: &VcsDiffCheckpointsInput) -> Result<String, VcsError>;
    async fn list_authored_paths(
        &self,
        input: &VcsListAuthoredPathsInput,
    ) -> Result<Option<std::collections::HashSet<String>>, VcsError>;
    async fn delete_checkpoint_refs(
        &self,
        input: &VcsDeleteCheckpointRefsInput,
    ) -> Result<(), VcsError>;
}

pub struct VcsExecuteInput {
    pub operation: String,
    pub cwd: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
    pub clear_env: Vec<String>,
    pub allow_non_zero_exit: bool,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub output_mode: Option<crate::process::OutputMode>,
    pub append_truncation_marker: bool,
    pub on_stdout_chunk: Option<crate::process::StdoutChunkCallback>,
}

impl VcsExecuteInput {
    pub fn new(operation: impl Into<String>, cwd: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            operation: operation.into(),
            cwd: cwd.into(),
            args,
            stdin: None,
            env: None,
            clear_env: Vec::new(),
            allow_non_zero_exit: false,
            timeout_ms: None,
            max_output_bytes: None,
            output_mode: None,
            append_truncation_marker: false,
            on_stdout_chunk: None,
        }
    }
}

pub struct VcsCaptureCheckpointInput {
    pub cwd: String,
    pub checkpoint_ref: CheckpointRef,
}

pub struct VcsHasCheckpointInput {
    pub cwd: String,
    pub checkpoint_ref: CheckpointRef,
}

pub struct VcsRestoreCheckpointInput {
    pub cwd: String,
    pub checkpoint_ref: CheckpointRef,
    pub fallback_to_head: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointDiffFormat {
    Patch,
    Numstat,
}

pub struct VcsDiffCheckpointsInput {
    pub cwd: String,
    pub from_checkpoint_ref: CheckpointRef,
    pub to_checkpoint_ref: CheckpointRef,
    pub fallback_from_to_head: bool,
    pub ignore_whitespace: bool,
    pub format: CheckpointDiffFormat,
    /// Limits the diff to these exact paths. An empty list yields an empty diff.
    pub file_paths: Option<Vec<String>>,
}

pub struct VcsListAuthoredPathsInput {
    pub cwd: String,
    pub from_checkpoint_ref: CheckpointRef,
    pub to_checkpoint_ref: CheckpointRef,
}

pub struct VcsDeleteCheckpointRefsInput {
    pub cwd: String,
    pub checkpoint_refs: Vec<CheckpointRef>,
}
