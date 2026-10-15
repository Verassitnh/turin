//! Subagents and failures inside turns: a failed turn keeps its transcript
//! visible, a live child stays out of the fold while its turn settles, and a
//! subagent resumed by a later run folds with each run's own turn.

use super::derive_rows_v2::in_attempt;
use super::*;

const PARENT_RUN: &str = "parent-run";

fn at(second: u32) -> String {
    format!("2026-10-06T10:00:{second:02}Z")
}

/// A v2 turn item in `run` at `ordinal` (upstream's `base`), completed when
/// its second is reached.
fn v2_item(id: &str, run: &str, ordinal: u64, second: u32, payload: TurnItemPayload) -> TurnItem {
    TurnItem {
        run_id: Some(run.into()),
        ordinal,
        started_at: Some(at(second)),
        completed_at: Some(at(second)),
        ..turn_item(id, &at(second), payload)
    }
}

fn subagent_item(id: &str, second: u32, status: TurnItemStatus, origin: &str) -> ProjectedTurnItem {
    let app_owned = origin == "app_owned";
    let item = TurnItem {
        run_id: Some(PARENT_RUN.into()),
        ordinal: u64::from(second),
        status,
        started_at: Some(at(second)),
        completed_at: (status != TurnItemStatus::Running).then(|| at(second)),
        ..turn_item(
            id,
            &at(second),
            TurnItemPayload::Subagent {
                subagent_id: id.into(),
                origin: origin.into(),
                child_thread_id: app_owned.then(|| format!("{id}-thread")),
                prompt: format!("Inspect {id}"),
                progress: None,
                result: None,
            },
        )
    };
    projected(u64::from(second), item)
}

/// The settled parent turn: prompt, reasoning, command, the given children,
/// then the answer. The latest run has completed.
fn settled_rows(children: Vec<ProjectedTurnItem>) -> Vec<Arc<MessagesTimelineRow>> {
    let mut items = vec![
        projected(
            0,
            v2_item(
                "user",
                PARENT_RUN,
                0,
                0,
                user_message_payload("user", "Delegate the review"),
            ),
        ),
        projected(
            1,
            v2_item(
                "reasoning",
                PARENT_RUN,
                1,
                1,
                TurnItemPayload::Reasoning {
                    text: "Two children can split this.".into(),
                    streaming: false,
                },
            ),
        ),
        projected(
            2,
            v2_item(
                "command",
                PARENT_RUN,
                2,
                2,
                command_payload("pwd", Some("/repo"), Some(0)),
            ),
        ),
    ];
    items.extend(children);
    items.push(projected(
        8,
        v2_item(
            "assistant",
            PARENT_RUN,
            8,
            8,
            assistant_message_payload("assistant", "Both children are running.", false),
        ),
    ));
    derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            PARENT_RUN,
            RunStatus::Completed,
            Some(&at(0)),
            Some(&at(8)),
        )),
        ..rows_input(entries_from(items), false)
    })
}

#[test]
fn keeps_historical_failures_and_preceding_work_visible_without_disclosures() {
    let failed_at = "2026-09-20T12:00:00.000Z";
    let failure_message = "The provider stopped this turn.\nRetry later.";
    for class in [
        ProviderFailureClass::ProviderError,
        ProviderFailureClass::UsageLimit,
    ] {
        let at_failed = |item: TurnItem| TurnItem {
            run_id: Some("failed-run".into()),
            started_at: Some(failed_at.into()),
            completed_at: Some(failed_at.into()),
            ..item
        };
        let items = vec![
            projected(
                0,
                at_failed(turn_item(
                    "user",
                    failed_at,
                    user_message_payload("user", "Build it"),
                )),
            ),
            projected(
                1,
                at_failed(turn_item(
                    "command",
                    failed_at,
                    command_payload("pwd", Some(""), Some(0)),
                )),
            ),
            projected(
                2,
                TurnItem {
                    status: TurnItemStatus::Failed,
                    ..at_failed(turn_item(
                        "failure",
                        failed_at,
                        TurnItemPayload::Error {
                            failure: ProviderFailure {
                                class,
                                message: failure_message.into(),
                                code: None,
                                retryable: Some(true),
                                reset_at: None,
                            },
                            retry: None,
                        },
                    ))
                },
            ),
        ];
        let superseded = TimelineAttempt {
            attempt_ordinal: 1,
            ..attempt(
                "superseded-attempt",
                "failed-run",
                AttemptStatus::Superseded,
                None,
            )
        };
        let latest = latest_run(
            "newer-run",
            RunStatus::Completed,
            Some(failed_at),
            Some(failed_at),
        );
        let superseded_entries = |entries: Vec<TimelineEntry>| -> Vec<TimelineEntry> {
            entries
                .into_iter()
                .map(|entry| in_attempt(entry, &superseded))
                .collect()
        };

        let rows = derive(&MessagesTimelineRowsInput {
            latest_run: Some(latest.clone()),
            ..rows_input(superseded_entries(entries_from(items.clone())), false)
        });
        assert!(!has_kind(&rows, MessagesTimelineRowKind::TurnFold));
        assert!(!has_kind(&rows, MessagesTimelineRowKind::AttemptFold));
        assert!(!has_kind(&rows, MessagesTimelineRowKind::WorkToggle));
        let work: Vec<WorkLogEntry> = rows
            .iter()
            .flat_map(|row| match &row.content {
                Row::Work {
                    grouped_entries, ..
                } => grouped_entries.clone(),
                _ => Vec::new(),
            })
            .collect();
        let work_ids: Vec<&str> = work.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(work_ids, vec!["command", "failure"], "{class:?}");
        let failure = work.last().expect("failure entry");
        assert_eq!(failure.detail.as_deref(), Some(failure_message));
        assert_eq!(failure.created_at, failed_at);

        // Without the command, an assistant message before the failure stays
        // the message the copy control belongs to.
        let mut without_tools = entries_from(
            items
                .iter()
                .filter(|row| row.item.id != "command")
                .cloned()
                .collect(),
        );
        without_tools.insert(
            1,
            message_entry(
                "assistant-before-failure",
                failed_at,
                assistant_message(
                    "assistant-before-failure",
                    "Checking the workspace.",
                    Some("failed-run"),
                    failed_at,
                    failed_at,
                    false,
                ),
            ),
        );
        let compact = derive(&rows_input(without_tools, false));
        assert_eq!(
            kinds(&compact),
            vec!["message", "message", "work", "assistant-meta"],
            "{class:?}"
        );
        match &compact[1].content {
            Row::Message {
                show_assistant_meta,
                ..
            } => assert!(!show_assistant_meta),
            other => panic!("expected a message row, got {other:?}"),
        }
        match &compact.last().expect("meta row").content {
            Row::AssistantMeta {
                show_assistant_copy_button,
                message,
                ..
            } => {
                assert!(show_assistant_copy_button);
                assert_eq!(message.id, "assistant-before-failure");
            }
            other => panic!("expected the assistant meta row, got {other:?}"),
        }
    }
}

#[test]
fn keeps_a_running_child_visible_while_the_rest_of_the_settled_turn_folds() {
    for origin in ["app_owned", "provider_native"] {
        let rows = settled_rows(vec![subagent_item(
            "child",
            3,
            TurnItemStatus::Running,
            origin,
        )]);
        // The reasoning and command still fold; only the live card stays out.
        assert_eq!(
            ids(&rows),
            vec!["user", "turn-fold:parent-run", "child", "assistant"],
            "{origin}"
        );
    }
}

#[test]
fn folds_a_settled_child_with_its_settled_turn() {
    for status in [TurnItemStatus::Completed, TurnItemStatus::Failed] {
        let rows = settled_rows(vec![subagent_item("child", 3, status, "app_owned")]);
        assert!(!ids(&rows).contains(&"child"), "{status:?}");
        assert!(
            has_kind(&rows, MessagesTimelineRowKind::TurnFold),
            "{status:?}"
        );
    }
}

#[test]
fn keeps_a_launch_batch_visible_while_any_member_is_still_running() {
    let rows = settled_rows(vec![
        subagent_item("done", 3, TurnItemStatus::Completed, "app_owned"),
        subagent_item("live", 4, TurnItemStatus::Running, "app_owned"),
    ]);
    assert_eq!(
        ids(&rows),
        vec!["user", "turn-fold:parent-run", "done", "assistant"]
    );
    match find_id(&rows, "done") {
        Some(Row::Event {
            subagents: Some(subagents),
            ..
        }) => {
            let members: Vec<&str> = subagents.iter().map(|item| item.item.id.as_str()).collect();
            assert_eq!(members, vec!["done", "live"]);
        }
        other => panic!("expected a grouped subagent row, got {other:?}"),
    }
}

#[test]
fn keeps_a_running_child_visible_when_a_steer_supersedes_its_attempt() {
    let superseded = TimelineAttempt {
        attempt_ordinal: 1,
        ..attempt(
            "superseded-attempt",
            PARENT_RUN,
            AttemptStatus::Superseded,
            None,
        )
    };
    let entries = entries_from(vec![
        projected(
            2,
            v2_item(
                "command",
                PARENT_RUN,
                2,
                2,
                command_payload("pwd", Some("/repo"), Some(0)),
            ),
        ),
        subagent_item("child", 3, TurnItemStatus::Running, "app_owned"),
    ])
    .into_iter()
    .map(|entry| in_attempt(entry, &superseded))
    .collect();
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            PARENT_RUN,
            RunStatus::Running,
            Some(&at(0)),
            None,
        )),
        ..rows_input(entries, false)
    });
    assert_eq!(ids(&rows), vec!["attempt-fold:superseded-attempt", "child"]);
}

#[test]
fn keeps_each_turns_fold_between_its_own_prompt_and_answer_for_a_resumed_subagent() {
    // Stored shape from #15891: resuming a subagent moves its item to the
    // resuming run, but the item keeps the ordinal from the turn that started it.
    let run1 = "run-1";
    let run2 = "run-2";
    let item = |id: &str, run: &str, ordinal: u64, second: u32, payload: TurnItemPayload| {
        projected(ordinal, v2_item(id, run, ordinal, second, payload))
    };
    let items = vec![
        item(
            "prompt-1",
            run1,
            1_000_001,
            0,
            user_message_payload("prompt-1", "Prompt prompt-1"),
        ),
        item(
            "reasoning-1",
            run1,
            1_000_002,
            1,
            TurnItemPayload::Reasoning {
                text: "Thinking".into(),
                streaming: false,
            },
        ),
        projected(
            1_000_003,
            TurnItem {
                completed_at: Some(at(40)),
                updated_at: at(40),
                ..v2_item(
                    "subagent",
                    run2,
                    1_000_003,
                    2,
                    TurnItemPayload::Subagent {
                        subagent_id: "subagent".into(),
                        origin: "provider_native".into(),
                        child_thread_id: None,
                        prompt: "Research the bug".into(),
                        progress: None,
                        result: Some("Found it".into()),
                    },
                )
            },
        ),
        item(
            "answer-1",
            run1,
            1_000_004,
            3,
            assistant_message_payload("answer-1", "Answer answer-1", false),
        ),
        item(
            "prompt-2",
            run2,
            2_000_001,
            30,
            user_message_payload("prompt-2", "Prompt prompt-2"),
        ),
        item(
            "reasoning-2",
            run2,
            2_000_002,
            31,
            TurnItemPayload::Reasoning {
                text: "Thinking".into(),
                streaming: false,
            },
        ),
        item(
            "answer-2",
            run2,
            2_000_003,
            32,
            assistant_message_payload("answer-2", "Answer answer-2", false),
        ),
        item(
            "send-message",
            run2,
            2_000_004,
            33,
            dynamic_tool_payload(
                Some("SendMessage"),
                json!({ "to": "subagent" }),
                Some(json!("Sent")),
            ),
        ),
        item(
            "closing-2",
            run2,
            2_000_005,
            41,
            assistant_message_payload("closing-2", "Answer closing-2", false),
        ),
    ];
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            run2,
            RunStatus::Completed,
            Some(&at(30)),
            Some(&at(41)),
        )),
        ..rows_input(entries_from(items), false)
    });
    assert_eq!(
        ids(&rows),
        vec![
            "prompt-1",
            "turn-fold:run-1",
            "answer-1",
            "prompt-2",
            "turn-fold:run-2",
            "closing-2",
        ]
    );
}
