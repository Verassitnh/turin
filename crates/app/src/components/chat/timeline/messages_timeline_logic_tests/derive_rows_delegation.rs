//! A `delegate_task` call is removed from the timeline once its returned task
//! id identifies the child card it started, whatever the output envelope.

use super::*;

const RUN: &str = "resource-run";
const AT: &str = "2026-09-08T10:00:02Z";

#[derive(Clone, Copy, Debug)]
enum Envelope {
    Direct,
    Structured,
    Text,
}

/// An app-owned subagent card whose prompt names its role.
fn child(id: &str, role: &str) -> TimelineEntry {
    let prompt = if role == "general" {
        id.to_string()
    } else {
        format!("Act as the {role} sub-agent for this task.\n\n{id}")
    };
    event_entry(
        id,
        AT,
        projected(
            0,
            TurnItem {
                run_id: Some(RUN.into()),
                ..turn_item(
                    id,
                    AT,
                    TurnItemPayload::Subagent {
                        subagent_id: id.into(),
                        origin: "app_owned".into(),
                        child_thread_id: None,
                        prompt,
                        progress: None,
                        result: None,
                    },
                )
            },
        ),
    )
}

/// The `t3-code.delegate_task` call that started child `task_id`. Its output
/// names the child in the given envelope; a failed call keeps its error tone.
fn delegation(
    id: &str,
    task_id: &str,
    failed: bool,
    running: bool,
    envelope: Envelope,
    role: &str,
) -> TimelineEntry {
    let output = (!running).then(|| {
        let task = json!({ "taskId": task_id });
        match envelope {
            Envelope::Direct => task,
            Envelope::Structured => json!({
                "content": task.to_string(),
                "structuredContent": task,
            }),
            Envelope::Text => json!({
                "content": [{ "type": "text", "text": task.to_string() }],
            }),
        }
    });
    // Upstream sends the task rewritten for `b`; the output is what matches.
    let input = json!({ "task": if task_id == "b" { "a" } else { task_id }, "role": role });
    let item_status = if failed {
        TurnItemStatus::Failed
    } else if running {
        TurnItemStatus::Running
    } else {
        TurnItemStatus::Completed
    };
    let lifecycle = if failed {
        ToolLifecycleStatus::Failed
    } else if running {
        ToolLifecycleStatus::InProgress
    } else {
        ToolLifecycleStatus::Completed
    };
    work_entry(
        id,
        AT,
        WorkLogEntry {
            item_type: Some(TurnItemType::DynamicTool),
            tool_lifecycle_status: Some(lifecycle),
            projected_item: Some(projected(
                0,
                TurnItem {
                    status: item_status,
                    run_id: Some(RUN.into()),
                    ..turn_item(
                        id,
                        AT,
                        dynamic_tool_payload(Some("t3-code.delegate_task"), input, output),
                    )
                },
            )),
            ..work_log(
                id,
                AT,
                Some(RUN),
                "Delegated a child task",
                if failed {
                    WorkTone::Error
                } else {
                    WorkTone::Tool
                },
            )
        },
    )
}

#[test]
fn matches_delegation_calls_by_child_identity_with_each_output_envelope() {
    let cases = [
        (false, Envelope::Direct, "general"),
        (false, Envelope::Structured, "general"),
        (false, Envelope::Text, "general"),
        (false, Envelope::Structured, "research"),
        (true, Envelope::Direct, "general"),
        (true, Envelope::Direct, "research"),
    ];
    for (running, envelope, role) in cases {
        let mut entries = vec![
            child("a", role),
            delegation("delegate-a", "a", false, running, envelope, role),
        ];
        if !running {
            entries.push(child("b", role));
        }
        entries.extend([
            delegation("delegate-b", "b", false, running, envelope, role),
            delegation("unmatched", "other-child", false, running, envelope, role),
            child("c", role),
            delegation("failed", "c", true, running, envelope, role),
            child("d", role),
        ]);
        let rows = derive(&MessagesTimelineRowsInput {
            is_working: running,
            running_run_id: running.then(|| RUN.to_string()),
            expanded_run_ids: set(&[RUN]),
            ..rows_input(entries, running)
        });

        match find_id(&rows, "a") {
            Some(Row::Event {
                subagents: Some(subagents),
                ..
            }) if !running => {
                let members: Vec<&str> =
                    subagents.iter().map(|item| item.item.id.as_str()).collect();
                assert_eq!(members, vec!["a", "b"], "{envelope:?} {role}");
            }
            Some(_) if running => {}
            other => panic!("{envelope:?} {role}: unexpected row for a: {other:?}"),
        }
        assert!(
            find_id(&rows, "b").is_none(),
            "{envelope:?} {role} running {running}"
        );
        assert!(find_id(&rows, "c").is_some());
        assert!(find_id(&rows, "d").is_some());

        let visible_tools: Vec<&str> = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.kind(),
                    MessagesTimelineRowKind::Work | MessagesTimelineRowKind::WorkLive
                )
            })
            .flat_map(|row| grouped_ids(&row.content))
            .collect();
        assert!(visible_tools.contains(&"unmatched"));
        assert!(visible_tools.contains(&"failed"));
        assert_eq!(visible_tools.contains(&"delegate-a"), running);
        assert_eq!(visible_tools.contains(&"delegate-b"), running);
    }
}
