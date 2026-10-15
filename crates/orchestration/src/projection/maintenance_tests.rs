//! Maintenance behaviour against real `orchestration_events` rows. Events are
//! appended through the persistence event store (the same write the sink
//! does), so verify, rebuild, and compaction see what production sees.

use rusqlite::params;
use serde_json::{Value, json};
use turin_contracts::{EventId, NodeId, RunId, ThreadId, UtcDateTime};
use turin_persistence::{AgentEvent, AppendAgentEvents, OrchestrationEventStore, PersistenceError};

use crate::projection::maintenance::ProjectionMaintenance;
use crate::projection::store::ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION;
use crate::projection::test_support::{at, file_store, message, run, thread, turn_item};

/// Append one v2 thread event through the persistence event store.
async fn append(
    events: &OrchestrationEventStore,
    id: &str,
    event_type: &str,
    thread_id: &str,
    payload: Value,
) {
    events
        .append_agent_events(AppendAgentEvents {
            command_id: None,
            events: vec![AgentEvent {
                id: EventId::new(id).expect("event id"),
                thread_id: ThreadId::new(thread_id).expect("thread id"),
                event_type: event_type.to_owned(),
                occurred_at: at(1).parse::<UtcDateTime>().expect("time"),
                payload,
                run_id: None,
                node_id: None,
                driver: None,
                provider_instance_id: None,
                raw_event_id: None,
            }],
        })
        .await
        .expect("append event");
}

async fn create_thread(events: &OrchestrationEventStore, id: &str) {
    append(
        events,
        &format!("event:created:{id}"),
        "thread.created",
        id,
        thread(id),
    )
    .await;
}

#[tokio::test]
async fn verify_reports_an_empty_store_as_invalid_until_rebuilt() {
    let (store, _dir) = file_store().await;
    let maintenance = ProjectionMaintenance::new(store);

    let verification = maintenance.verify().await.expect("verify");

    assert!(!verification.valid);
    // Migrations seed a metadata row at an older schema; it is not the current one.
    assert_ne!(
        verification.schema_version,
        ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION as i64
    );
    assert_eq!(verification.expected_sequence, 0);
    assert!(verification.missing_thread_ids.is_empty());
}

#[tokio::test]
async fn rebuild_replays_the_event_log_and_restores_missing_threads() {
    let (store, _dir) = file_store().await;
    let events = OrchestrationEventStore::new(store.db().clone());
    create_thread(&events, "thread:rebuild-a").await;
    create_thread(&events, "thread:rebuild-b").await;
    let maintenance = ProjectionMaintenance::new(store.clone());

    let rebuilt = maintenance.rebuild().await.expect("rebuild");
    assert!(rebuilt.valid, "{rebuilt:?}");
    assert_eq!(rebuilt.expected_sequence, 2);
    assert_eq!(rebuilt.projection_sequence, 2);

    // Drop one projection row behind the store's back: verify must notice it.
    store
        .db()
        .write(|tx| {
            tx.execute(
                "DELETE FROM orchestration_v2_projection_threads WHERE thread_id = ?1",
                ["thread:rebuild-b"],
            )
            .map_err(|error| {
                turin_persistence::PersistenceError::sql("test.delete-thread", &error)
            })?;
            Ok(())
        })
        .await
        .expect("delete projection row");
    let damaged = maintenance.verify().await.expect("verify");
    assert!(!damaged.valid);
    assert_eq!(
        damaged.missing_thread_ids,
        vec![ThreadId::new("thread:rebuild-b").unwrap()]
    );

    let restored = maintenance.rebuild().await.expect("rebuild again");
    assert!(restored.valid, "{restored:?}");
    assert!(restored.missing_thread_ids.is_empty());
}

#[tokio::test]
async fn verify_flags_a_thread_whose_projection_payload_no_longer_decodes() {
    let (store, _dir) = file_store().await;
    let events = OrchestrationEventStore::new(store.db().clone());
    create_thread(&events, "thread:corrupt").await;
    let maintenance = ProjectionMaintenance::new(store.clone());
    maintenance.rebuild().await.expect("rebuild");

    store
        .db()
        .write(|tx| {
            tx.execute(
                "UPDATE orchestration_v2_projection_threads SET payload_json = '{}' WHERE thread_id = ?1",
                ["thread:corrupt"],
            )
            .map_err(|error| {
                turin_persistence::PersistenceError::sql("test.corrupt-thread", &error)
            })?;
            Ok(())
        })
        .await
        .expect("corrupt projection row");

    let verification = maintenance.verify().await.expect("verify");

    assert!(!verification.valid);
    assert_eq!(
        verification.unreadable_thread_ids,
        vec![ThreadId::new("thread:corrupt").unwrap()]
    );
}

#[tokio::test]
async fn compaction_keeps_only_the_newest_thread_state_and_entity_rows() {
    let (store, _dir) = file_store().await;
    let events = OrchestrationEventStore::new(store.db().clone());
    create_thread(&events, "thread:compact").await;
    // Two thread-state events for one thread: only the newer one is replayable.
    append(
        &events,
        "event:archived-old",
        "thread.archived",
        "thread:compact",
        json!({"threadId": "thread:compact", "archivedAt": at(2)}),
    )
    .await;
    append(
        &events,
        "event:archived-new",
        "thread.archived",
        "thread:compact",
        json!({"threadId": "thread:compact", "archivedAt": at(3)}),
    )
    .await;
    // Two writes of one message entity: the older is superseded.
    for (id, text) in [("event:message-old", "old"), ("event:message-new", "new")] {
        append(
            &events,
            id,
            "message.updated",
            "thread:compact",
            json!({"id": "message:compact", "text": text}),
        )
        .await;
    }
    let maintenance = ProjectionMaintenance::new(store);

    let report = maintenance.compact_event_store().await.expect("compact");

    assert_eq!(report.deleted_event_count, 2);
    assert_eq!(report.deleted_receipt_count, 0);
    assert!(report.reclaimable_bytes >= 0);
    let remaining = store_event_ids(&events).await;
    assert!(remaining.contains(&"event:created:thread:compact".to_owned()));
    assert!(remaining.contains(&"event:archived-new".to_owned()));
    assert!(!remaining.contains(&"event:archived-old".to_owned()));
    assert!(remaining.contains(&"event:message-new".to_owned()));
    assert!(!remaining.contains(&"event:message-old".to_owned()));
}

async fn store_event_ids(events: &OrchestrationEventStore) -> Vec<String> {
    use turin_persistence::ReadAgentEvents;
    events
        .read_agent_events(ReadAgentEvents {
            after_sequence: None,
            through_sequence: None,
            thread_id: None,
            command_id: None,
            event_type: None,
            limit: None,
            skip_unknown_event_types: false,
        })
        .await
        .expect("read events")
        .into_iter()
        .map(|stored| stored.event.id.as_str().to_owned())
        .collect()
}

/// One v2 thread event with optional run and node metadata, for batch appends.
fn agent(
    id: &str,
    event_type: &str,
    thread_id: &str,
    payload: Value,
    run_id: Option<&str>,
    node_id: Option<&str>,
) -> AgentEvent {
    AgentEvent {
        id: EventId::new(id).expect("event id"),
        thread_id: ThreadId::new(thread_id).expect("thread id"),
        event_type: event_type.to_owned(),
        occurred_at: at(1).parse::<UtcDateTime>().expect("time"),
        payload,
        run_id: run_id.map(|id| RunId::new(id).expect("run id")),
        node_id: node_id.map(|id| NodeId::new(id).expect("node id")),
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
    }
}

async fn append_batch(events: &OrchestrationEventStore, batch: Vec<AgentEvent>) {
    events
        .append_agent_events(AppendAgentEvents {
            command_id: None,
            events: batch,
        })
        .await
        .expect("append events");
}

/// Run one write statement against the event database (legacy rows the v2 store
/// never writes).
async fn exec(
    store: &crate::projection::store::ProjectionStore,
    sql: &'static str,
    values: Vec<String>,
) {
    store
        .db()
        .write(move |tx| {
            tx.execute(sql, rusqlite::params_from_iter(values.iter()))
                .map_err(|error| PersistenceError::sql("maintenance_tests.exec", &error))?;
            Ok(())
        })
        .await
        .expect("write fixture row");
}

fn execution_node(thread_id: &str, id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "threadId": thread_id,
        "runId": null,
        "parentNodeId": null,
        "rootNodeId": id,
        "kind": "root_turn",
        "status": status,
        "countsForRun": false,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "runtimeRequestId": null,
        "checkpointScopeId": null,
        "startedAt": at(1),
        "completedAt": null
    })
}

#[tokio::test]
async fn rebuild_replays_history_one_bounded_page_at_a_time_and_rolls_back_a_failure() {
    // 1,005 events cross two replay pages of 500.
    let (store, _dir) = file_store().await;
    let events = OrchestrationEventStore::new(store.db().clone());
    let thread_id = "thread:paged-rebuild";
    let mut batch = vec![agent(
        "event:paged-rebuild:0",
        "thread.created",
        thread_id,
        thread(thread_id),
        None,
        None,
    )];
    for index in 1..=1004 {
        let mut payload = thread(thread_id);
        payload["title"] = json!(format!("Rebuilt update {index}"));
        batch.push(agent(
            &format!("event:paged-rebuild:{index}"),
            "thread.metadata-updated",
            thread_id,
            payload,
            None,
            None,
        ));
    }
    append_batch(&events, batch).await;
    let maintenance = ProjectionMaintenance::new(store.clone());
    let thread_key = tid(thread_id);

    let rebuilt = maintenance.rebuild().await.expect("rebuild");

    assert!(rebuilt.valid, "{rebuilt:?}");
    assert_eq!(rebuilt.expected_sequence, 1005);
    assert_eq!(
        store
            .get_thread_projection(&thread_key)
            .await
            .expect("projection")
            .thread
            .title
            .as_str(),
        "Rebuilt update 1004"
    );
    let before = store
        .get_thread_projection(&thread_key)
        .await
        .expect("projection before failure");

    // Fail as the second page is requested. The write transaction rolls back.
    let failed = maintenance.rebuild_failing_after(500).await;
    assert!(failed.is_err());
    let after = store
        .get_thread_projection(&thread_key)
        .await
        .expect("projection after failure");
    assert_eq!(after, before);
    assert!(maintenance.verify().await.expect("verify").valid);
}

#[tokio::test]
async fn rebuild_keeps_run_and_node_rows_when_events_carry_run_metadata() {
    let (store, _dir) = file_store().await;
    let events = OrchestrationEventStore::new(store.db().clone());
    let thread_id = "thread:run-metadata";
    let run_id = "run:run-metadata";
    let node_id = "node:run-metadata";
    append_batch(
        &events,
        vec![
            agent(
                "event:run-metadata:created",
                "thread.created",
                thread_id,
                thread(thread_id),
                None,
                None,
            ),
            agent(
                "event:run-metadata:run",
                "run.created",
                thread_id,
                run(thread_id, run_id, 1, "running"),
                Some(run_id),
                Some(node_id),
            ),
            agent(
                "event:run-metadata:node",
                "node.updated",
                thread_id,
                execution_node(thread_id, node_id, "running"),
                Some(run_id),
                Some(node_id),
            ),
        ],
    )
    .await;

    let rebuilt = ProjectionMaintenance::new(store.clone())
        .rebuild()
        .await
        .expect("rebuild");
    let projection = store
        .get_thread_projection(&tid(thread_id))
        .await
        .expect("projection");

    assert!(rebuilt.valid, "{rebuilt:?}");
    assert_eq!(
        projection
            .runs
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>(),
        vec![run_id]
    );
    assert_eq!(
        projection
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        vec![node_id]
    );
}

fn tid(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

#[tokio::test]
async fn compaction_removes_superseded_imported_and_legacy_rows_and_keeps_the_projection() {
    let (store, _dir) = file_store().await;
    let events = OrchestrationEventStore::new(store.db().clone());
    let name = "thread:foundation-compact";
    let message_id = "message:foundation-compact";
    let node_id = "node:foundation-compact";
    let item_id = "turn-item:foundation-compact";
    let event_id = |suffix: &str| format!("event:foundation-compact:{suffix}");
    let visited = |thread_id: &str, suffix: &str| {
        let mut payload = thread(thread_id);
        payload["lastVisitedAt"] = json!(at(2));
        agent(
            &event_id(suffix),
            "thread.visited",
            thread_id,
            payload,
            None,
            None,
        )
    };

    let mut batch = vec![agent(
        &event_id("create"),
        "thread.created",
        name,
        thread(name),
        None,
        None,
    )];
    let mut metadata = thread(name);
    metadata["lastVisitedAt"] = json!(at(2));
    batch.push(agent(
        &event_id("meta"),
        "thread.metadata-updated",
        name,
        metadata,
        None,
        None,
    ));
    batch.push(visited(name, "visit-1"));
    batch.push(agent(
        &event_id("message-1"),
        "message.updated",
        name,
        message(name, message_id, None, "user", "streaming", &at(1)),
        None,
        None,
    ));
    batch.push(agent(
        &event_id("node-1"),
        "node.updated",
        name,
        execution_node(name, node_id, "running"),
        None,
        Some(node_id),
    ));
    batch.push(agent(
        &event_id("item-1"),
        "turn-item.updated",
        name,
        turn_item(
            name,
            item_id,
            None,
            1,
            "assistant_message",
            "completed",
            json!({ "messageId": message_id, "text": "streaming", "streaming": false }),
        ),
        None,
        None,
    ));
    // 501 superseded visits: the discovery must page past the 500-row window.
    for index in 0..501 {
        batch.push(visited(name, &format!("history-{index}")));
    }
    batch.push(visited(name, "visit-2"));
    batch.push(agent(
        &event_id("message-2"),
        "message.updated",
        name,
        message(name, message_id, None, "user", "final", &at(1)),
        None,
        None,
    ));
    batch.push(agent(
        &event_id("node-2"),
        "node.updated",
        name,
        execution_node(name, node_id, "completed"),
        None,
        Some(node_id),
    ));
    batch.push(agent(
        &event_id("item-2"),
        "turn-item.updated",
        name,
        turn_item(
            name,
            item_id,
            None,
            1,
            "assistant_message",
            "completed",
            json!({ "messageId": message_id, "text": "final", "streaming": false }),
        ),
        None,
        None,
    ));
    append_batch(&events, batch).await;

    let maintenance = ProjectionMaintenance::new(store.clone());
    assert!(maintenance.rebuild().await.expect("rebuild").valid);
    let before = store
        .get_thread_projection(&tid(name))
        .await
        .expect("projection before compaction");

    // A fully imported legacy thread: its v1 events and pre-migration receipts are
    // dead weight. A still-pending import keeps its rows.
    let imported = "thread:foundation-compact-v1-imported";
    let pending = "thread:foundation-compact-v1-pending";
    let now = at(1);
    for (stream, version) in [(imported, 1), (imported, 2), (pending, 1)] {
        exec(
            &store,
            "INSERT INTO orchestration_events (
               event_id, aggregate_kind, stream_id, stream_version, event_type,
               occurred_at, actor_kind, payload_json, metadata_json, application_event_version
             ) VALUES (?1, 'thread', ?2, ?3, 'thread.message-appended', ?4, 'user', '{}', '{}', 1)",
            vec![
                format!("event:v1:{stream}:{version}"),
                stream.to_owned(),
                version.to_string(),
                now.clone(),
            ],
        )
        .await;
    }
    exec(
        &store,
        "INSERT INTO orchestration_v2_legacy_imports (
           thread_id, source_updated_at, shell_imported_at, transcript_imported_at,
           imported_message_count, last_error
         ) VALUES (?1, ?2, ?2, ?2, 2, NULL), (?3, ?2, ?2, NULL, 0, NULL)",
        vec![imported.to_owned(), now.clone(), pending.to_owned()],
    )
    .await;
    exec(
        &store,
        "INSERT INTO orchestration_command_receipts (
           command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, command_type
         ) VALUES
           ('command:foundation-compact:legacy', 'thread', ?1, ?2, 1, 'accepted', 'legacy'),
           ('command:foundation-compact:pending', 'thread', ?3, ?2, 1, 'accepted', 'legacy')",
        vec![imported.to_owned(), now, pending.to_owned()],
    )
    .await;

    let report = maintenance.compact_event_store().await.expect("compact");

    // 503 superseded thread-state events, the superseded message and node, and
    // the two imported v1 events. Both turn-item updates stay.
    assert_eq!(report.deleted_event_count, 507);
    assert_eq!(report.deleted_receipt_count, 1);

    let remaining = store
        .db()
        .read(move |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT event_id FROM orchestration_events
                     WHERE aggregate_kind = 'thread' AND stream_id = ?1
                     ORDER BY sequence ASC",
                )
                .map_err(|error| PersistenceError::sql("maintenance_tests.remaining", &error))?;
            statement
                .query_map(params![name], |row| row.get::<_, String>(0))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                .map_err(|error| PersistenceError::sql("maintenance_tests.remaining", &error))
        })
        .await
        .expect("remaining events");
    assert_eq!(
        remaining,
        vec![
            event_id("create"),
            event_id("item-1"),
            event_id("visit-2"),
            event_id("message-2"),
            event_id("node-2"),
            event_id("item-2"),
        ]
    );

    let remaining_v1 = store
        .db()
        .read(move |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT stream_id FROM orchestration_events
                     WHERE application_event_version = 1 AND stream_id IN (?1, ?2)",
                )
                .map_err(|error| PersistenceError::sql("maintenance_tests.v1", &error))?;
            statement
                .query_map(params![imported, pending], |row| row.get::<_, String>(0))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                .map_err(|error| PersistenceError::sql("maintenance_tests.v1", &error))
        })
        .await
        .expect("remaining v1");
    assert_eq!(remaining_v1, vec![pending.to_owned()]);

    let remaining_receipts = store
        .db()
        .read(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT command_id FROM orchestration_command_receipts
                     WHERE command_id IN ('command:foundation-compact:legacy', 'command:foundation-compact:pending')",
                )
                .map_err(|error| PersistenceError::sql("maintenance_tests.receipts", &error))?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                .map_err(|error| PersistenceError::sql("maintenance_tests.receipts", &error))
        })
        .await
        .expect("remaining receipts");
    assert_eq!(
        remaining_receipts,
        vec!["command:foundation-compact:pending".to_owned()]
    );

    // Replay across the deletion gaps must still produce the same projection.
    assert!(maintenance.verify().await.expect("verify").valid);
    assert!(maintenance.rebuild().await.expect("rebuild").valid);
    assert_eq!(
        store
            .get_thread_projection(&tid(name))
            .await
            .expect("projection after compaction"),
        before
    );
}
