//! `deriveMessagesTimelineRows` cases about which tool entries count as live
//! work: runless entries of a provider-native subagent, separated and stale
//! in-progress tools, and a single completed tool rendered directly.

use super::*;

/// A runless command entry (`runId: null`) with the given lifecycle status.
fn runless_command(id: &str, at: &str, status: ToolLifecycleStatus) -> TimelineEntry {
    work_entry(
        &format!("{id}-entry"),
        at,
        WorkLogEntry {
            command: Some("git diff --stat".into()),
            request_kind: Some("command".into()),
            tool_lifecycle_status: Some(status),
            ..work_log(id, at, None, "Running git", WorkTone::Tool)
        },
    )
}

/// A command entry in `turn-1` whose command is `rg {id}`.
fn turn_command(id: &str, at: &str, label: &str, status: ToolLifecycleStatus) -> TimelineEntry {
    work_entry(
        &format!("{id}-entry"),
        at,
        WorkLogEntry {
            command: Some(format!("rg {id}")),
            request_kind: Some("command".into()),
            tool_lifecycle_status: Some(status),
            ..work_log(id, at, Some("turn-1"), label, WorkTone::Tool)
        },
    )
}

fn commentary(id: &str, at: &str, text: &str) -> TimelineEntry {
    message_entry(
        &format!("{id}-entry"),
        at,
        assistant_message(id, text, Some("turn-1"), at, at, false),
    )
}

/// Input for a turn whose `turn-1` is running, with the given entries.
pub(super) fn running_turn(timeline_entries: Vec<TimelineEntry>) -> MessagesTimelineRowsInput {
    MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-1",
            RunStatus::Running,
            Some("2026-01-01T00:00:00Z"),
            None,
        )),
        active_turn_started_at: Some("2026-01-01T00:00:00Z".into()),
        ..rows_input(timeline_entries, true)
    }
}

fn live_entry_ids(rows: &[Arc<MessagesTimelineRow>]) -> Vec<&str> {
    rows.iter()
        .filter_map(|row| match &row.content {
            Row::WorkLive { entry, .. } => Some(entry.id.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn shows_a_provider_native_subagents_runless_tools_as_live_work_while_it_works() {
    let entries = |status: ToolLifecycleStatus| {
        vec![
            message_entry(
                "task-entry",
                "2026-01-01T00:00:00Z",
                user_message("task", "Audit the adapters", None, "2026-01-01T00:00:00Z"),
            ),
            runless_command("command", "2026-01-01T00:00:05Z", status),
        ]
    };
    let running = derive(&MessagesTimelineRowsInput {
        runless_work_active: true,
        active_turn_started_at: Some("2026-01-01T00:00:00Z".into()),
        ..rows_input(entries(ToolLifecycleStatus::InProgress), true)
    });
    assert_eq!(kinds(&running), vec!["message", "working", "work-live"]);
    match find_kind(&running, MessagesTimelineRowKind::WorkLive) {
        Some(Row::WorkLive { entry, active, .. }) => {
            assert_eq!(entry.id, "command");
            assert!(active);
        }
        other => panic!("expected a live work row, got {other:?}"),
    }

    // Once the subagent settles, the same entries fold as finished history.
    let settled = derive(&rows_input(entries(ToolLifecycleStatus::Completed), false));
    assert_eq!(kinds(&settled), vec!["message", "turn-fold"]);
}

#[test]
fn does_not_treat_runless_entries_as_live_work_on_a_thread_with_runs() {
    let rows = derive(&running_turn(vec![runless_command(
        "runless-command",
        "2026-01-01T00:00:05Z",
        ToolLifecycleStatus::InProgress,
    )]));

    assert!(!has_kind(&rows, MessagesTimelineRowKind::WorkLive));
    assert_eq!(
        rows.last().map(|row| row.kind()),
        Some(MessagesTimelineRowKind::Thinking)
    );
}

#[test]
fn renders_a_single_completed_tool_call_directly() {
    let rows = derive(&running_turn(vec![
        turn_command(
            "completed-command",
            "2026-01-01T00:00:05Z",
            "Ran rg",
            ToolLifecycleStatus::Completed,
        ),
        commentary(
            "assistant-commentary",
            "2026-01-01T00:00:06Z",
            "Checking another thing.",
        ),
        turn_command(
            "running-command",
            "2026-01-01T00:00:07Z",
            "Running tests",
            ToolLifecycleStatus::InProgress,
        ),
    ]));

    assert_eq!(
        kinds(&rows),
        vec!["working", "work", "message", "work-live"]
    );
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
                    .map(|entry| (entry.id.as_str(), entry.command.as_deref()))
                    .collect::<Vec<_>>(),
                vec![("completed-command", Some("rg completed-command"))]
            );
            assert!(!is_expanded_tool_group);
            assert_eq!(display_label.as_deref(), Some("rg completed-command"));
        }
        other => panic!("expected a work row, got {other:?}"),
    }
}

#[test]
fn keeps_separated_in_progress_tool_runs_visible() {
    let rows = derive(&running_turn(vec![
        turn_command(
            "first-running",
            "2026-01-01T00:00:05Z",
            "Running first command",
            ToolLifecycleStatus::InProgress,
        ),
        commentary(
            "assistant-commentary",
            "2026-01-01T00:00:06Z",
            "Starting another command.",
        ),
        turn_command(
            "second-running",
            "2026-01-01T00:00:07Z",
            "Running second command",
            ToolLifecycleStatus::InProgress,
        ),
    ]));

    assert_eq!(
        kinds(&rows),
        vec!["working", "work-live", "message", "work-live"]
    );
    assert_eq!(
        live_entry_ids(&rows),
        vec!["first-running", "second-running"]
    );
}

#[test]
fn does_not_revive_stale_in_progress_tools_before_a_fresh_send_has_a_turn_id() {
    let rows = derive(&MessagesTimelineRowsInput {
        active_turn_started_at: Some("2026-01-01T00:01:00Z".into()),
        ..rows_input(
            vec![
                turn_command(
                    "stale-running",
                    "2026-01-01T00:00:05Z",
                    "Running stale command",
                    ToolLifecycleStatus::InProgress,
                ),
                message_entry(
                    "user-followup-entry",
                    "2026-01-01T00:01:00Z",
                    user_message("user-followup", "continue", None, "2026-01-01T00:01:00Z"),
                ),
            ],
            true,
        )
    });

    assert!(!has_kind(&rows, MessagesTimelineRowKind::WorkLive));
}

#[test]
fn does_not_revive_separated_historical_task_progress() {
    let stale_progress = work_entry(
        "stale-progress-entry",
        "2026-01-01T00:00:05Z",
        WorkLogEntry {
            source_activity_kind: Some("task.progress".into()),
            ..work_log(
                "stale-progress",
                "2026-01-01T00:00:05Z",
                Some("turn-1"),
                "Old progress",
                WorkTone::Thinking,
            )
        },
    );
    let rows = derive(&running_turn(vec![
        stale_progress,
        commentary(
            "assistant-commentary",
            "2026-01-01T00:00:06Z",
            "Starting another command.",
        ),
        turn_command(
            "running-command",
            "2026-01-01T00:00:07Z",
            "Running command",
            ToolLifecycleStatus::InProgress,
        ),
    ]));

    assert_eq!(live_entry_ids(&rows), vec!["running-command"]);
}
