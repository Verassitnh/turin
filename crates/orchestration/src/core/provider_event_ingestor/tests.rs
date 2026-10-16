//! Normalization tests for [`ProviderEventIngestor`]. Upstream's
//! `ProviderEventIngestor.test.ts` drives the same mapping through the Effect
//! layer and a real projection; these tests use an in-memory [`IngestReads`]
//! and check the domain events `normalize` returns, which is the part the
//! ingestor owns. Writes are covered by the EventSink tests.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
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
}

#[async_trait]
impl IngestReads for FakeReads {
    async fn app_thread(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2AppThread>, BoxError> {
        Ok(self.thread.lock().expect("lock").clone())
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
        Ok(None)
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
    }
}

fn ingestor_reads(thread: Option<OrchestrationV2AppThread>) -> Arc<FakeReads> {
    Arc::new(FakeReads {
        thread: Mutex::new(thread),
    })
}

async fn normalize_with(
    reads: Arc<FakeReads>,
    input: &ProviderEventInput,
) -> Vec<OrchestrationV2DomainEvent> {
    normalize(reads.as_ref(), input).await.expect("normalize")
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
