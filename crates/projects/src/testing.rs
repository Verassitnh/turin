//! Test doubles for the project seams, plus a real-git adapter.
//!
//! The fake project service enforces the upstream rules the tests depend on:
//! a workspace root belongs to one active project, bootstrap checks then
//! creates, and a create that loses the race fails with a conflict.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use turin_contracts::ProjectId;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::project::{
    Project, ProjectIconOverride, ProjectSnapshot, ReceivedProjectIcon,
};

use crate::managed_project_folders::{
    GitCli, GitCliError, GitExecute, GitExecuted, RepositoryProbe, RepositoryProbeError,
};
use crate::project_service::{
    ProjectBootstrap, ProjectCreateInput, ProjectDeleteInput, ProjectOperation,
    ProjectOperationError, ProjectServiceApi, ProjectServiceError, ProjectUpdateInput,
};

const FIXED_TIMESTAMP: &str = "2026-10-15T00:00:00.000Z";

#[derive(Debug, Clone)]
struct Record {
    id: ProjectId,
    title: String,
    workspace_root: String,
    icon: Option<ProjectIconOverride>,
    deleted: bool,
}

#[derive(Default)]
struct State {
    rows: Vec<Record>,
    fail_create: Option<String>,
}

/// In-memory `ProjectServiceApi`. Operations are serialized by one lock, so a
/// check and its write never interleave.
#[derive(Default)]
pub struct FakeProjects {
    state: Mutex<State>,
}

impl FakeProjects {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later `create` fail with an infrastructure error.
    pub fn fail_creates(&self, cause: &str) {
        self.state.lock().fail_create = Some(cause.to_owned());
    }

    /// Ids of active projects, in creation order.
    pub fn active_ids(&self) -> Vec<ProjectId> {
        self.state
            .lock()
            .rows
            .iter()
            .filter(|row| !row.deleted)
            .map(|row| row.id.clone())
            .collect()
    }

    fn to_project(record: &Record) -> Project {
        Project {
            id: record.id.clone(),
            title: trimmed(&record.title),
            workspace_root: trimmed(&record.workspace_root),
            repository_identity: None,
            favicon_path: None,
            project_icon: Some(
                record
                    .icon
                    .clone()
                    .map(|icon| ReceivedProjectIcon(Some(icon))),
            ),
            default_model_selection: None,
            default_thread_env_mode: None,
            auto_pull: None,
            scripts: Vec::new(),
            created_at: FIXED_TIMESTAMP.to_owned(),
            updated_at: FIXED_TIMESTAMP.to_owned(),
            deleted_at: record.deleted.then(|| FIXED_TIMESTAMP.to_owned()),
        }
    }

    fn active_owner(state: &State, workspace_root: &str) -> Option<Record> {
        state
            .rows
            .iter()
            .find(|row| !row.deleted && row.workspace_root == workspace_root)
            .cloned()
    }

    fn operation_error(
        operation: ProjectOperation,
        project_id: Option<&ProjectId>,
        cause: &str,
    ) -> ProjectOperationError {
        ProjectOperationError {
            operation,
            project_id: project_id.cloned(),
            workspace_root: None,
            cause: anyhow::anyhow!(cause.to_owned()),
        }
    }
}

fn trimmed(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("fake values are non-empty")
}

#[async_trait]
impl ProjectServiceApi for FakeProjects {
    async fn create(&self, input: ProjectCreateInput) -> Result<Project, ProjectServiceError> {
        let mut state = self.state.lock();
        if let Some(cause) = state.fail_create.clone() {
            return Err(FakeProjects::operation_error(
                ProjectOperation::DispatchProjectCommand,
                Some(&input.project_id),
                &cause,
            )
            .into());
        }
        let root = input.payload.workspace_root.as_str().to_owned();
        if let Some(owner) = FakeProjects::active_owner(&state, &root) {
            return Err(ProjectServiceError::Conflict {
                project_id: input.project_id,
                workspace_root: root,
                conflicting_project_id: owner.id,
            });
        }
        let record = Record {
            id: input.project_id.clone(),
            title: input.payload.title.as_str().to_owned(),
            workspace_root: root,
            icon: None,
            deleted: false,
        };
        state.rows.push(record.clone());
        Ok(FakeProjects::to_project(&record))
    }

    async fn bootstrap(
        &self,
        input: ProjectCreateInput,
    ) -> Result<ProjectBootstrap, ProjectServiceError> {
        // Upstream: look up the root, then create. A racing create loses with a conflict.
        let root = input.payload.workspace_root.as_str().to_owned();
        let existing = {
            let state = self.state.lock();
            FakeProjects::active_owner(&state, &root)
        };
        if let Some(existing) = existing {
            return Ok(ProjectBootstrap {
                project: FakeProjects::to_project(&existing),
                created: false,
            });
        }
        let project = self.create(input).await?;
        Ok(ProjectBootstrap {
            project,
            created: true,
        })
    }

    async fn update(&self, input: ProjectUpdateInput) -> Result<Project, ProjectServiceError> {
        let mut state = self.state.lock();
        let Some(row) = state
            .rows
            .iter_mut()
            .find(|row| row.id == input.project_id && !row.deleted)
        else {
            return Err(ProjectServiceError::NotFound {
                project_id: input.project_id,
            });
        };
        if let Some(title) = &input.payload.title {
            row.title = title.as_str().to_owned();
        }
        if let Some(icon) = &input.payload.project_icon {
            row.icon = icon.clone();
        }
        Ok(FakeProjects::to_project(row))
    }

    async fn delete(&self, input: ProjectDeleteInput) -> Result<Project, ProjectServiceError> {
        let mut state = self.state.lock();
        let Some(row) = state.rows.iter_mut().find(|row| row.id == input.project_id) else {
            return Err(ProjectServiceError::NotFound {
                project_id: input.project_id,
            });
        };
        row.deleted = true;
        Ok(FakeProjects::to_project(row))
    }

    async fn get_by_id(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        let state = self.state.lock();
        Ok(state
            .rows
            .iter()
            .find(|row| &row.id == project_id && (include_deleted || !row.deleted))
            .map(FakeProjects::to_project))
    }

    async fn get_by_workspace_root(
        &self,
        workspace_root: &str,
        include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        let state = self.state.lock();
        Ok(state
            .rows
            .iter()
            .find(|row| row.workspace_root == workspace_root && (include_deleted || !row.deleted))
            .map(FakeProjects::to_project))
    }

    async fn snapshot(&self) -> Result<ProjectSnapshot, ProjectOperationError> {
        let state = self.state.lock();
        Ok(ProjectSnapshot {
            projects: state
                .rows
                .iter()
                .filter(|row| !row.deleted)
                .map(FakeProjects::to_project)
                .collect(),
            updated_at: FIXED_TIMESTAMP.to_owned(),
        })
    }

    async fn get_shell(
        &self,
        _project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, ProjectOperationError> {
        Ok(None)
    }

    async fn list_shells(
        &self,
        _project_ids: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, ProjectOperationError> {
        Ok(Vec::new())
    }
}

/// Real `git` through the system binary, with its own environment so tests
/// never touch the process environment (tests run in parallel).
pub struct SystemGit {
    pub env: Vec<(String, String)>,
    pub removed_env: Vec<String>,
}

impl SystemGit {
    /// Identity from the environment, an empty global config, and no system config.
    pub fn with_identity(global_config: &Path) -> Self {
        let mut git = Self::isolated(global_config);
        git.env.extend(
            [
                ("GIT_AUTHOR_NAME", "Test"),
                ("GIT_AUTHOR_EMAIL", "test@test.com"),
                ("GIT_COMMITTER_NAME", "Test"),
                ("GIT_COMMITTER_EMAIL", "test@test.com"),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned())),
        );
        git
    }

    /// No identity anywhere: the caller supplies the global config.
    pub fn isolated(global_config: &Path) -> Self {
        Self {
            env: vec![
                (
                    "GIT_CONFIG_GLOBAL".to_owned(),
                    global_config.to_string_lossy().into_owned(),
                ),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ],
            removed_env: [
                "GIT_AUTHOR_NAME",
                "GIT_AUTHOR_EMAIL",
                "GIT_COMMITTER_NAME",
                "GIT_COMMITTER_EMAIL",
                "EMAIL",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        }
    }

    fn command(&self, cwd: &str, args: &[String]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for key in &self.removed_env {
            command.env_remove(key);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }
}

#[async_trait]
impl GitCli for SystemGit {
    async fn read_config_value(&self, cwd: &str, key: &str) -> Result<Option<String>, GitCliError> {
        let output = self
            .command(
                cwd,
                &["config".to_owned(), "--get".to_owned(), key.to_owned()],
            )
            .output()
            .await
            .map_err(|error| GitCliError {
                operation: "config".to_owned(),
                message: error.to_string(),
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    }

    async fn execute(&self, input: GitExecute) -> Result<GitExecuted, GitCliError> {
        let output = tokio::time::timeout(
            input.timeout.max(Duration::from_secs(60)),
            self.command(&input.cwd, &input.args).output(),
        )
        .await
        .map_err(|_| GitCliError {
            operation: input.operation.to_owned(),
            message: "timed out".to_owned(),
        })?
        .map_err(|error| GitCliError {
            operation: input.operation.to_owned(),
            message: error.to_string(),
        })?;
        let exit_code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if exit_code != 0 && !input.allow_non_zero_exit {
            return Err(GitCliError {
                operation: input.operation.to_owned(),
                message: stderr,
            });
        }
        Ok(GitExecuted { exit_code, stderr })
    }
}

/// Real repository detection through the system `git`.
pub struct SystemProbe;

#[async_trait]
impl RepositoryProbe for SystemProbe {
    async fn is_repository(&self, cwd: &str) -> Result<bool, RepositoryProbeError> {
        let output = tokio::process::Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => RepositoryProbeError::GitNotInstalled,
                _ => RepositoryProbeError::Failed(error.to_string()),
            })?;
        Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
    }
}

/// Probe for a machine with no git.
pub struct MissingGitProbe;

#[async_trait]
impl RepositoryProbe for MissingGitProbe {
    async fn is_repository(&self, _cwd: &str) -> Result<bool, RepositoryProbeError> {
        Err(RepositoryProbeError::GitNotInstalled)
    }
}

/// Git that answers `init` by blocking forever, after telling the test it started.
pub struct BlockingInitGit {
    pub started: tokio::sync::Notify,
}

#[async_trait]
impl GitCli for BlockingInitGit {
    async fn read_config_value(
        &self,
        _cwd: &str,
        _key: &str,
    ) -> Result<Option<String>, GitCliError> {
        Ok(None)
    }

    async fn execute(&self, _input: GitExecute) -> Result<GitExecuted, GitCliError> {
        self.started.notify_one();
        std::future::pending().await
    }
}

/// Git whose every command fails, as a broken install does.
pub struct BrokenGit;

#[async_trait]
impl GitCli for BrokenGit {
    async fn read_config_value(
        &self,
        _cwd: &str,
        _key: &str,
    ) -> Result<Option<String>, GitCliError> {
        Ok(None)
    }

    async fn execute(&self, input: GitExecute) -> Result<GitExecuted, GitCliError> {
        Err(GitCliError {
            operation: input.operation.to_owned(),
            message: "git is broken".to_owned(),
        })
    }
}
