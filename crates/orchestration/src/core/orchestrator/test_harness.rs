//! Fixtures shared by the command-group tests (`thread_delete`, `thread_fork`,
//! `thread_merge_back`). Same in-memory wiring as `tests.rs`, but it also hands
//! back the projection store, so a test can seed run rows the way the SQL-seeded
//! projection tests do. Upstream reaches those rows through the Effect layer.

use std::sync::Arc;

use serde_json::{Value, json};
use turin_contracts::{
    ThreadId,
    orchestration_v2::{OrchestrationV2ServerCommand, OrchestrationV2ThreadProjection},
};
use turin_persistence::{Db, OrchestrationEventStore};

use super::Orchestrator;
use super::services::NoRestartRecovery;
use crate::pipeline::command_receipt_store::CommandReceiptStore;
use crate::pipeline::effect_outbox::EffectOutbox;
use crate::pipeline::event_sink::EventSink;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort, RecordFilter};
use crate::projection::store::ProjectionStore;
use crate::projection::test_support::{insert_run_row, run, seed};

pub struct Harness {
    pub orchestrator: Orchestrator,
    pub projection: Arc<ProjectionStore>,
}

pub async fn harness() -> Harness {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let projection = Arc::new(ProjectionStore::new(db.clone()));
    let sink = EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db.clone()),
        projection.clone(),
    );
    let orchestrator = Orchestrator::new(
        projection.clone(),
        sink,
        CommandReceiptStore::new(db),
        Arc::new(NoRestartRecovery),
    );
    Harness {
        orchestrator,
        projection,
    }
}

pub fn thread(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

/// A server command decoded from its wire JSON, the way the transport delivers it.
pub fn command(value: Value) -> OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

/// `thread.create` on the wire. `title` is the thread's title.
pub fn create(command: &str, thread_id: &str, title: &str) -> OrchestrationV2ServerCommand {
    self::command(json!({
        "type": "thread.create",
        "createdBy": "user",
        "creationSource": "web",
        "commandId": command,
        "threadId": thread_id,
        "projectId": "project:test",
        "title": title,
        "modelSelection": {"instanceId": "codex", "model": "gpt-test"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
    }))
}

/// `thread.fork` on the wire, from the source's run `run_id`.
pub fn fork(
    command: &str,
    source: &str,
    target: &str,
    run_id: &str,
) -> OrchestrationV2ServerCommand {
    self::command(json!({
        "type": "thread.fork",
        "createdBy": "user",
        "creationSource": "web",
        "commandId": command,
        "sourceThreadId": source,
        "targetThreadId": target,
        "sourcePoint": {"type": "run", "runId": run_id},
    }))
}

/// `thread.merge_back` on the wire, from the fork's run `run_id`.
pub fn merge_back(
    command: &str,
    source: &str,
    target: &str,
    run_id: &str,
) -> OrchestrationV2ServerCommand {
    self::command(json!({
        "type": "thread.merge_back",
        "createdBy": "user",
        "creationSource": "web",
        "commandId": command,
        "sourceThreadId": source,
        "targetThreadId": target,
        "sourcePoint": {"type": "run", "runId": run_id},
    }))
}

/// A thread-scoped command with no payload, e.g. `thread.delete`.
pub fn thread_only(kind: &str, command: &str, thread_id: &str) -> OrchestrationV2ServerCommand {
    self::command(json!({"type": kind, "commandId": command, "threadId": thread_id}))
}

/// Inserts a run row (status as given, optional checkpoint) for `thread_id`.
pub async fn seed_run(
    projection: &ProjectionStore,
    thread_id: &str,
    run_id: &str,
    ordinal: u64,
    status: &str,
    checkpoint: bool,
) {
    let mut row = run(thread_id, run_id, ordinal, status);
    if checkpoint {
        row["checkpointId"] = json!(format!("checkpoint:{run_id}"));
    }
    seed(projection, move |conn| insert_run_row(conn, &row)).await;
}

/// The projection of `thread_id` with the given records loaded.
pub async fn records(
    orchestrator: &Orchestrator,
    thread_id: &str,
    fields: &[crate::pipeline::orchestrator_port::RecordField],
) -> Result<OrchestrationV2ThreadProjection, OrchestratorError> {
    orchestrator
        .get_thread_records(&thread(thread_id), fields, &RecordFilter::default())
        .await
}

/// The text of a dispatch error's cause (the part after the generic
/// "Failed to dispatch ..." message).
pub fn cause_text(error: &OrchestratorError) -> String {
    match std::error::Error::source(error) {
        Some(source) => source.to_string(),
        None => error.to_string(),
    }
}
