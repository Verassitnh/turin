//! Turns without app runs: a provider-native subagent thread folds each of
//! its runs like a normal turn, and imported V1 turns stay folded once the
//! thread's first V2 run starts.

use super::*;

/// `2026-09-25T22:51:SS` plus `second` seconds, as upstream's `at(second)`.
fn at(second: u32) -> String {
    let total = 22 * 3600 + 51 * 60 + second;
    format!(
        "2026-09-25T{:02}:{:02}:{:02}Z",
        total / 3600,
        (total / 60) % 60,
        total % 60
    )
}

/// A runless v2 item of the subagent thread, started at `second` and
/// completed at `end_second` (upstream's `base`).
fn runless_item(
    id: &str,
    ordinal: u64,
    second: u32,
    end_second: u32,
    payload: TurnItemPayload,
) -> TurnItem {
    TurnItem {
        node_id: Some("task-root".into()),
        ordinal,
        started_at: Some(at(second)),
        completed_at: Some(at(end_second)),
        ..turn_item(id, &at(end_second), payload)
    }
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        _ => "system",
    }
}

/// Upstream `shape`: folds by label, messages by role and id, others by kind.
fn shape(rows: &[Arc<MessagesTimelineRow>]) -> Vec<String> {
    rows.iter()
        .map(|row| match &row.content {
            Row::TurnFold { label, .. } => format!("fold:{label}"),
            Row::Message { message, .. } => format!("{}:{}", role_name(message.role), message.id),
            _ => row.kind().as_str().to_string(),
        })
        .collect()
}

#[test]
fn folds_each_run_of_a_provider_native_subagent_thread_like_a_normal_turn() {
    let items = |resume_status: Option<TurnItemStatus>, resume_failed: bool| {
        let prompt = |id: &str, ordinal: u64, second: u32| {
            projected(
                ordinal,
                runless_item(
                    id,
                    ordinal,
                    second,
                    second,
                    TurnItemPayload::UserMessage {
                        message_id: id.into(),
                        text: format!("Prompt {id}"),
                        created_by: Actor::Agent,
                        creation_source: CreationSource::Provider,
                        scheduled_task_id: None,
                        sender_thread_id: None,
                        input_intent: InputIntent::TurnStart,
                        attachments: Vec::new(),
                    },
                ),
            )
        };
        let answer = |id: &str, ordinal: u64, second: u32| {
            projected(
                ordinal,
                runless_item(
                    id,
                    ordinal,
                    second,
                    second,
                    assistant_message_payload(id, &format!("Answer {id}"), false),
                ),
            )
        };
        let mut out = vec![
            prompt("launch", 1, 0),
            projected(
                2,
                runless_item("launch-ls", 2, 4, 4, command_payload("ls src", None, None)),
            ),
            projected(
                3,
                runless_item(
                    "launch-thinking",
                    3,
                    8,
                    8,
                    TurnItemPayload::Reasoning {
                        text: "Not there.".into(),
                        streaming: false,
                    },
                ),
            ),
            answer("launch-answer", 4, 8),
            prompt("resume", 5, 72),
        ];
        if let Some(status) = resume_status {
            let mut resume_ls = runless_item(
                "resume-ls",
                6,
                77,
                77,
                command_payload("ls src", None, None),
            );
            resume_ls.status = status;
            if status == TurnItemStatus::Running {
                resume_ls.completed_at = None;
            }
            out.push(projected(6, resume_ls));
        }
        if resume_failed {
            out.push(projected(
                7,
                TurnItem {
                    status: TurnItemStatus::Failed,
                    ..runless_item(
                        "resume-error",
                        7,
                        80,
                        80,
                        TurnItemPayload::Error {
                            failure: ProviderFailure {
                                class: ProviderFailureClass::ProviderError,
                                message: "Subagent failed".into(),
                                code: None,
                                retryable: None,
                                reset_at: None,
                            },
                            retry: None,
                        },
                    )
                },
            ));
        }
        if resume_status != Some(TurnItemStatus::Running) {
            out.push(answer("resume-answer", 8, 80));
        }
        out
    };
    let rows = |resume_status: Option<TurnItemStatus>,
                resume_failed: bool,
                working: bool,
                expanded: &[&str]| {
        derive(&MessagesTimelineRowsInput {
            latest_run: None,
            runless_work_active: working,
            active_turn_started_at: working.then(|| at(72)),
            expanded_run_ids: set(expanded),
            ..rows_input(entries_from(items(resume_status, resume_failed)), working)
        })
    };

    // Settled: each run folds its work, keeping its prompt and final answer.
    let settled = rows(Some(TurnItemStatus::Completed), false, false, &[]);
    assert_eq!(
        shape(&settled),
        vec![
            "user:launch",
            "fold:Worked for 8.0s",
            "assistant:launch-answer",
            "user:resume",
            "fold:Worked for 8.0s",
            "assistant:resume-answer",
        ]
    );

    // Each fold opens on its own.
    let launch_run = match find_kind(&settled, MessagesTimelineRowKind::TurnFold) {
        Some(Row::TurnFold { run_id, .. }) => run_id.clone(),
        other => panic!("expected the launch fold, got {other:?}"),
    };
    let expanded = rows(
        Some(TurnItemStatus::Completed),
        false,
        false,
        &[&launch_run],
    );
    assert_eq!(
        shape(&expanded),
        vec![
            "user:launch",
            "fold:Worked for 8.0s",
            "work-toggle",
            "assistant:launch-answer",
            "user:resume",
            "fold:Worked for 8.0s",
            "assistant:resume-answer",
        ]
    );

    // While the resume runs, only the settled launch folds.
    assert_eq!(
        shape(&rows(Some(TurnItemStatus::Running), false, true, &[])),
        vec![
            "user:launch",
            "fold:Worked for 8.0s",
            "assistant:launch-answer",
            "user:resume",
            "working",
            "work-live",
        ]
    );

    // A failed run stays open, as on a normal thread.
    assert_eq!(
        shape(&rows(Some(TurnItemStatus::Completed), true, false, &[])),
        vec![
            "user:launch",
            "fold:Worked for 8.0s",
            "assistant:launch-answer",
            "user:resume",
            "work",
            "work",
            "assistant:resume-answer",
        ]
    );
}

#[test]
fn keeps_imported_v1_turns_folded_once_the_threads_first_v2_run_starts() {
    let at_v1 = |second: u32| format!("2026-01-01T00:00:{second:02}Z");
    let message = |id: &str, role: MessageRole, second: u32, run: Option<&str>| {
        message_entry(
            id,
            &at_v1(second),
            chat_message(id, role, id, run, &at_v1(second), &at_v1(second), false),
        )
    };
    let rows = |tail: Vec<TimelineEntry>| {
        derive(&MessagesTimelineRowsInput {
            latest_run: Some(latest_run(
                "run-1",
                RunStatus::Running,
                Some(&at_v1(20)),
                None,
            )),
            is_working: true,
            active_turn_started_at: Some(at_v1(20)),
            ..rows_input(
                [
                    vec![
                        message("imported-prompt", MessageRole::User, 0, None),
                        message("imported-update", MessageRole::Assistant, 4, None),
                        work_entry(
                            "imported-command",
                            &at_v1(5),
                            WorkLogEntry {
                                command: Some("git status".into()),
                                request_kind: Some("command".into()),
                                tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                                ..work_log(
                                    "imported-command",
                                    &at_v1(5),
                                    None,
                                    "Ran git",
                                    WorkTone::Tool,
                                )
                            },
                        ),
                        message("imported-answer", MessageRole::Assistant, 8, None),
                    ],
                    tail,
                ]
                .concat(),
                true,
            )
        })
    };
    let kinds_of = |rows: &[Arc<MessagesTimelineRow>]| -> Vec<String> {
        rows.iter()
            .map(|row| match &row.content {
                Row::Message { message, .. } => {
                    format!("{}:{}", role_name(message.role), message.id)
                }
                _ => row.kind().as_str().to_string(),
            })
            .collect()
    };

    // V2 work starts from a sent prompt, or with no new prompt (a wake or a resume).
    let with_prompt = rows(vec![message(
        "new-prompt",
        MessageRole::User,
        20,
        Some("run-1"),
    )]);
    assert_eq!(
        kinds_of(&with_prompt)[..4],
        [
            "user:imported-prompt",
            "turn-fold",
            "assistant:imported-answer",
            "user:new-prompt",
        ]
    );
    let without_prompt = kinds_of(&rows(Vec::new()));
    assert!(without_prompt.iter().any(|kind| kind == "turn-fold"));
    assert!(
        !without_prompt
            .iter()
            .any(|kind| kind == "assistant:imported-update")
    );

    // Find must open the fold holding a folded imported message by its synthetic key.
    let timeline_entries = vec![
        message("imported-prompt", MessageRole::User, 0, None),
        message("imported-update", MessageRole::Assistant, 4, None),
        message("imported-answer", MessageRole::Assistant, 8, None),
    ];
    let fold_input = MessagesTimelineRowsInput {
        ..rows_input(timeline_entries, false)
    };
    let fold_run_id = timeline_entry_turn_fold_run_id(&fold_input, "imported-update")
        .expect("imported turn fold key");
    // The rendered turn-fold row carries the same key, so find can map it back.
    let fold_row = derive(&fold_input);
    match find_kind(&fold_row, MessagesTimelineRowKind::TurnFold) {
        Some(Row::TurnFold { run_id, .. }) => assert_eq!(run_id, &fold_run_id),
        other => panic!("expected a turn fold, got {other:?}"),
    }
    assert_eq!(
        timeline_entry_turn_fold_run_id(&fold_input, "imported-answer"),
        None
    );
    let expanded = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&[&fold_run_id]),
        ..fold_input
    });
    assert!(expanded.iter().any(|row| matches!(
        &row.content,
        Row::Message { message, .. } if message.id == "imported-update"
    )));
}
