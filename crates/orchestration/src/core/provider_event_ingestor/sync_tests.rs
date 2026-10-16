//! The ingestor over a real event log and projection: a native subagent's
//! thread follows the model its provider reports, and a finished provider
//! turn is reported to analytics once. Upstream covers the first in
//! `ProviderEventIngestor.test.ts` ("moves a native subagent's thread to the
//! model its provider reports later").

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use turin_contracts::{ProviderSessionId, ThreadId};
use turin_persistence::{Db, OrchestrationEventStore};

use super::*;
use crate::pipeline::effect_outbox::EffectOutbox;
use crate::projection::store::ProjectionStore;
use crate::projection::test_support::{T0, thread as thread_json};

struct Fixture {
    ingestor: ProviderEventIngestor,
    projection: Arc<ProjectionStore>,
    sink: Arc<EventSink>,
}

async fn fixture() -> Fixture {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let projection = Arc::new(ProjectionStore::new(db.clone()));
    let sink = Arc::new(EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db),
        projection.clone(),
    ));
    let ingestor = ProviderEventIngestor::new(
        sink.clone(),
        projection.clone(),
        Arc::new(ThreadCommandExecutor::new()),
    );
    Fixture {
        ingestor,
        projection,
        sink,
    }
}

fn input(event: Value) -> ProviderEventInput {
    ProviderEventInput {
        provider_session_id: ProviderSessionId::new("provider-session:1").expect("id"),
        provider_instance_id: serde_json::from_value(json!("codex")).expect("instance"),
        command_id: None,
        thread_id: ThreadId::new("thread:root").expect("id"),
        run_id: None,
        node_id: None,
        raw_event_id: None,
        event: serde_json::from_value(event).expect("event"),
        analytics_context: None,
    }
}

fn kinds(
    stored: &[turin_contracts::orchestration_v2::OrchestrationV2StoredEvent],
) -> Vec<(String, String)> {
    stored
        .iter()
        .map(|stored| {
            let value = serde_json::to_value(&stored.event).expect("event encodes");
            (
                value["type"].as_str().unwrap_or_default().to_owned(),
                value["threadId"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

async fn model_of(fixture: &Fixture, thread: &str) -> Value {
    let thread = fixture
        .projection
        .get_thread(&ThreadId::new(thread).expect("id"))
        .await
        .expect("thread reads");
    serde_json::to_value(&thread.model_selection).expect("selection encodes")
}

#[tokio::test]
async fn a_native_subagent_thread_moves_to_the_model_its_provider_reports_later() {
    let fixture = fixture().await;
    let root = crate::projection::test_support::event(
        "thread.created",
        "thread:root",
        T0,
        thread_json("thread:root"),
    );
    fixture
        .sink
        .write(WriteInput::events(vec![root]))
        .await
        .expect("root thread writes");

    // The subagent's thread starts on the parent's model and options.
    let mut child = thread_json("thread:child");
    child["title"] = json!("review design");
    child["modelSelection"] = json!({
        "instanceId": "codex", "model": "gpt-test",
        "options": [{"id": "reasoningEffort", "value": "xhigh"}],
    });
    child["lineage"] = json!({
        "parentThreadId": "thread:root", "relationshipToParent": "subagent",
        "rootThreadId": "thread:root",
    });
    fixture
        .ingestor
        .ingest(
            &input(json!({"type": "app_thread.created", "driver": "codex", "app_thread": child})),
            &IngestGuard::default(),
        )
        .await
        .expect("child thread ingests");

    let subagent = |extra: Value| {
        let mut subagent = json!({
            "id": "node:late-model-subagent", "threadId": "thread:root", "runId": null,
            "parentNodeId": "node:root", "origin": "provider_native", "createdBy": "agent",
            "driver": "codex", "providerInstanceId": "codex", "providerThreadId": null,
            "childThreadId": "thread:child", "nativeTaskRef": null,
            "prompt": "Review the design", "title": "review design", "model": "gpt-6.1-sol",
            "status": "running", "result": null, "startedAt": T0, "completedAt": null,
            "updatedAt": T0,
        });
        subagent
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().cloned().unwrap_or_default());
        input(json!({"type": "subagent.updated", "driver": "codex", "subagent": subagent}))
    };

    let first = fixture
        .ingestor
        .ingest(&subagent(json!({})), &IngestGuard::default())
        .await
        .expect("first update");
    assert_eq!(
        kinds(&first),
        vec![
            ("subagent.updated".to_owned(), "thread:root".to_owned()),
            (
                "thread.model-selection-updated".to_owned(),
                "thread:child".to_owned()
            ),
        ]
    );
    // The parent's options belong to the parent's model.
    assert_eq!(
        model_of(&fixture, "thread:child").await,
        json!({"instanceId": "codex", "model": "gpt-6.1-sol"})
    );

    let repeated = fixture
        .ingestor
        .ingest(&subagent(json!({})), &IngestGuard::default())
        .await
        .expect("repeated update");
    assert_eq!(
        kinds(&repeated)
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec!["subagent.updated"]
    );

    // A selection the provider reports, effort included, replaces the thread's.
    let reported_selection = json!({
        "instanceId": "codex", "model": "gpt-6.1-sol",
        "options": [{"id": "reasoningEffort", "value": "low"}],
    });
    let reported = fixture
        .ingestor
        .ingest(
            &subagent(json!({"modelSelection": reported_selection})),
            &IngestGuard::default(),
        )
        .await
        .expect("reported update");
    assert_eq!(
        kinds(&reported)
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec!["subagent.updated", "thread.model-selection-updated"]
    );
    assert_eq!(model_of(&fixture, "thread:child").await, reported_selection);

    // A subagent recovered after a restart reports its model but no selection;
    // the same model keeps the effort the thread holds.
    let recovered = fixture
        .ingestor
        .ingest(&subagent(json!({})), &IngestGuard::default())
        .await
        .expect("recovered update");
    assert_eq!(
        kinds(&recovered)
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec!["subagent.updated"]
    );
    assert_eq!(model_of(&fixture, "thread:child").await, reported_selection);
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Map<String, Value>>>);

impl ProviderTurnAnalytics for Recorder {
    fn record(&self, properties: Map<String, Value>) {
        self.0.lock().expect("lock").push(properties);
    }
}

#[tokio::test]
async fn a_finished_provider_turn_is_reported_to_analytics_once() {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let projection = Arc::new(ProjectionStore::new(db.clone()));
    let sink = Arc::new(EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db),
        projection.clone(),
    ));
    let recorder = Arc::new(Recorder::default());
    let ingestor = ProviderEventIngestor::new(
        sink.clone(),
        projection,
        Arc::new(ThreadCommandExecutor::new()),
    )
    .with_analytics(recorder.clone());
    sink.write(WriteInput::events(vec![
        crate::projection::test_support::event(
            "thread.created",
            "thread:root",
            T0,
            thread_json("thread:root"),
        ),
    ]))
    .await
    .expect("root thread writes");

    let turn = |status: &str| {
        input(json!({
            "type": "provider_turn.updated", "driver": "codex",
            "provider_turn": {
                "id": "provider-turn:1", "providerThreadId": "provider-thread:1",
                "nodeId": "node:1", "runAttemptId": null, "nativeTurnRef": null,
                "ordinal": 1, "status": status, "startedAt": T0, "completedAt": null,
            },
        }))
    };
    ingestor
        .ingest(&turn("running"), &IngestGuard::default())
        .await
        .expect("running");
    assert!(
        recorder.0.lock().expect("lock").is_empty(),
        "a running turn is not reported"
    );
    ingestor
        .ingest(&turn("completed"), &IngestGuard::default())
        .await
        .expect("completed");
    ingestor
        .ingest(&turn("failed"), &IngestGuard::default())
        .await
        .expect("failed later");
    let recorded = recorder.0.lock().expect("lock");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["terminalStatus"], "completed");
    assert_eq!(recorded[0]["usageStatus"], "unavailable");
}

/// A user-input request with its owning node and timeline item, pending on
/// provider turn `provider-turn:q`.
fn question(
    id: &str,
    capability: Value,
) -> Vec<turin_contracts::orchestration_v2::OrchestrationV2DomainEvent> {
    use crate::projection::test_support::{event, turn_item};
    let node = json!({
        "id": format!("node:{id}"), "threadId": "thread:root", "runId": null,
        "parentNodeId": null, "rootNodeId": format!("node:{id}"), "kind": "user_input_request",
        "status": "waiting", "countsForRun": false, "providerThreadId": null,
        "providerTurnId": null, "nativeItemRef": null, "runtimeRequestId": null,
        "checkpointScopeId": null, "startedAt": T0, "completedAt": null,
    });
    let request = json!({
        "id": id, "threadId": "thread:root", "nodeId": format!("node:{id}"),
        "providerTurnId": "provider-turn:q", "nativeRequestRef": null, "kind": "user_input",
        "status": "pending", "responseCapability": capability, "createdAt": T0, "resolvedAt": null,
    });
    let mut item = turn_item(
        "thread:root",
        &format!("item:{id}"),
        None,
        1,
        "user_input_request",
        "waiting",
        json!({"requestId": id, "questions": [{
            "id": "q1", "header": "Name", "question": "Name?", "options": [], "required": true
        }]}),
    );
    item["providerTurnId"] = json!("provider-turn:q");
    item["nodeId"] = json!(format!("node:{id}"));
    vec![
        event("node.updated", "thread:root", T0, node),
        event("runtime-request.updated", "thread:root", T0, request),
        event("turn-item.updated", "thread:root", T0, item),
    ]
}

/// Upstream `ProviderEventIngestor.test.ts` ("cancels native user inputs when
/// the turn ends"): only requests the provider itself waits on are closed. A
/// message-capable (async) question outlives the turn.
#[tokio::test]
async fn a_finished_turn_cancels_native_questions_but_keeps_message_ones() {
    let fixture = fixture().await;
    let mut events = vec![crate::projection::test_support::event(
        "thread.created",
        "thread:root",
        T0,
        thread_json("thread:root"),
    )];
    events.extend(question(
        "request:live",
        json!({"type": "live", "providerSessionId": "provider-session:1"}),
    ));
    events.extend(question("request:message", json!({"type": "message"})));
    events.extend(question(
        "request:gone",
        json!({"type": "not_resumable", "reason": "The run ended."}),
    ));
    crate::projection::test_support::apply_all(&fixture.projection, events).await;

    fixture
        .ingestor
        .ingest(
            &input(json!({
                "type": "provider_turn.updated", "driver": "codex",
                "provider_turn": {
                    "id": "provider-turn:q", "providerThreadId": "provider-thread:1",
                    "nodeId": "node:request:live", "runAttemptId": null, "nativeTurnRef": null,
                    "ordinal": 1, "status": "completed", "startedAt": T0, "completedAt": T0,
                },
            })),
            &IngestGuard::default(),
        )
        .await
        .expect("the turn ends");

    let projection = fixture
        .projection
        .get_thread_projection(&ThreadId::new("thread:root").expect("id"))
        .await
        .expect("projection reads");
    let status = |id: &str| {
        projection
            .runtime_requests
            .iter()
            .find(|request| request.id.as_str() == id)
            .map(|request| format!("{:?}", request.status))
            .expect("request")
    };
    assert_eq!(status("request:live"), "Cancelled");
    assert_eq!(status("request:gone"), "Cancelled");
    assert_eq!(
        status("request:message"),
        "Pending",
        "an async question stays open"
    );
}
