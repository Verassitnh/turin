//! "New project" from a name: upstream's `ManagedProjectFolders.createNamedProject`
//! (`turin-projects`) over the write runtime.
//!
//! Upstream has no setting or folder picker for this. The server keeps
//! name-only projects in `<baseDir>/projects/<slug>` (advertised to clients as
//! `serverConfig.newProjectsRoot`), each a new Git repository with a README, an
//! icon and a first commit, and adds `-2`, `-3`, ... when a folder is taken.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::project::{Project, ProjectSnapshot};
use turin_contracts::{ProjectId, TrimmedNonEmptyString};
use turin_projects::managed_project_folders::{
    GitCli, ManagedProjectFolders, NamedProject, RepositoryProbe, RepositoryProbeError,
};
use turin_projects::project_service::{
    ProjectBootstrap, ProjectCreateInput, ProjectDeleteInput, ProjectOperation,
    ProjectOperationError, ProjectServiceApi, ProjectServiceError, ProjectUpdateInput,
};
use turin_projects::testing::SystemGit;

use crate::app_runtime::write::commands::Commands;

/// The base directory the managed folders live under: the data directory's
/// parent for the default layout (`~/.turin/userdata`), else the data
/// directory itself (the flat `TURIN_DATA_DIR` layout).
pub fn managed_base_dir(data_dir: &Path) -> PathBuf {
    match (data_dir.file_name(), data_dir.parent()) {
        (Some(name), Some(parent)) if name == "userdata" => parent.to_path_buf(),
        _ => data_dir.to_path_buf(),
    }
}

/// `serverConfig.newProjectsRoot`: `<baseDir>/projects`.
pub fn new_projects_root(data_dir: &Path) -> PathBuf {
    managed_base_dir(data_dir).join("projects")
}

/// The system `git`, with the user's own configuration.
pub fn system_git() -> Arc<dyn GitCli> {
    Arc::new(SystemGit {
        env: Vec::new(),
        removed_env: Vec::new(),
    })
}

/// Named projects never probe for a Scratch checkout; the probe is unused.
struct NoProbe;

#[async_trait]
impl RepositoryProbe for NoProbe {
    async fn is_repository(&self, _cwd: &str) -> Result<bool, RepositoryProbeError> {
        Ok(false)
    }
}

/// The slice of `ProjectService` that `createNamedProject` calls, over the
/// write path's `project.create` command.
struct CommandsProjects(Commands);

fn unsupported(operation: ProjectOperation) -> ProjectOperationError {
    ProjectOperationError {
        operation,
        project_id: None,
        workspace_root: None,
        cause: anyhow::anyhow!("not used when starting a project from a name"),
    }
}

#[async_trait]
impl ProjectServiceApi for CommandsProjects {
    async fn create(&self, input: ProjectCreateInput) -> Result<Project, ProjectServiceError> {
        let title = input.payload.title.as_str().to_owned();
        let workspace_root = input.payload.workspace_root.as_str().to_owned();
        let project_id = self
            .0
            .create_project_with_id(input.project_id, &title, &workspace_root)
            .await
            .map_err(|error| {
                ProjectServiceError::Operation(ProjectOperationError {
                    operation: ProjectOperation::DispatchProjectCommand,
                    project_id: None,
                    workspace_root: Some(workspace_root.clone()),
                    cause: anyhow::anyhow!(error.message),
                })
            })?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        Ok(Project {
            id: project_id,
            title: input.payload.title,
            workspace_root: input.payload.workspace_root,
            repository_identity: None,
            favicon_path: None,
            project_icon: None,
            default_model_selection: None,
            default_thread_env_mode: None,
            auto_pull: None,
            scripts: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
        })
    }

    async fn bootstrap(
        &self,
        _: ProjectCreateInput,
    ) -> Result<ProjectBootstrap, ProjectServiceError> {
        Err(unsupported(ProjectOperation::DispatchProjectCommand).into())
    }

    async fn update(&self, _: ProjectUpdateInput) -> Result<Project, ProjectServiceError> {
        Err(unsupported(ProjectOperation::DispatchProjectCommand).into())
    }

    async fn delete(&self, _: ProjectDeleteInput) -> Result<Project, ProjectServiceError> {
        Err(unsupported(ProjectOperation::DispatchProjectCommand).into())
    }

    async fn get_by_id(
        &self,
        _: &ProjectId,
        _: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        Ok(None)
    }

    /// Only asked after a failed create, to avoid removing another project's
    /// folder. `None` lets the unused folder go.
    async fn get_by_workspace_root(
        &self,
        _: &str,
        _: bool,
    ) -> Result<Option<Project>, ProjectOperationError> {
        Ok(None)
    }

    async fn snapshot(&self) -> Result<ProjectSnapshot, ProjectOperationError> {
        Err(unsupported(ProjectOperation::ListProjects))
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

/// Starts a project named `name` under `<base_dir>/projects`.
pub async fn create_named_project(
    commands: Commands,
    base_dir: PathBuf,
    git: Arc<dyn GitCli>,
    name: &str,
) -> Result<NamedProject, String> {
    let name =
        TrimmedNonEmptyString::new(name).map_err(|_| "Enter a name for the project.".to_owned())?;
    ManagedProjectFolders::new(
        base_dir,
        Arc::new(NoProbe),
        git,
        Arc::new(CommandsProjects(commands)),
    )
    .create_named_project(name.as_str())
    .await
    .map_err(|error| {
        // The error's own text is generic; its cause says what went wrong.
        match std::error::Error::source(&error) {
            Some(cause) => format!("{error} {cause}"),
            None => error.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_puts_projects_beside_userdata() {
        assert_eq!(
            new_projects_root(Path::new("/Users/me/.turin/userdata")),
            Path::new("/Users/me/.turin/projects")
        );
    }

    #[test]
    fn a_flat_dev_data_dir_keeps_projects_inside_it() {
        assert_eq!(
            new_projects_root(Path::new("/tmp/dev-turin")),
            Path::new("/tmp/dev-turin/projects")
        );
    }
}
