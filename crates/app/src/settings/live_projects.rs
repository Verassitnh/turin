//! Project commands that are not on [`Commands`] yet, using the same commit
//! path as `app_runtime::write::commands`.

use turin_contracts::{CommandId, EventId, ProjectId, UtcDateTime};
use turin_orchestration::pipeline::event_sink::ProjectCommit;
use turin_orchestration::pipeline::project_commands::{
    ProjectCommand, ProjectCommandRow, ProjectCommandState, ProjectDeleteCommand,
    ProjectMetaUpdateCommand, plan_project_command,
};

use crate::app_runtime::write::commands::CommandError;
use crate::app_runtime::write::runtime::WriteRuntime;
use crate::state::contracts::ProjectIconOverride;
use crate::state::models::ProjectIcon;

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn command_id() -> CommandId {
    CommandId::new(new_id()).expect("uuid command id")
}

pub fn project_icon_override(icon: &ProjectIcon) -> ProjectIconOverride {
    use crate::state::contracts::ProjectIconOverride as O;
    use turin_contracts::project::{ProjectEmoji, ProjectLucideIconName, ProjectMonogramText};
    match icon {
        ProjectIcon::Monogram { text, color } => O::Monogram {
            text: ProjectMonogramText::new(text.as_str()).expect("monogram text"),
            color: *color,
        },
        ProjectIcon::Emoji(emoji) => O::Emoji {
            emoji: ProjectEmoji::new(emoji.as_str()).expect("emoji"),
        },
        ProjectIcon::Lucide { name, color } => O::Lucide {
            name: ProjectLucideIconName::new(name.as_str()).expect("lucide name"),
            color: *color,
        },
    }
}

pub async fn commit_project(
    runtime: &WriteRuntime,
    command: ProjectCommand,
) -> Result<(), CommandError> {
    let stores = &runtime.stores;
    let project_id = command.project_id().clone();
    let row = |row: turin_orchestration::projection::project_store::ProjectRow| ProjectCommandRow {
        project_id: row.project_id,
        scripts: row.scripts,
        deleted_at: row.deleted_at,
    };
    let project = stores
        .projects
        .get(project_id.clone(), true)
        .await
        .map_err(|error| CommandError::new(error.to_string()))?
        .map(row);
    let workspace_root = match &command {
        ProjectCommand::Create(create) => Some(create.workspace_root.clone()),
        ProjectCommand::MetaUpdate(update) => update.workspace_root.clone(),
        ProjectCommand::Delete(_) => None,
    };
    let workspace_owner = match workspace_root {
        Some(root) => stores
            .projects
            .find_active_by_workspace_root(root)
            .await
            .map_err(|error| CommandError::new(error.to_string()))?
            .map(row),
        None => None,
    };
    let now = UtcDateTime::now();
    let event = plan_project_command(
        &command,
        &ProjectCommandState {
            project,
            workspace_owner,
        },
        EventId::new(new_id()).expect("uuid event id"),
        now,
    )
    .map_err(|rejection| CommandError::new(rejection.to_string()))?;
    stores
        .sink
        .commit_project_command(ProjectCommit {
            command_id: command.command_id().clone(),
            project_id,
            command_type: command.command_type().to_owned(),
            accepted_at: now,
            event,
        })
        .await
        .map_err(|error| CommandError::new(error.to_string()))?;
    Ok(())
}

pub async fn delete_project(
    runtime: &WriteRuntime,
    project_id: &ProjectId,
) -> Result<(), CommandError> {
    commit_project(
        runtime,
        ProjectCommand::Delete(ProjectDeleteCommand {
            command_id: command_id(),
            project_id: project_id.clone(),
        }),
    )
    .await
}

pub async fn set_project_icons(
    runtime: &WriteRuntime,
    project_ids: &[ProjectId],
    favicon_path: Option<String>,
    project_icon: Option<ProjectIcon>,
) -> Result<(), CommandError> {
    let override_icon = project_icon.as_ref().map(project_icon_override);
    for project_id in project_ids {
        commit_project(
            runtime,
            ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
                command_id: command_id(),
                project_id: project_id.clone(),
                title: None,
                workspace_root: None,
                default_model_selection: None,
                default_thread_env_mode: None,
                auto_pull: None,
                favicon_path: Some(favicon_path.clone()),
                project_icon: Some(override_icon.clone()),
                scripts: None,
            }),
        )
        .await?;
    }
    Ok(())
}
