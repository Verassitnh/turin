//! `ManagedProjectFolders` (upstream `project/ManagedProjectFolders.ts`) wired
//! into the runtime: the Scratch project ("No project") and its per-thread
//! folders. The service creates projects through the narrow
//! [`ProjectServiceApi`], which the app serves over the project store and the
//! same command planner `Commands::create_project` uses.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::project::{
    Project, ProjectCreatePayload, ProjectSnapshot, ProjectUpdatePayload, ReceivedProjectIcon,
};
use turin_contracts::{CommandId, ProjectId};
use turin_orchestration::projection::project_store::ProjectRow;
use turin_orchestration::pipeline::project_commands::{
    ProjectCommand, ProjectCreateCommand, ProjectMetaUpdateCommand,
};
use turin_projects::managed_project_folders::ManagedProjectFolders;
use turin_projects::project_service::{
    ProjectBootstrap, ProjectCreateInput, ProjectDeleteInput, ProjectOperation,
    ProjectOperationError, ProjectServiceApi, ProjectServiceError, ProjectUpdateInput,
};
use turin_projects::testing::SystemProbe;

use super::commands::commit_project_command;
use crate::app_runtime::Stores;

/// The service over the same base directory as the palette's New project
/// (`<base>/projects`): Scratch lives in `<base>/scratch`.
pub fn build(stores: Arc<Stores>, data_dir: &Path) -> Arc<ManagedProjectFolders> {
    use crate::components::command_palette::named_project::{managed_base_dir, system_git};
    Arc::new(ManagedProjectFolders::new(
        managed_base_dir(data_dir),
        Arc::new(SystemProbe),
        // The user's own git environment (identity, default branch).
        system_git(),
        Arc::new(RuntimeProjects { stores }),
    ))
}

/// The project operations managed folders use, over the stores.
struct RuntimeProjects {
    stores: Arc<Stores>,
}

fn operation_error(
    operation: ProjectOperation,
    project_id: Option<&ProjectId>,
    workspace_root: Option<&str>,
    cause: impl Into<anyhow::Error>,
) -> ProjectOperationError {
    ProjectOperationError {
        operation,
        project_id: project_id.cloned(),
        workspace_root: workspace_root.map(str::to_owned),
        cause: cause.into(),
    }
}

fn trimmed(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("project store row fields are non-empty")
}

fn project_from_row(row: ProjectRow) -> Project {
    Project {
        id: row.project_id,
        title: trimmed(&row.title),
        workspace_root: trimmed(&row.workspace_root),
        repository_identity: None,
        favicon_path: Some(
            row.favicon_path
                .and_then(|path| TrimmedNonEmptyString::new(&path).ok()),
        ),
        project_icon: Some(
            row.project_icon
                .map(|icon| ReceivedProjectIcon(Some(icon.0))),
        ),
        default_model_selection: row.default_model_selection,
        default_thread_env_mode: Some(row.default_thread_env_mode),
        auto_pull: Some(row.auto_pull),
        scripts: row.scripts,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
    }
}

fn create_command(
    command_id: CommandId,
    project_id: ProjectId,
    payload: ProjectCreatePayload,
) -> ProjectCommand {
    ProjectCommand::Create(ProjectCreateCommand {
        command_id,
        project_id,
        title: payload.title.to_string(),
        workspace_root: payload.workspace_root.to_string(),
        scripts: payload.scripts,
    })
}

fn update_command(
    command_id: CommandId,
    project_id: ProjectId,
    payload: ProjectUpdatePayload,
) -> ProjectCommand {
    ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
        command_id,
        project_id,
        title: payload.title.map(|title| title.to_string()),
        workspace_root: payload.workspace_root.map(|root| root.to_string()),
        default_model_selection: payload.default_model_selection,
        default_thread_env_mode: payload.default_thread_env_mode,
        auto_pull: payload.auto_pull,
        favicon_path: payload
            .favicon_path
            .map(|path| path.map(|path| path.to_string())),
        project_icon: payload.project_icon,
        scripts: payload.scripts,
    })
}

impl RuntimeProjects {
    async fn commit(
        &self,
        project_id: &ProjectId,
        workspace_root: Option<&str>,
        command: ProjectCommand,
    ) -> Result<Project, ProjectServiceError> {
        commit_project_command(&self.stores, command)
            .await
            .map_err(|error| {
                ProjectServiceError::Operation(operation_error(
                    ProjectOperation::DispatchProjectCommand,
                    Some(project_id),
                    workspace_root,
                    anyhow::anyhow!(error.message),
                ))
            })?;
        self.get_by_id(project_id, false)
            .await?
            .ok_or_else(|| ProjectServiceError::NotFound {
                project_id: project_id.clone(),
            })
    }
}

#[async_trait]
impl ProjectServiceApi for RuntimeProjects {
    async fn create(&self, input: ProjectCreateInput) -> Result<Project, ProjectServiceError> {
        let workspace_root = input.payload.workspace_root.to_string();
        let command = create_command(input.command_id, input.project_id.clone(), input.payload);
        self.commit(&input.project_id, Some(&workspace_root), command)
            .await
    }

    /// The project of the workspace root, created when there is none.
    async fn bootstrap(
        &self,
        input: ProjectCreateInput,
    ) -> Result<ProjectBootstrap, ProjectServiceError> {
        let workspace_root = input.payload.workspace_root.to_string();
        if let Some(existing) = self.get_by_workspace_root(&workspace_root, false).await? {
            return Ok(ProjectBootstrap {
                project: existing,
                created: false,
            });
        }
        let project_id = input.project_id.clone();
        match self.create(input).await {
            Ok(project) => Ok(ProjectBootstrap {
                project,
                created: true,
            }),
            // A racing create of the same root wins; use its project.
            Err(error) => match self.get_by_workspace_root(&workspace_root, false).await? {
                Some(winner) if winner.id != project_id => Err(ProjectServiceError::Conflict {
                    project_id,
                    workspace_root,
                    conflicting_project_id: winner.id,
                }),
                Some(winner) => Ok(ProjectBootstrap {
                    project: winner,
                    created: false,
                }),
                None => Err(error),
            },
        }
    }

    async fn update(&self, input: ProjectUpdateInput) -> Result<Project, ProjectServiceError> {
        let command = update_command(input.command_id, input.project_id.clone(), input.payload);
        self.commit(&input.project_id, None, command).await
    }

    async fn delete(&self, input: ProjectDeleteInput) -> Result<Project, ProjectServiceError> {
        // Managed folders never delete projects.
        Err(ProjectServiceError::NotFound {
            project_id: input.project_id,
        })
    }

    async fn get_by_id(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        let row = self
            .stores
            .projects
            .get(project_id.clone(), include_deleted)
            .await
            .map_err(|cause| {
                operation_error(ProjectOperation::ReadProject, Some(project_id), None, cause)
            })?;
        Ok(row.map(project_from_row))
    }

    async fn get_by_workspace_root(
        &self,
        workspace_root: &str,
        _include_deleted: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        let row = self
            .stores
            .projects
            .find_active_by_workspace_root(workspace_root.to_owned())
            .await
            .map_err(|cause| {
                operation_error(
                    ProjectOperation::ReadProject,
                    None,
                    Some(workspace_root),
                    cause,
                )
            })?;
        Ok(row.map(project_from_row))
    }

    async fn snapshot(&self) -> Result<ProjectSnapshot, ProjectOperationError> {
        Err(operation_error(
            ProjectOperation::ListProjects,
            None,
            None,
            anyhow::anyhow!("not used by managed project folders"),
        ))
    }

    async fn get_shell(
        &self,
        _: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, ProjectOperationError> {
        Ok(None)
    }

    async fn list_shells(
        &self,
        _: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, ProjectOperationError> {
        Ok(Vec::new())
    }
}
