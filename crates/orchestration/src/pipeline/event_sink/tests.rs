//! Ports of the EventSink / EffectOutbox / CommandReceipt cases in upstream
//! `FoundationPersistence.test.ts`, run against a minimal in-test
//! `ProjectionApplier` that maintains only the rows the sink's guards read.

use super::*;
use crate::pipeline::domain_event::test_support::{
    metadata_updated, thread_created, thread_json,
};
use crate::pipeline::effect_outbox::{EffectRequest, EffectStatus};
use futures::StreamExt;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent as Event;

struct TestProjection {
    fail: AtomicBool,
}

impl TestProjection {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            fail: AtomicBool::new(false),
        })
    }
}

fn json_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

impl ProjectionApplier for TestProjection {
    fn normalize_turn_item(
        &self,
        _tx: &Transaction<'_>,
        event: Event,
        _run_ordinal: Option<u64>,
    ) -> Result<Event, PersistenceError> {
        Ok(event)
    }

    fn apply_event(
        &self,
        tx: &Transaction<'_>,
        event: &Event,
    ) -> Result<(), PersistenceError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(decode_err("TestProjection.apply", "projection failed"));
        }
        let value = serde_json::to_value(event).unwrap();
        let kind = json_text(&value, "type").unwrap();
        let payload = value["payload"].clone();
        let payload_json = payload.to_string();
        match kind.as_str() {
            "run.created" | "run.updated" => {
                tx.execute(
                    "INSERT INTO orchestration_v2_projection_runs (run_id, thread_id, ordinal, provider, provider_thread_id, status, requested_at, completed_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(run_id) DO UPDATE SET status = excluded.status, payload_json = excluded.payload_json",
                    params![
                        json_text(&payload, "id"),
                        json_text(&payload, "threadId"),
                        payload["ordinal"].as_i64(),
                        json_text(&payload, "providerInstanceId"),
                        json_text(&payload, "providerThreadId"),
                        json_text(&payload, "status"),
                        json_text(&payload, "requestedAt"),
                        json_text(&payload, "completedAt"),
                        payload_json,
                    ],
                )
                .map_err(|err| PersistenceError::sql("TestProjection.run", &err))?;
            }
            "provider-thread.updated" => {
                tx.execute(
                    "INSERT INTO orchestration_v2_projection_provider_threads (provider_thread_id, thread_id, owner_node_id, provider, provider_session_id, status, first_run_ordinal, last_run_ordinal, updated_at, payload_json) VALUES (?1, ?2, NULL, ?3, NULL, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(provider_thread_id) DO UPDATE SET status = excluded.status, last_run_ordinal = excluded.last_run_ordinal, payload_json = excluded.payload_json",
                    params![
                        json_text(&payload, "id"),
                        json_text(&payload, "appThreadId"),
                        json_text(&payload, "providerInstanceId"),
                        json_text(&payload, "status"),
                        payload["firstRunOrdinal"].as_i64(),
                        payload["lastRunOrdinal"].as_i64(),
                        json_text(&payload, "updatedAt"),
                        payload_json,
                    ],
                )
                .map_err(|err| PersistenceError::sql("TestProjection.provider-thread", &err))?;
            }
            "runtime-request.updated" => {
                tx.execute(
                    "INSERT INTO orchestration_v2_projection_runtime_requests (runtime_request_id, thread_id, node_id, provider_turn_id, kind, status, created_at, resolved_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8) ON CONFLICT(runtime_request_id) DO UPDATE SET status = excluded.status, payload_json = excluded.payload_json",
                    params![
                        json_text(&payload, "id"),
                        json_text(&value, "threadId"),
                        json_text(&payload, "nodeId"),
                        json_text(&payload, "providerTurnId"),
                        json_text(&payload, "kind"),
                        json_text(&payload, "status"),
                        json_text(&payload, "createdAt"),
                        payload_json,
                    ],
                )
                .map_err(|err| PersistenceError::sql("TestProjection.request", &err))?;
            }
            _ => {}
        }
        tx.execute(
            "INSERT INTO test_projection (event_id) VALUES (?1)",
            [json_text(&value, "id")],
        )
        .map_err(|err| PersistenceError::sql("TestProjection.apply", &err))?;
        Ok(())
    }

    fn apply_project_event(
        &self,
        _tx: &Transaction<'_>,
        _event: &ProjectEvent,
    ) -> Result<(), PersistenceError> {
        Ok(())
    }

    fn schema_version(&self) -> i64 {
        2
    }
}

async fn setup() -> (Db, EventSink, Arc<TestProjection>) {
    let db = Db::open_in_memory().await.unwrap();
    db.write(|tx| {
        tx.execute_batch("CREATE TABLE test_projection (event_id TEXT PRIMARY KEY);")
            .map_err(|err| PersistenceError::sql("test.setup", &err))
    })
    .await
    .unwrap();
    let projection = TestProjection::new();
    let sink = EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db.clone()),
        projection.clone(),
    );
    (db, sink, projection)
}

async fn count(db: &Db, table: &'static str) -> i64 {
    db.read(move |conn| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .map_err(|err| PersistenceError::sql("test.count", &err))
    })
    .await
    .unwrap()
}

fn terminal_cleanup(command_id: &CommandId, thread_id: &ThreadId, id: &str) -> PendingEffect {
    PendingEffect {
        id: id.into(),
        command_id: command_id.clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::TerminalCleanup {},
        available_at: None,
    }
}

fn commit_input(command: &str, thread: &str, effects: Vec<PendingEffect>) -> CommandCommit {
    CommandCommit {
        command_id: CommandId::new(command).unwrap(),
        thread_id: ThreadId::new(thread).unwrap(),
        command_type: "foundation.test".into(),
        accepted_at: UtcDateTime::now(),
        events: vec![thread_created(&format!("event:{command}"), thread)],
        effects,
        cancel_unsettled_effects: None,
    }
}

#[tokio::test]
async fn command_receipt_event_projection_and_effect_commit_together() {
    let (db, sink, _) = setup().await;
    let command_id = CommandId::new("cmd:sink:test").unwrap();
    let thread_id = ThreadId::new("thread:sink:test").unwrap();
    let input = commit_input(
        "cmd:sink:test",
        "thread:sink:test",
        vec![terminal_cleanup(&command_id, &thread_id, "effect:sink:test")],
    );
    let first = sink.commit_command(input.clone()).await.unwrap();
    assert!(first.committed);
    assert!(first.receipt.result_sequence > 0);
    assert_eq!(first.stored_events.len(), 1);
    let repeated = sink.commit_command(input).await.unwrap();
    assert!(!repeated.committed);
    assert_eq!(repeated.receipt, first.receipt);
    assert_eq!(repeated.stored_events, first.stored_events);
    assert_eq!(count(&db, "orchestration_events").await, 1);
    assert_eq!(count(&db, "orchestration_command_receipts").await, 1);
    assert_eq!(count(&db, "orchestration_v2_effect_outbox").await, 1);
    assert_eq!(count(&db, "test_projection").await, 1);
    let metadata_sequence: i64 = db
        .read(|conn| {
            conn.query_row(
                "SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name = 'thread-projections'",
                [],
                |row| row.get(0),
            )
            .map_err(|err| PersistenceError::sql("test.metadata", &err))
        })
        .await
        .unwrap();
    assert_eq!(metadata_sequence as u64, first.receipt.result_sequence);
}

#[tokio::test]
async fn projection_failure_rolls_back_receipt_event_and_effect() {
    let (db, sink, projection) = setup().await;
    projection.fail.store(true, Ordering::SeqCst);
    let command_id = CommandId::new("cmd:sink:rollback").unwrap();
    let thread_id = ThreadId::new("thread:sink:rollback").unwrap();
    let result = sink
        .commit_command(commit_input(
            "cmd:sink:rollback",
            "thread:sink:rollback",
            vec![terminal_cleanup(&command_id, &thread_id, "effect:rollback")],
        ))
        .await;
    assert!(matches!(result, Err(SinkError::Write { event_count: 1, .. })));
    assert_eq!(count(&db, "orchestration_events").await, 0);
    assert_eq!(count(&db, "orchestration_command_receipts").await, 0);
    assert_eq!(count(&db, "orchestration_v2_effect_outbox").await, 0);
}

#[tokio::test]
async fn a_command_that_produces_no_events_is_refused_and_leaves_no_receipt() {
    let (db, sink, _) = setup().await;
    let mut input = commit_input("cmd:empty", "thread:empty", vec![]);
    input.events.clear();
    assert!(sink.commit_command(input).await.is_err());
    assert_eq!(count(&db, "orchestration_command_receipts").await, 0);
}

#[tokio::test]
async fn retries_keep_one_durable_effect_and_do_not_wake_claimers() {
    let (_, sink, _) = setup().await;
    let command_id = CommandId::new("cmd:idempotent").unwrap();
    let thread_id = ThreadId::new("thread:idempotent").unwrap();
    let input = commit_input(
        "cmd:idempotent",
        "thread:idempotent",
        vec![terminal_cleanup(&command_id, &thread_id, "effect:idempotent")],
    );
    assert!(sink.commit_command(input.clone()).await.unwrap().committed);
    // The commit's own wakeup is consumed here...
    sink.outbox().await_available().await;
    assert!(!sink.commit_command(input).await.unwrap().committed);
    // ...and the retry produces none.
    let woke = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        sink.outbox().await_available(),
    )
    .await;
    assert!(woke.is_err(), "an idempotent retry must not wake claimers");
    assert_eq!(
        sink.outbox()
            .list_by_command_id(command_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_command_id_used_by_a_project_command_is_not_a_thread_replay() {
    let (_, sink, _) = setup().await;
    let project = project_commit("cmd:shared-id", "project:shared");
    assert!(sink.commit_project_command(project).await.unwrap().committed);
    let result = sink
        .commit_command(commit_input("cmd:shared-id", "thread:shared", vec![]))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn rejected_commands_record_a_receipt_once() {
    let (_, sink, _) = setup().await;
    let thread = "thread:rejected";
    sink.commit_command(commit_input("cmd:created", thread, vec![]))
        .await
        .unwrap();
    let rejected = sink
        .commit_rejected_command(
            CommandId::new("cmd:rejected").unwrap(),
            ThreadId::new(thread).unwrap(),
            "message.dispatch".into(),
            UtcDateTime::now(),
            "no capacity".into(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(rejected.error.as_deref(), Some("no capacity"));
    assert!(rejected.result_sequence > 0);
    let again = sink
        .commit_rejected_command(
            CommandId::new("cmd:rejected").unwrap(),
            ThreadId::new(thread).unwrap(),
            "message.dispatch".into(),
            UtcDateTime::now(),
            "a different reason".into(),
        )
        .await
        .unwrap();
    assert_eq!(again.error.as_deref(), Some("no capacity"));
}

fn project_commit(command: &str, project: &str) -> ProjectCommit {
    ProjectCommit {
        command_id: CommandId::new(command).unwrap(),
        project_id: ProjectId::new(project).unwrap(),
        command_type: "project.create".into(),
        accepted_at: UtcDateTime::now(),
        event: ProjectEventInput {
            event_id: EventId::new(format!("event:{command}")).unwrap(),
            aggregate_id: project.into(),
            event_type: "project.created".into(),
            occurred_at: "2026-06-21T00:00:00.000Z".into(),
            command_id: Some(CommandId::new(command).unwrap()),
            causation_event_id: None,
            correlation_id: Some(CommandId::new(command).unwrap()),
            payload: json!({"projectId": project, "title": "P", "workspaceRoot": "/p",
                "defaultModelSelection": null, "faviconPath": null, "projectIcon": null,
                "scripts": [], "createdAt": "2026-06-21T00:00:00.000Z", "updatedAt": "2026-06-21T00:00:00.000Z"}),
            metadata: json!({}),
        },
    }
}

#[tokio::test]
async fn project_commands_are_idempotent_and_rejections_replay() {
    let (db, sink, _) = setup().await;
    let first = sink
        .commit_project_command(project_commit("cmd:project", "project:a"))
        .await
        .unwrap();
    assert!(first.committed);
    assert!(first.receipt.result_sequence > 0);
    let retry = sink
        .commit_project_command(project_commit("cmd:project", "project:a"))
        .await
        .unwrap();
    assert!(!retry.committed);
    assert_eq!(retry.receipt, first.receipt);
    assert_eq!(count(&db, "orchestration_events").await, 1);

    let rejected = sink
        .commit_rejected_project_command(
            CommandId::new("cmd:project-rejected").unwrap(),
            ProjectId::new("project:a").unwrap(),
            "project.delete".into(),
            UtcDateTime::now(),
            "{\"_tag\":\"x\"}".into(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(rejected.result_sequence, first.receipt.result_sequence);
    let replay = sink
        .commit_rejected_project_command(
            CommandId::new("cmd:project-rejected").unwrap(),
            ProjectId::new("project:a").unwrap(),
            "project.delete".into(),
            UtcDateTime::now(),
            "different".into(),
        )
        .await
        .unwrap();
    assert_eq!(replay.error.as_deref(), Some("{\"_tag\":\"x\"}"));
}

#[test]
fn project_event_actor_follows_persistence_precedence() {
    let mut event = project_commit("provider:cmd", "p").event;
    assert_eq!(infer_actor_kind(&event), "provider");
    event.command_id = Some(CommandId::new("server:cmd").unwrap());
    event.metadata = json!({"providerTurnId": "t"});
    // A server-prefixed command wins over provider metadata, as in persistence.
    assert_eq!(infer_actor_kind(&event), "server");
    event.command_id = Some(CommandId::new("client-cmd").unwrap());
    assert_eq!(infer_actor_kind(&event), "provider");
    event.metadata = json!({});
    assert_eq!(infer_actor_kind(&event), "client");
    event.command_id = None;
    assert_eq!(infer_actor_kind(&event), "server");
}

#[tokio::test]
async fn paginates_catch_up_beyond_the_event_store_read_limit() {
    let (_, sink, _) = setup().await;
    let thread = "thread:large-catch-up";
    let event_count = 1_005;
    let mut events = vec![thread_created("event:catch-up:0", thread)];
    events.extend((1..event_count).map(|index| {
        metadata_updated(
            &format!("event:catch-up:{index}"),
            thread,
            &format!("Catch-up update {index}"),
        )
    }));
    sink.write(WriteInput::events(events)).await.unwrap();
    let stream = sink
        .stream(StreamOptions {
            after_sequence: Some(0),
            ..StreamOptions::default()
        })
        .await
        .unwrap();
    let replayed: Vec<_> = stream.take(event_count).collect().await;
    let sequences: Vec<u64> = replayed
        .into_iter()
        .map(|item| item.unwrap().sequence)
        .collect();
    assert_eq!(sequences, (1..=event_count as u64).collect::<Vec<_>>());
}

#[tokio::test]
async fn does_not_lose_or_duplicate_events_while_transitioning_from_catch_up_to_live() {
    let (_, sink, _) = setup().await;
    let thread = "thread:stream-race";
    let created = sink
        .write(WriteInput::events(vec![thread_created(
            "event:stream-race:0",
            thread,
        )]))
        .await
        .unwrap();
    let mut after = created[0].sequence as i64;
    for index in 1..=32 {
        let mut stream = sink
            .stream(StreamOptions {
                thread_id: Some(ThreadId::new(thread).unwrap()),
                after_sequence: Some(after),
                event_type: None,
            })
            .await
            .unwrap();
        let reader = tokio::spawn(async move { stream.next().await });
        tokio::task::yield_now().await;
        let written = sink
            .write(WriteInput::events(vec![metadata_updated(
                &format!("event:stream-race:{index}"),
                thread,
                &format!("Race update {index}"),
            )]))
            .await
            .unwrap();
        let received = reader.await.unwrap().expect("stream ended early").unwrap();
        assert_eq!(received.sequence, written[0].sequence);
        assert_eq!(event_type(&received.event), "thread.metadata-updated");
        after = received.sequence as i64;
    }
}

#[tokio::test]
async fn filters_worker_replay_and_live_queues_without_losing_matching_events() {
    let (db, sink, _) = setup().await;
    let thread = "thread:filtered-worker";
    let other = "thread:filtered-worker-other";
    let run = |id: &str, thread: &str, status: &str| -> Event {
        serde_json::from_value(json!({
            "type": "run.updated",
            "id": format!("event:{id}"),
            "threadId": thread,
            "runId": id,
            "occurredAt": "2026-06-21T00:00:00.000Z",
            "payload": run_json(id, thread, status),
        }))
        .unwrap()
    };
    let history = sink
        .write(WriteInput::events(vec![
            thread_created("event:filtered:created", thread),
            thread_created("event:filtered:other", other),
            run("run:filtered", thread, "completed"),
            run("run:filtered-other", other, "completed"),
        ]))
        .await
        .unwrap();
    // Unrelated payloads are skipped in SQL, before decoding, even when a
    // retained row is unreadable.
    let corrupted = history[0].sequence;
    db.write(move |tx| {
        tx.execute(
            "UPDATE orchestration_events SET payload_json = 'unreadable unrelated payload' WHERE sequence = ?1",
            [corrupted],
        )
        .map_err(|err| PersistenceError::sql("test.corrupt", &err))?;
        Ok(())
    })
    .await
    .unwrap();
    let mut stream = sink
        .stream(StreamOptions {
            thread_id: Some(ThreadId::new(thread).unwrap()),
            after_sequence: None,
            event_type: Some("run.updated".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap().sequence,
        history[2].sequence
    );
    // The worker is busy with the previous batch while the thread publishes a
    // lot of unrelated output. Its live queue receives just run updates.
    sink.write(WriteInput::events(
        (0..1_001)
            .map(|index| {
                metadata_updated(
                    &format!("event:filtered:output:{index}"),
                    thread,
                    &format!("Output {index}"),
                )
            })
            .collect(),
    ))
    .await
    .unwrap();
    let live = sink
        .write(WriteInput::events(vec![
            run("run:filtered", thread, "interrupted"),
            run("run:filtered", thread, "failed"),
        ]))
        .await
        .unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap().sequence,
        live[0].sequence
    );
    assert_eq!(
        stream.next().await.unwrap().unwrap().sequence,
        live[1].sequence
    );
}

#[tokio::test]
async fn publishes_live_events_in_commit_order_across_concurrent_writers() {
    let (_, sink, _) = setup().await;
    let start = sink.latest_sequence(None).await.unwrap();
    let stream = sink
        .stream(StreamOptions {
            after_sequence: Some(start),
            ..StreamOptions::default()
        })
        .await
        .unwrap();
    let writers = 24;
    let mut tasks = Vec::new();
    for index in 0..writers {
        let sink = sink.clone();
        tasks.push(tokio::spawn(async move {
            let thread = format!("thread:publish-order:{index}");
            sink.write(WriteInput {
                effects: vec![terminal_cleanup(
                    &CommandId::new(format!("cmd:publish-order:{index}")).unwrap(),
                    &ThreadId::new(&thread).unwrap(),
                    &format!("effect:publish-order:{index}"),
                )],
                ..WriteInput::events(vec![thread_created(
                    &format!("event:publish-order:{index}"),
                    &thread,
                )])
            })
            .await
            .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let published: Vec<u64> = stream
        .take(writers)
        .map(|item| item.unwrap().sequence)
        .collect()
        .await;
    let mut sorted = published.clone();
    sorted.sort_unstable();
    assert_eq!(published, sorted);
    assert_eq!(published.len(), writers);
}

#[tokio::test]
async fn a_dropped_stream_leaves_the_live_hub() {
    let (_, sink, _) = setup().await;
    let stream = sink.stream(StreamOptions::default()).await.unwrap();
    assert_eq!(sink.live.subscribers.lock().len(), 1);
    drop(stream);
    assert_eq!(sink.live.subscribers.lock().len(), 0);
}

fn run_json(id: &str, thread: &str, status: &str) -> Value {
    json!({
        "id": id, "threadId": thread, "ordinal": 1, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
        "providerThreadId": null, "userMessageId": format!("message:{id}"),
        "rootNodeId": null, "activeAttemptId": null, "status": status,
        "requestedAt": "2026-06-21T00:00:00.000Z", "startedAt": "2026-06-21T00:00:00.000Z",
        "completedAt": "2026-06-21T00:00:00.000Z", "checkpointId": null, "contextHandoffId": null,
    })
}

fn run_event(kind: &str, event_id: &str, payload: Value) -> Event {
    serde_json::from_value(json!({
        "type": kind, "id": event_id,
        "threadId": payload["threadId"], "runId": payload["id"],
        "occurredAt": "2026-06-21T00:00:00.000Z", "payload": payload,
    }))
    .unwrap()
}

fn provider_thread_json(last_run_ordinal: u64, status: &str, tasks: Value) -> Value {
    json!({
        "id": "provider-thread:owner", "driver": "codex", "providerInstanceId": "codex",
        "providerSessionId": null, "appThreadId": "thread:owner", "ownerNodeId": null,
        "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": status,
        "firstRunOrdinal": 1, "lastRunOrdinal": last_run_ordinal, "handoffIds": [],
        "forkedFrom": null, "pendingBackgroundTasks": tasks,
        "createdAt": "2026-06-21T00:00:00.000Z", "updatedAt": "2026-06-21T00:00:00.000Z",
    })
}

fn provider_thread_event(event_id: &str, payload: Value) -> Event {
    serde_json::from_value(json!({
        "type": "provider-thread.updated", "id": event_id, "threadId": "thread:owner",
        "driver": "codex", "providerInstanceId": "codex",
        "occurredAt": "2026-06-21T00:00:00.000Z", "payload": payload,
    }))
    .unwrap()
}

#[tokio::test]
async fn guards_post_terminal_provider_thread_writes_by_attempt_and_run_ordinal() {
    let (db, sink, _) = setup().await;
    let mut run = run_json("run:owner", "thread:owner", "completed");
    run["activeAttemptId"] = json!("attempt:owner");
    run["providerThreadId"] = json!("provider-thread:owner");
    let bg = json!([{"taskId": "bg-owner", "description": "sleep 20", "kind": "command"}]);
    sink.write(WriteInput::events(vec![
        thread_created("event:owner:thread", "thread:owner"),
        run_event("run.created", "event:owner:run", run.clone()),
        provider_thread_event("event:owner:pt", provider_thread_json(1, "idle", bg.clone())),
    ]))
    .await
    .unwrap();

    let owner_write = |attempt: &str, ordinal: u64, event: Event| ProviderThreadOwnerWrite {
        write: WriteInput::events(vec![event]),
        provider_thread_id: ProviderThreadId::new("provider-thread:owner").unwrap(),
        run_id: RunId::new("run:owner").unwrap(),
        active_attempt_id: RunAttemptId::new(attempt).unwrap(),
        expected_last_run_ordinal: ordinal,
    };

    let cleared = sink
        .write_if_provider_thread_owner(owner_write(
            "attempt:owner",
            1,
            provider_thread_event(
                "event:owner:clear",
                provider_thread_json(1, "idle", json!([])),
            ),
        ))
        .await
        .unwrap();
    assert!(cleared.committed);
    assert_eq!(cleared.stored_events.len(), 1);

    // A replacement attempt takes the run.
    let mut replaced = run.clone();
    replaced["activeAttemptId"] = json!("attempt:owner:replacement");
    replaced["status"] = json!("running");
    sink.write(WriteInput::events(vec![run_event(
        "run.updated",
        "event:owner:replacement",
        replaced.clone(),
    )]))
    .await
    .unwrap();
    let superseded = sink
        .write_if_provider_thread_owner(owner_write(
            "attempt:owner",
            1,
            provider_thread_event(
                "event:owner:superseded",
                provider_thread_json(1, "active", bg),
            ),
        ))
        .await
        .unwrap();
    assert!(!superseded.committed);
    assert!(superseded.stored_events.is_empty());

    // A newer run claims the provider thread.
    replaced["status"] = json!("completed");
    sink.write(WriteInput::events(vec![
        run_event("run.updated", "event:owner:replacement-completed", replaced),
        provider_thread_event(
            "event:owner:newer-run",
            provider_thread_json(2, "active", json!([])),
        ),
    ]))
    .await
    .unwrap();
    let stale_ordinal = sink
        .write_if_provider_thread_owner(owner_write(
            "attempt:owner:replacement",
            1,
            provider_thread_event(
                "event:owner:stale-ordinal",
                provider_thread_json(1, "idle", json!([])),
            ),
        ))
        .await
        .unwrap();
    assert!(!stale_ordinal.committed);

    let stored: Value = db
        .read(|conn| {
            conn.query_row(
                "SELECT payload_json FROM orchestration_v2_projection_provider_threads WHERE provider_thread_id = 'provider-thread:owner'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|json| serde_json::from_str(&json).unwrap())
            .map_err(|err| PersistenceError::sql("test.read", &err))
        })
        .await
        .unwrap();
    assert_eq!(stored["lastRunOrdinal"], 2);
    assert_eq!(stored["status"], "active");
    assert_eq!(stored["pendingBackgroundTasks"], json!([]));
}

#[tokio::test]
async fn write_if_run_current_requires_the_expected_status_and_attempt() {
    let (_, sink, _) = setup().await;
    let mut run = run_json("run:current", "thread:current", "running");
    run["activeAttemptId"] = json!("attempt:current");
    sink.write(WriteInput::events(vec![
        thread_created("event:current:thread", "thread:current"),
        run_event("run.created", "event:current:run", run.clone()),
    ]))
    .await
    .unwrap();
    let mut completed = run.clone();
    completed["status"] = json!("completed");
    let attempt = |attempt: &str, status: OrchestrationV2RunStatus, id: &str| RunCurrentWrite {
        write: WriteInput::events(vec![run_event("run.updated", id, completed.clone())]),
        thread_id: ThreadId::new("thread:current").unwrap(),
        run_id: RunId::new("run:current").unwrap(),
        active_attempt_id: RunAttemptId::new(attempt).unwrap(),
        expected_status: status,
    };
    let wrong_status = sink
        .write_if_run_current(attempt(
            "attempt:current",
            OrchestrationV2RunStatus::Waiting,
            "event:current:wrong-status",
        ))
        .await
        .unwrap();
    assert!(!wrong_status.committed);
    let wrong_attempt = sink
        .write_if_run_current(attempt(
            "attempt:other",
            OrchestrationV2RunStatus::Running,
            "event:current:wrong-attempt",
        ))
        .await
        .unwrap();
    assert!(!wrong_attempt.committed);
    let current = sink
        .write_if_run_current(attempt(
            "attempt:current",
            OrchestrationV2RunStatus::Running,
            "event:current:ok",
        ))
        .await
        .unwrap();
    assert!(current.committed);
    // The run left `running`, so the same guard now refuses.
    let again = sink
        .write_if_run_current(attempt(
            "attempt:current",
            OrchestrationV2RunStatus::Running,
            "event:current:again",
        ))
        .await
        .unwrap();
    assert!(!again.committed);
}

#[tokio::test]
async fn write_if_provider_thread_unchanged_compares_the_whole_row() {
    let (_, sink, _) = setup().await;
    let base = provider_thread_json(1, "idle", json!([]));
    sink.write(WriteInput::events(vec![
        thread_created("event:unchanged:thread", "thread:owner"),
        provider_thread_event("event:unchanged:pt", base.clone()),
    ]))
    .await
    .unwrap();
    let expected: OrchestrationV2ProviderThread = serde_json::from_value(base.clone()).unwrap();
    let mut changed = base.clone();
    changed["status"] = json!("active");
    let first = sink
        .write_if_provider_thread_unchanged(
            expected.clone(),
            vec![provider_thread_event("event:unchanged:update", changed)],
        )
        .await
        .unwrap();
    assert!(first.committed);
    // A second writer holding the old row is refused.
    let stale = sink
        .write_if_provider_thread_unchanged(
            expected,
            vec![provider_thread_event("event:unchanged:stale", base)],
        )
        .await
        .unwrap();
    assert!(!stale.committed);
}

fn runtime_request_event(event_id: &str, status: &str, capability: Value) -> Event {
    serde_json::from_value(json!({
        "type": "runtime-request.updated", "id": event_id, "threadId": "thread:guard",
        "nodeId": "node:guard", "occurredAt": "2026-06-21T00:00:00.000Z",
        "payload": {
            "id": "request:guard", "nodeId": "node:guard", "providerTurnId": "turn:guard",
            "nativeRequestRef": null, "kind": "user_input", "status": status,
            "responseCapability": capability, "createdAt": "2026-06-21T00:00:00.000Z",
            "resolvedAt": null,
        },
    }))
    .unwrap()
}

#[tokio::test]
async fn guards_user_input_cancellations_against_answers_that_raced_them() {
    let (db, sink, _) = setup().await;
    let live = json!({"type": "live", "providerSessionId": "session:guard"});
    sink.write(WriteInput::events(vec![
        thread_created("event:guard:thread", "thread:guard"),
        runtime_request_event("event:guard:pending", "pending", live.clone()),
    ]))
    .await
    .unwrap();
    // The user answers between terminal normalization's read and its write.
    sink.write(WriteInput::events(vec![runtime_request_event(
        "event:guard:resolved",
        "resolved",
        live.clone(),
    )]))
    .await
    .unwrap();
    let stored = sink
        .write(WriteInput {
            guard_pending_user_input_cancellations: true,
            ..WriteInput::events(vec![runtime_request_event(
                "event:guard:cancel",
                "cancelled",
                json!({"type": "not_resumable", "reason": "The thread was deleted."}),
            )])
        })
        .await
        .unwrap();
    assert!(stored.is_empty(), "the stale cancellation must be dropped");
    let status: String = db
        .read(|conn| {
            conn.query_row(
                "SELECT status FROM orchestration_v2_projection_runtime_requests WHERE runtime_request_id = 'request:guard'",
                [],
                |row| row.get(0),
            )
            .map_err(|err| PersistenceError::sql("test.status", &err))
        })
        .await
        .unwrap();
    assert_eq!(status, "resolved");
}

#[tokio::test]
async fn cancel_unsettled_effects_commit_with_the_command_and_wake_the_worker() {
    let (_, sink, _) = setup().await;
    let thread_id = ThreadId::new("thread:stop").unwrap();
    let start_command = CommandId::new("cmd:stop:start").unwrap();
    sink.commit_command(commit_input(
        "cmd:stop:start",
        "thread:stop",
        vec![PendingEffect {
            id: "effect:stop:start".into(),
            command_id: start_command,
            thread_id: thread_id.clone(),
            request: EffectRequest::ProviderTurnStart {
                run_id: RunId::new("run:stop").unwrap(),
            },
            available_at: None,
        }],
    ))
    .await
    .unwrap();
    let stop = CommandCommit {
        events: vec![metadata_updated("event:stop", "thread:stop", "Stopped")],
        cancel_unsettled_effects: Some(CancelUnsettled {
            effect_types: vec![EffectType::ProviderTurnStart],
            reason: "Run was interrupted.".into(),
        }),
        ..commit_input("cmd:stop", "thread:stop", vec![])
    };
    let result = sink.commit_command(stop).await.unwrap();
    assert_eq!(result.cancelled_effect_count, 1);
    let row = sink
        .outbox()
        .get("effect:stop:start".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, EffectStatus::Cancelled);
    assert_eq!(row.last_error.as_deref(), Some("Run was interrupted."));
}

#[test]
fn thread_json_helper_decodes() {
    assert_eq!(thread_json("thread:x")["id"], "thread:x");
}
