//! Port of upstream `ProjectCommands.test.ts`.

use super::*;
use serde_json::json;

const PROJECT: &str = "project-scripts";

fn now() -> UtcDateTime {
    "2026-01-01T00:00:00.000Z".parse().unwrap()
}

fn project_id() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn command_id(id: &str) -> CommandId {
    CommandId::new(id).unwrap()
}

fn row() -> ProjectCommandRow {
    ProjectCommandRow {
        project_id: project_id(),
        scripts: vec![],
        deleted_at: None,
    }
}

fn script(id: &str) -> ProjectScript {
    serde_json::from_value(json!({
        "id": id, "name": "Install dependencies", "command": "vp i",
        "icon": "configure", "runOnWorktreeCreate": false,
    }))
    .unwrap()
}

fn plan(
    command: &ProjectCommand,
    state: ProjectCommandState,
) -> Result<ProjectEventInput, ProjectCommandRejection> {
    plan_project_command(
        command,
        &state,
        EventId::new("event:planned").unwrap(),
        now(),
    )
}

fn meta() -> ProjectMetaUpdateCommand {
    ProjectMetaUpdateCommand {
        command_id: command_id("cmd-update"),
        project_id: project_id(),
        title: None,
        workspace_root: None,
        default_model_selection: None,
        default_thread_env_mode: None,
        auto_pull: None,
        favicon_path: None,
        project_icon: None,
        scripts: None,
    }
}

fn update(fields: ProjectMetaUpdateCommand) -> Result<ProjectEventInput, ProjectCommandRejection> {
    plan(
        &ProjectCommand::MetaUpdate(fields),
        ProjectCommandState {
            project: Some(row()),
            workspace_owner: None,
        },
    )
}

fn payload_of(result: Result<ProjectEventInput, ProjectCommandRejection>) -> Value {
    result.expect("expected a planned event").payload
}

fn failure_of(
    result: Result<ProjectEventInput, ProjectCommandRejection>,
) -> ProjectCommandRejection {
    result.expect_err("expected a rejection")
}

fn create(id: &str, project: &str, root: &str) -> ProjectCommand {
    ProjectCommand::Create(ProjectCreateCommand {
        command_id: command_id(id),
        project_id: ProjectId::new(project).unwrap(),
        title: "Scripts".into(),
        workspace_root: root.into(),
        scripts: None,
    })
}

#[test]
fn creates_projects_with_empty_scripts_and_no_model_default() {
    let event = plan(
        &create("cmd-create", PROJECT, "/tmp/scripts"),
        ProjectCommandState::default(),
    )
    .unwrap();
    assert_eq!(event.event_type, "project.created");
    assert_eq!(event.occurred_at, "2026-01-01T00:00:00.000Z");
    assert_eq!(event.payload["scripts"], json!([]));
    assert!(event.payload["defaultModelSelection"].is_null());
    assert_eq!(event.correlation_id, Some(command_id("cmd-create")));
    assert_eq!(event.metadata, json!({}));
}

#[test]
fn only_treats_metadata_updates_as_explicit_model_defaults() {
    let selection = json!({
        "instanceId": "codex", "model": "gpt-5.6-sol",
        "options": [{"id": "reasoningEffort", "value": "high"}],
    });
    let payload = payload_of(update(ProjectMetaUpdateCommand {
        default_model_selection: Some(Some(serde_json::from_value(selection.clone()).unwrap())),
        ..meta()
    }));
    assert_eq!(payload["defaultModelSelection"], selection);
}

#[test]
fn carries_every_edited_field_and_omits_the_rest() {
    let icon = json!({"kind": "lucide", "name": "alarm-clock", "color": "violet"});
    let payload = payload_of(update(ProjectMetaUpdateCommand {
        scripts: Some(vec![script("lint")]),
        default_thread_env_mode: Some(Some(serde_json::from_value(json!("worktree")).unwrap())),
        auto_pull: Some(true),
        favicon_path: Some(Some("brand/icon.svg".into())),
        project_icon: Some(Some(serde_json::from_value(icon.clone()).unwrap())),
        ..meta()
    }));
    assert_eq!(payload["scripts"], json!([script("lint")]));
    assert_eq!(payload["defaultThreadEnvMode"], "worktree");
    assert_eq!(payload["autoPull"], true);
    assert_eq!(payload["faviconPath"], "brand/icon.svg");
    assert_eq!(payload["projectIcon"], icon);

    let renamed = payload_of(update(ProjectMetaUpdateCommand {
        title: Some("Renamed".into()),
        ..meta()
    }));
    assert!(renamed.get("defaultThreadEnvMode").is_none());
    let cleared = payload_of(update(ProjectMetaUpdateCommand {
        default_thread_env_mode: Some(None),
        ..meta()
    }));
    assert!(cleared["defaultThreadEnvMode"].is_null());
    assert!(cleared.get("defaultThreadEnvMode").is_some());
}

#[test]
fn rejects_a_new_script_id_that_cannot_have_a_shortcut() {
    let long = "a".repeat(25);
    for id in [
        "install-javascript-dependencies",
        "A",
        "a.b",
        "a b",
        "-a",
        long.as_str(),
    ] {
        let failure = failure_of(update(ProjectMetaUpdateCommand {
            scripts: Some(vec![script("lint"), script(id)]),
            ..meta()
        }));
        assert_eq!(failure.tag(), "ProjectCommandInvariantError", "{id}");
        let message = failure.to_string();
        assert!(message.contains("Script ID"), "{message}");
        assert!(message.contains("24"), "{message}");
        // The detail is persisted in the rejected receipt, so it omits the raw ID.
        assert!(!message.contains(&format!("'{id}'")), "{message}");
    }
}

#[test]
fn accepts_a_script_id_at_the_shortcut_length_limit() {
    let scripts = vec![script(&"a".repeat(24))];
    let payload = payload_of(update(ProjectMetaUpdateCommand {
        scripts: Some(scripts.clone()),
        ..meta()
    }));
    assert_eq!(payload["scripts"], json!(scripts));
}

#[test]
fn keeps_legacy_scripts_editable_and_removable_while_rejecting_new_invalid_ones() {
    let legacy = script("install-javascript-dependencies");
    let with_legacy = |scripts: Vec<ProjectScript>| {
        plan(
            &ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
                command_id: command_id("cmd-repair-script"),
                scripts: Some(scripts),
                ..meta()
            }),
            ProjectCommandState {
                project: Some(ProjectCommandRow {
                    scripts: vec![legacy.clone()],
                    ..row()
                }),
                workspace_owner: None,
            },
        )
    };
    let mut edited = legacy.clone();
    edited.command = turin_contracts::TrimmedNonEmptyString::new("vp install").unwrap();
    for scripts in [vec![edited, script("lint")], vec![]] {
        assert_eq!(
            payload_of(with_legacy(scripts.clone()))["scripts"],
            json!(scripts)
        );
    }
    assert_eq!(
        failure_of(with_legacy(vec![
            legacy.clone(),
            script("another.invalid.id")
        ]))
        .tag(),
        "ProjectCommandInvariantError"
    );
}

#[test]
fn limits_monograms_to_two_graphemes() {
    let monogram = |text: &str| {
        serde_json::from_value::<ProjectIconOverride>(
            json!({"kind": "monogram", "text": text, "color": "violet"}),
        )
        .unwrap()
    };
    for text in ["T3", "é", "किखि", "क्ष्म", "각"] {
        let icon = monogram(text);
        let payload = payload_of(update(ProjectMetaUpdateCommand {
            project_icon: Some(Some(icon.clone())),
            ..meta()
        }));
        assert_eq!(payload["projectIcon"], json!(icon), "{text}");
    }
    for text in ["ABC", "किखिगि"] {
        assert_eq!(
            failure_of(update(ProjectMetaUpdateCommand {
                project_icon: Some(Some(monogram(text))),
                ..meta()
            }))
            .tag(),
            "ProjectCommandInvariantError",
            "{text}"
        );
    }
}

#[test]
fn rejects_a_workspace_root_held_by_another_active_project() {
    let owner = ProjectCommandRow {
        project_id: ProjectId::new("project-existing").unwrap(),
        ..row()
    };
    let rejected = failure_of(plan(
        &create(
            "cmd-duplicate-root",
            "project-duplicate-root",
            "/tmp/project",
        ),
        ProjectCommandState {
            project: None,
            workspace_owner: Some(owner.clone()),
        },
    ));
    assert_eq!(rejected.tag(), "ProjectWorkspaceConflictError");
    assert_eq!(
        rejected.to_string(),
        "Active project 'project-existing' already exists for workspace root '/tmp/project'."
    );
    let moved = failure_of(plan(
        &ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
            command_id: command_id("cmd-move-root"),
            workspace_root: Some("/tmp/project".into()),
            ..meta()
        }),
        ProjectCommandState {
            project: Some(row()),
            workspace_owner: Some(owner),
        },
    ));
    assert_eq!(moved.tag(), "ProjectWorkspaceConflictError");
}

#[test]
fn requires_the_project_to_exist_and_to_be_absent_on_create() {
    let twice = create("cmd-create-twice", PROJECT, "/tmp/twice");
    let existing = ProjectCommandState {
        project: Some(row()),
        workspace_owner: None,
    };
    assert!(
        failure_of(plan(&twice, existing))
            .to_string()
            .contains("cannot be created twice")
    );
    let deleted = ProjectCommandRow {
        deleted_at: Some("2026-01-01T00:00:00.000Z".into()),
        ..row()
    };
    let deleted_state = ProjectCommandState {
        project: Some(deleted.clone()),
        workspace_owner: None,
    };
    assert!(
        failure_of(plan(&twice, deleted_state))
            .to_string()
            .contains("cannot be created twice")
    );
    let commands = [
        ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
            command_id: command_id("cmd-missing"),
            ..meta()
        }),
        ProjectCommand::Delete(ProjectDeleteCommand {
            command_id: command_id("cmd-missing"),
            project_id: project_id(),
        }),
    ];
    for command in &commands {
        for project in [None, Some(deleted.clone())] {
            let state = ProjectCommandState {
                project,
                workspace_owner: None,
            };
            assert_eq!(
                failure_of(plan(command, state)).tag(),
                "ProjectCommandMissingProjectError"
            );
        }
    }
}

#[test]
fn deletes_with_a_single_project_deleted_event() {
    let event = plan(
        &ProjectCommand::Delete(ProjectDeleteCommand {
            command_id: command_id("cmd-delete"),
            project_id: project_id(),
        }),
        ProjectCommandState {
            project: Some(row()),
            workspace_owner: None,
        },
    )
    .unwrap();
    assert_eq!(event.event_type, "project.deleted");
    assert_eq!(
        event.payload,
        json!({"projectId": PROJECT, "deletedAt": "2026-01-01T00:00:00.000Z"})
    );
}

#[test]
fn rejections_round_trip_through_receipt_json() {
    let rejection = ProjectCommandRejection::MissingProject {
        command_type: "project.delete".into(),
        project_id: project_id(),
    };
    let encoded = encode_project_command_rejection(&rejection);
    assert_eq!(
        serde_json::from_str::<Value>(&encoded).unwrap(),
        json!({"_tag": "ProjectCommandMissingProjectError", "commandType": "project.delete", "projectId": PROJECT})
    );
    assert_eq!(decode_project_command_rejection(&encoded), Some(rejection));
    assert_eq!(decode_project_command_rejection("legacy text"), None);
}
