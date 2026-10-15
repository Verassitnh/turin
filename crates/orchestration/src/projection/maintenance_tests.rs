//! Maintenance behaviour against real `orchestration_events` rows. Events are
//! appended through the persistence event store (the same write the sink
//! does), so verify, rebuild, and compaction see what production sees.

use serde_json::{Value, json};
use turin_contracts::{EventId, ThreadId, UtcDateTime};
use turin_persistence::{AgentEvent, AppendAgentEvents, OrchestrationEventStore};

use crate::projection::maintenance::ProjectionMaintenance;
use crate::projection::store::ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION;
use crate::projection::test_support::{at, file_store, thread};

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
