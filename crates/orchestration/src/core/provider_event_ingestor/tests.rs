//! Normalization tests for [`ProviderEventIngestor`]. Upstream's
//! `ProviderEventIngestor.test.ts` drives the same mapping through the Effect
//! layer and a real projection; these tests use an in-memory [`IngestReads`]
//! and check the domain events `normalize` returns, which is the part the
//! ingestor owns. Writes are covered by the EventSink tests.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2RunStatus;
use turin_contracts::{
    NodeId, PlanId, ProviderSessionId, RunId, ThreadId,
    orchestration_v2::{OrchestrationV2AppThread, OrchestrationV2DomainEvent},
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::provider_adapter::ProviderAdapterV2Event;

use super::*;

const T0: &str = "2026-01-01T00:00:00.000Z";

#[derive(Default)]
struct FakeReads {
    thread: Mutex<Option<OrchestrationV2AppThread>>,
    rewind: Mutex<RewindRecords>,
    plan: Mutex<Option<OrchestrationV2PlanArtifact>>,
}

#[async_trait]
impl IngestReads for FakeReads {
    async fn app_thread(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2AppThread>, BoxError> {
        Ok(self.thread.lock().expect("lock").clone())
    }

    async fn rewind_records(&self, _thread_id: &ThreadId) -> Result<RewindRecords, BoxError> {
        Ok(self.rewind.lock().expect("lock").clone())
    }

    async fn node_stored(&self, _node: &OrchestrationV2ExecutionNode) -> Result<bool, BoxError> {
        Ok(false)
    }

    async fn pending_native_user_inputs(
        &self,
        _thread_id: &ThreadId,
        _provider_turn_id: &ProviderTurnId,
    ) -> Result<PendingNativeUserInputs, BoxError> {
        Ok(PendingNativeUserInputs::default())
    }

    async fn plan(
        &self,
        _thread_id: &ThreadId,
        _plan_id: &PlanId,
    ) -> Result<Option<OrchestrationV2PlanArtifact>, BoxError> {
        Ok(self.plan.lock().expect("lock").clone())
    }
}

fn session_json() -> Value {
    json!({
        "id": "provider-session:1",
        "driver": "codex",
        "providerInstanceId": "codex",
        "status": "ready",
        "cwd": "/",
        "model": null,
        "capabilities": crate::projection::test_support::capabilities(),
        "createdAt": T0,
        "updatedAt": T0,
        "lastError": null,
    })
}

fn app_thread(title: &str) -> OrchestrationV2AppThread {
    serde_json::from_value(json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": "thread:a",
        "projectId": "project:test",
        "title": title,
        "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread:a"},
        "forkedFrom": null,
        "createdAt": T0,
        "updatedAt": T0,
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "lastVisitedAt": null,
        "deletedAt": null,
    }))
    .expect("app thread fixture")
}

fn input(event: Value) -> ProviderEventInput {
    ProviderEventInput {
        provider_session_id: ProviderSessionId::new("provider-session:1").expect("fixture id"),
        provider_instance_id: ProviderInstanceId::new("codex").expect("fixture id"),
        command_id: None,
        thread_id: ThreadId::new("thread:a").expect("fixture id"),
        run_id: Some(RunId::new("run:1").expect("fixture id")),
        node_id: Some(NodeId::new("node:1").expect("fixture id")),
        raw_event_id: None,
        event: serde_json::from_value::<ProviderAdapterV2Event>(event).expect("event fixture"),
        analytics_context: None,
    }
}

fn ingestor_reads(thread: Option<OrchestrationV2AppThread>) -> Arc<FakeReads> {
    Arc::new(FakeReads {
        thread: Mutex::new(thread),
        ..FakeReads::default()
    })
}

async fn normalize_with(
    reads: Arc<FakeReads>,
    input: &ProviderEventInput,
) -> Vec<OrchestrationV2DomainEvent> {
    normalize(reads.as_ref(), &|_| false, input)
        .await
        .expect("normalize")
}

#[tokio::test]
async fn provider_session_updates_map_to_provider_session_events_on_the_input_thread() {
    let event = json!({
        "type": "provider_session.updated",
        "driver": "codex",
        "provider_session": session_json(),
    });

    let events = normalize_with(ingestor_reads(None), &input(event)).await;

    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        OrchestrationV2DomainEvent::ProviderSessionUpdated { thread_id, .. }
            if thread_id == &ThreadId::new("thread:a").expect("fixture id")
    ));
}

#[tokio::test]
async fn a_new_app_thread_is_created_and_an_unchanged_one_is_skipped() {
    let event = json!({
        "type": "app_thread.created",
        "driver": "codex",
        "app_thread": serde_json::to_value(app_thread("First")).expect("thread json"),
    });

    let created = normalize_with(ingestor_reads(None), &input(event.clone())).await;
    assert!(matches!(
        created.as_slice(),
        [OrchestrationV2DomainEvent::ThreadCreated { .. }]
    ));

    let unchanged = normalize_with(ingestor_reads(Some(app_thread("First"))), &input(event)).await;
    assert!(unchanged.is_empty(), "same title and model write nothing");
}

#[tokio::test]
async fn a_renamed_app_thread_updates_its_metadata() {
    let event = json!({
        "type": "app_thread.created",
        "driver": "codex",
        "app_thread": serde_json::to_value(app_thread("Second")).expect("thread json"),
    });

    let events = normalize_with(ingestor_reads(Some(app_thread("First"))), &input(event)).await;

    assert!(matches!(
        events.as_slice(),
        [OrchestrationV2DomainEvent::ThreadMetadataUpdated { .. }]
    ));
}

#[tokio::test]
async fn a_failed_terminal_with_an_item_ordinal_writes_a_failure_item() {
    let event = json!({
        "type": "turn.terminal",
        "driver": "codex",
        "provider_thread_id": "provider-thread:1",
        "provider_turn_id": "provider-turn:1",
        "run_ordinal": 1,
        "status": "failed",
        "failure": {
            "class": "provider_error",
            "message": "boom",
            "code": null,
            "retryable": null,
            "resetAt": null,
        },
        "failure_item_ordinal": 3,
        "thread_disposition": "reusable",
    });

    let events = normalize_with(ingestor_reads(None), &input(event)).await;

    assert!(matches!(
        events.as_slice(),
        [OrchestrationV2DomainEvent::TurnItemUpdated { .. }]
    ));
}

#[tokio::test]
async fn a_failed_terminal_without_an_item_ordinal_writes_no_item() {
    let event = json!({
        "type": "turn.terminal",
        "driver": "codex",
        "provider_thread_id": "provider-thread:1",
        "provider_turn_id": "provider-turn:1",
        "run_ordinal": 1,
        "status": "failed",
        "failure": {
            "class": "provider_error",
            "message": "boom",
            "code": null,
            "retryable": null,
            "resetAt": null,
        },
        "thread_disposition": "reusable",
    });

    let events = normalize_with(ingestor_reads(None), &input(event)).await;

    assert!(events.is_empty());
}

fn node_json(run: &str, ordinal: u64, status: &str) -> Value {
    json!({
        "id": format!("node:{run}:root"), "threadId": "thread:a", "runId": run,
        "parentNodeId": null, "rootNodeId": format!("node:{run}:root"), "kind": "root_turn",
        "status": status, "countsForRun": true, "providerThreadId": "provider-thread:rewound",
        "providerTurnId": null, "nativeItemRef": null, "runtimeRequestId": null,
        "checkpointScopeId": "checkpoint-scope:rewound", "startedAt": T0, "completedAt": null,
        "ordinal": ordinal,
    })
}

/// A run as execution leaves it: attempt, provider turn, root node and, once
/// completed, a checkpoint. A `None` native turn id is a weak ref.
fn seed_run(
    records: &mut RewindRecords,
    ordinal: u64,
    provider_thread: &str,
    status: &str,
    native_turn_id: Option<&str>,
) {
    let run_id = format!("run:rewound:{ordinal}");
    let mut run = crate::projection::test_support::run("thread:a", &run_id, ordinal, status);
    run["providerThreadId"] = json!(provider_thread);
    run["activeAttemptId"] = json!(format!("attempt:rewound:{ordinal}"));
    records.runs.push(serde_json::from_value(run).expect("run"));
    records.attempts.push(
        serde_json::from_value(json!({
            "id": format!("attempt:rewound:{ordinal}"), "runId": run_id, "attemptOrdinal": 1,
            "rootNodeId": format!("node:{run_id}:root"), "providerInstanceId": "codex",
            "providerThreadId": provider_thread,
            "providerTurnId": format!("provider-turn:rewound:{ordinal}"),
            "reason": "initial", "status": status, "startedAt": T0, "completedAt": null,
        }))
        .expect("attempt"),
    );
    let native_ref = match native_turn_id {
        Some(id) => json!({"driver": "codex", "nativeId": id, "strength": "strong"}),
        None => {
            json!({"driver": "codex", "nativeId": format!("synthetic:{ordinal}"), "strength": "weak"})
        }
    };
    records.provider_turns.push(
        serde_json::from_value(json!({
            "id": format!("provider-turn:rewound:{ordinal}"), "providerThreadId": provider_thread,
            "nodeId": format!("node:{run_id}:root"),
            "runAttemptId": format!("attempt:rewound:{ordinal}"), "nativeTurnRef": native_ref,
            "ordinal": ordinal, "status": status, "startedAt": T0, "completedAt": null,
        }))
        .expect("turn"),
    );
    let mut node = node_json(&run_id, ordinal, status);
    node["providerThreadId"] = json!(provider_thread);
    node.as_object_mut().expect("object").remove("ordinal");
    records
        .nodes
        .push(serde_json::from_value(node).expect("node"));
    if status == "completed" {
        records.checkpoints.push(
            serde_json::from_value(json!({
                "id": format!("checkpoint:rewound:{ordinal}"), "threadId": "thread:a",
                "scopeId": "checkpoint-scope:rewound", "runId": run_id,
                "nodeId": format!("node:{run_id}:root"), "parentCheckpointId": null,
                "ordinalWithinScope": ordinal, "appRunOrdinal": ordinal,
                "ref": format!("refs/t3/checkpoints/rewound/{ordinal}"), "status": "ready",
                "files": [], "capturedAt": T0,
            }))
            .expect("checkpoint"),
        );
    }
}

#[tokio::test]
async fn a_native_rewind_rolls_back_the_runs_that_fell_off_the_branch() {
    let mut records = RewindRecords::default();
    seed_run(
        &mut records,
        1,
        "provider-thread:other",
        "completed",
        Some("other-turn"),
    );
    seed_run(
        &mut records,
        2,
        "provider-thread:rewound",
        "completed",
        Some("kept-turn"),
    );
    seed_run(
        &mut records,
        3,
        "provider-thread:rewound",
        "completed",
        Some("abandoned-turn"),
    );
    seed_run(
        &mut records,
        4,
        "provider-thread:rewound",
        "interrupted",
        None,
    );
    seed_run(&mut records, 5, "provider-thread:rewound", "running", None);
    let reads = Arc::new(FakeReads {
        rewind: Mutex::new(records),
        ..FakeReads::default()
    });
    let mut event = input(json!({
        "type": "provider_thread.updated",
        "driver": "codex",
        "provider_thread": {
            "id": "provider-thread:rewound", "driver": "codex", "providerInstanceId": "codex",
            "providerSessionId": "provider-session:1", "appThreadId": "thread:a",
            "ownerNodeId": null, "nativeThreadRef": null, "nativeConversationHeadRef": null,
            "status": "idle", "firstRunOrdinal": 2, "lastRunOrdinal": 5, "handoffIds": [],
            "forkedFrom": null, "createdAt": T0, "updatedAt": T0,
        },
        "retained_native_turn_ids": ["turn-started-outside-t3", "kept-turn"],
    }));
    event.run_id = Some(RunId::new("run:rewound:5").expect("run id"));

    let events = normalize_with(reads, &event).await;

    let mut stale = Vec::new();
    let mut rolled_back = Vec::new();
    let mut nodes = Vec::new();
    for event in &events {
        match event {
            OrchestrationV2DomainEvent::CheckpointCaptured { payload, .. } => {
                assert_eq!(
                    payload.status,
                    turin_contracts::orchestration_v2::OrchestrationV2CheckpointStatus::Stale
                );
                stale.push(payload.run_id.clone().expect("run").to_string());
            }
            OrchestrationV2DomainEvent::RunUpdated { payload, .. } => {
                assert_eq!(payload.status, OrchestrationV2RunStatus::RolledBack);
                rolled_back.push(payload.id.to_string());
            }
            OrchestrationV2DomainEvent::NodeUpdated { payload, .. } => {
                nodes.push(payload.id.to_string());
            }
            OrchestrationV2DomainEvent::ProviderThreadUpdated { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(stale, vec!["run:rewound:3"]);
    assert_eq!(rolled_back, vec!["run:rewound:3", "run:rewound:4"]);
    assert_eq!(
        nodes,
        vec!["node:run:rewound:3:root", "node:run:rewound:4:root"]
    );
}

fn todo(
    steps: &[(&str, &str, &str)],
) -> Vec<turin_contracts::orchestration_v2::OrchestrationV2PlanStep> {
    steps
        .iter()
        .map(|(id, text, status)| {
            serde_json::from_value(json!({"id": id, "text": text, "status": status})).expect("step")
        })
        .collect()
}

fn at(seconds: i64) -> UtcDateTime {
    UtcDateTime::from_datetime(
        chrono::DateTime::parse_from_rfc3339("2026-09-07T00:00:00.000Z")
            .expect("time")
            .with_timezone(&chrono::Utc)
            + chrono::Duration::seconds(seconds),
    )
}

fn durations(
    steps: &[turin_contracts::orchestration_v2::OrchestrationV2PlanStep],
) -> Vec<Option<u64>> {
    steps.iter().map(|step| step.duration_ms).collect()
}

#[test]
fn plan_step_durations_carry_through_consecutive_ingestion() {
    let first = with_plan_step_durations(
        &todo(&[
            ("a", "Verify", "running"),
            ("b", "Verify", "pending"),
            ("c", "Report", "pending"),
        ]),
        None,
        at(0),
    );
    let second = with_plan_step_durations(
        &todo(&[
            ("a", "Verify", "completed"),
            ("b", "Verify", "pending"),
            ("c", "Report", "pending"),
        ]),
        Some(&first),
        at(3),
    );
    assert_eq!(durations(&second), vec![Some(3_000), None, None]);
    let third = with_plan_step_durations(
        &todo(&[
            ("a", "Verify", "completed"),
            ("b", "Verify", "completed"),
            ("c", "Report", "pending"),
        ]),
        Some(&second),
        at(7),
    );
    assert_eq!(durations(&third), vec![Some(3_000), Some(4_000), None]);
    let fourth = with_plan_step_durations(
        &todo(&[
            ("a", "Verify", "completed"),
            ("b", "Verify", "completed"),
            ("c", "Report", "completed"),
        ]),
        Some(&third),
        at(12),
    );
    assert_eq!(
        durations(&fourth),
        vec![Some(3_000), Some(4_000), Some(5_000)]
    );

    // Changed text inherits no timing: the inserted task runs 2s on its own.
    let inserted = with_plan_step_durations(
        &todo(&[
            ("a", "Inserted task", "running"),
            ("b", "Verify", "completed"),
            ("c", "Different completed task", "completed"),
        ]),
        Some(&fourth),
        at(12),
    );
    let finished = with_plan_step_durations(
        &todo(&[
            ("a", "Inserted task", "completed"),
            ("b", "Verify", "completed"),
            ("c", "Different completed task", "completed"),
        ]),
        Some(&inserted),
        at(14),
    );
    assert_eq!(durations(&finished), vec![Some(2_000), Some(4_000), None]);
}

#[test]
fn a_large_file_change_is_stored_bounded() {
    let big = "x".repeat(40_000);
    let item: OrchestrationV2TurnItem =
        serde_json::from_value(crate::projection::test_support::turn_item(
            "thread:a",
            "item:fc",
            Some("run:1"),
            1,
            "file_change",
            "completed",
            json!({"fileName": "a.txt", "diffStr": big, "oldStr": "small", "newStr": big}),
        ))
        .expect("file change");
    let OrchestrationV2TurnItem::FileChange {
        diff_str,
        old_str,
        new_str,
        ..
    } = bound_file_change_for_storage(item)
    else {
        panic!("still a file change");
    };
    for bounded in [diff_str.expect("diff"), new_str.expect("new")] {
        assert!(
            bounded.ends_with("\n… output truncated for transport"),
            "{}",
            &bounded[bounded.len() - 40..]
        );
        assert!(bounded.len() <= 32_768 + 40);
    }
    assert_eq!(old_str.as_deref(), Some("small"));
}

#[tokio::test]
async fn an_oom_killed_failed_turn_reads_as_out_of_memory() {
    let event = json!({
        "type": "turn.terminal", "driver": "codex",
        "provider_thread_id": "provider-thread:1", "provider_turn_id": "provider-turn:1",
        "run_ordinal": 1, "status": "failed",
        "failure": {"class": "provider_error", "message": "boom", "code": null, "retryable": null, "resetAt": null},
        "failure_item_ordinal": 3, "thread_disposition": "reusable",
    });
    let events = normalize(ingestor_reads(None).as_ref(), &|_| true, &input(event))
        .await
        .expect("normalize");
    let [OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. }] = events.as_slice() else {
        panic!("one failure item");
    };
    let text = serde_json::to_string(payload).expect("item encodes");
    assert!(text.contains("Killed: out of memory."), "{text}");
    assert!(text.contains("oom_kill"), "{text}");
}

#[test]
fn analytics_properties_carry_usage_model_and_duration() {
    let turn = serde_json::from_value(json!({
        "id": "provider-turn:1", "providerThreadId": "provider-thread:1", "nodeId": "node:1",
        "runAttemptId": null, "nativeTurnRef": null, "ordinal": 1, "status": "completed",
        "startedAt": T0, "completedAt": "2026-01-01T00:00:02.500Z",
        "turnTokenUsage": {"usageScope": "main_agent", "hasSubagents": false,
            "usageStatus": "complete", "inputTokens": 10, "outputTokens": 4},
    }))
    .expect("turn");
    let context = ProviderTurnAnalyticsContext {
        model_selection: serde_json::from_value(json!({
            "instanceId": "codex", "model": "gpt-5.4",
            "options": [{"id": "reasoningEffort", "value": "high"}],
        }))
        .expect("selection"),
        runtime_mode: None,
        interaction_mode: None,
    };
    let driver = serde_json::from_value(json!("codex")).expect("driver");
    let properties = extras::provider_turn_analytics_properties(&driver, &turn, Some(&context));
    assert_eq!(properties["provider"], "codex");
    assert_eq!(properties["usageStatus"], "complete");
    assert_eq!(properties["inputTokens"], 10);
    assert_eq!(properties["model"], "gpt-5.4");
    assert_eq!(properties["effort"], "high");
    assert_eq!(properties["durationMs"], 2_500);
}
