//! Port of upstream/packages/contracts/src/git.ts

use crate::base_schemas::{NonNegativeInt, PositiveInt, ProjectId, ThreadId, TrimmedNonEmptyString};
use crate::source_control::{SourceControlProviderError, SourceControlProviderInfo};
use crate::vcs::VcsDriverKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// Upstream bounds listRefs limit with this; enforced at the service boundary.
#[allow(dead_code)]
const GIT_LIST_BRANCHES_MAX_LIMIT: u64 = 200;

/// `GitFilePath`: a non-empty, *untrimmed* path string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitFilePath(pub String);

impl GitFilePath {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.is_empty() {
            Err("git file paths must be non-empty".to_owned())
        } else {
            Ok(Self(value))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for GitFilePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GitFilePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for GitFilePath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GitFilePath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        GitFilePath::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Serde adapter for `Option<Vec<GitFilePath>>` (upstream checks non-empty on
/// each element; the min-length-1 array check is noted in PORTING.md).
mod file_paths {
    use super::*;

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<GitFilePath>>, D::Error> {
        let values = Option::<Vec<GitFilePath>>::deserialize(d)?;
        if let Some(paths) = &values {
            if paths.is_empty() {
                return Err(serde::de::Error::custom("filePaths must not be empty"));
            }
        }
        Ok(values)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GitStackedAction {
    #[serde(rename = "commit")]
    Commit,
    #[serde(rename = "push")]
    Push,
    #[serde(rename = "create_pr")]
    CreatePr,
    #[serde(rename = "commit_push")]
    CommitPush,
    #[serde(rename = "commit_push_pr")]
    CommitPushPr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GitActionProgressPhase {
    #[serde(rename = "branch")]
    Branch,
    #[serde(rename = "commit")]
    Commit,
    #[serde(rename = "push")]
    Push,
    #[serde(rename = "pr")]
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitActionProgressKind {
    ActionStarted,
    PhaseStarted,
    HookStarted,
    HookOutput,
    HookFinished,
    ActionFinished,
    ActionFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitActionProgressStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitStepStatus {
    Created,
    SkippedNoChanges,
    SkippedNotRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPushStepStatus {
    Pushed,
    SkippedNotRequested,
    SkippedUpToDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitBranchStepStatus {
    Created,
    SkippedNotRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrStepStatus {
    Created,
    OpenedExisting,
    SkippedNotRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GitPullRequestState {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "closed")]
    Closed,
    #[serde(rename = "merged")]
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GitPreparePullRequestThreadMode {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "worktree")]
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunStackedActionToastRunAction {
    pub kind: GitStackedAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum GitRunStackedActionToastCta {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "open_pr")]
    OpenPr {
        label: TrimmedNonEmptyString,
        url: String,
    },
    #[serde(rename = "run_action")]
    RunAction {
        label: TrimmedNonEmptyString,
        action: GitRunStackedActionToastRunAction,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunStackedActionToast {
    pub title: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<TrimmedNonEmptyString>,
    pub cta: GitRunStackedActionToastCta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRef {
    pub name: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_remote: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<TrimmedNonEmptyString>,
    pub current: bool,
    pub is_default: bool,
    pub worktree_path: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsWorktree {
    pub path: TrimmedNonEmptyString,
    pub ref_name: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitResolvedPullRequest {
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub url: String,
    pub base_branch: TrimmedNonEmptyString,
    pub head_branch: TrimmedNonEmptyString,
    pub state: GitPullRequestState,
}

// RPC Inputs

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusInput {
    pub cwd: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusSubscriptionInput {
    pub cwd: TrimmedNonEmptyString,
    /// Passive observers receive cached remote status without retaining its refresh loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_remote: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsPullInput {
    pub cwd: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunStackedActionInput {
    pub action_id: TrimmedNonEmptyString,
    pub cwd: TrimmedNonEmptyString,
    pub action: GitStackedAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_branch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "file_paths::deserialize")]
    pub file_paths: Option<Vec<GitFilePath>>,
    /// The thread the action runs beside; a pull request it creates is linked to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VcsListRefsRefKind {
    All,
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsListRefsInput {
    pub cwd: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_matching_remote_refs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_kind: Option<VcsListRefsRefKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<PositiveInt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsCreateWorktreeInput {
    pub cwd: TrimmedNonEmptyString,
    pub ref_name: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_ref_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref_name: Option<TrimmedNonEmptyString>,
    pub path: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPullRequestRefInput {
    pub cwd: TrimmedNonEmptyString,
    pub reference: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPreparePullRequestThreadInput {
    pub cwd: TrimmedNonEmptyString,
    pub reference: TrimmedNonEmptyString,
    pub mode: GitPreparePullRequestThreadMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRemoveWorktreeInput {
    pub cwd: TrimmedNonEmptyString,
    pub path: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsCreateRefInput {
    pub cwd: TrimmedNonEmptyString,
    pub ref_name: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_ref: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsCreateRefResult {
    pub ref_name: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsSwitchRefInput {
    pub cwd: TrimmedNonEmptyString,
    pub ref_name: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsInitInput {
    pub cwd: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<VcsDriverKind>,
}

// RPC Results

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusChangeRequest {
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub url: String,
    pub base_ref: TrimmedNonEmptyString,
    pub head_ref: TrimmedNonEmptyString,
    pub state: GitPullRequestState,
    /// Optional for compatibility with older servers and providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    /// Last provider-side activity (ISO), including comments and metadata
    /// edits. This is not the time a change request closed or merged.
    /// Optional for old servers and providers whose lookups do not report it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub updated_at: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusWorkingTreeFile {
    pub path: GitFilePath,
    pub insertions: NonNegativeInt,
    pub deletions: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusWorkingTree {
    pub files: Vec<VcsStatusWorkingTreeFile>,
    pub insertions: NonNegativeInt,
    pub deletions: NonNegativeInt,
}

/// Totals for the diff panel's Changes view: merge-base with the base branch
/// to the working tree, untracked files included. Absent on older servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusBranchChanges {
    pub base_ref: Option<TrimmedNonEmptyString>,
    pub insertions: NonNegativeInt,
    pub deletions: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusLocalResult {
    pub is_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_provider: Option<SourceControlProviderInfo>,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    pub ref_name: Option<TrimmedNonEmptyString>,
    pub has_working_tree_changes: bool,
    pub working_tree: VcsStatusWorkingTree,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_changes: Option<VcsStatusBranchChanges>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusRemoteResult {
    pub has_upstream: bool,
    pub ahead_count: NonNegativeInt,
    pub behind_count: NonNegativeInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead_of_default_count: Option<NonNegativeInt>,
    pub pr: Option<VcsStatusChangeRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusResult {
    pub is_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_provider: Option<SourceControlProviderInfo>,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    pub ref_name: Option<TrimmedNonEmptyString>,
    pub has_working_tree_changes: bool,
    pub working_tree: VcsStatusWorkingTree,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_changes: Option<VcsStatusBranchChanges>,
    pub has_upstream: bool,
    pub ahead_count: NonNegativeInt,
    pub behind_count: NonNegativeInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead_of_default_count: Option<NonNegativeInt>,
    pub pr: Option<VcsStatusChangeRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VcsStatusStreamEvent {
    #[serde(rename = "snapshot")]
    Snapshot {
        local: VcsStatusLocalResult,
        remote: Option<VcsStatusRemoteResult>,
    },
    #[serde(rename = "localUpdated")]
    LocalUpdated { local: VcsStatusLocalResult },
    #[serde(rename = "remoteUpdated")]
    RemoteUpdated { remote: Option<VcsStatusRemoteResult> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsListRefsResult {
    pub refs: Vec<VcsRef>,
    pub is_repo: bool,
    pub has_primary_remote: bool,
    pub next_cursor: Option<NonNegativeInt>,
    pub total_count: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsCreateWorktreeResult {
    pub worktree: VcsWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitResolvePullRequestResult {
    pub pull_request: GitResolvedPullRequest,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPreparePullRequestThreadResult {
    pub pull_request: GitResolvedPullRequest,
    pub branch: TrimmedNonEmptyString,
    pub worktree_path: Option<TrimmedNonEmptyString>,
    /// False when the checkout could not be brought to the pull request head
    /// — a reused worktree holding local commits or uncommitted changes keeps
    /// its own state, so the code being handed over is older than the pull
    /// request. Legacy responses default to true.
    #[serde(default = "default_true")]
    pub is_on_pull_request_head: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsSwitchRefResult {
    pub ref_name: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStackedBranchStep {
    pub status: GitBranchStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStackedCommitStep {
    pub status: GitCommitStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStackedPushStep {
    pub status: GitPushStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_branch: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_upstream: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStackedPrStep {
    pub status: GitPrStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunStackedActionResult {
    pub action: GitStackedAction,
    pub branch: GitStackedBranchStep,
    pub commit: GitStackedCommitStep,
    pub push: GitStackedPushStep,
    pub pr: GitStackedPrStep,
    pub toast: GitRunStackedActionToast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VcsPullStatus {
    #[serde(rename = "pulled")]
    Pulled,
    #[serde(rename = "skipped_up_to_date")]
    SkippedUpToDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsPullResult {
    pub status: VcsPullStatus,
    pub ref_name: TrimmedNonEmptyString,
    pub upstream_ref: Option<TrimmedNonEmptyString>,
}

// RPC / domain errors

// Well-known git failures, recognized from stderr at the driver and carried as
// a closed set of diagnostic tags. Git's stderr itself stays off the error: it
// echoes argv and remote URLs, which can hold credentials. The tag names the
// cause for logs and callers; it does not select a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCommandFailureReason {
    AuthenticationFailed,
    BranchAlreadyExists,
    BranchCheckedOutInWorktree,
    HostKeyUnverified,
    NotARepository,
    PathAlreadyExists,
    RemoteUnreachable,
    TagWouldBeClobbered,
}

/// `GitCommandError` (Schema.TaggedError). Display mirrors upstream's message
/// getter: `Git command failed in {operation} ({cwd}): {detail} (reason)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GitCommandError {
    #[serde(rename = "GitCommandError")]
    GitCommandError {
        operation: String,
        command: String,
        cwd: String,
        // `Schema.Number` upstream: floats, not ints.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_length: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_length: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_length: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<GitCommandFailureReason>,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

impl std::fmt::Display for GitCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitCommandError { operation, cwd, detail, reason, .. } => write!(
                f,
                "Git command failed in {operation} ({cwd}): {detail}"
            )
            .and_then(|_| match reason {
                Some(reason) => write!(f, " ({reason})"),
                None => Ok(()),
            }),
        }
    }
}

impl std::error::Error for GitCommandError {}

/// `TextGenerationError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TextGenerationError {
    #[serde(rename = "TextGenerationError")]
    #[error("Text generation failed in {operation}: {detail}")]
    TextGenerationError {
        operation: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

/// `GitManagerError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GitManagerError {
    #[serde(rename = "GitManagerError")]
    #[error("Git manager failed in {operation}: {detail}")]
    GitManagerError {
        operation: String,
        cwd: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

/// `GitPullRequestMaterializationError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GitPullRequestMaterializationError {
    #[serde(rename = "GitPullRequestMaterializationError")]
    #[error("Failed to materialize pull request #{pull_request_number} branch {head_branch} as {local_branch}.")]
    GitPullRequestMaterializationError {
        cwd: TrimmedNonEmptyString,
        pull_request_number: PositiveInt,
        head_repository: Option<TrimmedNonEmptyString>,
        head_branch: TrimmedNonEmptyString,
        local_branch: TrimmedNonEmptyString,
        cause: serde_json::Value,
    },
}

/// `GitManagerServiceError`: union of the git manager's tagged errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(untagged)]
pub enum GitManagerServiceError {
    #[error(transparent)]
    Manager(GitManagerError),
    #[error(transparent)]
    Materialization(GitPullRequestMaterializationError),
    #[error(transparent)]
    Command(GitCommandError),
    #[error(transparent)]
    SourceControlProvider(SourceControlProviderError),
    #[error(transparent)]
    TextGeneration(TextGenerationError),
}

/// `GitActionProgressEvent`: union on `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum GitActionProgressEvent {
    #[serde(rename = "action_started")]
    ActionStarted {
        action_id: TrimmedNonEmptyString,
        cwd: TrimmedNonEmptyString,
        action: GitStackedAction,
        phases: Vec<GitActionProgressPhase>,
    },
    #[serde(rename = "phase_started")]
    PhaseStarted {
        action_id: TrimmedNonEmptyString,
        cwd: TrimmedNonEmptyString,
        action: GitStackedAction,
        phase: GitActionProgressPhase,
        label: TrimmedNonEmptyString,
    },
    #[serde(rename = "hook_started")]
    HookStarted {
        action_id: TrimmedNonEmptyString,
        cwd: TrimmedNonEmptyString,
        action: GitStackedAction,
        hook_name: TrimmedNonEmptyString,
    },
    #[serde(rename = "hook_output")]
    HookOutput {
        action_id: TrimmedNonEmptyString,
        cwd: TrimmedNonEmptyString,
        action: GitStackedAction,
        hook_name: Option<TrimmedNonEmptyString>,
        stream: GitActionProgressStream,
        text: TrimmedNonEmptyString,
    },
    #[serde(rename = "hook_finished")]
    HookFinished {
        action_id: TrimmedNonEmptyString,
        cwd: TrimmedNonEmptyString,
        action: GitStackedAction,
        hook_name: TrimmedNonEmptyString,
        exit_code: Option<i64>,
        duration_ms: Option<NonNegativeInt>,
    },
    #[serde(rename = "action_finished")]
    ActionFinished {
        action_id: TrimmedNonEmptyString,
        cwd: TrimmedNonEmptyString,
        action: GitStackedAction,
        result: GitRunStackedActionResult,
    },
    #[serde(rename = "action_failed")]
    ActionFailed {
        action_id: TrimmedNonEmptyString,
        cwd: TrimmedNonEmptyString,
        action: GitStackedAction,
        phase: Option<GitActionProgressPhase>,
        message: TrimmedNonEmptyString,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_worktree_input_accepts_omitted_new_ref_name() {
        let parsed: VcsCreateWorktreeInput = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "refName": "feature/existing",
            "path": "/tmp/worktree",
        }))
        .unwrap();
        assert_eq!(parsed.new_ref_name, None);
        assert_eq!(parsed.ref_name.as_str(), "feature/existing");

        let with_base: VcsCreateWorktreeInput = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "refName": "0123456789abcdef",
            "newRefName": "feature/new",
            "baseRefName": "origin/main",
            "path": "/tmp/worktree",
        }))
        .unwrap();
        assert_eq!(with_base.base_ref_name.as_ref().unwrap().as_str(), "origin/main");
    }

    #[test]
    fn prepare_pull_request_input_accepts_references() {
        let parsed: GitPreparePullRequestThreadInput = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "reference": "#42",
            "mode": "worktree",
        }))
        .unwrap();
        assert_eq!(parsed.reference.as_str(), "#42");
        assert_eq!(parsed.mode, GitPreparePullRequestThreadMode::Worktree);
    }

    fn resolved_pr() -> serde_json::Value {
        serde_json::json!({
            "number": 42,
            "title": "PR threads",
            "url": "https://github.com/pingdotgg/codething-mvp/pull/42",
            "baseBranch": "main",
            "headBranch": "feature/pr-threads",
            "state": "open",
        })
    }

    #[test]
    fn prepare_result_defaults_legacy_responses_to_head() {
        let parsed: GitPreparePullRequestThreadResult = serde_json::from_value(serde_json::json!({
            "pullRequest": resolved_pr(),
            "branch": "feature/pr-threads",
            "worktreePath": "/tmp/pr-threads",
        }))
        .unwrap();
        assert!(parsed.is_on_pull_request_head);

        let explicit: GitPreparePullRequestThreadResult = serde_json::from_value(serde_json::json!({
            "pullRequest": resolved_pr(),
            "branch": "feature/pr-threads",
            "worktreePath": "/tmp/pr-threads",
            "isOnPullRequestHead": false,
        }))
        .unwrap();
        assert!(!explicit.is_on_pull_request_head);
    }

    #[test]
    fn resolve_pull_request_result_decodes() {
        let parsed: GitResolvePullRequestResult =
            serde_json::from_value(serde_json::json!({ "pullRequest": resolved_pr() })).unwrap();
        assert_eq!(parsed.pull_request.number.get(), 42);
        assert_eq!(parsed.pull_request.head_branch.as_str(), "feature/pr-threads");
    }

    #[test]
    fn run_stacked_action_input_paths() {
        // Leading and trailing whitespace in file paths is preserved.
        let parsed: GitRunStackedActionInput = serde_json::from_value(serde_json::json!({
            "actionId": "action-1",
            "cwd": "/repo",
            "action": "commit",
            "filePaths": [" leading.txt", "trailing.txt "],
        }))
        .unwrap();
        assert_eq!(parsed.file_paths.as_ref().unwrap()[0].as_str(), " leading.txt");
        assert_eq!(parsed.file_paths.as_ref().unwrap()[1].as_str(), "trailing.txt ");

        assert!(serde_json::from_value::<GitRunStackedActionInput>(serde_json::json!({
            "actionId": "action-1",
            "cwd": "/repo",
            "action": "commit",
            "filePaths": [""],
        }))
        .is_err());
    }

    #[test]
    fn status_local_preserves_path_whitespace() {
        let parsed: VcsStatusLocalResult = serde_json::from_value(serde_json::json!({
            "isRepo": true,
            "hasPrimaryRemote": true,
            "isDefaultRef": false,
            "refName": "main",
            "hasWorkingTreeChanges": true,
            "workingTree": {
                "files": [
                    { "path": " leading.txt", "insertions": 1, "deletions": 0 },
                    { "path": "trailing.txt ", "insertions": 0, "deletions": 1 },
                ],
                "insertions": 1,
                "deletions": 1,
            },
        }))
        .unwrap();
        assert_eq!(parsed.working_tree.files[0].path.as_str(), " leading.txt");
        assert_eq!(parsed.working_tree.files[1].path.as_str(), "trailing.txt ");

        assert!(serde_json::from_value::<VcsStatusLocalResult>(serde_json::json!({
            "isRepo": true,
            "hasPrimaryRemote": true,
            "isDefaultRef": false,
            "refName": "main",
            "hasWorkingTreeChanges": true,
            "workingTree": {
                "files": [{ "path": "", "insertions": 0, "deletions": 0 }],
                "insertions": 0,
                "deletions": 0,
            },
        }))
        .is_err());
    }

    #[test]
    fn run_stacked_action_result_decodes_server_toast() {
        let parsed: GitRunStackedActionResult = serde_json::from_value(serde_json::json!({
            "action": "commit_push",
            "branch": { "status": "created", "name": "feature/server-owned-toast" },
            "commit": {
                "status": "created",
                "commitSha": "89abcdef01234567",
                "subject": "feat: move toast state into git manager",
            },
            "push": {
                "status": "pushed",
                "branch": "feature/server-owned-toast",
                "upstreamBranch": "origin/feature/server-owned-toast",
            },
            "pr": { "status": "skipped_not_requested" },
            "toast": {
                "title": "Pushed 89abcde to origin/feature/server-owned-toast",
                "description": "feat: move toast state into git manager",
                "cta": {
                    "kind": "run_action",
                    "label": "Create PR",
                    "action": { "kind": "create_pr" },
                },
            },
        }))
        .unwrap();
        assert!(matches!(
            parsed.toast.cta,
            GitRunStackedActionToastCta::RunAction { ref action, .. } if action.kind == GitStackedAction::CreatePr
        ));
    }
}

impl std::fmt::Display for GitCommandFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AuthenticationFailed => "authentication_failed",
            Self::BranchAlreadyExists => "branch_already_exists",
            Self::BranchCheckedOutInWorktree => "branch_checked_out_in_worktree",
            Self::HostKeyUnverified => "host_key_unverified",
            Self::NotARepository => "not_a_repository",
            Self::PathAlreadyExists => "path_already_exists",
            Self::RemoteUnreachable => "remote_unreachable",
            Self::TagWouldBeClobbered => "tag_would_be_clobbered",
        })
    }
}
