//! Work entry labels: what a live row and a settled row say about a tool
//! call, including shell wrappers, thread reads, and legacy tool titles.

use super::*;
use crate::components::chat::timeline::messages_timeline_logic::{
    ThreadReadTargetShell, live_work_entry_label, thread_read_label_prefix, thread_read_target_id,
    thread_read_target_title, work_entry_is_visible_in_group, work_entry_read_output,
};

const AT: &str = "2026-09-01T12:00:00Z";

/// Upstream's shared `entry`: a plain tool call.
fn base() -> WorkLogEntry {
    work_log("tool-1", AT, None, "Tool call", WorkTone::Tool)
}

/// A structured payload carrying `tool_name` with `input`.
fn structured(tool_name: &str, input: Value) -> Option<TurnItem> {
    Some(turn_item(
        "structured",
        AT,
        dynamic_tool_payload(Some(tool_name), input, None),
    ))
}

fn live(entry: &WorkLogEntry, active: bool) -> String {
    live_work_entry_label(entry, None, active)
}

fn display(entry: &WorkLogEntry) -> String {
    work_entry_display_label(entry, None)
}

#[test]
fn uses_live_titles_only_for_active_thread_shells() {
    let shell =
        |title: &str, archived: Option<&str>, deleted: Option<&str>| ThreadReadTargetShell {
            title: title.into(),
            archived_at: archived.map(str::to_string),
            deleted_at: deleted.map(str::to_string),
        };
    let at = "2026-10-07T12:00:00Z";
    assert_eq!(
        thread_read_target_title(Some(&shell(" Review auth flow ", None, None))).as_deref(),
        Some("Review auth flow")
    );
    assert_eq!(
        thread_read_target_title(Some(&shell("Harden session refresh", None, None))).as_deref(),
        Some("Harden session refresh")
    );
    assert_eq!(
        thread_read_target_title(Some(&shell("Review auth flow", Some(at), None))),
        None
    );
    assert_eq!(
        thread_read_target_title(Some(&shell("Review auth flow", None, Some(at)))),
        None
    );
    assert_eq!(
        thread_read_target_title(Some(&shell("  ", None, None))),
        None
    );
    assert_eq!(thread_read_target_title(None), None);
}

#[test]
fn previews_reasoning_in_the_live_row_and_falls_back_to_a_short_label_while_empty() {
    let thought = WorkLogEntry {
        item_type: Some(TurnItemType::Reasoning),
        detail: Some("Check **ordering** first.".into()),
        tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
        ..WorkLogEntry {
            tone: WorkTone::Thinking,
            ..base()
        }
    };
    let detail = thought.detail.clone().expect("detail");
    assert_eq!(live(&thought, true), detail);
    let two_paragraphs = WorkLogEntry {
        detail: Some("First paragraph.\n\nSecond paragraph.".into()),
        ..thought.clone()
    };
    assert_eq!(
        live(&two_paragraphs, true),
        "First paragraph. Second paragraph."
    );
    let blank = WorkLogEntry {
        detail: Some("  ".into()),
        ..thought.clone()
    };
    assert_eq!(live(&blank, true), "Thinking");
    let empty_done = WorkLogEntry {
        detail: Some(String::new()),
        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
        ..thought.clone()
    };
    assert_eq!(live(&empty_done, false), "Thought");
    let done = WorkLogEntry {
        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
        ..thought.clone()
    };
    assert_eq!(live(&done, false), detail);
    assert_eq!(display(&thought), detail);
    assert!(work_entry_is_visible_in_group(&thought, false));
    assert!(work_entry_is_visible_in_group(&done, false));
    assert!(!work_entry_is_visible_in_group(&blank, false));
}

#[test]
fn uses_the_same_friendly_label_in_both_views_for_a_browser_click() {
    let cases = [
        (
            ToolLifecycleStatus::InProgress,
            "Clicking in the preview browser",
        ),
        (
            ToolLifecycleStatus::Completed,
            "Clicked in the preview browser",
        ),
        (
            ToolLifecycleStatus::Failed,
            "Failed to click in the preview browser",
        ),
        (
            ToolLifecycleStatus::Declined,
            "Declined to click in the preview browser",
        ),
        (
            ToolLifecycleStatus::Stopped,
            "Stopped clicking in the preview browser",
        ),
    ];
    for (status, label) in cases {
        let entry = WorkLogEntry {
            tool_title: Some("T3-code.preview_click".into()),
            detail: Some("{\"ok\":true}".into()),
            tool_lifecycle_status: Some(status),
            ..base()
        };
        assert_eq!(
            live(&entry, status == ToolLifecycleStatus::InProgress),
            label,
            "{status:?}"
        );
        assert_eq!(display(&entry), label, "{status:?}");
    }
}

#[test]
fn names_the_read_thread_in_the_label() {
    let cases = [
        (ToolLifecycleStatus::InProgress, true, "Reading thread"),
        (ToolLifecycleStatus::Completed, false, "Read thread"),
        (ToolLifecycleStatus::Failed, false, "Failed to read thread"),
        (
            ToolLifecycleStatus::Declined,
            false,
            "Declined to read thread",
        ),
        (
            ToolLifecycleStatus::Stopped,
            false,
            "Stopped reading thread",
        ),
    ];
    for (status, active, prefix) in cases {
        let thread_read = WorkLogEntry {
            item_type: Some(TurnItemType::DynamicTool),
            tool_lifecycle_status: Some(status),
            structured_payload: structured(
                "t3-code.t3_thread_read",
                json!({ "threadId": " thread-child ", "view": "activity" }),
            ),
            ..base()
        };
        assert_eq!(
            thread_read_target_id(&thread_read).as_deref(),
            Some("thread-child")
        );
        assert_eq!(
            thread_read_label_prefix(&live(&thread_read, active)).as_deref(),
            Some(prefix),
            "{status:?}"
        );
        assert_eq!(
            thread_read_label_prefix(&display(&thread_read)).as_deref(),
            Some(prefix),
            "{status:?}"
        );
    }
}

#[test]
fn finds_no_target_for_other_tools_or_thread_reads_without_one() {
    let payload = |tool_name: &str, input: Value| WorkLogEntry {
        structured_payload: structured(tool_name, input),
        ..base()
    };
    assert_eq!(
        thread_read_target_id(&payload(
            "t3-code.t3_thread_wait",
            json!({ "threadId": "t" })
        )),
        None
    );
    assert_eq!(
        thread_read_target_id(&payload(
            "t3-code.t3_thread_read",
            json!({ "threadId": "  " })
        )),
        None
    );
    assert_eq!(
        thread_read_target_id(&payload("t3-code.t3_thread_read", Value::Null)),
        None
    );
}

#[test]
fn uses_the_active_summary_state_for_legacy_tools_without_a_lifecycle_status() {
    let entry = WorkLogEntry {
        tool_title: Some("T3-code.preview_click".into()),
        ..base()
    };
    assert_eq!(live(&entry, true), "Clicking in the preview browser");
    assert_eq!(live(&entry, false), "Clicked in the preview browser");
}

#[test]
fn keeps_the_latest_live_activity_in_the_present_tense_after_the_call_completes() {
    let entry = WorkLogEntry {
        tool_title: Some("T3-code.preview_click".into()),
        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
        ..base()
    };
    assert_eq!(live(&entry, true), "Clicking in the preview browser");
    assert_eq!(live(&entry, false), "Clicked in the preview browser");
}

#[test]
fn labels_file_reads_with_the_path_and_never_the_file_body() {
    let read = WorkLogEntry {
        item_type: Some(TurnItemType::DynamicTool),
        tool_title: Some("Read".into()),
        detail: Some("---\nname: env\n---\n cons t x = 1".into()),
        tool_data: Some(json!({ "input": { "file_path": "src/env.ts" } })),
        structured_payload: structured("Read", json!({ "file_path": "src/env.ts" })),
        ..WorkLogEntry {
            label: "Read".into(),
            ..base()
        }
    };
    assert_eq!(display(&read), "Read src/env.ts");
    assert_eq!(
        work_entry_read_output(&read, None).as_deref(),
        Some("src/env.ts")
    );
    assert_eq!(
        work_entry_read_output(&read, Some("/workspace/ohseearr")).as_deref(),
        Some("/workspace/ohseearr/src/env.ts")
    );

    let from_location = WorkLogEntry {
        tool_data: Some(json!({ "locations": [{ "path": "src/from-location.ts" }] })),
        structured_payload: structured("Read", json!({})),
        ..base()
    };
    assert_eq!(
        work_entry_read_output(&from_location, Some("/workspace/ohseearr")).as_deref(),
        Some("/workspace/ohseearr/src/from-location.ts")
    );
    let no_path = WorkLogEntry {
        detail: Some("---".into()),
        tool_data: Some(json!({})),
        ..base()
    };
    assert_eq!(work_entry_read_output(&no_path, None), None);
}

#[test]
fn labels_claude_grep_from_structured_input_instead_of_a_generic_tool_heading() {
    let grep = WorkLogEntry {
        item_type: Some(TurnItemType::DynamicTool),
        tool_title: Some("Grep".into()),
        tool_data: Some(json!({ "input": { "pattern": "TODO", "path": "apps/web" } })),
        structured_payload: structured("Grep", json!({ "pattern": "TODO", "path": "apps/web" })),
        ..WorkLogEntry {
            label: "Grep".into(),
            ..base()
        }
    };
    assert_eq!(display(&grep), "Searched TODO in web");
}

#[test]
fn labels_file_searches_with_the_adapter_title_and_its_search_target() {
    let search = WorkLogEntry {
        item_type: Some(TurnItemType::FileSearch),
        tool_title: Some("Searched TODO in web".into()),
        detail: Some("TODO".into()),
        tool_data: Some(json!({ "type": "file_search", "pattern": "TODO" })),
        ..WorkLogEntry {
            label: "Searched TODO in web".into(),
            ..base()
        }
    };
    assert_eq!(display(&search), "Searched TODO in web");
}

#[test]
fn keeps_a_multi_line_approval_prompt_as_its_label() {
    let prompt = "Allow this command?\nrm -rf dist";
    let approval = WorkLogEntry {
        item_type: Some(TurnItemType::ApprovalRequest),
        request_kind: Some("command".into()),
        detail: Some(prompt.into()),
        ..base()
    };
    assert_eq!(display(&approval), prompt);
}

#[test]
fn keeps_custom_titles_and_output_for_unrecognized_tools() {
    let unknown = WorkLogEntry {
        tool_title: Some("mcp__github__search_issues".into()),
        ..base()
    };
    assert_eq!(live(&unknown, true), "Mcp__github__search_issues");
    let with_output = WorkLogEntry {
        detail: Some("Found 3 issues".into()),
        ..unknown.clone()
    };
    assert_eq!(display(&with_output), "Found 3 issues");
}

#[test]
fn keeps_command_summaries_compact_without_replacing_the_full_command_in_expanded_rows() {
    let command = WorkLogEntry {
        command: Some("vp test run".into()),
        detail: Some("All tests passed".into()),
        ..base()
    };
    assert_eq!(live(&command, true), "Running vp");
    assert_eq!(live(&command, false), "Ran vp");
    assert_eq!(display(&command), "vp test run");
}

#[test]
fn summarizes_the_program_inside_a_shell_wrapper_while_preserving_the_expanded_command() {
    let wrapped = "/bin/zsh -lc 'vp test run apps/web/src/session-logic.test.ts'";
    let entry = WorkLogEntry {
        command: Some(wrapped.into()),
        ..base()
    };
    assert_eq!(live(&entry, true), "Running vp");
    assert_eq!(live(&entry, false), "Ran vp");
    assert_eq!(
        display(&entry),
        "vp test run apps/web/src/session-logic.test.ts"
    );
    assert_eq!(entry.command.as_deref(), Some(wrapped));
}

#[test]
fn uses_present_tense_for_a_live_command_and_the_outcome_once_it_is_no_longer_live() {
    let cases = [
        (ToolLifecycleStatus::InProgress, "Running vp", "Running vp"),
        (ToolLifecycleStatus::Completed, "Running vp", "Ran vp"),
        (ToolLifecycleStatus::Failed, "Failed vp", "Failed vp"),
        (ToolLifecycleStatus::Declined, "Declined vp", "Declined vp"),
        (ToolLifecycleStatus::Stopped, "Stopped vp", "Stopped vp"),
    ];
    for (status, live_label, settled_label) in cases {
        let command = WorkLogEntry {
            command: Some("/bin/bash -lc 'vp test run'".into()),
            tool_lifecycle_status: Some(status),
            ..base()
        };
        assert_eq!(live(&command, true), live_label, "{status:?}");
        assert_eq!(live(&command, false), settled_label, "{status:?}");
    }
}

#[test]
fn renders_a_settled_legacy_tool_call_directly_with_its_completed_presentation() {
    let cases = [
        ("preview_click", "Clicked in the preview browser"),
        ("task_status", "Got delegated task status"),
    ];
    for (tool, label) in cases {
        let rows = derive(&rows_input(
            vec![work_entry(
                "browser-entry",
                AT,
                WorkLogEntry {
                    item_type: Some(TurnItemType::DynamicTool),
                    tool_data: Some(json!({ "server": "t3-code", "tool": tool })),
                    ..base()
                },
            )],
            false,
        ));
        match find_kind(&rows, MessagesTimelineRowKind::Work) {
            Some(Row::Work {
                grouped_entries,
                is_expanded_tool_group,
                display_label,
                ..
            }) => {
                assert_eq!(
                    grouped_entries
                        .iter()
                        .map(|entry| entry.id.as_str())
                        .collect::<Vec<_>>(),
                    vec!["tool-1"],
                    "{tool}"
                );
                assert!(!is_expanded_tool_group, "{tool}");
                assert_eq!(display_label.as_deref(), Some(label), "{tool}");
            }
            other => panic!("{tool}: expected a work row, got {other:?}"),
        }
    }
}
