//! `expanded tool group scrolling`, `computeMessageDurationStart`,
//! `normalizeCompactToolLabel`, `resolveAssistantMessageCopyState` and
//! `shouldCollapseUserMessage` from MessagesTimeline.logic.test.ts.

use super::*;
use crate::components::chat::timeline::work_log_presentation::normalize_compact_tool_label;

const ENTRIES: [&str; 2] = ["first", "second"];

#[test]
fn follows_appended_calls_only_at_the_hard_end() {
    let appended = ["first", "second", "third"];
    assert!(should_follow_work_group_append(&ENTRIES, &appended, 0.0));
    assert!(should_follow_work_group_append(&ENTRIES, &appended, 0.5));
    assert!(should_follow_work_group_append(&ENTRIES, &appended, 1.0));
    assert!(!should_follow_work_group_append(&ENTRIES, &appended, 1.01));
    assert!(!should_follow_work_group_append(&ENTRIES, &appended, 10.0));
    assert!(!should_follow_work_group_append(
        &ENTRIES,
        &appended,
        f64::INFINITY
    ));
}

#[test]
fn does_not_follow_output_updates_prepends_or_replacements() {
    assert!(!should_follow_work_group_append(&ENTRIES, &ENTRIES, 0.0));
    assert!(!should_follow_work_group_append(
        &ENTRIES,
        &["older", "first", "second"],
        0.0
    ));
    assert!(!should_follow_work_group_append(
        &ENTRIES,
        &["replacement", "second", "third"],
        0.0
    ));
    assert!(!should_follow_work_group_append::<&str>(&[], &ENTRIES, 0.0));
}

#[test]
fn restores_the_visible_tool_and_its_offset_inside_expanded_output() {
    let anchor = WorkGroupScrollAnchor {
        entry_id: "second".into(),
        offset: 120.0,
    };
    assert_eq!(
        resolve_work_group_scroll_index(&ENTRIES, Some(&anchor)),
        Some(WorkGroupScrollIndex {
            index: 1,
            view_offset: -120.0
        })
    );
    assert_eq!(
        resolve_work_group_scroll_index(&["older", "first", "second"], Some(&anchor)),
        Some(WorkGroupScrollIndex {
            index: 2,
            view_offset: -120.0
        })
    );
}

#[test]
fn starts_normally_when_the_saved_tool_no_longer_exists() {
    assert_eq!(resolve_work_group_scroll_index(&ENTRIES, None), None);
    let anchor = WorkGroupScrollAnchor {
        entry_id: "removed".into(),
        offset: 120.0,
    };
    assert_eq!(
        resolve_work_group_scroll_index(&ENTRIES, Some(&anchor)),
        None
    );
}

fn duration_message<'a>(
    id: &'a str,
    role: MessageRole,
    created_at: &'a str,
    updated_at: &'a str,
    streaming: bool,
) -> TimelineDurationMessage<'a> {
    TimelineDurationMessage {
        id,
        role,
        created_at,
        updated_at,
        streaming,
    }
}

fn map(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn duration_start_without_a_preceding_user_message() {
    let result = compute_message_duration_start([duration_message(
        "a1",
        MessageRole::Assistant,
        "2026-01-01T00:00:05Z",
        "2026-01-01T00:00:10Z",
        false,
    )]);
    assert_eq!(result, map(&[("a1", "2026-01-01T00:00:05Z")]));
}

#[test]
fn duration_start_uses_the_user_message_for_the_first_response() {
    let result = compute_message_duration_start([
        duration_message(
            "u1",
            MessageRole::User,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            false,
        ),
        duration_message(
            "a1",
            MessageRole::Assistant,
            "2026-01-01T00:00:30Z",
            "2026-01-01T00:00:30Z",
            false,
        ),
    ]);
    assert_eq!(
        result,
        map(&[
            ("u1", "2026-01-01T00:00:00Z"),
            ("a1", "2026-01-01T00:00:00Z")
        ])
    );
}

#[test]
fn duration_start_uses_the_previous_completed_assistant() {
    let result = compute_message_duration_start([
        duration_message(
            "u1",
            MessageRole::User,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            false,
        ),
        duration_message(
            "a1",
            MessageRole::Assistant,
            "2026-01-01T00:00:30Z",
            "2026-01-01T00:00:30Z",
            false,
        ),
        duration_message(
            "a2",
            MessageRole::Assistant,
            "2026-01-01T00:00:55Z",
            "2026-01-01T00:00:55Z",
            false,
        ),
    ]);
    assert_eq!(
        result,
        map(&[
            ("u1", "2026-01-01T00:00:00Z"),
            ("a1", "2026-01-01T00:00:00Z"),
            ("a2", "2026-01-01T00:00:30Z"),
        ])
    );
}

#[test]
fn duration_start_does_not_advance_for_a_streaming_message() {
    let result = compute_message_duration_start([
        duration_message(
            "u1",
            MessageRole::User,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            false,
        ),
        duration_message(
            "a1",
            MessageRole::Assistant,
            "2026-01-01T00:00:30Z",
            "2026-01-01T00:00:40Z",
            true,
        ),
        duration_message(
            "a2",
            MessageRole::Assistant,
            "2026-01-01T00:00:55Z",
            "2026-01-01T00:00:55Z",
            false,
        ),
    ]);
    assert_eq!(
        result,
        map(&[
            ("u1", "2026-01-01T00:00:00Z"),
            ("a1", "2026-01-01T00:00:00Z"),
            ("a2", "2026-01-01T00:00:00Z"),
        ])
    );
}

#[test]
fn duration_start_resets_on_a_new_user_message() {
    let result = compute_message_duration_start([
        duration_message(
            "u1",
            MessageRole::User,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            false,
        ),
        duration_message(
            "a1",
            MessageRole::Assistant,
            "2026-01-01T00:00:30Z",
            "2026-01-01T00:00:30Z",
            false,
        ),
        duration_message(
            "u2",
            MessageRole::User,
            "2026-01-01T00:01:00Z",
            "2026-01-01T00:01:00Z",
            false,
        ),
        duration_message(
            "a2",
            MessageRole::Assistant,
            "2026-01-01T00:01:20Z",
            "2026-01-01T00:01:20Z",
            false,
        ),
    ]);
    assert_eq!(
        result,
        map(&[
            ("u1", "2026-01-01T00:00:00Z"),
            ("a1", "2026-01-01T00:00:00Z"),
            ("u2", "2026-01-01T00:01:00Z"),
            ("a2", "2026-01-01T00:01:00Z"),
        ])
    );
}

#[test]
fn duration_start_ignores_system_messages() {
    let result = compute_message_duration_start([
        duration_message(
            "u1",
            MessageRole::User,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            false,
        ),
        duration_message(
            "s1",
            MessageRole::System,
            "2026-01-01T00:00:01Z",
            "2026-01-01T00:00:01Z",
            false,
        ),
        duration_message(
            "a1",
            MessageRole::Assistant,
            "2026-01-01T00:00:30Z",
            "2026-01-01T00:00:30Z",
            false,
        ),
    ]);
    assert_eq!(
        result,
        map(&[
            ("u1", "2026-01-01T00:00:00Z"),
            ("s1", "2026-01-01T00:00:00Z"),
            ("a1", "2026-01-01T00:00:00Z"),
        ])
    );
}

#[test]
fn duration_start_of_nothing_is_empty() {
    assert!(compute_message_duration_start(std::iter::empty()).is_empty());
}

#[test]
fn normalizes_trailing_completion_wording() {
    assert_eq!(
        normalize_compact_tool_label("Ran command complete"),
        "Ran command"
    );
    assert_eq!(
        normalize_compact_tool_label("Read file completed"),
        "Read file"
    );
}

#[test]
fn copy_state_for_completed_streaming_empty_and_interim_messages() {
    assert_eq!(
        resolve_assistant_message_copy_state(Some("Ship it"), true, false),
        AssistantMessageCopyState {
            text: Some("Ship it".into()),
            visible: true
        }
    );
    assert_eq!(
        resolve_assistant_message_copy_state(Some("Still streaming"), true, true),
        AssistantMessageCopyState {
            text: Some("Still streaming".into()),
            visible: false
        }
    );
    assert_eq!(
        resolve_assistant_message_copy_state(Some("   "), true, false),
        AssistantMessageCopyState {
            text: None,
            visible: false
        }
    );
    assert_eq!(
        resolve_assistant_message_copy_state(Some("Interim thought"), false, false),
        AssistantMessageCopyState {
            text: Some("Interim thought".into()),
            visible: false
        }
    );
}

#[test]
fn copies_the_rendered_representation_of_codex_directives() {
    let text = [
        "Created :codex-file-citation{path=\"outputs/report.xlsx\" purpose=\"output\"}.",
        "",
        "::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
    ]
    .join("\n");
    assert_eq!(
        resolve_assistant_message_copy_state(Some(&text), true, false),
        AssistantMessageCopyState {
            text: Some(
                "Created [report.xlsx](<outputs/report.xlsx>).\n\nHello World (Document template)"
                    .into()
            ),
            visible: true,
        }
    );
}

#[test]
fn measures_file_links_and_context_chips_by_their_label() {
    let links: Vec<String> = (0..8)
        .map(|index| {
            format!("[file{index}.ts](/workspace/projects/example/packages/some/deeply/nested/directory/file{index}.ts)")
        })
        .collect();
    let text = format!(
        "Compare {} with [terminal 1](t3-context://v1/terminal/{}).",
        links.join(", "),
        "a".repeat(36)
    );
    assert!(text.len() > 600);
    assert!(!should_collapse_user_message(&text));
    assert!(should_collapse_user_message(&"More text. ".repeat(80)));
}
