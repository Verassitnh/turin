//! A selection of `deriveMessagesTimelineRows` cases from
//! MessagesTimeline.logic.test.ts.

use super::*;

fn thought(id: &str, at: &str) -> TimelineEntry {
    work_entry(
        id,
        at,
        WorkLogEntry {
            item_type: Some(TurnItemType::Reasoning),
            detail: Some(id.to_string()),
            tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
            ..work_log(id, at, Some("steered-run"), "Thinking", WorkTone::Thinking)
        },
    )
}

#[test]
fn stops_stranded_thinking_after_a_steer_and_follows_the_next_thought_or_tool() {
    let at = "2026-10-01T06:19:10Z";
    let first = thought("first-thought", at);
    let next = thought("next-thought", at);
    let steer = message_entry(
        "steer",
        at,
        ChatMessage {
            input_intent: Some(InputIntent::Steer),
            ..user_message("steer", "Also evaluate speed", Some("steered-run"), at)
        },
    );
    let tool = work_entry(
        "tool",
        at,
        WorkLogEntry {
            command: Some("cat file".into()),
            tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
            ..work_log(
                "tool",
                at,
                Some("steered-run"),
                "Running cat",
                WorkTone::Tool,
            )
        },
    );
    let rows = |entries: Vec<TimelineEntry>| {
        derive(&MessagesTimelineRowsInput {
            running_run_id: Some("steered-run".into()),
            active_turn_started_at: Some(at.into()),
            ..rows_input(entries, true)
        })
    };

    let only_first = rows(vec![first.clone()]);
    let live = live_rows(&only_first);
    assert!(
        matches!(live.first(), Some(Row::WorkLive { active: true, entry, .. }) if entry.id == "first-thought")
    );

    let after_steer = rows(vec![first.clone(), steer.clone()]);
    assert!(!has_kind(&after_steer, MessagesTimelineRowKind::WorkLive));
    assert_eq!(
        after_steer.last().map(|row| row.kind()),
        Some(MessagesTimelineRowKind::Thinking)
    );
    let work = find_kind(&after_steer, MessagesTimelineRowKind::Work).expect("work row");
    assert_eq!(grouped_ids(work), vec!["first-thought"]);
    assert_eq!(
        grouped_entries(work)[0].tool_lifecycle_status,
        Some(ToolLifecycleStatus::Completed)
    );

    for entries in [
        vec![first.clone(), steer.clone(), next.clone()],
        vec![first.clone(), next.clone()],
        vec![first.clone(), steer.clone(), next.clone(), tool.clone()],
    ] {
        let last_id = entries.last().map(|entry| entry.id.clone());
        let derived = rows(entries);
        let active: Vec<_> = live_rows(&derived)
            .into_iter()
            .filter(|row| matches!(row, Row::WorkLive { active: true, .. }))
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(live_entry_id(active[0]).map(str::to_string), last_id);
    }
    // Presentation must not rewrite the retained provider lifecycle.
    match &first.kind {
        TimelineEntryKind::Work { entry } => {
            assert_eq!(
                entry.tool_lifecycle_status,
                Some(ToolLifecycleStatus::InProgress)
            )
        }
        _ => unreachable!(),
    }
}

#[test]
fn groups_approval_and_user_input_requests_with_commands() {
    for status in [TurnItemStatus::Waiting, TurnItemStatus::Completed] {
        let fixture = make_streaming_timeline_fixture("");
        let common = |id: &str, payload: TurnItemPayload| TurnItem {
            run_id: Some(fixture.run_id.clone()),
            status,
            ..TurnItem::new(id, &fixture.thread_id, status, fixture.time(1), payload)
        };
        let items = vec![
            common("command", command_payload("pwd", Some(""), None)),
            common(
                "approval",
                TurnItemPayload::ApprovalRequest {
                    request_id: "approval-request".into(),
                    request_kind: ProviderRequestKind::Command,
                    prompt: Some("Allow pwd?".into()),
                    app_name: None,
                    options: Vec::new(),
                },
            ),
            common(
                "input",
                TurnItemPayload::UserInputRequest {
                    request_id: "input-request".into(),
                    questions: Vec::new(),
                    question_answer: None,
                },
            ),
        ];
        let entries = entries_from(
            items
                .into_iter()
                .enumerate()
                .map(|(position, item)| projected(position as u64, item))
                .collect(),
        );
        assert!(
            entries
                .iter()
                .all(|entry| matches!(entry.kind, TimelineEntryKind::Work { .. }))
        );
        let input = MessagesTimelineRowsInput {
            running_run_id: Some(fixture.run_id.clone()),
            active_turn_started_at: Some(fixture.time(0)),
            ..rows_input(entries, status == TurnItemStatus::Waiting)
        };
        let rows = derive(&input);
        let work_rows: Vec<_> = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.kind(),
                    MessagesTimelineRowKind::WorkToggle | MessagesTimelineRowKind::WorkLive
                )
            })
            .collect();
        assert_eq!(work_rows.len(), 1, "status {status:?}");
        let group_id = match &work_rows[0].content {
            Row::WorkToggle {
                group_id, expanded, ..
            }
            | Row::WorkLive {
                group_id, expanded, ..
            } => {
                assert!(!expanded);
                group_id.clone()
            }
            _ => unreachable!(),
        };
        let expanded = derive(&MessagesTimelineRowsInput {
            expanded_work_group_ids: set(&[&group_id]),
            ..input.clone()
        });
        let group = rows_of_expanded_group(&expanded);
        assert_eq!(
            group,
            vec![
                Some(TurnItemType::CommandExecution),
                Some(TurnItemType::ApprovalRequest),
                Some(TurnItemType::UserInputRequest),
            ]
        );
        assert!(!has_kind(&rows, MessagesTimelineRowKind::Event));
    }
}

fn rows_of_expanded_group(rows: &[Arc<MessagesTimelineRow>]) -> Vec<Option<TurnItemType>> {
    rows.iter()
        .find_map(|row| match &row.content {
            Row::Work {
                is_expanded_tool_group: true,
                grouped_entries,
                ..
            } => Some(
                grouped_entries
                    .iter()
                    .map(|entry| entry.item_type)
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn presents_project_mcp_calls_and_summarizes_clones() {
    let fixture = make_streaming_timeline_fixture("");
    let tool = |id: &str, name: &str, output: Value| TurnItem {
        run_id: Some(fixture.run_id.clone()),
        title: Some("Custom provider title".into()),
        ..TurnItem::new(
            id,
            &fixture.thread_id,
            TurnItemStatus::Completed,
            fixture.time(1),
            dynamic_tool_payload(Some(name), json!({}), Some(output)),
        )
    };
    let items = vec![
        tool("list", "T3-code.t3_project_list", json!({ "projects": [] })),
        tool(
            "clone",
            "mcp__t3_code__t3_project_clone",
            json!({ "cwd": "/tmp/repo" }),
        ),
        tool(
            "failed-clone",
            "t3_project_clone",
            json!({ "isError": true }),
        ),
    ];
    let entries = entries_from(
        items
            .into_iter()
            .enumerate()
            .map(|(position, item)| projected(position as u64, item))
            .collect(),
    );
    let work: Vec<&WorkLogEntry> = entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            TimelineEntryKind::Work { entry } => Some(entry),
            _ => None,
        })
        .collect();
    assert_eq!(work_entry_display_label(work[0], None), "Listed projects");
    assert_eq!(
        work_entry_display_label(work[1], None),
        "Cloned a repository"
    );
    assert_eq!(
        work_entry_display_label(work[2], None),
        "Failed to clone a repository"
    );
    let rows = derive(&MessagesTimelineRowsInput {
        running_run_id: Some(fixture.run_id.clone()),
        active_turn_started_at: Some(fixture.time(0)),
        ..rows_input(entries, false)
    });
    match find_kind(&rows, MessagesTimelineRowKind::WorkToggle) {
        Some(Row::WorkToggle {
            summary,
            has_failure,
            ..
        }) => {
            assert_eq!(summary, "Listed projects 1 time and cloned 1 repository");
            assert!(has_failure);
        }
        other => panic!("expected a work toggle, got {other:?}"),
    }
}

#[test]
fn settled_turns_fold_behind_a_worked_for_header() {
    let fixture = make_streaming_timeline_fixture("Streaming");
    let entries = derive_timeline_entries_from_visible_turn_items(&fixture.entries_input());
    let input = MessagesTimelineRowsInput {
        running_run_id: Some(fixture.run_id.clone()),
        latest_run: Some(latest_run(
            &fixture.run_id,
            RunStatus::Running,
            Some(&fixture.time(5)),
            None,
        )),
        active_turn_started_at: Some(fixture.time(5)),
        ..rows_input(entries, true)
    };
    let rows = derive(&input);
    // The settled history run folds; the live run stays open.
    let fold = find_kind(&rows, MessagesTimelineRowKind::TurnFold).expect("turn fold");
    let Row::TurnFold {
        run_id,
        expanded,
        label,
        ..
    } = fold
    else {
        unreachable!()
    };
    assert_eq!(run_id, &fixture.history_run_id);
    assert!(!expanded);
    assert!(label.starts_with("Worked for"), "{label}");
    assert!(find_id(&rows, "history-work").is_none());

    let expanded = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&[&fixture.history_run_id]),
        ..input
    });
    assert!(matches!(
        find_kind(&expanded, MessagesTimelineRowKind::TurnFold),
        Some(Row::TurnFold { expanded: true, .. })
    ));
    assert!(expanded.len() > rows.len());
}
