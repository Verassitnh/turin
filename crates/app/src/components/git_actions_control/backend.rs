//! The git control's data seam: everything upstream's `GitActionsControl`
//! reads from atoms and stores (`vcsEnvironment.status`, `vcsActionManager`,
//! `useGitStackedAction`, `useVcsPullAction`, `useVcsInitAction`,
//! `useSourceControlPublishRepositoryAction`, `sourceControlEnvironment.discovery`,
//! `threadEnvironment.updateMetadata`, `useOpenInPreferredEditor`) gathered
//! behind [`GitActionsBackend`].
//!
//! T22 implements it on `turin-git` / `turin-source-control` / the
//! orchestrator; [`super::mock_source`] provides an in-memory one for now.
//! The environment write-scope gates (`AuthSourceControlWriteScope`,
//! `AuthOrchestrationOperateScope`) went with the auth scopes: one process has
//! no sessions, so every action is permitted.

use std::rc::Rc;

use gpui_kit::{App, Task};
use turin_contracts::git::{
    GitRunStackedActionResult, GitStackedAction, VcsPullResult, VcsStatusResult,
};
use turin_contracts::source_control::{
    SourceControlCloneProtocol, SourceControlPublishRepositoryResult,
    SourceControlRepositoryVisibility,
};
use turin_contracts::{EnvironmentId, ThreadId};

/// Where an action runs: the thread (if it has started) and its checkout.
/// Captured when the action is requested, so a retained toast's action keeps
/// running where it was started even after the user navigates away
/// (GitActionsControl.lifecycle.test.ts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitActionTarget {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub cwd: String,
}

/// `vcsActionManager.stateAtom(scope)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VcsActionState {
    pub is_running: bool,
    pub operation: Option<String>,
    pub current_label: Option<String>,
    pub last_output_line: Option<String>,
    pub phase_started_at_ms: Option<i64>,
    pub hook_started_at_ms: Option<i64>,
}

/// `useEnvironmentQuery(vcsEnvironment.status(...))`.
#[derive(Clone, Debug, Default)]
pub struct GitStatusView {
    pub status: Option<VcsStatusResult>,
    pub error: Option<String>,
}

/// The thread a control targets, as the shell/draft stores describe it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitThreadContext {
    /// A server thread exists (the draft has launched).
    pub is_server_thread: bool,
    pub project_id: Option<String>,
    /// The thread's stored branch.
    pub branch: Option<String>,
}

/// `runImmediateGitAction.run` input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStackedActionInput {
    pub action_id: String,
    pub action: GitStackedAction,
    pub commit_message: Option<String>,
    pub feature_branch: bool,
    pub file_paths: Option<Vec<String>>,
    /// A pull request the action opens is linked to this thread.
    pub thread_id: Option<ThreadId>,
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishRepositoryInput {
    pub repository: String,
    pub visibility: SourceControlRepositoryVisibility,
    pub remote_name: String,
    pub protocol: SourceControlCloneProtocol,
}

/// `getPublishProviderReadiness` plus the account/host the dialog shows. Turin
/// ships GitHub only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishProviderState {
    pub label: String,
    pub ready: bool,
    pub hint: Option<String>,
    pub account: Option<String>,
    pub host: String,
}

pub trait GitActionsBackend {
    fn status(&self, cwd: &str, cx: &App) -> GitStatusView;
    fn action_state(&self, cwd: &str, cx: &App) -> VcsActionState;
    /// `vcsEnvironment.refreshStatus` (window focus, opening the menu).
    fn refresh_status(&self, cwd: &str, cx: &mut App);
    fn run_stacked_action(
        &self,
        target: &GitActionTarget,
        input: RunStackedActionInput,
        cx: &mut App,
    ) -> Task<Result<GitRunStackedActionResult, String>>;
    fn pull(&self, target: &GitActionTarget, cx: &mut App) -> Task<Result<VcsPullResult, String>>;
    fn init(&self, target: &GitActionTarget, cx: &mut App) -> Task<Result<(), String>>;
    fn publish_provider(&self, cx: &App) -> PublishProviderState;
    fn publish_repository(
        &self,
        target: &GitActionTarget,
        input: PublishRepositoryInput,
        cx: &mut App,
    ) -> Task<Result<SourceControlPublishRepositoryResult, String>>;
    /// `threadEnvironment.updateMetadata` with the branch patch
    /// (`resolveThreadBranchMetadataPatch`).
    fn update_thread_branch(
        &self,
        target: &GitActionTarget,
        branch: Option<String>,
        expected_branch: Option<String>,
        cx: &mut App,
    ) -> Task<Result<(), String>>;
    /// `useOpenInPreferredEditor` on a changed file.
    fn open_in_editor(&self, path: String, cwd: &str, cx: &mut App) -> Task<Result<(), String>>;
    /// `useOpenLink` / `useOpenPrLink`.
    fn open_link(&self, url: String, cx: &mut App);
}

pub type SharedGitActionsBackend = Rc<dyn GitActionsBackend>;
