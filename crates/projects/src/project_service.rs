//! Port of upstream/apps/server/src/project/ProjectService.ts (surface).
//!
//! This module holds the error and input types and the [`ProjectServiceApi`]
//! trait that other project services call. The implementation follows.

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, UtcDateTime};
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::project::{
    Project, ProjectCreatePayload, ProjectSnapshot, ProjectUpdatePayload, ReceivedProjectIcon,
};
use turin_contracts::{CommandId, ProjectId};
use turin_git::workspace::workspace_paths::WorkspacePaths;

use crate::keyed_lock::KeyedLock;
use crate::project_enrichment_service::{ProjectEnrichment, ProjectEnrichmentService};
use crate::project_ports::{
    CommandStatus, EventIdAllocator, PortError, ProjectCommand, ProjectCommandPlanner,
    ProjectCommandReceipt, ProjectCommandRejection, ProjectEventSink, ProjectPlanState, ProjectRow,
    ProjectSettingsOverrides, ProjectStoreReads, ThreadDeletions,
};

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
pub(crate) fn create_payload(
    title: TrimmedNonEmptyString,
    workspace_root: String,
) -> ProjectCreatePayload {
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

/// What [`ProjectService`] needs from the orchestrator and storage.
pub struct ProjectServiceDeps {
    pub projects: Arc<dyn ProjectStoreReads>,
    pub enrichment: Arc<ProjectEnrichmentService>,
    pub workspace_paths: Arc<WorkspacePaths>,
    pub planner: Arc<dyn ProjectCommandPlanner>,
    pub event_sink: Arc<dyn ProjectEventSink>,
    pub event_ids: Arc<dyn EventIdAllocator>,
    pub threads: Arc<dyn ThreadDeletions>,
    pub settings: Arc<dyn ProjectSettingsOverrides>,
}

/// Upstream `ProjectService`: project commands, reads, and project deletion.
pub struct ProjectService {
    projects: Arc<dyn ProjectStoreReads>,
    enrichment: Arc<ProjectEnrichmentService>,
    workspace_paths: Arc<WorkspacePaths>,
    planner: Arc<dyn ProjectCommandPlanner>,
    event_sink: Arc<dyn ProjectEventSink>,
    event_ids: Arc<dyn EventIdAllocator>,
    threads: Arc<dyn ThreadDeletions>,
    settings: Arc<dyn ProjectSettingsOverrides>,
    // Commands for one project run in order. Commands that claim a workspace root
    // also hold that root, so two projects cannot both claim it.
    project_locks: KeyedLock<ProjectId>,
    workspace_locks: KeyedLock<String>,
}

fn operation_error(
    operation: ProjectOperation,
    project_id: Option<&ProjectId>,
    workspace_root: Option<&str>,
    cause: PortError,
) -> ProjectOperationError {
    ProjectOperationError {
        operation,
        project_id: project_id.cloned(),
        workspace_root: workspace_root.map(str::to_owned),
        cause,
    }
}

/// Maps a `ProjectEnrichment` onto the wire `Project`, as upstream `toProject` does.
fn to_project(row: ProjectRow, enrichment: ProjectEnrichment) -> Project {
    Project {
        id: row.project_id,
        title: row.title,
        workspace_root: row.workspace_root,
        repository_identity: Some(enrichment.repository_identity),
        favicon_path: Some(row.favicon_path.or_else(|| {
            enrichment
                .favicon_path
                .and_then(|path| TrimmedNonEmptyString::new(path).ok())
        })),
        project_icon: Some(row.project_icon.map(|icon| ReceivedProjectIcon(Some(icon)))),
        default_model_selection: row.default_model_selection,
        default_thread_env_mode: Some(row.default_thread_env_mode),
        auto_pull: Some(row.auto_pull),
        scripts: row.scripts,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
    }
}

impl ProjectService {
    pub fn new(deps: ProjectServiceDeps) -> Self {
        Self {
            projects: deps.projects,
            enrichment: deps.enrichment,
            workspace_paths: deps.workspace_paths,
            planner: deps.planner,
            event_sink: deps.event_sink,
            event_ids: deps.event_ids,
            threads: deps.threads,
            settings: deps.settings,
            project_locks: KeyedLock::new(),
            workspace_locks: KeyedLock::new(),
        }
    }

    async fn normalize_workspace_root(
        &self,
        project_id: Option<&ProjectId>,
        workspace_root: &str,
        create_if_missing: bool,
    ) -> Result<String, ProjectOperationError> {
        self.workspace_paths
            .normalize_workspace_root(workspace_root, create_if_missing)
            .await
            .map_err(|cause| {
                operation_error(
                    ProjectOperation::NormalizeWorkspace,
                    project_id,
                    Some(workspace_root),
                    cause.into(),
                )
            })
    }

    async fn read_row(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<ProjectRow>, ProjectOperationError> {
        self.projects
            .get(project_id, include_deleted)
            .await
            .map_err(|cause| {
                operation_error(ProjectOperation::ReadProject, Some(project_id), None, cause)
            })
    }

    /// Deleted projects only read what is already cached.
    async fn hydrate(&self, row: ProjectRow) -> Project {
        let root = row.workspace_root.as_str().to_owned();
        let enrichment = if row.deleted_at.is_none() {
            self.enrichment.get_available(&root)
        } else {
            self.enrichment.peek(&root)
        };
        to_project(row, enrichment)
    }

    /// Plans one command against rows read under its locks, then commits its
    /// event or its rejection. A reused command id resolves to the receipt it
    /// already has.
    async fn commit(&self, command: ProjectCommand) -> Result<(), ProjectServiceError> {
        let project_id = command.project_id().clone();
        let workspace_root = command.claimed_workspace_root().map(str::to_owned);

        let plan_and_commit = async {
            let project = self.read_row(&project_id, true).await?;
            let workspace_owner = match &workspace_root {
                None => None,
                Some(root) => self
                    .projects
                    .find_active_by_workspace_root(root)
                    .await
                    .map_err(|cause| {
                        operation_error(
                            ProjectOperation::DispatchProjectCommand,
                            Some(&project_id),
                            None,
                            cause,
                        )
                    })?,
            };
            let now = UtcDateTime::now();
            let event_id = self
                .event_ids
                .allocate_event(command.command_id())
                .map_err(|cause| {
                    operation_error(
                        ProjectOperation::DispatchProjectCommand,
                        Some(&project_id),
                        None,
                        cause,
                    )
                })?;
            let state = ProjectPlanState {
                project,
                workspace_owner,
            };
            let planned = self.planner.plan(&command, &state, &event_id, &now);
            match &planned {
                Ok(event) => {
                    self.event_sink
                        .commit_project_command(
                            command.command_id(),
                            &project_id,
                            command.command_type(),
                            &now,
                            event,
                        )
                        .await
                }
                Err(rejection) => {
                    self.event_sink
                        .commit_rejected_project_command(
                            command.command_id(),
                            &project_id,
                            command.command_type(),
                            &now,
                            &rejection.encode(),
                        )
                        .await
                }
            }
        };

        let receipt: ProjectCommandReceipt = {
            let _project_guard = self.project_locks.lock(project_id.clone()).await;
            let result: Result<ProjectCommandReceipt, anyhow::Error> = match &workspace_root {
                None => plan_and_commit.await,
                Some(root) => {
                    let _workspace_guard = self.workspace_locks.lock(root.clone()).await;
                    plan_and_commit.await
                }
            };
            result.map_err(|cause| {
                ProjectServiceError::from(operation_error(
                    ProjectOperation::DispatchProjectCommand,
                    Some(&project_id),
                    None,
                    cause,
                ))
            })?
        };

        if receipt.project_id != project_id || receipt.command_type != command.command_type() {
            return Err(operation_error(
                ProjectOperation::DispatchProjectCommand,
                Some(&project_id),
                None,
                anyhow::anyhow!(
                    "Command {} was already used by {} for {}.",
                    command.command_id(),
                    receipt.command_type,
                    receipt.project_id
                ),
            )
            .into());
        }
        // A retried command re-plans against the state it already produced, so its
        // first receipt, not the new plan, decides the outcome.
        if receipt.status == CommandStatus::Accepted {
            return Ok(());
        }
        let rejection = receipt
            .error
            .as_ref()
            .and_then(ProjectCommandRejection::decode);
        match rejection {
            Some(ProjectCommandRejection::WorkspaceConflict {
                workspace_root,
                conflicting_project_id,
            }) => Err(ProjectServiceError::Conflict {
                project_id,
                workspace_root,
                conflicting_project_id,
            }),
            Some(ProjectCommandRejection::MissingProject { .. }) => {
                Err(ProjectServiceError::NotFound { project_id })
            }
            None => {
                let cause = match receipt.error {
                    Some(value) => anyhow::anyhow!("{value}"),
                    None => anyhow::anyhow!("The command was previously rejected."),
                };
                Err(operation_error(
                    ProjectOperation::DispatchProjectCommand,
                    Some(&project_id),
                    None,
                    cause,
                )
                .into())
            }
        }
    }

    async fn read_committed(&self, project_id: &ProjectId) -> Result<Project, ProjectServiceError> {
        let Some(row) = self.read_row(project_id, true).await? else {
            return Err(operation_error(
                ProjectOperation::ReadProject,
                Some(project_id),
                None,
                anyhow::anyhow!("The accepted project command did not produce a project row."),
            )
            .into());
        };
        Ok(self.hydrate(row).await)
    }

    /// Refuses a non-empty project without force, else deletes its live threads first.
    async fn delete_child_threads(
        &self,
        input: &ProjectDeleteInput,
    ) -> Result<(), ProjectServiceError> {
        let project_id = &input.project_id;
        // Upstream: the V2 shell is the only record of which threads are live.
        let project_threads =
            self.threads
                .project_thread_ids(project_id)
                .await
                .map_err(|cause| {
                    operation_error(ProjectOperation::ListThreads, Some(project_id), None, cause)
                })?;
        if !project_threads.is_empty() && input.force != Some(true) {
            return Err(ProjectServiceError::NotEmpty {
                project_id: project_id.clone(),
            });
        }
        // Delete children durably before the project so a failed cascade can be retried.
        for thread_id in &project_threads {
            self.threads
                .delete_child_thread(project_id, &input.command_id, thread_id)
                .await
                .map_err(|cause| {
                    operation_error(
                        ProjectOperation::DeleteThread,
                        Some(project_id),
                        None,
                        cause,
                    )
                })?;
        }
        Ok(())
    }

    /// Best-effort: a settings write failure must not block the deletion it cleans up after.
    async fn clear_project_settings_overrides(&self, project_id: &ProjectId) {
        if let Err(cause) = self.settings.clear_project_override(project_id).await {
            tracing::warn!(
                project_id = %project_id,
                error = %cause,
                "Failed to clear settings overrides for deleted project"
            );
        }
    }

    async fn enrich_shell(
        &self,
        mut shell: OrchestrationProjectShell,
    ) -> OrchestrationProjectShell {
        let enrichment = self.enrichment.get_available(shell.workspace_root.as_str());
        shell.repository_identity = Some(enrichment.repository_identity);
        shell
    }
}

#[async_trait]
impl ProjectServiceApi for ProjectService {
    async fn create(&self, input: ProjectCreateInput) -> Result<Project, ProjectServiceError> {
        let workspace_root = self
            .normalize_workspace_root(
                Some(&input.project_id),
                input.payload.workspace_root.as_str(),
                input
                    .payload
                    .create_workspace_root_if_missing
                    .unwrap_or(false),
            )
            .await?;
        self.commit(ProjectCommand::Create {
            command_id: input.command_id,
            project_id: input.project_id.clone(),
            title: input.payload.title,
            workspace_root: workspace_root.clone(),
            scripts: input.payload.scripts,
        })
        .await?;
        self.enrichment.invalidate([workspace_root.as_str()]);
        self.read_committed(&input.project_id).await
    }

    async fn bootstrap(
        &self,
        input: ProjectCreateInput,
    ) -> Result<ProjectBootstrap, ProjectServiceError> {
        let root = input.payload.workspace_root.as_str().to_owned();
        if let Some(existing) = self.get_by_workspace_root(&root, false).await? {
            return Ok(ProjectBootstrap {
                project: existing,
                created: false,
            });
        }
        Ok(ProjectBootstrap {
            project: self.create(input).await?,
            created: true,
        })
    }

    async fn update(&self, input: ProjectUpdateInput) -> Result<Project, ProjectServiceError> {
        let project_id = input.project_id.clone();
        let Some(existing) = self.read_row(&project_id, false).await? else {
            return Err(ProjectServiceError::NotFound { project_id });
        };
        let previous_root = existing.workspace_root.as_str().to_owned();
        let workspace_root = match &input.payload.workspace_root {
            None => previous_root.clone(),
            Some(root) => {
                self.normalize_workspace_root(Some(&project_id), root.as_str(), false)
                    .await?
            }
        };
        let payload = input.payload;
        self.commit(ProjectCommand::MetaUpdate {
            command_id: input.command_id,
            project_id: project_id.clone(),
            title: payload.title,
            workspace_root: (workspace_root != previous_root).then(|| workspace_root.clone()),
            default_model_selection: payload.default_model_selection,
            auto_pull: payload.auto_pull,
            project_icon: payload.project_icon,
            favicon_path: payload.favicon_path,
            default_thread_env_mode: payload.default_thread_env_mode,
            scripts: payload.scripts,
        })
        .await?;
        if workspace_root != previous_root {
            self.enrichment
                .invalidate([previous_root.as_str(), workspace_root.as_str()]);
        }
        self.read_committed(&project_id).await
    }

    async fn delete(&self, input: ProjectDeleteInput) -> Result<Project, ProjectServiceError> {
        let project_id = input.project_id.clone();
        // A deleted row still reaches commit, so a retried command id replays its
        // receipt and any other command id is rejected as not found.
        let Some(existing) = self.read_row(&project_id, true).await? else {
            return Err(ProjectServiceError::NotFound { project_id });
        };
        let is_first_delete = existing.deleted_at.is_none();
        if is_first_delete {
            self.delete_child_threads(&input).await?;
        }
        self.commit(ProjectCommand::Delete {
            command_id: input.command_id,
            project_id: project_id.clone(),
        })
        .await?;
        if is_first_delete {
            self.clear_project_settings_overrides(&project_id).await;
        }
        self.enrichment
            .invalidate([existing.workspace_root.as_str()]);
        self.read_committed(&project_id).await
    }

    async fn get_by_id(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        match self.read_row(project_id, include_deleted).await? {
            None => Ok(None),
            Some(row) => Ok(Some(self.hydrate(row).await)),
        }
    }

    async fn get_by_workspace_root(
        &self,
        workspace_root: &str,
        include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        let normalized = self
            .normalize_workspace_root(None, workspace_root, false)
            .await?;
        let row = if include_deleted {
            self.projects
                .list(true)
                .await
                .map_err(|cause| {
                    operation_error(ProjectOperation::ListProjects, None, None, cause)
                })?
                .into_iter()
                .find(|row| row.workspace_root.as_str() == normalized)
        } else {
            self.projects
                .find_active_by_workspace_root(&normalized)
                .await
                .map_err(|cause| {
                    operation_error(ProjectOperation::ListProjects, None, None, cause)
                })?
        };
        match row {
            None => Ok(None),
            Some(row) => Ok(Some(self.hydrate(row).await)),
        }
    }

    async fn snapshot(&self) -> Result<ProjectSnapshot, ProjectOperationError> {
        let rows =
            self.projects.list(false).await.map_err(|cause| {
                operation_error(ProjectOperation::ListProjects, None, None, cause)
            })?;
        let mut projects = Vec::with_capacity(rows.len());
        for row in rows {
            projects.push(self.hydrate(row).await);
        }
        Ok(ProjectSnapshot {
            projects,
            updated_at: UtcDateTime::now().to_string(),
        })
    }

    async fn get_shell(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, ProjectOperationError> {
        let shell = self.projects.get_shell(project_id).await.map_err(|cause| {
            operation_error(ProjectOperation::ReadProject, Some(project_id), None, cause)
        })?;
        match shell {
            None => Ok(None),
            Some(shell) => Ok(Some(self.enrich_shell(shell).await)),
        }
    }

    async fn list_shells(
        &self,
        project_ids: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, ProjectOperationError> {
        let shells = self
            .projects
            .list_shells(project_ids)
            .await
            .map_err(|cause| operation_error(ProjectOperation::ListProjects, None, None, cause))?;
        let mut enriched = Vec::with_capacity(shells.len());
        for shell in shells {
            enriched.push(self.enrich_shell(shell).await);
        }
        Ok(enriched)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::future::join_all;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use turin_contracts::base_schemas::UtcDateTime;
    use turin_contracts::{CommandId, EventId, ProjectId, ThreadId};

    use super::*;
    use crate::project_enrichment_service::{
        FaviconSource, ProjectEnrichmentService, ProjectEnrichmentServiceOptions,
        RepositoryIdentitySource,
    };
    use crate::project_ports::{
        CommandStatus, EventIdAllocator, PortError, ProjectCommandPlanner, ProjectCommandReceipt,
        ProjectCommandRejection, ProjectEventPayload, ProjectEventSink, ProjectPlanState,
        ProjectRow, ProjectSettingsOverrides, ProjectStoreReads, ThreadDeletions,
    };

    fn pid(value: &str) -> ProjectId {
        ProjectId::new(value).expect("id")
    }

    fn cid(value: &str) -> CommandId {
        CommandId::new(value).expect("id")
    }

    fn tid(value: &str) -> ThreadId {
        ThreadId::new(value).expect("id")
    }

    /// Rows, receipts, and the events applied to them. The event sink, the
    /// read model, and the planner's input all read the same rows.
    #[derive(Default)]
    struct StoreState {
        rows: Vec<ProjectRow>,
        receipts: HashMap<String, ProjectCommandReceipt>,
        events: Vec<Value>,
        fail_next_write: bool,
    }

    #[derive(Default)]
    struct FakeStore {
        state: Mutex<StoreState>,
    }

    fn apply_event(
        rows: &mut Vec<ProjectRow>,
        event: &Value,
        at: &UtcDateTime,
    ) -> Result<(), PortError> {
        let project_id = pid(event["projectId"].as_str().unwrap_or_default());
        match event["type"].as_str() {
            Some("project.created") => rows.push(ProjectRow {
                project_id,
                title: trimmed(event["title"].as_str().unwrap_or_default()),
                workspace_root: trimmed(event["workspaceRoot"].as_str().unwrap_or_default()),
                favicon_path: None,
                default_model_selection: None,
                default_thread_env_mode: None,
                auto_pull: false,
                project_icon: None,
                scripts: Vec::new(),
                created_at: at.to_string(),
                updated_at: at.to_string(),
                deleted_at: None,
            }),
            Some("project.meta.updated") => {
                let row = rows
                    .iter_mut()
                    .find(|row| row.project_id == project_id)
                    .ok_or_else(|| anyhow::anyhow!("no row for {project_id}"))?;
                if let Some(title) = event["title"].as_str() {
                    row.title = trimmed(title);
                }
                if let Some(root) = event["workspaceRoot"].as_str() {
                    row.workspace_root = trimmed(root);
                }
                row.updated_at = at.to_string();
            }
            Some("project.deleted") => {
                let row = rows
                    .iter_mut()
                    .find(|row| row.project_id == project_id)
                    .ok_or_else(|| anyhow::anyhow!("no row for {project_id}"))?;
                row.deleted_at = Some(at.to_string());
            }
            other => return Err(anyhow::anyhow!("unknown event {other:?}")),
        }
        Ok(())
    }

    #[async_trait]
    impl ProjectStoreReads for FakeStore {
        async fn get(
            &self,
            project_id: &ProjectId,
            include_deleted: bool,
        ) -> Result<Option<ProjectRow>, PortError> {
            Ok(self
                .state
                .lock()
                .rows
                .iter()
                .find(|row| {
                    &row.project_id == project_id && (include_deleted || row.deleted_at.is_none())
                })
                .cloned())
        }

        async fn find_active_by_workspace_root(
            &self,
            workspace_root: &str,
        ) -> Result<Option<ProjectRow>, PortError> {
            Ok(self
                .state
                .lock()
                .rows
                .iter()
                .find(|row| {
                    row.deleted_at.is_none() && row.workspace_root.as_str() == workspace_root
                })
                .cloned())
        }

        async fn list(&self, include_deleted: bool) -> Result<Vec<ProjectRow>, PortError> {
            Ok(self
                .state
                .lock()
                .rows
                .iter()
                .filter(|row| include_deleted || row.deleted_at.is_none())
                .cloned()
                .collect())
        }

        async fn get_shell(
            &self,
            _project_id: &ProjectId,
        ) -> Result<Option<OrchestrationProjectShell>, PortError> {
            Ok(None)
        }

        async fn list_shells(
            &self,
            _project_ids: Option<&[ProjectId]>,
        ) -> Result<Vec<OrchestrationProjectShell>, PortError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl ProjectEventSink for FakeStore {
        async fn commit_project_command(
            &self,
            command_id: &CommandId,
            project_id: &ProjectId,
            command_type: &str,
            accepted_at: &UtcDateTime,
            event: &ProjectEventPayload,
        ) -> Result<ProjectCommandReceipt, PortError> {
            let mut state = self.state.lock();
            if let Some(existing) = state.receipts.get(command_id.as_str()) {
                return Ok(existing.clone());
            }
            if std::mem::take(&mut state.fail_next_write) {
                return Err(anyhow::anyhow!("row write failed"));
            }
            apply_event(&mut state.rows, &event.0, accepted_at)?;
            state.events.push(event.0.clone());
            let receipt = ProjectCommandReceipt {
                command_id: command_id.clone(),
                project_id: project_id.clone(),
                command_type: command_type.to_owned(),
                status: CommandStatus::Accepted,
                error: None,
            };
            state
                .receipts
                .insert(command_id.as_str().to_owned(), receipt.clone());
            Ok(receipt)
        }

        async fn commit_rejected_project_command(
            &self,
            command_id: &CommandId,
            project_id: &ProjectId,
            command_type: &str,
            _rejected_at: &UtcDateTime,
            error: &Value,
        ) -> Result<ProjectCommandReceipt, PortError> {
            let mut state = self.state.lock();
            if let Some(existing) = state.receipts.get(command_id.as_str()) {
                return Ok(existing.clone());
            }
            let receipt = ProjectCommandReceipt {
                command_id: command_id.clone(),
                project_id: project_id.clone(),
                command_type: command_type.to_owned(),
                status: CommandStatus::Rejected,
                error: Some(error.clone()),
            };
            state
                .receipts
                .insert(command_id.as_str().to_owned(), receipt.clone());
            Ok(receipt)
        }
    }

    /// The upstream planning rules the service depends on, as test data.
    struct FakePlanner;

    impl ProjectCommandPlanner for FakePlanner {
        fn plan(
            &self,
            command: &ProjectCommand,
            state: &ProjectPlanState,
            event_id: &EventId,
            _now: &UtcDateTime,
        ) -> Result<ProjectEventPayload, ProjectCommandRejection> {
            let _ = event_id;
            match command {
                ProjectCommand::Create {
                    project_id,
                    title,
                    workspace_root,
                    ..
                } => {
                    if let Some(owner) = &state.workspace_owner {
                        return Err(ProjectCommandRejection::WorkspaceConflict {
                            workspace_root: workspace_root.clone(),
                            conflicting_project_id: owner.project_id.clone(),
                        });
                    }
                    Ok(ProjectEventPayload(json!({
                        "type": "project.created",
                        "projectId": project_id.as_str(),
                        "title": title.as_str(),
                        "workspaceRoot": workspace_root,
                    })))
                }
                ProjectCommand::MetaUpdate {
                    project_id,
                    title,
                    workspace_root,
                    ..
                } => {
                    match &state.project {
                        Some(project) if project.deleted_at.is_none() => {}
                        _ => {
                            return Err(ProjectCommandRejection::MissingProject {
                                project_id: project_id.clone(),
                            });
                        }
                    }
                    if let (Some(root), Some(owner)) = (workspace_root, &state.workspace_owner)
                        && owner.project_id != *project_id
                    {
                        return Err(ProjectCommandRejection::WorkspaceConflict {
                            workspace_root: root.clone(),
                            conflicting_project_id: owner.project_id.clone(),
                        });
                    }
                    Ok(ProjectEventPayload(json!({
                        "type": "project.meta.updated",
                        "projectId": project_id.as_str(),
                        "title": title.as_ref().map(|title| title.as_str()),
                        "workspaceRoot": workspace_root,
                    })))
                }
                ProjectCommand::Delete { project_id, .. } => match &state.project {
                    Some(project) if project.deleted_at.is_none() => {
                        Ok(ProjectEventPayload(json!({
                            "type": "project.deleted",
                            "projectId": project_id.as_str(),
                        })))
                    }
                    _ => Err(ProjectCommandRejection::MissingProject {
                        project_id: project_id.clone(),
                    }),
                },
            }
        }
    }

    #[derive(Default)]
    struct FakeEventIds {
        next: Mutex<u64>,
    }

    impl EventIdAllocator for FakeEventIds {
        fn allocate_event(&self, _command_id: &CommandId) -> Result<EventId, PortError> {
            let mut next = self.next.lock();
            *next += 1;
            Ok(EventId::new(format!("event-{next}")).expect("id"))
        }
    }

    #[derive(Default)]
    struct FakeThreads {
        threads: Mutex<Vec<(ThreadId, ProjectId)>>,
        /// Threads whose next delete fails once, as a cascade interrupted mid-way.
        fail_once: Mutex<Vec<ThreadId>>,
        deleted: Mutex<Vec<(ProjectId, CommandId, ThreadId)>>,
    }

    #[async_trait]
    impl ThreadDeletions for FakeThreads {
        async fn project_thread_ids(
            &self,
            project_id: &ProjectId,
        ) -> Result<Vec<ThreadId>, PortError> {
            Ok(self
                .threads
                .lock()
                .iter()
                .filter(|(_, owner)| owner == project_id)
                .map(|(thread, _)| thread.clone())
                .collect())
        }

        async fn delete_child_thread(
            &self,
            project_id: &ProjectId,
            command_id: &CommandId,
            thread_id: &ThreadId,
        ) -> Result<(), PortError> {
            self.deleted
                .lock()
                .push((project_id.clone(), command_id.clone(), thread_id.clone()));
            {
                let mut fail_once = self.fail_once.lock();
                if let Some(index) = fail_once.iter().position(|thread| thread == thread_id) {
                    fail_once.remove(index);
                    return Err(anyhow::anyhow!("thread delete interrupted"));
                }
            }
            self.threads
                .lock()
                .retain(|(thread, _)| thread != thread_id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeSettings {
        cleared: Mutex<Vec<ProjectId>>,
    }

    #[async_trait]
    impl ProjectSettingsOverrides for FakeSettings {
        async fn clear_project_override(&self, project_id: &ProjectId) -> Result<(), PortError> {
            self.cleared.lock().push(project_id.clone());
            Ok(())
        }
    }

    /// Metadata sources that find nothing; these tests do not read enrichment.
    struct NoMetadata;

    #[async_trait]
    impl RepositoryIdentitySource for NoMetadata {
        async fn resolve(
            &self,
            _workspace_root: &str,
        ) -> Option<turin_contracts::environment::RepositoryIdentity> {
            None
        }
    }

    #[async_trait]
    impl FaviconSource for NoMetadata {
        async fn resolve_path(
            &self,
            _workspace_root: &str,
        ) -> Result<Option<String>, crate::project_favicon_resolver::ProjectFaviconResolutionError>
        {
            Ok(None)
        }
    }

    struct Harness {
        dir: tempfile::TempDir,
        service: ProjectService,
        store: Arc<FakeStore>,
        threads: Arc<FakeThreads>,
        settings: Arc<FakeSettings>,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let store = Arc::new(FakeStore::default());
            let threads = Arc::new(FakeThreads::default());
            let settings = Arc::new(FakeSettings::default());
            let enrichment = Arc::new(ProjectEnrichmentService::new(
                Arc::new(NoMetadata),
                Arc::new(NoMetadata),
                ProjectEnrichmentServiceOptions::default(),
            ));
            let service = ProjectService::new(ProjectServiceDeps {
                projects: Arc::clone(&store) as Arc<dyn ProjectStoreReads>,
                enrichment,
                workspace_paths: Arc::new(WorkspacePaths::new(String::new())),
                planner: Arc::new(FakePlanner),
                event_sink: Arc::clone(&store) as Arc<dyn ProjectEventSink>,
                event_ids: Arc::new(FakeEventIds::default()),
                threads: Arc::clone(&threads) as Arc<dyn ThreadDeletions>,
                settings: Arc::clone(&settings) as Arc<dyn ProjectSettingsOverrides>,
            });
            Self {
                dir,
                service,
                store,
                threads,
                settings,
            }
        }

        /// An existing folder under the temp dir, so the workspace normalizes.
        fn root(&self, name: &str) -> String {
            let path = self.dir.path().join(name);
            std::fs::create_dir_all(&path).expect("root");
            path.to_string_lossy().into_owned()
        }

        fn create(
            &self,
            command: &str,
            project: &str,
            title: &str,
            root: &str,
        ) -> ProjectCreateInput {
            ProjectCreateInput {
                command_id: cid(command),
                project_id: pid(project),
                payload: create_payload(trimmed(title), root.to_owned()),
            }
        }

        fn update_title(&self, command: &str, project: &str, title: &str) -> ProjectUpdateInput {
            let mut payload = update_payload();
            payload.title = Some(trimmed(title));
            ProjectUpdateInput {
                command_id: cid(command),
                project_id: pid(project),
                payload,
            }
        }

        fn delete(&self, command: &str, project: &str) -> ProjectDeleteInput {
            ProjectDeleteInput {
                command_id: cid(command),
                project_id: pid(project),
                force: None,
            }
        }
    }

    #[tokio::test]
    async fn creates_updates_resolves_snapshots_and_soft_deletes_projects() {
        let h = Harness::new();
        let root = h.root("alpha");
        let created = h
            .service
            .create(h.create("command:create", "project:alpha", "Alpha", &root))
            .await
            .expect("create");
        assert_eq!(created.title.as_str(), "Alpha");
        assert_eq!(created.workspace_root.as_str(), root);

        let renamed = h
            .service
            .update(h.update_title("command:rename", "project:alpha", "Alpha Two"))
            .await
            .expect("update");
        assert_eq!(renamed.title.as_str(), "Alpha Two");

        let read = h
            .service
            .get_by_id(&pid("project:alpha"), false)
            .await
            .expect("read")
            .expect("project");
        assert_eq!(read.title.as_str(), "Alpha Two");
        let by_root = h
            .service
            .get_by_workspace_root(&root, false)
            .await
            .expect("read")
            .expect("project");
        assert_eq!(by_root.id, pid("project:alpha"));
        assert_eq!(
            h.service.snapshot().await.expect("snapshot").projects.len(),
            1
        );

        let deleted = h
            .service
            .delete(h.delete("command:delete", "project:alpha"))
            .await
            .expect("delete");
        assert!(deleted.deleted_at.is_some());
        assert!(
            h.service
                .get_by_id(&pid("project:alpha"), false)
                .await
                .expect("read")
                .is_none()
        );
        assert!(
            h.service
                .get_by_id(&pid("project:alpha"), true)
                .await
                .expect("read")
                .is_some()
        );
        assert!(
            h.service
                .snapshot()
                .await
                .expect("snapshot")
                .projects
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_active_workspace_collisions() {
        let h = Harness::new();
        let root = h.root("shared");
        h.service
            .create(h.create("command:alpha", "project:alpha", "Alpha", &root))
            .await
            .expect("alpha");

        let error = h
            .service
            .create(h.create("command:beta", "project:beta", "Beta", &root))
            .await
            .expect_err("collision");
        assert!(matches!(
            error,
            ProjectServiceError::Conflict { conflicting_project_id, .. }
                if conflicting_project_id == pid("project:alpha")
        ));
    }

    #[tokio::test]
    async fn auto_bootstraps_a_workspace_exactly_once() {
        let h = Harness::new();
        let root = h.root("boot");
        let first = h
            .service
            .bootstrap(h.create("command:boot-a", "project:a", "Boot", &root))
            .await
            .expect("bootstrap");
        assert!(first.created);
        let second = h
            .service
            .bootstrap(h.create("command:boot-b", "project:b", "Boot", &root))
            .await
            .expect("bootstrap again");
        assert!(!second.created);
        assert_eq!(second.project.id, pid("project:a"));
        assert_eq!(h.store.state.lock().events.len(), 1);
    }

    #[tokio::test]
    async fn commits_the_event_once_per_command_id() {
        let h = Harness::new();
        let root = h.root("once");
        h.service
            .create(h.create("command:once", "project:once", "Once", &root))
            .await
            .expect("first");
        // The same command id is a retry: its first receipt decides, even though
        // the workspace now belongs to this project and a re-plan would conflict.
        h.service
            .create(h.create("command:once", "project:once", "Once", &root))
            .await
            .expect("retry replays the accepted receipt");
        assert_eq!(h.store.state.lock().events.len(), 1);
    }

    #[tokio::test]
    async fn replays_a_rejected_receipt_instead_of_replanning_the_command() {
        let h = Harness::new();
        let root = h.root("replay");
        h.service
            .create(h.create("command:owner", "project:owner", "Owner", &root))
            .await
            .expect("owner");
        let rejected = h
            .service
            .create(h.create("command:late", "project:late", "Late", &root))
            .await
            .expect_err("conflict");
        assert!(matches!(rejected, ProjectServiceError::Conflict { .. }));

        h.service
            .delete(h.delete("command:drop-owner", "project:owner"))
            .await
            .expect("delete owner");
        // The workspace is free now, but the stored rejection still answers.
        let replayed = h
            .service
            .create(h.create("command:late", "project:late", "Late", &root))
            .await
            .expect_err("replayed");
        assert!(matches!(replayed, ProjectServiceError::Conflict { .. }));
    }

    #[tokio::test]
    async fn a_new_command_succeeds_once_the_workspace_frees_up() {
        let h = Harness::new();
        let root = h.root("freed");
        h.service
            .create(h.create("command:owner", "project:owner", "Owner", &root))
            .await
            .expect("owner");
        h.service
            .create(h.create("command:late", "project:late", "Late", &root))
            .await
            .expect_err("conflict");
        h.service
            .delete(h.delete("command:drop-owner", "project:owner"))
            .await
            .expect("delete owner");

        let retried = h
            .service
            .create(h.create("command:fresh", "project:late", "Late", &root))
            .await
            .expect("new command plans against the freed workspace");
        assert_eq!(retried.id, pid("project:late"));
    }

    #[tokio::test]
    async fn rejects_a_move_onto_a_workspace_another_active_project_holds() {
        let h = Harness::new();
        let first_root = h.root("first");
        let second_root = h.root("second");
        h.service
            .create(h.create("command:first", "project:first", "First", &first_root))
            .await
            .expect("first");
        h.service
            .create(h.create("command:second", "project:second", "Second", &second_root))
            .await
            .expect("second");

        let mut payload = update_payload();
        payload.workspace_root = Some(trimmed(&first_root));
        let error = h
            .service
            .update(ProjectUpdateInput {
                command_id: cid("command:move"),
                project_id: pid("project:second"),
                payload,
            })
            .await
            .expect_err("move conflicts");
        assert!(matches!(
            error,
            ProjectServiceError::Conflict { conflicting_project_id, .. }
                if conflicting_project_id == pid("project:first")
        ));
    }

    #[tokio::test]
    async fn treats_a_deleted_project_as_missing_for_updates_and_new_deletes() {
        let h = Harness::new();
        let root = h.root("gone");
        h.service
            .create(h.create("command:gone", "project:gone", "Gone", &root))
            .await
            .expect("create");
        h.service
            .delete(h.delete("command:delete", "project:gone"))
            .await
            .expect("delete");

        let update = h
            .service
            .update(h.update_title("command:after-delete", "project:gone", "Renamed"))
            .await
            .expect_err("update of a deleted project");
        assert!(matches!(update, ProjectServiceError::NotFound { .. }));

        let second_delete = h
            .service
            .delete(h.delete("command:second-delete", "project:gone"))
            .await
            .expect_err("new delete of a deleted project");
        assert!(matches!(
            second_delete,
            ProjectServiceError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn returns_the_deleted_project_when_a_completed_delete_is_retried() {
        let h = Harness::new();
        let root = h.root("retry");
        h.service
            .create(h.create("command:retry", "project:retry", "Retry", &root))
            .await
            .expect("create");
        let first = h
            .service
            .delete(h.delete("command:delete-retry", "project:retry"))
            .await
            .expect("delete");
        let again = h
            .service
            .delete(h.delete("command:delete-retry", "project:retry"))
            .await
            .expect("retry replays");
        assert_eq!(again.deleted_at, first.deleted_at);
    }

    #[tokio::test]
    async fn rolls_back_the_event_and_receipt_when_the_row_write_fails() {
        let h = Harness::new();
        let root = h.root("rollback");
        h.store.state.lock().fail_next_write = true;
        let error = h
            .service
            .create(h.create("command:rollback", "project:rollback", "Rollback", &root))
            .await
            .expect_err("write fails");
        assert!(
            matches!(error, ProjectServiceError::Operation(ref op) if op.operation == ProjectOperation::DispatchProjectCommand)
        );
        assert!(
            h.service
                .get_by_id(&pid("project:rollback"), true)
                .await
                .expect("read")
                .is_none()
        );

        // No receipt was stored, so the same command id plans and commits now.
        h.service
            .create(h.create("command:rollback", "project:rollback", "Rollback", &root))
            .await
            .expect("retry commits");
        assert_eq!(h.store.state.lock().events.len(), 1);
    }

    #[tokio::test]
    async fn serializes_two_projects_claiming_the_same_workspace_root() {
        let h = Harness::new();
        let root = h.root("race");
        let results = join_all([
            h.service
                .create(h.create("command:race-a", "project:race-a", "A", &root)),
            h.service
                .create(h.create("command:race-b", "project:race-b", "B", &root)),
        ])
        .await;
        let successes = results.iter().filter(|result| result.is_ok()).count();
        let conflicts = results
            .iter()
            .filter(|result| matches!(result, Err(ProjectServiceError::Conflict { .. })))
            .count();
        assert_eq!((successes, conflicts), (1, 1));
    }

    #[tokio::test]
    async fn refuses_a_non_empty_project_without_force_then_deletes_its_threads_first() {
        let h = Harness::new();
        let root = h.root("threads");
        h.service
            .create(h.create("command:threads", "project:threads", "Threads", &root))
            .await
            .expect("create");
        {
            let mut threads = h.threads.threads.lock();
            threads.push((tid("thread-1"), pid("project:threads")));
            threads.push((tid("thread-2"), pid("project:threads")));
        }

        let refused = h
            .service
            .delete(h.delete("command:delete-threads", "project:threads"))
            .await
            .expect_err("not empty");
        assert!(matches!(refused, ProjectServiceError::NotEmpty { .. }));
        assert!(h.threads.deleted.lock().is_empty());

        let forced = ProjectDeleteInput {
            force: Some(true),
            ..h.delete("command:delete-threads", "project:threads")
        };
        h.service.delete(forced).await.expect("forced delete");
        let deleted: Vec<ThreadId> = h
            .threads
            .deleted
            .lock()
            .iter()
            .map(|(_, _, thread)| thread.clone())
            .collect();
        assert_eq!(deleted, vec![tid("thread-1"), tid("thread-2")]);
        assert_eq!(*h.settings.cleared.lock(), vec![pid("project:threads")]);
    }

    #[tokio::test]
    async fn retries_a_partial_thread_cascade_under_the_same_delete_command() {
        let h = Harness::new();
        let root = h.root("cascade");
        h.service
            .create(h.create("command:cascade", "project:cascade", "Cascade", &root))
            .await
            .expect("create");
        {
            let mut threads = h.threads.threads.lock();
            threads.push((tid("thread-a"), pid("project:cascade")));
            threads.push((tid("thread-b"), pid("project:cascade")));
        }
        h.threads.fail_once.lock().push(tid("thread-b"));
        let forced = ProjectDeleteInput {
            force: Some(true),
            ..h.delete("command:cascade-delete", "project:cascade")
        };

        let error = h
            .service
            .delete(forced.clone())
            .await
            .expect_err("cascade interrupted");
        assert!(
            matches!(error, ProjectServiceError::Operation(ref op) if op.operation == ProjectOperation::DeleteThread)
        );
        assert!(h.settings.cleared.lock().is_empty());

        h.service
            .delete(forced)
            .await
            .expect("retry completes the cascade");
        assert!(h.threads.threads.lock().is_empty());
        assert_eq!(*h.settings.cleared.lock(), vec![pid("project:cascade")]);
        // Each thread is attempted under the project command's id, so the retry
        // resumes the same cascade rather than starting a new one.
        let commands: Vec<CommandId> = h
            .threads
            .deleted
            .lock()
            .iter()
            .map(|(_, command, _)| command.clone())
            .collect();
        assert!(
            commands
                .iter()
                .all(|command| command == &cid("command:cascade-delete"))
        );
    }
}
