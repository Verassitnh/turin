//! Port of upstream/apps/server/src/orchestration-v2/ProjectCommands.ts

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::keybindings::{KeybindingCommand, MAX_SCRIPT_ID_LENGTH};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::project::{ProjectIconOverride, ProjectScript};
use turin_contracts::{CommandId, EventId, ProjectId, UtcDateTime, js_len};
use turin_persistence::ProjectEventInput;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCreateCommand {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    pub scripts: Option<Vec<ProjectScript>>,
}

/// Absent fields are left unchanged; `Some(None)` clears a nullable field.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectMetaUpdateCommand {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub title: Option<String>,
    pub workspace_root: Option<String>,
    pub default_model_selection: Option<Option<ModelSelection>>,
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    pub auto_pull: Option<bool>,
    pub favicon_path: Option<Option<String>>,
    pub project_icon: Option<Option<ProjectIconOverride>>,
    pub scripts: Option<Vec<ProjectScript>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectDeleteCommand {
    pub command_id: CommandId,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectCommand {
    Create(ProjectCreateCommand),
    MetaUpdate(ProjectMetaUpdateCommand),
    Delete(ProjectDeleteCommand),
}

impl ProjectCommand {
    pub fn command_type(&self) -> &'static str {
        match self {
            Self::Create(_) => "project.create",
            Self::MetaUpdate(_) => "project.meta.update",
            Self::Delete(_) => "project.delete",
        }
    }

    pub fn command_id(&self) -> &CommandId {
        match self {
            Self::Create(command) => &command.command_id,
            Self::MetaUpdate(command) => &command.command_id,
            Self::Delete(command) => &command.command_id,
        }
    }

    pub fn project_id(&self) -> &ProjectId {
        match self {
            Self::Create(command) => &command.project_id,
            Self::MetaUpdate(command) => &command.project_id,
            Self::Delete(command) => &command.project_id,
        }
    }
}

/// `ProjectCommandRejection`: the union of the three tagged errors. A
/// rejected receipt stores it as JSON, so a retried command id replays the
/// same typed error even when a fresh plan would now succeed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum ProjectCommandRejection {
    #[error("Project command invariant failed ({command_type}): {detail}")]
    #[serde(rename = "ProjectCommandInvariantError", rename_all = "camelCase")]
    Invariant {
        command_type: String,
        detail: String,
    },
    /// The command targets a project that does not exist or was deleted.
    #[error("Project '{project_id}' does not exist for command '{command_type}'.")]
    #[serde(rename = "ProjectCommandMissingProjectError", rename_all = "camelCase")]
    MissingProject {
        command_type: String,
        project_id: ProjectId,
    },
    #[error(
        "Active project '{conflicting_project_id}' already exists for workspace root '{workspace_root}'."
    )]
    #[serde(rename = "ProjectWorkspaceConflictError", rename_all = "camelCase")]
    WorkspaceConflict {
        workspace_root: String,
        conflicting_project_id: ProjectId,
    },
}

impl ProjectCommandRejection {
    /// Upstream's `_tag`.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Invariant { .. } => "ProjectCommandInvariantError",
            Self::MissingProject { .. } => "ProjectCommandMissingProjectError",
            Self::WorkspaceConflict { .. } => "ProjectWorkspaceConflictError",
        }
    }
}

pub fn encode_project_command_rejection(rejection: &ProjectCommandRejection) -> String {
    serde_json::to_string(rejection).expect("rejection encodes")
}

/// `None` for receipts that predate structured rejections.
pub fn decode_project_command_rejection(raw: &str) -> Option<ProjectCommandRejection> {
    serde_json::from_str(raw).ok()
}

/// The fields of upstream `ProjectRow` the planner reads (the full row is
/// T13a's `projection::project_store::ProjectRow`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCommandRow {
    pub project_id: ProjectId,
    pub scripts: Vec<ProjectScript>,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectCommandState {
    /// The target project's row, including a soft-deleted one; only create
    /// sees deleted rows as taken.
    pub project: Option<ProjectCommandRow>,
    /// The active project that holds the command's requested workspace root,
    /// if any.
    pub workspace_owner: Option<ProjectCommandRow>,
}

fn is_script_run_command(value: &str) -> bool {
    KeybindingCommand::new(value).is_ok_and(|command| command.is_script_run())
}

/// Decide one project command against the rows it touches. The caller reads
/// `state` under the project's lock and commits the planned event.
pub fn plan_project_command(
    command: &ProjectCommand,
    state: &ProjectCommandState,
    event_id: EventId,
    now: UtcDateTime,
) -> Result<ProjectEventInput, ProjectCommandRejection> {
    let command_type = command.command_type();
    let project_id = command.project_id();
    let invariant = |detail: String| ProjectCommandRejection::Invariant {
        command_type: command_type.to_owned(),
        detail,
    };
    let missing_project = || ProjectCommandRejection::MissingProject {
        command_type: command_type.to_owned(),
        project_id: project_id.clone(),
    };
    let active_project = state
        .project
        .as_ref()
        .filter(|project| project.deleted_at.is_none());
    let require_workspace_available = |workspace_root: &str| match state.workspace_owner.as_ref() {
        Some(owner) if &owner.project_id != project_id => {
            Err(ProjectCommandRejection::WorkspaceConflict {
                workspace_root: workspace_root.to_owned(),
                conflicting_project_id: owner.project_id.clone(),
            })
        }
        _ => Ok(()),
    };
    let occurred_at = now.to_string();
    let event = |event_type: &str, payload: Value| ProjectEventInput {
        event_id,
        aggregate_id: project_id.to_string(),
        event_type: event_type.to_owned(),
        occurred_at: occurred_at.clone(),
        command_id: Some(command.command_id().clone()),
        causation_event_id: None,
        correlation_id: Some(command.command_id().clone()),
        payload,
        metadata: json!({}),
    };

    match command {
        ProjectCommand::Create(create) => {
            if state.project.is_some() {
                return Err(invariant(format!(
                    "Project '{project_id}' already exists and cannot be created twice."
                )));
            }
            require_workspace_available(&create.workspace_root)?;
            Ok(event(
                "project.created",
                json!({
                    "projectId": project_id,
                    "title": create.title,
                    "workspaceRoot": create.workspace_root,
                    // Project creation has no user model choice. Older clients sent an
                    // automatic seed, but only a metadata update records an explicit default.
                    "defaultModelSelection": null,
                    "faviconPath": null,
                    "projectIcon": null,
                    "scripts": create.scripts.clone().unwrap_or_default(),
                    "createdAt": occurred_at,
                    "updatedAt": occurred_at,
                }),
            ))
        }

        ProjectCommand::MetaUpdate(update) => {
            let Some(project) = active_project else {
                return Err(missing_project());
            };
            if let Some(Some(ProjectIconOverride::Monogram { text, .. })) = &update.project_icon
                && text.as_str().graphemes(true).count() > 2
            {
                return Err(invariant(
                    "Project monograms must contain at most two characters.".to_owned(),
                ));
            }
            if let Some(scripts) = &update.scripts {
                // Persisted IDs predate shortcut validation. Let users edit or remove them
                // without allowing another invalid ID to enter the project.
                let existing_ids: HashSet<&str> = project
                    .scripts
                    .iter()
                    .map(|script| script.id.as_str())
                    .collect();
                for script in scripts {
                    let id = script.id.as_str();
                    if !existing_ids.contains(id)
                        && !is_script_run_command(&format!("script.{id}.run"))
                    {
                        // The raw ID is unbounded user input and this detail is persisted.
                        return Err(invariant(format!(
                            "Script IDs must be 1-{MAX_SCRIPT_ID_LENGTH} lowercase letters, digits or hyphens, starting with a letter or digit (got {} characters).",
                            js_len(id)
                        )));
                    }
                }
            }
            if let Some(workspace_root) = &update.workspace_root {
                require_workspace_available(workspace_root)?;
            }
            let mut payload = Map::new();
            payload.insert("projectId".into(), json!(project_id));
            let mut set = |key: &str, value: Value| {
                payload.insert(key.to_owned(), value);
            };
            if let Some(title) = &update.title {
                set("title", json!(title));
            }
            if let Some(workspace_root) = &update.workspace_root {
                set("workspaceRoot", json!(workspace_root));
            }
            if let Some(selection) = &update.default_model_selection {
                set("defaultModelSelection", json!(selection));
            }
            if let Some(mode) = &update.default_thread_env_mode {
                set("defaultThreadEnvMode", json!(mode));
            }
            if let Some(auto_pull) = update.auto_pull {
                set("autoPull", json!(auto_pull));
            }
            if let Some(favicon_path) = &update.favicon_path {
                set("faviconPath", json!(favicon_path));
            }
            if let Some(icon) = &update.project_icon {
                set("projectIcon", json!(icon));
            }
            if let Some(scripts) = &update.scripts {
                set("scripts", json!(scripts));
            }
            set("updatedAt", json!(occurred_at));
            Ok(event("project.meta-updated", Value::Object(payload)))
        }

        ProjectCommand::Delete(_) => {
            if active_project.is_none() {
                return Err(missing_project());
            }
            // Thread children are deleted by ProjectService before this event commits.
            Ok(event(
                "project.deleted",
                json!({ "projectId": project_id, "deletedAt": occurred_at }),
            ))
        }
    }
}

#[cfg(test)]
mod tests;
