//! Port of upstream/apps/server/src/project/ProjectService.ts (surface).
//!
//! This module holds the error and input types and the [`ProjectServiceApi`]
//! trait that other project services call. The implementation follows.

use async_trait::async_trait;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::project::{Project, ProjectCreatePayload, ProjectSnapshot, ProjectUpdatePayload};
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::{CommandId, ProjectId};

use crate::project_ports::PortError;

/// Upstream `ProjectCreateInput`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCreateInput {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub payload: ProjectCreatePayload,
}

/// Upstream `ProjectUpdateInput`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectUpdateInput {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub payload: ProjectUpdatePayload,
}

/// Upstream `ProjectDeleteInput`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectDeleteInput {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub force: Option<bool>,
}

/// Upstream `bootstrap` result.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectBootstrap {
    pub project: Project,
    pub created: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectOperation {
    NormalizeWorkspace,
    ReadProject,
    ListProjects,
    ListThreads,
    DeleteThread,
    DispatchProjectCommand,
}

impl ProjectOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NormalizeWorkspace => "normalize-workspace",
            Self::ReadProject => "read-project",
            Self::ListProjects => "list-projects",
            Self::ListThreads => "list-threads",
            Self::DeleteThread => "delete-thread",
            Self::DispatchProjectCommand => "dispatch-project-command",
        }
    }
}

/// Upstream `ProjectOperationError`: an infrastructure failure in one operation.
#[derive(Debug, thiserror::Error)]
pub struct ProjectOperationError {
    pub operation: ProjectOperation,
    pub project_id: Option<ProjectId>,
    pub workspace_root: Option<String>,
    #[source]
    pub cause: PortError,
}

impl std::fmt::Display for ProjectOperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Project operation '{}' failed", self.operation.as_str())?;
        if let Some(project_id) = &self.project_id {
            write!(f, " for {project_id}")?;
        }
        f.write_str(".")
    }
}

/// Upstream `ProjectServiceError` union.
#[derive(Debug, thiserror::Error)]
pub enum ProjectServiceError {
    #[error("Project {project_id} was not found.")]
    NotFound { project_id: ProjectId },
    #[error("Workspace {workspace_root} already belongs to project {conflicting_project_id}.")]
    Conflict {
        project_id: ProjectId,
        workspace_root: String,
        conflicting_project_id: ProjectId,
    },
    #[error("Project {project_id} is not empty.")]
    NotEmpty { project_id: ProjectId },
    #[error(transparent)]
    Operation(#[from] ProjectOperationError),
}

/// Upstream `ProjectService` as other services use it.
#[async_trait]
pub trait ProjectServiceApi: Send + Sync {
    async fn create(&self, input: ProjectCreateInput) -> Result<Project, ProjectServiceError>;
    async fn bootstrap(
        &self,
        input: ProjectCreateInput,
    ) -> Result<ProjectBootstrap, ProjectServiceError>;
    async fn update(&self, input: ProjectUpdateInput) -> Result<Project, ProjectServiceError>;
    async fn delete(&self, input: ProjectDeleteInput) -> Result<Project, ProjectServiceError>;
    async fn get_by_id(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError>;
    async fn get_by_workspace_root(
        &self,
        workspace_root: &str,
        include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError>;
    async fn snapshot(&self) -> Result<ProjectSnapshot, ProjectOperationError>;
    async fn get_shell(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, ProjectOperationError>;
    async fn list_shells(
        &self,
        project_ids: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, ProjectOperationError>;
}

/// Shorthand for command ids built from known prefixes. The prefixes are
/// non-empty, so construction only fails on a bug.
pub(crate) fn command_id(value: impl AsRef<str>) -> CommandId {
    CommandId::new(value).expect("project command ids are non-empty")
}

pub(crate) fn project_id(value: impl AsRef<str>) -> ProjectId {
    ProjectId::new(value).expect("project ids are non-empty")
}

pub(crate) fn trimmed(value: impl AsRef<str>) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("title is non-empty")
}

/// A create payload with only the required fields set.
pub(crate) fn create_payload(title: TrimmedNonEmptyString, workspace_root: String) -> ProjectCreatePayload {
    ProjectCreatePayload {
        title,
        workspace_root: trimmed(workspace_root),
        create_workspace_root_if_missing: None,
        default_model_selection: None,
        scripts: None,
    }
}

/// An update payload with no field changed.
pub(crate) fn update_payload() -> ProjectUpdatePayload {
    ProjectUpdatePayload {
        title: None,
        workspace_root: None,
        default_model_selection: None,
        auto_pull: None,
        project_icon: None,
        favicon_path: None,
        default_thread_env_mode: None,
        scripts: None,
    }
}
