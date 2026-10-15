//! Port of upstream/apps/server/src/orchestration-v2/ProjectCommands.ts
//!
//! Decide one project command against the rows it touches, returning the event
//! to commit or a typed rejection.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::keybindings::MAX_SCRIPT_ID_LENGTH;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::project::{ProjectIconOverride, ProjectScript};
use turin_contracts::{CommandId, EventId, ProjectId, UtcDateTime};
use turin_persistence::ProjectEventInput;
use unicode_segmentation::UnicodeSegmentation;

use crate::projection::project_store::ProjectRow;

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCreateCommand {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    pub scripts: Option<Vec<ProjectScript>>,
}

/// A field that may be left alone (`None`), cleared (`Some(None)`) or set.
pub type Edit<T> = Option<Option<T>>;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectMetaUpdateCommand {
    pub command_id: Option<CommandId>,
    pub project_id: Option<ProjectId>,
    pub title: Option<String>,
    pub workspace_root: Option<String>,
    pub default_model_selection: Edit<ModelSelection>,
    pub default_thread_env_mode: Edit<ThreadEnvMode>,
    pub auto_pull: Option<bool>,
    pub favicon_path: Edit<String>,
    pub project_icon: Edit<ProjectIconOverride>,
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
    fn command_type(&self) -> &'static str {
        match self {
            Self::Create(_) => "project.create",
            Self::MetaUpdate(_) => "project.meta.update",
            Self::Delete(_) => "project.delete",
        }
    }

    pub fn command_id(&self) -> &CommandId {
        match self {
            Self::Create(command) => &command.command_id,
            Self::MetaUpdate(command) => command
                .command_id
                .as_ref()
                .expect("meta update carries a command id"),
            Self::Delete(command) => &command.command_id,
        }
    }

    pub fn project_id(&self) -> &ProjectId {
        match self {
            Self::Create(command) => &command.project_id,
            Self::MetaUpdate(command) => command
                .project_id
                .as_ref()
                .expect("meta update carries a project id"),
            Self::Delete(command) => &command.project_id,
        }
    }
}

/// Why a project command was refused. Stored in the rejected receipt as JSON
/// (`_tag`-tagged, like upstream's `Schema.TaggedError`s) so a retried command
/// id replays the same typed error even when a fresh plan would now succeed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all_fields = "camelCase")]
pub enum ProjectCommandRejection {
    #[serde(rename = "ProjectCommandInvariantError")]
    #[error("Project command invariant failed ({command_type}): {detail}")]
    Invariant {
        command_type: String,
        detail: String,
    },
    /// The command targets a project that does not exist or was deleted.
    #[serde(rename = "ProjectCommandMissingProjectError")]
    #[error("Project '{project_id}' does not exist for command '{command_type}'.")]
    MissingProject {
        command_type: String,
        project_id: ProjectId,
    },
    #[serde(rename = "ProjectWorkspaceConflictError")]
    #[error(
        "Active project '{conflicting_project_id}' already exists for workspace root '{workspace_root}'."
    )]
    WorkspaceConflict {
        workspace_root: String,
        conflicting_project_id: ProjectId,
    },
}

/// `encodeProjectCommandRejection`.
pub fn encode_project_command_rejection(rejection: &ProjectCommandRejection) -> String {
    serde_json::to_string(rejection).expect("rejection serializes")
}

/// `decodeProjectCommandRejection`: `None` for receipts that predate structured rejections.
pub fn decode_project_command_rejection(text: &str) -> Option<ProjectCommandRejection> {
    serde_json::from_str(text).ok()
}

/// The rows a command touches.
#[derive(Debug, Clone, Default)]
pub struct ProjectCommandState {
    /// The target project's row, including a soft-deleted one; only create
    /// sees deleted rows as taken.
    pub project: Option<ProjectRow>,
    /// The active project that holds the command's requested workspace root, if any.
    pub workspace_owner: Option<ProjectRow>,
}

/// `isScriptRunCommand("script.<id>.run")`: `^[a-z0-9][a-z0-9-]*$`, at most 24 chars.
fn is_shortcut_script_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !id.is_empty()
        && id.len() <= MAX_SCRIPT_ID_LENGTH
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn double_option_json<T: Serialize>(value: &Option<T>) -> Value {
    serde_json::to_value(value).expect("payload field serializes")
}

/// Decide one project command against the rows it touches. The caller reads
/// `state` under the project's lock and commits the planned event.
pub fn plan_project_command(
    command: &ProjectCommand,
    state: &ProjectCommandState,
    event_id: EventId,
    now: &UtcDateTime,
) -> Result<ProjectEventInput, ProjectCommandRejection> {
    let command_type = command.command_type().to_owned();
    let project_id = command.project_id();
    let invariant = |detail: String| ProjectCommandRejection::Invariant {
        command_type: command_type.clone(),
        detail,
    };
    let missing_project = || ProjectCommandRejection::MissingProject {
        command_type: command_type.clone(),
        project_id: project_id.clone(),
    };
    let active_project = state
        .project
        .as_ref()
        .filter(|project| project.deleted_at.is_none());
    let require_workspace_available = |workspace_root: &str| match &state.workspace_owner {
        Some(owner) if &owner.project_id != project_id => {
            Err(ProjectCommandRejection::WorkspaceConflict {
                workspace_root: workspace_root.to_owned(),
                conflicting_project_id: owner.project_id.clone(),
            })
        }
        _ => Ok(()),
    };
    let occurred_at = now.to_string();
    let base = |event_type: &str, payload: Value| ProjectEventInput {
        event_id: event_id.clone(),
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
            Ok(base(
                "project.created",
                json!({
                    "projectId": create.project_id,
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
            let project = active_project.ok_or_else(missing_project)?;
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
                let existing: Vec<&str> = project.scripts.iter().map(|s| s.id.as_str()).collect();
                for script in scripts {
                    let id = script.id.as_str();
                    if !existing.contains(&id) && !is_shortcut_script_id(id) {
                        // The raw ID is unbounded user input and this detail is persisted.
                        return Err(invariant(format!(
                            "Script IDs must be 1-{MAX_SCRIPT_ID_LENGTH} lowercase letters, digits or hyphens, starting with a letter or digit (got {} characters).",
                            id.encode_utf16().count()
                        )));
                    }
                }
            }
            if let Some(workspace_root) = &update.workspace_root {
                require_workspace_available(workspace_root)?;
            }
            let mut payload = Map::new();
            payload.insert("projectId".into(), json!(project_id));
            if let Some(title) = &update.title {
                payload.insert("title".into(), json!(title));
            }
            if let Some(root) = &update.workspace_root {
                payload.insert("workspaceRoot".into(), json!(root));
            }
            if let Some(selection) = &update.default_model_selection {
                payload.insert(
                    "defaultModelSelection".into(),
                    double_option_json(selection),
                );
            }
            if let Some(mode) = &update.default_thread_env_mode {
                payload.insert("defaultThreadEnvMode".into(), double_option_json(mode));
            }
            if let Some(auto_pull) = update.auto_pull {
                payload.insert("autoPull".into(), json!(auto_pull));
            }
            if let Some(favicon) = &update.favicon_path {
                payload.insert("faviconPath".into(), double_option_json(favicon));
            }
            if let Some(icon) = &update.project_icon {
                payload.insert("projectIcon".into(), double_option_json(icon));
            }
            if let Some(scripts) = &update.scripts {
                payload.insert("scripts".into(), json!(scripts));
            }
            payload.insert("updatedAt".into(), json!(occurred_at));
            Ok(base("project.meta-updated", Value::Object(payload)))
        }
        ProjectCommand::Delete(_) => {
            if active_project.is_none() {
                return Err(missing_project());
            }
            // Thread children are deleted by ProjectService before this event commits.
            Ok(base(
                "project.deleted",
                json!({"projectId": project_id, "deletedAt": occurred_at}),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    fn pid(id: &str) -> ProjectId {
        ProjectId::new(id).unwrap()
    }
    fn cid(id: &str) -> CommandId {
        CommandId::new(id).unwrap()
    }

    fn row(project_id: &str, workspace_root: &str) -> ProjectRow {
        ProjectRow {
            project_id: pid(project_id),
            title: "Scripts".into(),
            workspace_root: workspace_root.into(),
            default_model_selection: None,
            default_thread_env_mode: None,
            auto_pull: false,
            favicon_path: None,
            project_icon: None,
            scripts: Vec::new(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
            deleted_at: None,
        }
    }

    fn script(id: &str) -> ProjectScript {
        serde_json::from_value(json!({
            "id": id, "name": "Install dependencies", "command": "vp i",
            "icon": "configure", "runOnWorktreeCreate": false
        }))
        .unwrap()
    }

    fn plan(
        command: ProjectCommand,
        state: ProjectCommandState,
    ) -> Result<ProjectEventInput, ProjectCommandRejection> {
        plan_project_command(
            &command,
            &state,
            EventId::new("event:planned").unwrap(),
            &NOW.parse().unwrap(),
        )
    }

    fn state(project: Option<ProjectRow>) -> ProjectCommandState {
        ProjectCommandState {
            project,
            workspace_owner: None,
        }
    }

    fn update(
        edit: impl FnOnce(&mut ProjectMetaUpdateCommand),
    ) -> Result<ProjectEventInput, ProjectCommandRejection> {
        let mut command = ProjectMetaUpdateCommand {
            command_id: Some(cid("cmd-update")),
            project_id: Some(pid("project-scripts")),
            ..Default::default()
        };
        edit(&mut command);
        plan(
            ProjectCommand::MetaUpdate(command),
            state(Some(row("project-scripts", "/tmp/scripts"))),
        )
    }

    fn payload(result: Result<ProjectEventInput, ProjectCommandRejection>) -> Value {
        result.expect("planned").payload
    }

    fn create(project: &str, root: &str) -> ProjectCommand {
        ProjectCommand::Create(ProjectCreateCommand {
            command_id: cid("cmd-create"),
            project_id: pid(project),
            title: "Scripts".into(),
            workspace_root: root.into(),
            scripts: None,
        })
    }

    #[test]
    fn creates_projects_with_empty_scripts_and_no_model_default() {
        let event = plan(create("project-scripts", "/tmp/scripts"), state(None)).unwrap();
        assert_eq!(event.event_type, "project.created");
        assert_eq!(event.occurred_at, NOW);
        assert_eq!(event.payload["scripts"], json!([]));
        assert_eq!(event.payload["defaultModelSelection"], Value::Null);
        assert_eq!(event.correlation_id, Some(cid("cmd-create")));
    }

    #[test]
    fn only_treats_metadata_updates_as_explicit_model_defaults() {
        let selection: ModelSelection = serde_json::from_value(json!({
            "instanceId": "codex", "model": "gpt-5.6-sol",
            "options": [{"id": "reasoningEffort", "value": "high"}]
        }))
        .unwrap();
        let expected = serde_json::to_value(&selection).unwrap();
        let got = payload(update(|c| {
            c.default_model_selection = Some(Some(selection))
        }));
        assert_eq!(got["defaultModelSelection"], expected);
    }

    #[test]
    fn carries_every_edited_field_and_omits_the_rest() {
        let got = payload(update(|c| {
            c.scripts = Some(vec![script("lint")]);
            c.default_thread_env_mode = Some(Some(ThreadEnvMode::Worktree));
            c.auto_pull = Some(true);
            c.favicon_path = Some(Some("brand/icon.svg".into()));
            c.project_icon = Some(Some(
                serde_json::from_value(
                    json!({"kind": "lucide", "name": "alarm-clock", "color": "violet"}),
                )
                .unwrap(),
            ));
        }));
        assert_eq!(got["defaultThreadEnvMode"], json!("worktree"));
        assert_eq!(got["autoPull"], json!(true));
        assert_eq!(got["faviconPath"], json!("brand/icon.svg"));
        assert_eq!(
            got["projectIcon"],
            json!({"kind": "lucide", "name": "alarm-clock", "color": "violet"})
        );
        assert_eq!(got["scripts"].as_array().unwrap().len(), 1);
        let renamed = payload(update(|c| c.title = Some("Renamed".into())));
        assert!(renamed.get("defaultThreadEnvMode").is_none());
        assert_eq!(renamed["title"], json!("Renamed"));
        let cleared = payload(update(|c| c.default_thread_env_mode = Some(None)));
        assert_eq!(cleared["defaultThreadEnvMode"], Value::Null);
    }

    #[test]
    fn rejects_a_new_script_id_that_cannot_have_a_shortcut() {
        for id in [
            "install-javascript-dependencies",
            "A",
            "a.b",
            "a b",
            "-a",
            &"a".repeat(25),
        ] {
            let failure =
                update(|c| c.scripts = Some(vec![script("lint"), script(id)])).unwrap_err();
            let ProjectCommandRejection::Invariant { .. } = &failure else {
                panic!("expected an invariant error for {id}: {failure:?}");
            };
            let message = failure.to_string();
            assert!(message.contains("Script ID"), "{message}");
            assert!(message.contains("24"), "{message}");
            // The detail is persisted in the rejected receipt, so it omits the raw ID.
            assert!(!message.contains(&format!("'{id}'")), "{message}");
        }
    }

    #[test]
    fn accepts_a_script_id_at_the_shortcut_length_limit() {
        let id = "a".repeat(24);
        let got = payload(update(|c| c.scripts = Some(vec![script(&id)])));
        assert_eq!(got["scripts"][0]["id"], json!(id));
    }

    #[test]
    fn keeps_legacy_scripts_editable_and_removable_while_rejecting_new_invalid_ones() {
        let legacy = script("install-javascript-dependencies");
        let mut project = row("project-scripts", "/tmp/scripts");
        project.scripts = vec![legacy.clone()];
        let with_legacy = |scripts: Vec<ProjectScript>| {
            plan(
                ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
                    command_id: Some(cid("cmd-repair-script")),
                    project_id: Some(pid("project-scripts")),
                    scripts: Some(scripts),
                    ..Default::default()
                }),
                state(Some(project.clone())),
            )
        };
        let mut edited = legacy.clone();
        edited.command = turin_contracts::TrimmedNonEmptyString::new("vp install").unwrap();
        for scripts in [vec![edited, script("lint")], vec![]] {
            let expected = serde_json::to_value(&scripts).unwrap();
            assert_eq!(payload(with_legacy(scripts))["scripts"], expected);
        }
        assert!(matches!(
            with_legacy(vec![legacy, script("another.invalid.id")]),
            Err(ProjectCommandRejection::Invariant { .. })
        ));
    }

    #[test]
    fn limits_monograms_to_two_graphemes() {
        let monogram = |text: &str| -> ProjectIconOverride {
            serde_json::from_value(json!({"kind": "monogram", "text": text, "color": "violet"}))
                .unwrap()
        };
        for text in ["T3", "é", "किखि", "क्ष्म", "각"] {
            let got = payload(update(|c| c.project_icon = Some(Some(monogram(text)))));
            assert_eq!(got["projectIcon"]["text"], json!(text), "{text}");
        }
        for text in ["ABC", "किखिगि"] {
            assert!(
                matches!(
                    update(|c| c.project_icon = Some(Some(monogram(text)))),
                    Err(ProjectCommandRejection::Invariant { .. })
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_a_workspace_root_held_by_another_active_project() {
        let owner = row("project-existing", "/tmp/project");
        let failure = plan(
            create("project-duplicate-root", "/tmp/project"),
            ProjectCommandState {
                project: None,
                workspace_owner: Some(owner.clone()),
            },
        )
        .unwrap_err();
        assert!(matches!(
            failure,
            ProjectCommandRejection::WorkspaceConflict { .. }
        ));
        assert_eq!(
            failure.to_string(),
            "Active project 'project-existing' already exists for workspace root '/tmp/project'."
        );
        let moved = plan(
            ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
                command_id: Some(cid("cmd-move-root")),
                project_id: Some(pid("project-scripts")),
                workspace_root: Some("/tmp/project".into()),
                ..Default::default()
            }),
            ProjectCommandState {
                project: Some(row("project-scripts", "/tmp/scripts")),
                workspace_owner: Some(owner),
            },
        );
        assert!(matches!(
            moved,
            Err(ProjectCommandRejection::WorkspaceConflict { .. })
        ));
        // The project's own root is not a conflict.
        let own = row("project-scripts", "/tmp/scripts");
        assert!(
            plan(
                ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
                    command_id: Some(cid("cmd-same-root")),
                    project_id: Some(pid("project-scripts")),
                    workspace_root: Some("/tmp/scripts".into()),
                    ..Default::default()
                }),
                ProjectCommandState {
                    project: Some(own.clone()),
                    workspace_owner: Some(own)
                },
            )
            .is_ok()
        );
    }

    #[test]
    fn requires_the_project_to_exist_and_to_be_absent_on_create() {
        let mut deleted = row("project-scripts", "/tmp/scripts");
        deleted.deleted_at = Some(NOW.into());
        for project in [row("project-scripts", "/tmp/scripts"), deleted.clone()] {
            let failure = plan(
                create("project-scripts", "/tmp/twice"),
                state(Some(project)),
            )
            .unwrap_err();
            assert!(failure.to_string().contains("cannot be created twice"));
        }
        let commands = [
            ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
                command_id: Some(cid("cmd-missing")),
                project_id: Some(pid("project-scripts")),
                ..Default::default()
            }),
            ProjectCommand::Delete(ProjectDeleteCommand {
                command_id: cid("cmd-missing"),
                project_id: pid("project-scripts"),
            }),
        ];
        for command in commands {
            for project in [None, Some(deleted.clone())] {
                assert!(matches!(
                    plan(command.clone(), state(project)),
                    Err(ProjectCommandRejection::MissingProject { .. })
                ));
            }
        }
    }

    #[test]
    fn deletes_with_a_single_project_deleted_event() {
        let event = plan(
            ProjectCommand::Delete(ProjectDeleteCommand {
                command_id: cid("cmd-delete"),
                project_id: pid("project-scripts"),
            }),
            state(Some(row("project-scripts", "/tmp/scripts"))),
        )
        .unwrap();
        assert_eq!(event.event_type, "project.deleted");
        assert_eq!(
            event.payload,
            json!({"projectId": "project-scripts", "deletedAt": NOW})
        );
    }

    #[test]
    fn rejections_round_trip_through_their_receipt_json() {
        let rejection = ProjectCommandRejection::WorkspaceConflict {
            workspace_root: "/tmp/p".into(),
            conflicting_project_id: pid("project-1"),
        };
        let text = encode_project_command_rejection(&rejection);
        assert!(text.contains("\"_tag\":\"ProjectWorkspaceConflictError\""));
        assert_eq!(decode_project_command_rejection(&text), Some(rejection));
        assert_eq!(decode_project_command_rejection("plain text"), None);
    }
}
