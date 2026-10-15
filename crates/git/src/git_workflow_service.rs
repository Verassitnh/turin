//! Port of upstream/apps/server/src/git/GitWorkflowService.ts
//!
//! Routes every call through [`VcsDriverRegistry`]. A directory that is not a
//! repository gets an explicit empty status or ref list. Anything detected as
//! a non-git driver is an error: git commands use `GitCommandError` with
//! `command: "vcs-route"`, and manager calls use `GitManagerError`.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::git::{
    GitCommandError, GitManagerError, GitManagerServiceError, GitPreparePullRequestThreadInput,
    GitPreparePullRequestThreadResult, GitPullRequestRefInput, GitResolvePullRequestResult,
    GitRunStackedActionInput, GitRunStackedActionResult, VcsCreateRefInput, VcsCreateRefResult,
    VcsCreateWorktreeInput, VcsCreateWorktreeResult, VcsListRefsInput, VcsListRefsResult,
    VcsPullResult, VcsRemoveWorktreeInput, VcsStatusInput, VcsStatusLocalResult,
    VcsStatusRemoteResult, VcsStatusResult, VcsStatusWorkingTree, VcsSwitchRefInput,
    VcsSwitchRefResult,
};
use turin_contracts::vcs::{VcsDriverKind, VcsError};

use crate::git_manager::{
    GitManager, GitManagerRemoteStatusOptions, GitRunStackedActionOptions,
};
use crate::git_vcs_driver_core::{
    CreateWorktreeOptions, ExecuteGitInput, GitDeleteLocalBranchInput, GitFetchRemoteInput,
    GitRemoteBranchExistsInput, GitRemoteExistsInput, GitRenameBranchInput, GitRenameBranchResult,
    GitResolveRemoteTrackingCommitInput, GitResolveRemoteTrackingCommitResult, GitVcsDriverCore,
};
use crate::vcs_driver_registry::{VcsDriverRegistry, VcsDriverResolveInput};

pub struct GitWorkflowService {
    registry: Arc<VcsDriverRegistry>,
    git: Arc<GitVcsDriverCore>,
    manager: Arc<GitManager>,
}

impl GitWorkflowService {
    pub fn new(
        registry: Arc<VcsDriverRegistry>,
        git: Arc<GitVcsDriverCore>,
        manager: Arc<GitManager>,
    ) -> Self {
        Self {
            registry,
            git,
            manager,
        }
    }

    pub async fn is_repository(&self, cwd: &str) -> Result<bool, GitManagerServiceError> {
        Ok(self.detect_manager("GitWorkflowService.isRepository", cwd).await? == Some(true))
    }

    pub async fn has_commit(&self, cwd: &str, ref_name: &str) -> Result<bool, GitCommandError> {
        self.ensure_command("GitWorkflowService.hasCommit", cwd).await?;
        let mut input = ExecuteGitInput::new(
            "GitWorkflowService.hasCommit",
            cwd,
            vec![
                "rev-parse".into(),
                "--verify".into(),
                format!("{ref_name}^{{commit}}"),
            ],
        );
        input.allow_non_zero_exit = true;
        let result = self.git.execute(input).await?;
        Ok(result.exit_code == Some(0))
    }

    /// A ref in any namespace whose last component is this name.
    /// A name containing `*?[\` is not a ref name and matches nothing.
    pub async fn has_ref_named(&self, cwd: &str, ref_name: &str) -> Result<bool, GitCommandError> {
        if ref_name.contains(['*', '?', '[', '\\']) {
            return Ok(false);
        }
        self.ensure_command("GitWorkflowService.hasRefNamed", cwd).await?;
        let result = self
            .git
            .execute(ExecuteGitInput::new(
                "GitWorkflowService.hasRefNamed",
                cwd,
                vec![
                    "for-each-ref".into(),
                    "--count=1".into(),
                    "--format=%(refname)".into(),
                    format!("refs/**/{ref_name}"),
                ],
            ))
            .await?;
        Ok(!result.stdout.trim().is_empty())
    }

    pub async fn status(
        &self,
        input: &VcsStatusInput,
    ) -> Result<VcsStatusResult, GitManagerServiceError> {
        if self
            .detect_manager("GitWorkflowService.status", input.cwd.as_str())
            .await?
            != Some(true)
        {
            return Ok(non_repository_status());
        }
        self.manager.status(input).await
    }

    pub async fn local_status(
        &self,
        input: &VcsStatusInput,
    ) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
        if self
            .detect_manager("GitWorkflowService.localStatus", input.cwd.as_str())
            .await?
            != Some(true)
        {
            return Ok(non_repository_local());
        }
        self.manager.local_status(input).await
    }

    pub async fn remote_status(
        &self,
        input: &VcsStatusInput,
        options: GitManagerRemoteStatusOptions,
    ) -> Result<Option<VcsStatusRemoteResult>, GitManagerServiceError> {
        if self
            .detect_manager("GitWorkflowService.remoteStatus", input.cwd.as_str())
            .await?
            != Some(true)
        {
            return Ok(None);
        }
        self.manager.remote_status(input, options).await.map(Some)
    }

    pub fn invalidate_local_status(&self, cwd: &str) {
        self.manager.invalidate_local_status(cwd);
    }

    pub fn invalidate_remote_status(&self, cwd: &str) {
        self.manager.invalidate_remote_status(cwd);
    }

    pub fn invalidate_status(&self, cwd: &str) {
        self.manager.invalidate_status(cwd);
    }

    pub async fn pull_current_branch(&self, cwd: &str) -> Result<VcsPullResult, GitCommandError> {
        self.ensure_command("GitWorkflowService.pullCurrentBranch", cwd)
            .await?;
        self.git.pull_current_branch(cwd).await
    }

    pub async fn run_stacked_action(
        &self,
        input: &GitRunStackedActionInput,
        options: GitRunStackedActionOptions,
    ) -> Result<GitRunStackedActionResult, GitManagerServiceError> {
        self.ensure_manager("GitWorkflowService.runStackedAction", input.cwd.as_str())
            .await?;
        self.manager.run_stacked_action(input, options).await
    }

    pub async fn resolve_pull_request(
        &self,
        input: &GitPullRequestRefInput,
    ) -> Result<GitResolvePullRequestResult, GitManagerServiceError> {
        self.ensure_manager("GitWorkflowService.resolvePullRequest", input.cwd.as_str())
            .await?;
        self.manager.resolve_pull_request(input).await
    }

    pub async fn prepare_pull_request_thread(
        &self,
        input: &GitPreparePullRequestThreadInput,
    ) -> Result<GitPreparePullRequestThreadResult, GitManagerServiceError> {
        self.ensure_manager(
            "GitWorkflowService.preparePullRequestThread",
            input.cwd.as_str(),
        )
        .await?;
        self.manager.prepare_pull_request_thread(input).await
    }

    pub async fn list_refs(
        &self,
        input: &VcsListRefsInput,
    ) -> Result<VcsListRefsResult, GitCommandError> {
        if self
            .detect_command("GitWorkflowService.listRefs", input.cwd.as_str())
            .await?
            != Some(true)
        {
            return Ok(VcsListRefsResult {
                refs: Vec::new(),
                is_repo: false,
                has_primary_remote: false,
                next_cursor: None,
                total_count: 0,
            });
        }
        self.git.list_refs(input).await
    }

    pub async fn create_worktree(
        &self,
        input: &VcsCreateWorktreeInput,
        options: CreateWorktreeOptions,
    ) -> Result<VcsCreateWorktreeResult, GitCommandError> {
        self.ensure_command("GitWorkflowService.createWorktree", input.cwd.as_str())
            .await?;
        self.manager.create_worktree(input, options).await
    }

    pub async fn list_local_branch_names(&self, cwd: &str) -> Result<Vec<String>, GitCommandError> {
        self.ensure_command("GitWorkflowService.listLocalBranchNames", cwd)
            .await?;
        self.git.list_local_branch_names(cwd).await
    }

    pub async fn fetch_remote(&self, input: &GitFetchRemoteInput) -> Result<(), GitCommandError> {
        self.ensure_command("GitWorkflowService.fetchRemote", &input.cwd)
            .await?;
        self.git.fetch_remote(input).await
    }

    pub async fn remote_exists(&self, input: &GitRemoteExistsInput) -> Result<bool, GitCommandError> {
        self.ensure_command("GitWorkflowService.remoteExists", &input.cwd)
            .await?;
        self.git.remote_exists(input).await
    }

    pub async fn remote_branch_exists(
        &self,
        input: &GitRemoteBranchExistsInput,
    ) -> Result<bool, GitCommandError> {
        self.ensure_command("GitWorkflowService.remoteBranchExists", &input.cwd)
            .await?;
        self.git.remote_branch_exists(input).await
    }

    pub async fn resolve_remote_tracking_commit(
        &self,
        input: &GitResolveRemoteTrackingCommitInput,
    ) -> Result<GitResolveRemoteTrackingCommitResult, GitCommandError> {
        self.ensure_command("GitWorkflowService.resolveRemoteTrackingCommit", &input.cwd)
            .await?;
        self.git.resolve_remote_tracking_commit(input).await
    }

    pub async fn remove_worktree(&self, input: &VcsRemoveWorktreeInput) -> Result<(), GitCommandError> {
        self.ensure_command("GitWorkflowService.removeWorktree", input.cwd.as_str())
            .await?;
        self.git.remove_worktree(input).await
    }

    pub async fn prune_worktrees(&self, cwd: &str) -> Result<(), GitCommandError> {
        self.ensure_command("GitWorkflowService.pruneWorktrees", cwd)
            .await?;
        self.git.prune_worktrees(cwd).await
    }

    pub async fn delete_local_branch(
        &self,
        input: &GitDeleteLocalBranchInput,
    ) -> Result<(), GitCommandError> {
        self.ensure_command("GitWorkflowService.deleteLocalBranch", &input.cwd)
            .await?;
        self.git.delete_local_branch(input).await
    }

    pub async fn create_ref(
        &self,
        input: &VcsCreateRefInput,
    ) -> Result<VcsCreateRefResult, GitCommandError> {
        self.ensure_command("GitWorkflowService.createRef", input.cwd.as_str())
            .await?;
        self.git.create_ref(input).await
    }

    pub async fn switch_ref(
        &self,
        input: &VcsSwitchRefInput,
    ) -> Result<VcsSwitchRefResult, GitCommandError> {
        self.ensure_command("GitWorkflowService.switchRef", input.cwd.as_str())
            .await?;
        self.git.switch_ref(input).await
    }

    pub async fn rename_branch(
        &self,
        input: &GitRenameBranchInput,
    ) -> Result<GitRenameBranchResult, GitManagerServiceError> {
        self.ensure_manager("GitWorkflowService.renameBranch", &input.cwd)
            .await?;
        self.git
            .rename_branch(input)
            .await
            .map_err(GitManagerServiceError::Command)
    }

    async fn ensure_manager(&self, operation: &str, cwd: &str) -> Result<(), GitManagerServiceError> {
        let handle = self
            .registry
            .resolve(&VcsDriverResolveInput {
                cwd: cwd.to_owned(),
                requested_kind: None,
            })
            .await
            .map_err(|error| manager_route(operation, cwd, error))?;
        if handle.kind != VcsDriverKind::Git {
            return Err(GitManagerServiceError::Manager(GitManagerError::GitManagerError {
                operation: operation.to_owned(),
                cwd: cwd.to_owned(),
                detail: format!(
                    "The {operation} workflow currently supports Git repositories only; detected {}. ({cwd})",
                    handle.kind
                ),
                cause: None,
            }));
        }
        Ok(())
    }

    async fn ensure_command(&self, operation: &str, cwd: &str) -> Result<(), GitCommandError> {
        let handle = self
            .registry
            .resolve(&VcsDriverResolveInput {
                cwd: cwd.to_owned(),
                requested_kind: None,
            })
            .await
            .map_err(|error| command_route(operation, cwd, error))?;
        if handle.kind != VcsDriverKind::Git {
            return Err(command_error(
                operation,
                cwd,
                &format!(
                    "The {operation} command currently supports Git repositories only; detected {}.",
                    handle.kind
                ),
            ));
        }
        Ok(())
    }

    async fn detect_manager(
        &self,
        operation: &str,
        cwd: &str,
    ) -> Result<Option<bool>, GitManagerServiceError> {
        let handle = self
            .registry
            .detect(&VcsDriverResolveInput {
                cwd: cwd.to_owned(),
                requested_kind: None,
            })
            .await
            .map_err(|error| {
                GitManagerServiceError::Manager(GitManagerError::GitManagerError {
                    operation: operation.to_owned(),
                    cwd: cwd.to_owned(),
                    detail: "Failed to detect a VCS repository for this Git workflow.".to_owned(),
                    cause: Some(serde_json::Value::String(error.to_string())),
                })
            })?;
        match handle {
            None => Ok(Some(false)),
            Some(handle) if handle.kind == VcsDriverKind::Git => Ok(Some(true)),
            Some(handle) => Err(GitManagerServiceError::Manager(GitManagerError::GitManagerError {
                operation: operation.to_owned(),
                cwd: cwd.to_owned(),
                detail: format!(
                    "The {operation} workflow currently supports Git repositories only; detected {}. ({cwd})",
                    handle.kind
                ),
                cause: None,
            })),
        }
    }

    async fn detect_command(
        &self,
        operation: &str,
        cwd: &str,
    ) -> Result<Option<bool>, GitCommandError> {
        let handle = self
            .registry
            .detect(&VcsDriverResolveInput {
                cwd: cwd.to_owned(),
                requested_kind: None,
            })
            .await
            .map_err(|error| {
                command_error(
                    operation,
                    cwd,
                    "Failed to detect a VCS repository for this Git command.",
                )
                .with_cause_text(&error.to_string())
            })?;
        match handle {
            None => Ok(Some(false)),
            Some(handle) if handle.kind == VcsDriverKind::Git => Ok(Some(true)),
            Some(handle) => Err(command_error(
                operation,
                cwd,
                &format!(
                    "The {operation} command currently supports Git repositories only; detected {}.",
                    handle.kind
                ),
            )),
        }
    }
}

fn manager_route(operation: &str, cwd: &str, error: VcsError) -> GitManagerServiceError {
    GitManagerServiceError::Manager(GitManagerError::GitManagerError {
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        detail: "Failed to resolve the VCS driver for this Git workflow.".to_owned(),
        cause: Some(serde_json::Value::String(error.to_string())),
    })
}

fn command_route(operation: &str, cwd: &str, error: VcsError) -> GitCommandError {
    command_error(
        operation,
        cwd,
        "Failed to resolve the VCS driver for this Git command.",
    )
    .with_cause_text(&error.to_string())
}

fn command_error(operation: &str, cwd: &str, detail: &str) -> GitCommandError {
    GitCommandError::GitCommandError {
        operation: operation.to_owned(),
        command: "vcs-route".to_owned(),
        cwd: cwd.to_owned(),
        argument_count: None,
        exit_code: None,
        stdout_length: None,
        stderr_length: None,
        output_length: None,
        reason: None,
        detail: detail.to_owned(),
        cause: None,
    }
}

trait WithCauseText {
    fn with_cause_text(self, cause: &str) -> Self;
}

impl WithCauseText for GitCommandError {
    fn with_cause_text(mut self, cause: &str) -> Self {
        let GitCommandError::GitCommandError { cause: slot, .. } = &mut self;
        *slot = Some(serde_json::Value::String(cause.to_owned()));
        self
    }
}

fn non_repository_local() -> VcsStatusLocalResult {
    VcsStatusLocalResult {
        is_repo: false,
        source_control_provider: None,
        has_primary_remote: false,
        is_default_ref: false,
        ref_name: None,
        has_working_tree_changes: false,
        working_tree: VcsStatusWorkingTree {
            files: Vec::new(),
            insertions: 0,
            deletions: 0,
        },
        branch_changes: None,
    }
}

fn non_repository_status() -> VcsStatusResult {
    VcsStatusResult {
        is_repo: false,
        source_control_provider: None,
        has_primary_remote: false,
        is_default_ref: false,
        ref_name: None,
        has_working_tree_changes: false,
        working_tree: VcsStatusWorkingTree {
            files: Vec::new(),
            insertions: 0,
            deletions: 0,
        },
        branch_changes: None,
        has_upstream: false,
        ahead_count: 0,
        behind_count: 0,
        ahead_of_default_count: Some(0),
        pr: None,
    }
}

#[async_trait]
impl crate::vcs_status_broadcaster::GitWorkflowService for GitWorkflowService {
    async fn local_status(
        &self,
        cwd: &str,
    ) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
        workflow_local(self, cwd).await
    }

    async fn remote_status(
        &self,
        cwd: &str,
        options: crate::vcs_status_broadcaster::GitRemoteStatusOptions,
    ) -> Result<Option<VcsStatusRemoteResult>, GitManagerServiceError> {
        let cwd = TrimmedNonEmptyString::new(cwd).map_err(|_| {
            GitManagerServiceError::Manager(GitManagerError::GitManagerError {
                operation: "GitWorkflowService.remoteStatus".into(),
                cwd: cwd.to_owned(),
                detail: "Expected a non-empty cwd.".into(),
                cause: None,
            })
        })?;
        GitWorkflowService::remote_status(
            self,
            &VcsStatusInput { cwd },
            GitManagerRemoteStatusOptions {
                refresh_upstream: options.refresh_upstream,
                refresh_missing_pull_request: options.refresh_missing_pull_request == Some(true),
            },
        )
        .await
    }

    async fn invalidate_local_status(&self, cwd: &str) {
        self.manager.invalidate_local_status(cwd);
    }

    async fn invalidate_remote_status(&self, cwd: &str) {
        self.manager.invalidate_remote_status(cwd);
    }

    async fn invalidate_status(&self, cwd: &str) {
        self.manager.invalidate_status(cwd);
    }

    async fn pull_current_branch(&self, cwd: &str) -> Result<VcsPullResult, GitManagerServiceError> {
        GitWorkflowService::pull_current_branch(self, cwd)
            .await
            .map_err(GitManagerServiceError::Command)
    }
}

async fn workflow_local(
    service: &GitWorkflowService,
    cwd: &str,
) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
    let cwd = TrimmedNonEmptyString::new(cwd).map_err(|_| {
        GitManagerServiceError::Manager(GitManagerError::GitManagerError {
            operation: "GitWorkflowService.localStatus".into(),
            cwd: cwd.to_owned(),
            detail: "Expected a non-empty cwd.".into(),
            cause: None,
        })
    })?;
    service
        .local_status(&VcsStatusInput { cwd })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_vcs_driver::GitVcsDriver;
    use crate::process::ProcessRunner;
    use crate::vcs_process::VcsProcess;
    use crate::vcs_project_config::VcsProjectConfig;
    use turin_contracts::git::{GitCommitStepStatus, GitStackedAction};

    fn workflow() -> GitWorkflowService {
        let registry = Arc::new(VcsDriverRegistry::new(
            Arc::new(VcsProjectConfig::new()),
            Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
        ));
        let git = Arc::new(GitVcsDriverCore::new());
        let manager = Arc::new(GitManager::new(Arc::clone(&git)));
        GitWorkflowService::new(registry, git, manager)
    }

    fn git(cwd: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success());
    }

    #[tokio::test]
    async fn a_plain_directory_is_an_explicit_empty_status() {
        let dir = tempfile::tempdir().unwrap();
        let service = workflow();
        let cwd = TrimmedNonEmptyString::new(dir.path().to_str().unwrap()).unwrap();
        let status = service.status(&VcsStatusInput { cwd }).await.unwrap();
        assert!(!status.is_repo);
        assert!(status.working_tree.files.is_empty());
        assert!(!service
            .is_repository(dir.path().to_str().unwrap())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn commits_through_the_workflow_on_a_temp_repo() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "test@test.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "initial"]);
        std::fs::write(dir.path().join("note.txt"), "note\n").unwrap();

        let service = workflow();
        let cwd = dir.path().to_str().unwrap();
        assert!(service.has_commit(cwd, "HEAD").await.unwrap());
        assert!(service.has_ref_named(cwd, "main").await.unwrap());
        assert!(!service.has_ref_named(cwd, "ma*n").await.unwrap());
        let result = service
            .run_stacked_action(
                &GitRunStackedActionInput {
                    action_id: TrimmedNonEmptyString::new("action-2").unwrap(),
                    cwd: TrimmedNonEmptyString::new(cwd).unwrap(),
                    action: GitStackedAction::Commit,
                    commit_message: Some(TrimmedNonEmptyString::new("Record the note").unwrap()),
                    feature_branch: Some(true),
                    file_paths: None,
                    thread_id: None,
                    project_id: None,
                },
                GitRunStackedActionOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.commit.status, GitCommitStepStatus::Created);
        assert_eq!(
            result.branch.name.as_ref().map(|name| name.as_str()),
            Some("feature/record-the-note")
        );
    }
}
