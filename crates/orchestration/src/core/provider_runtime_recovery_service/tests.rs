//! Upstream `ProviderRuntimeRecoveryService.test.ts` and
//! `ProviderRuntimeRecoveryService.regression.test.ts`. Upstream mocks the
//! projection store, event sink and outbox with partial objects. Here they are
//! the real ones, in memory: each case seeds projection rows, runs recovery, and
//! reads back the events the command committed. Because the store picks the
//! threads, a fixture must satisfy the store's recovery query (a nonterminal
//! run, a pending request, an open background item, ...), which upstream's
//! mocked `getRecoveryThreadIds` did not require.
//!
//! Not ported: the `EffectWorker.runRecoveryOnce` call counts (this service
//! does not depend on the worker), the 1,000-thread projection-read spy (the
//! store is real, so the case asserts only the candidate thread was touched),
//! and `ProviderRuntimeRecoveryPerformance.test.ts` (timing).

use std::sync::Arc;

use serde_json::{Value, json};
use turin_contracts::{
    CheckpointScopeId, CommandId, ProviderSessionId, RunId, RuntimeRequestId, ThreadId,
    orchestration_v2::OrchestrationV2DomainEvent,
    settings::{ServerSettings, default_server_settings},
};
use turin_persistence::{Db, OrchestrationEventStore, ReadAgentEvents};

use super::*;
use crate::{
    pipeline::{
        domain_event::stored_event_from_agent,
        effect_outbox::{EffectOutbox, EffectRequest, PendingEffect},
        event_sink::EventSink,
        test_support::StaticSettings,
    },
    projection::test_support::{T0, capabilities, event, message, run, thread, turn_item},
};

const CLAUDE: &str = "claude";
const CODEX: &str = "codex";

struct Fixture {
    store: Arc<ProjectionStore>,
    sink: Arc<EventSink>,
    service: ProviderRuntimeRecoveryService,
}

async fn fixture_with(settings: ServerSettings) -> Fixture {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let store = Arc::new(ProjectionStore::new(db.clone()));
    let sink = Arc::new(EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db),
        store.clone(),
    ));
    let service = ProviderRuntimeRecoveryService::new(
        Arc::new(StaticSettings(settings)),
        store.clone(),
        sink.clone(),
    );
    Fixture {
        store,
        sink,
        service,
    }
}

async fn fixture() -> Fixture {
    fixture_with(default_server_settings()).await
}

fn tid(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

impl Fixture {
    /// Seeds one projection row through its event.
    async fn put(&self, kind: &str, thread_id: &str, payload: Value) {
        self.store
            .apply(event(kind, thread_id, T0, payload))
            .await
            .expect("seed");
    }

    async fn thread(&self, id: &str) {
        self.put("thread.created", id, thread(id)).await;
    }

    async fn thread_on(&self, id: &str, instance: &str) {
        let mut payload = thread(id);
        payload["providerInstanceId"] = json!(instance);
        self.put("thread.created", id, payload).await;
    }

    /// Every event recovery committed for the thread, in order.
    async fn committed(&self, thread_id: &str) -> Vec<OrchestrationV2DomainEvent> {
        let rows = self
            .sink
            .events()
            .read_agent_events(ReadAgentEvents {
                after_sequence: None,
                through_sequence: None,
                thread_id: Some(tid(thread_id)),
                command_id: None,
                event_type: None,
                limit: None,
                skip_unknown_event_types: false,
            })
            .await
            .expect("read events");
        rows.iter()
            .map(|row| stored_event_from_agent(row).expect("decode").event)
            .collect()
    }

    async fn reconcile(&self, trigger: ReconcileTrigger) -> RecoverySummary {
        self.service.reconcile(trigger).await.expect("reconcile")
    }
}

// ---------------------------------------------------------------------------
// Fixtures for the rows upstream writes as partial object literals
// ---------------------------------------------------------------------------

fn patch(mut base: Value, fields: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(fields.as_object().cloned().unwrap());
    base
}

fn run_json(thread_id: &str, id: &str, ordinal: u64, status: &str, instance: &str) -> Value {
    patch(
        run(thread_id, id, ordinal, status),
        json!({"providerInstanceId": instance, "modelSelection": {"instanceId": instance, "model": "m"}}),
    )
}

fn node_json(
    thread_id: &str,
    id: &str,
    run_id: Option<&str>,
    kind: &str,
    status: &str,
    root: Option<&str>,
) -> Value {
    json!({
        "id": id, "threadId": thread_id, "runId": run_id, "parentNodeId": null,
        "rootNodeId": root.unwrap_or(id), "kind": kind, "status": status, "countsForRun": false,
        "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
        "runtimeRequestId": null, "checkpointScopeId": null, "startedAt": T0, "completedAt": null
    })
}

fn attempt_json(id: &str, run_id: &str, root: &str, status: &str) -> Value {
    json!({
        "id": id, "runId": run_id, "attemptOrdinal": 1, "rootNodeId": root,
        "providerInstanceId": CODEX, "providerThreadId": "provider-thread:any",
        "providerTurnId": null, "reason": "initial", "status": status,
        "startedAt": T0, "completedAt": null
    })
}

fn provider_thread_json(
    id: &str,
    thread_id: &str,
    driver: &str,
    instance: &str,
    status: &str,
    tasks: Value,
) -> Value {
    json!({
        "id": id, "driver": driver, "providerInstanceId": instance, "providerSessionId": null,
        "appThreadId": thread_id, "ownerNodeId": null, "nativeThreadRef": null,
        "nativeConversationHeadRef": null, "status": status, "firstRunOrdinal": null,
        "lastRunOrdinal": null, "handoffIds": [], "forkedFrom": null,
        "pendingBackgroundTasks": tasks, "createdAt": T0, "updatedAt": T0
    })
}

fn session_json(id: &str, driver: &str, instance: &str, status: &str) -> Value {
    json!({
        "id": id, "driver": driver, "providerInstanceId": instance, "status": status,
        "cwd": "/repo", "model": null, "capabilities": capabilities(),
        "createdAt": T0, "updatedAt": T0, "lastError": null
    })
}

fn provider_turn_json(id: &str, provider_thread: &str, node: &str, attempt: &str) -> Value {
    json!({
        "id": id, "providerThreadId": provider_thread, "nodeId": node, "runAttemptId": attempt,
        "nativeTurnRef": null, "ordinal": 1, "status": "running", "startedAt": T0,
        "completedAt": null, "tokenUsage": null, "turnTokenUsage": null
    })
}

fn request_json(thread_id: &str, id: &str, node: &str, capability: Value) -> Value {
    json!({
        "id": id, "threadId": thread_id, "nodeId": node, "providerTurnId": null,
        "nativeRequestRef": null, "kind": "user_input", "status": "pending",
        "responseCapability": capability, "createdAt": T0, "resolvedAt": null
    })
}

#[allow(clippy::too_many_arguments)]
fn subagent_json(
    thread_id: &str,
    id: &str,
    run_id: Option<&str>,
    origin: &str,
    driver: &str,
    instance: &str,
    status: &str,
    child: Option<&str>,
) -> Value {
    json!({
        "id": id, "threadId": thread_id, "runId": run_id, "parentNodeId": "node:parent",
        "origin": origin, "createdBy": "agent", "driver": driver, "providerInstanceId": instance,
        "providerThreadId": null, "childThreadId": child, "nativeTaskRef": null,
        "prompt": "Do the work", "title": "Member", "model": null, "status": status,
        "result": null, "startedAt": T0, "completedAt": null, "updatedAt": T0
    })
}

#[allow(clippy::too_many_arguments)]
fn item(
    thread_id: &str,
    id: &str,
    run_id: Option<&str>,
    node: Option<&str>,
    provider_thread: Option<&str>,
    kind: &str,
    status: &str,
    extra: Value,
) -> Value {
    let mut value = turn_item(thread_id, id, run_id, 1, kind, status, extra);
    value["nodeId"] = json!(node);
    value["providerThreadId"] = json!(provider_thread);
    value
}

fn command_item(
    thread_id: &str,
    id: &str,
    run_id: Option<&str>,
    node: Option<&str>,
    provider_thread: Option<&str>,
) -> Value {
    item(
        thread_id,
        id,
        run_id,
        node,
        provider_thread,
        "command_execution",
        "running",
        json!({"input": "sleep 30", "output": null}),
    )
}

fn subagent_item(
    thread_id: &str,
    id: &str,
    run_id: &str,
    node: &str,
    provider_thread: &str,
    origin: &str,
    status: &str,
) -> Value {
    item(
        thread_id,
        id,
        Some(run_id),
        Some(node),
        Some(provider_thread),
        "subagent",
        status,
        json!({
            "subagentId": node, "origin": origin, "driver": CLAUDE, "providerInstanceId": CLAUDE,
            "childThreadId": if origin == "app_owned" { json!(format!("thread:delegated-task:{node}")) } else { Value::Null },
            "prompt": "Do the work", "progress": null, "result": null, "title": format!("subagent {node}")
        }),
    )
}

// ---------------------------------------------------------------------------
// Event readers
// ---------------------------------------------------------------------------

fn wire(event: &OrchestrationV2DomainEvent) -> Value {
    serde_json::to_value(event).expect("event encodes")
}

fn event_type(event: &OrchestrationV2DomainEvent) -> String {
    wire(event)["type"].as_str().unwrap().to_owned()
}

fn status_of(event: &OrchestrationV2DomainEvent) -> Option<String> {
    wire(event)["payload"]["status"].as_str().map(str::to_owned)
}

fn payload_id(event: &OrchestrationV2DomainEvent) -> String {
    wire(event)["payload"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn of_type<'a>(
    events: &'a [OrchestrationV2DomainEvent],
    kind: &str,
) -> Vec<&'a OrchestrationV2DomainEvent> {
    events
        .iter()
        .filter(|event| event_type(event) == kind)
        .collect()
}

fn cancelled_ids(events: &[OrchestrationV2DomainEvent], kind: &str) -> Vec<String> {
    let mut ids: Vec<String> = of_type(events, kind)
        .into_iter()
        .filter(|event| status_of(event).as_deref() == Some("cancelled"))
        .map(payload_id)
        .collect();
    ids.sort();
    ids
}

fn work_notes(events: &[OrchestrationV2DomainEvent]) -> Vec<(String, Vec<String>)> {
    of_type(events, "run.background-work-cancelled")
        .into_iter()
        .map(|event| {
            let payload = &wire(event)["payload"];
            (
                payload["runId"].as_str().unwrap().to_owned(),
                payload["restartCancelledBackgroundWork"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|work| work["kind"].as_str().unwrap().to_owned())
                    .collect(),
            )
        })
        .collect()
}

async fn enqueue(fixture: &Fixture, effects: Vec<PendingEffect>) {
    fixture
        .sink
        .outbox()
        .enqueue(effects)
        .await
        .expect("enqueue");
}

fn respond_effect(thread_id: &str, id: &str) -> PendingEffect {
    PendingEffect {
        id: id.to_owned(),
        command_id: CommandId::new(format!("command:{id}")).unwrap(),
        thread_id: tid(thread_id),
        request: EffectRequest::RuntimeRequestRespond {
            provider_session_id: ProviderSessionId::new("session:any").unwrap(),
            request_id: RuntimeRequestId::new("request:any").unwrap(),
            decision: None,
            answers: None,
        },
        available_at: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn leaves_durable_effects_for_the_worker_after_runtime_reconciliation() {
    let fixture = fixture().await;
    let summary = fixture.service.recover().await.expect("recover");
    assert_eq!(summary, RecoverySummary::default());
}

#[tokio::test]
async fn reads_recovery_projections_only_for_threads_that_need_runtime_recovery() {
    let fixture = fixture().await;
    for index in 0..50 {
        let id = format!("thread_recovery_settled_{index}");
        fixture.thread(&id).await;
        fixture
            .put(
                "run.created",
                &id,
                run_json(&id, &format!("run:{id}"), 1, "completed", CODEX),
            )
            .await;
    }
    let candidate = "thread_recovery_candidate";
    fixture.thread(candidate).await;
    fixture
        .put(
            "run.created",
            candidate,
            run_json(candidate, "run:candidate", 1, "running", CODEX),
        )
        .await;

    let summary = fixture.service.recover().await.expect("recover");

    assert_eq!(summary.terminalized_runs, 1);
    for index in 0..50 {
        let id = format!("thread_recovery_settled_{index}");
        assert!(fixture.committed(&id).await.is_empty(), "{id} untouched");
    }
    assert!(!fixture.committed(candidate).await.is_empty());
}

#[tokio::test]
async fn expires_orphaned_runtime_requests_before_command_readiness() {
    let fixture = fixture().await;
    let id = "thread_recovery_requests";
    fixture.thread(id).await;
    fixture
        .put(
            "runtime-request.updated",
            id,
            request_json(
                id,
                "request_orphaned",
                "node_orphaned",
                json!({"type": "not_resumable", "reason": "old process"}),
            ),
        )
        .await;

    fixture.service.recover().await.expect("recover");

    let events = fixture.committed(id).await;
    assert_eq!(event_type(&events[0]), "runtime-request.updated");
    let payload = &wire(&events[0])["payload"];
    assert_eq!(payload["status"], "expired");
    assert_eq!(payload["responseCapability"]["type"], "not_resumable");
}

#[tokio::test]
async fn preserves_async_questions_across_startup_and_shutdown() {
    let fixture = fixture().await;
    let id = "async-recovery-thread";
    fixture.thread(id).await;
    fixture
        .put(
            "runtime-request.updated",
            id,
            request_json(
                id,
                "async-recovery-request",
                "async-recovery-node",
                json!({"type": "message"}),
            ),
        )
        .await;

    assert_eq!(
        fixture
            .reconcile(ReconcileTrigger::Startup)
            .await
            .closed_requests,
        0
    );
    assert_eq!(
        fixture
            .reconcile(ReconcileTrigger::Startup)
            .await
            .closed_requests,
        0
    );
    assert_eq!(
        fixture
            .reconcile(ReconcileTrigger::Shutdown)
            .await
            .closed_requests,
        0
    );
    assert!(fixture.committed(id).await.is_empty());
}

#[tokio::test]
async fn uses_the_same_reconciliation_path_to_cancel_runtime_requests_during_shutdown() {
    let fixture = fixture().await;
    let id = "thread_shutdown_requests";
    fixture.thread(id).await;
    fixture
        .put(
            "runtime-request.updated",
            id,
            request_json(
                id,
                "request_shutdown",
                "node_shutdown",
                json!({"type": "live", "providerSessionId": "session:any"}),
            ),
        )
        .await;
    enqueue(&fixture, vec![respond_effect(id, "effect:respond")]).await;

    let summary = fixture.reconcile(ReconcileTrigger::Shutdown).await;

    assert_eq!(summary.closed_requests, 1);
    assert_eq!(summary.retired_effects, 1);
    let events = fixture.committed(id).await;
    let payload = &wire(&events[0])["payload"];
    assert_eq!(event_type(&events[0]), "runtime-request.updated");
    assert_eq!(payload["status"], "cancelled");
    assert_eq!(payload["responseCapability"]["type"], "not_resumable");
    assert!(
        payload["responseCapability"]["reason"]
            .as_str()
            .unwrap()
            .contains("shut down")
    );
}

#[tokio::test]
async fn preserves_a_replayable_waiting_run_while_cancelling_its_process_bound_background_work() {
    let fixture = fixture().await;
    let id = "thread_waiting_checkpoint";
    let run_id = "run_waiting_checkpoint";
    let provider_thread = "provider_thread_waiting_checkpoint";
    let item_id = "turn_item_waiting_checkpoint";
    fixture.thread(id).await;
    let mut waiting = run_json(id, run_id, 1, "waiting", CLAUDE);
    waiting["providerThreadId"] = json!(provider_thread);
    fixture.put("run.created", id, waiting).await;
    fixture
        .put(
            "provider-thread.updated",
            id,
            provider_thread_json(
                provider_thread,
                id,
                CLAUDE,
                CLAUDE,
                "idle",
                json!([{"taskId": item_id, "description": "Finish background task"}]),
            ),
        )
        .await;
    // The roster entry is the same task as the item: it carries the item's id
    // as its native id, so the roster adds nothing (upstream's fixture reaches
    // the same result through an `undefined` owner).
    let mut command = command_item(id, item_id, Some(run_id), None, Some(provider_thread));
    command["nativeItemRef"] = json!({"driver": CLAUDE, "nativeId": item_id, "strength": "strong"});
    fixture.put("turn-item.updated", id, command).await;
    // A checkpoint capture is still running for the run, and the process loss
    // finds it replayable.
    enqueue(
        &fixture,
        vec![PendingEffect {
            id: "effect:checkpoint".to_owned(),
            command_id: CommandId::new(format!("command:effect:checkpoint.capture:{run_id}"))
                .unwrap(),
            thread_id: tid(id),
            request: EffectRequest::CheckpointCapture {
                run_id: RunId::new(run_id).unwrap(),
                scope_id: CheckpointScopeId::new("scope:any").unwrap(),
            },
            available_at: None,
        }],
    )
    .await;
    fixture
        .sink
        .outbox()
        .claim_next("worker".to_owned(), 60_000, false)
        .await
        .expect("claim")
        .expect("claimed");

    let summary = fixture.reconcile(ReconcileTrigger::Startup).await;

    assert_eq!(summary.terminalized_runs, 0);
    assert_eq!(summary.requeued_effects, 1);
    let events = fixture.committed(id).await;
    // The run stays waiting for its checkpoint; it only records the work the
    // restart cancelled so the next provider turn can be told.
    assert!(of_type(&events, "run.updated").is_empty());
    assert_eq!(
        work_notes(&events),
        vec![(run_id.to_owned(), vec!["shell".to_owned()])]
    );
    assert_eq!(cancelled_ids(&events, "turn-item.updated"), vec![item_id]);
    let thread_event = of_type(&events, "provider-thread.updated");
    assert_eq!(payload_id(thread_event[0]), provider_thread);
    assert_eq!(
        wire(thread_event[0])["payload"]["pendingBackgroundTasks"],
        json!([])
    );
}

#[tokio::test]
async fn records_cancelled_work_on_the_provider_thread_that_lost_it_after_a_provider_switch() {
    let fixture = fixture().await;
    let id = "thread_switch_note";
    fixture.thread(id).await;
    // Claude launched a background subagent, then the thread switched to Codex.
    fixture
        .put(
            "provider-thread.updated",
            id,
            provider_thread_json(
                "provider_thread_switch_note_claude",
                id,
                "claudeAgent",
                CLAUDE,
                "idle",
                json!([{"taskId": "task-claude", "description": "Watch the build"}]),
            ),
        )
        .await;
    fixture
        .put(
            "provider-thread.updated",
            id,
            provider_thread_json(
                "provider_thread_switch_note_codex",
                id,
                CODEX,
                CODEX,
                "idle",
                json!([]),
            ),
        )
        .await;
    let mut claude_run = run_json(id, "run_switch_note_claude", 1, "completed", CLAUDE);
    claude_run["providerThreadId"] = json!("provider_thread_switch_note_claude");
    let mut codex_run = run_json(id, "run_switch_note_codex", 2, "completed", CODEX);
    codex_run["providerThreadId"] = json!("provider_thread_switch_note_codex");
    fixture.put("run.created", id, claude_run).await;
    fixture.put("run.created", id, codex_run).await;
    let mut subagent = item(
        id,
        "turn_item_switch_note_subagent",
        Some("run_switch_note_claude"),
        None,
        Some("provider_thread_switch_note_claude"),
        "subagent",
        "running",
        json!({
            "subagentId": "node:switch-note", "origin": "provider_native", "driver": CLAUDE,
            "providerInstanceId": CLAUDE, "childThreadId": null, "prompt": "Watch",
            "progress": null, "result": null, "title": "Background subagent"
        }),
    );
    subagent["title"] = json!("Background subagent");
    fixture.put("turn-item.updated", id, subagent).await;

    fixture.reconcile(ReconcileTrigger::Startup).await;

    // The Codex run is later, but only Claude's turns may be told about it.
    assert_eq!(
        work_notes(&fixture.committed(id).await),
        vec![(
            "run_switch_note_claude".to_owned(),
            vec!["subagent".to_owned(), "task".to_owned()]
        )]
    );
}

#[tokio::test]
async fn cancels_a_stale_waiting_run_when_no_checkpoint_capture_can_finish_it() {
    let fixture = fixture().await;
    let id = "thread_stale_waiting";
    fixture.thread(id).await;
    fixture
        .put(
            "run.created",
            id,
            run_json(id, "run_stale_waiting", 1, "waiting", CODEX),
        )
        .await;

    let summary = fixture.reconcile(ReconcileTrigger::Startup).await;

    assert_eq!(summary.terminalized_runs, 1);
    let events = fixture.committed(id).await;
    assert_eq!(
        status_of(of_type(&events, "run.updated")[0]).as_deref(),
        Some("cancelled")
    );
}

#[tokio::test]
async fn closes_a_secret_request_cards_form_when_its_run_is_recovered() {
    let fixture = fixture().await;
    let id = "thread_secret_recovery";
    fixture.thread(id).await;
    fixture
        .put(
            "run.created",
            id,
            run_json(id, "run_secret_recovery", 1, "running", CODEX),
        )
        .await;
    fixture
        .put(
            "turn-item.updated",
            id,
            item(
                id,
                "turn-item:secret-request:recovery",
                Some("run_secret_recovery"),
                None,
                None,
                "secret_request",
                "waiting",
                json!({"secretStatus": "pending", "label": "GitHub token", "reason": "Used as GH_TOKEN."}),
            ),
        )
        .await;

    fixture.reconcile(ReconcileTrigger::Startup).await;

    let events = fixture.committed(id).await;
    let card = &wire(of_type(&events, "turn-item.updated")[0])["payload"];
    assert_eq!(card["status"], "cancelled");
    assert_eq!(card["secretStatus"], "cancelled");
}

async fn recovers_queued_or_preparing_work(status: &str) {
    let fixture = fixture().await;
    let id = "thread_queued_restart";
    fixture.thread(id).await;
    let mut queued = run_json(id, "run_queued_restart", 1, status, CODEX);
    queued["queuePosition"] = json!(1);
    if status == "preparing" {
        queued["workspacePreparation"] =
            json!({"type": "worktree", "baseRef": "parent-local-commit", "startFromOrigin": false});
    }
    fixture.put("run.created", id, queued).await;
    fixture
        .put(
            "run-attempt.created",
            id,
            attempt_json(
                "attempt_queued_restart",
                "run_queued_restart",
                "node_queued_restart",
                "pending",
            ),
        )
        .await;
    fixture
        .put(
            "node.updated",
            id,
            node_json(
                id,
                "node_queued_restart",
                Some("run_queued_restart"),
                "root_turn",
                "pending",
                None,
            ),
        )
        .await;
    // The queued run's user message is an ordinary durable one, not a buffered wake.
    fixture
        .put(
            "message.updated",
            id,
            message(
                id,
                "message:run_queued_restart",
                Some("run_queued_restart"),
                "user",
                "hi",
                T0,
            ),
        )
        .await;

    let summary = fixture.reconcile(ReconcileTrigger::Startup).await;

    assert_eq!(
        summary.terminalized_runs,
        usize::from(status == "preparing")
    );
    let events = fixture.committed(id).await;
    let run_event = wire(of_type(&events, "run.updated")[0]);
    let attempts = of_type(&events, "run-attempt.updated");
    let nodes = of_type(&events, "node.updated");
    // No effect rides along: neither run is continued.
    let continuation = fixture
        .sink
        .outbox()
        .get("effect:restart-continuation:run_queued_restart".to_owned())
        .await
        .unwrap();
    assert!(continuation.is_none());
    if status == "preparing" {
        assert_eq!(run_event["payload"]["status"], "cancelled");
        assert_eq!(status_of(attempts[0]).as_deref(), Some("cancelled"));
        assert_eq!(status_of(nodes[0]).as_deref(), Some("cancelled"));
    } else {
        assert_eq!(run_event["payload"]["status"], "queued");
        assert_eq!(run_event["payload"]["queuePosition"], 1);
        assert_eq!(run_event["payload"]["queueHeld"], true);
        assert!(attempts.is_empty());
        assert!(nodes.is_empty());
    }
}

#[tokio::test]
async fn recovers_queued_work_after_restart() {
    recovers_queued_or_preparing_work("queued").await;
}

#[tokio::test]
async fn recovers_preparing_work_after_restart() {
    recovers_queued_or_preparing_work("preparing").await;
}

#[tokio::test]
async fn cancels_the_complete_in_flight_subtree_and_stops_its_persisted_session_without_reopening_it()
 {
    let fixture = fixture().await;
    let id = "thread_recovery_cancel";
    let run_id = "run_recovery_cancel";
    let provider_thread = "provider_thread_recovery_cancel";
    fixture.thread(id).await;
    let mut starting = run_json(id, run_id, 1, "starting", CODEX);
    starting["providerThreadId"] = json!(provider_thread);
    fixture.put("run.created", id, starting).await;
    fixture
        .put(
            "provider-session.updated",
            id,
            session_json("provider_session_recovery_cancel", CODEX, CODEX, "ready"),
        )
        .await;
    fixture
        .put(
            "provider-thread.updated",
            id,
            provider_thread_json(provider_thread, id, CODEX, CODEX, "active", json!([])),
        )
        .await;
    fixture
        .put(
            "run-attempt.created",
            id,
            attempt_json(
                "attempt_recovery_cancel",
                run_id,
                "node_recovery_cancel",
                "running",
            ),
        )
        .await;
    fixture
        .put(
            "node.updated",
            id,
            node_json(
                id,
                "node_recovery_cancel",
                Some(run_id),
                "root_turn",
                "running",
                None,
            ),
        )
        .await;
    fixture
        .put(
            "provider-turn.updated",
            id,
            provider_turn_json(
                "provider_turn_recovery_cancel",
                provider_thread,
                "node_recovery_cancel",
                "attempt_recovery_cancel",
            ),
        )
        .await;
    let mut streaming = message(
        id,
        "message_recovery_cancel",
        Some(run_id),
        "assistant",
        "par",
        T0,
    );
    streaming["streaming"] = json!(true);
    fixture.put("message.updated", id, streaming).await;
    fixture
        .put(
            "turn-item.updated",
            id,
            item(
                id,
                "turn_item_recovery_cancel",
                Some(run_id),
                Some("node_recovery_cancel"),
                None,
                "reasoning",
                "running",
                json!({"text": "thinking", "streaming": true}),
            ),
        )
        .await;
    enqueue(
        &fixture,
        vec![
            respond_effect(id, "effect:respond-1"),
            respond_effect(id, "effect:respond-2"),
        ],
    )
    .await;

    let summary = fixture.service.recover().await.expect("recover");

    assert_eq!(summary.terminalized_runs, 1);
    assert_eq!(summary.stopped_sessions, 1);
    assert_eq!(summary.retired_effects, 2);
    let events = fixture.committed(id).await;
    assert_eq!(
        events
            .iter()
            .map(|event| (event_type(event), status_of(event)))
            .collect::<Vec<_>>(),
        [
            ("run.updated", Some("cancelled")),
            ("run-attempt.updated", Some("cancelled")),
            ("node.updated", Some("cancelled")),
            ("provider-turn.updated", Some("cancelled")),
            ("message.updated", None),
            ("turn-item.updated", Some("cancelled")),
            ("provider-thread.updated", Some("idle")),
            ("provider-session.updated", Some("stopped")),
        ]
        .map(|(kind, status)| (kind.to_owned(), status.map(str::to_owned)))
    );
    assert_eq!(wire(&events[4])["payload"]["streaming"], false);
}

#[tokio::test]
async fn cancels_workflow_members_and_child_roots_without_run_ids_after_restart() {
    let fixture = fixture().await;
    let parent = "thread_recovery_workflow_members";
    let child = "thread_recovery_workflow_member_child";
    let member = "node_recovery_workflow_member";
    let child_root = "node_recovery_workflow_member_root";
    fixture.thread_on(parent, "claudeAgent").await;
    fixture.thread_on(child, "claudeAgent").await;
    fixture
        .put(
            "subagent.updated",
            parent,
            subagent_json(
                parent,
                member,
                None,
                "provider_native",
                "claudeAgent",
                "claudeAgent",
                "running",
                Some(child),
            ),
        )
        .await;
    fixture
        .put(
            "node.updated",
            parent,
            node_json(parent, member, None, "subagent", "running", None),
        )
        .await;
    fixture
        .put(
            "node.updated",
            child,
            node_json(child, child_root, None, "root_turn", "running", None),
        )
        .await;

    fixture.reconcile(ReconcileTrigger::Startup).await;

    let summary_of = |events: Vec<OrchestrationV2DomainEvent>| {
        events
            .iter()
            .map(|event| (event_type(event), status_of(event)))
            .collect::<Vec<_>>()
    };
    let expect = |rows: &[(&str, &str)]| {
        rows.iter()
            .map(|(kind, status)| ((*kind).to_owned(), Some((*status).to_owned())))
            .collect::<Vec<_>>()
    };
    // The store orders the two threads by id, so each thread is checked on its own.
    assert_eq!(
        summary_of(fixture.committed(parent).await),
        expect(&[
            ("subagent.updated", "cancelled"),
            ("node.updated", "cancelled")
        ])
    );
    assert_eq!(
        summary_of(fixture.committed(child).await),
        expect(&[("node.updated", "cancelled")])
    );
}

#[tokio::test]
async fn clears_persisted_pending_background_tasks_and_terminalizes_stale_background_items_on_settled_runs()
 {
    let fixture = fixture().await;
    let id = "thread_recovery_background";
    let settled_run = "run_recovery_background_settled";
    let active_run = "run_recovery_background_active";
    let idle_thread = "provider_thread_recovery_background_idle";
    let active_thread = "provider_thread_recovery_background_active";
    let secondary_thread = "provider_thread_recovery_background_secondary";
    let secondary = "claude-secondary";
    let subagent_instance = "claude-subagent";
    fixture.thread(id).await;
    fixture
        .put(
            "provider-session.updated",
            id,
            session_json(
                "provider_session_recovery_background",
                CLAUDE,
                CLAUDE,
                "ready",
            ),
        )
        .await;
    for (thread_id, instance, status, tasks) in [
        (
            idle_thread,
            CLAUDE,
            "idle",
            json!([{"taskId": "bg-settled", "description": "sleep 30"}]),
        ),
        (
            active_thread,
            CLAUDE,
            "active",
            json!([{"taskId": "bg-active", "description": "npm test"}]),
        ),
        (secondary_thread, secondary, "idle", json!([])),
    ] {
        fixture
            .put(
                "provider-thread.updated",
                id,
                provider_thread_json(thread_id, id, CLAUDE, instance, status, tasks),
            )
            .await;
    }
    fixture
        .put(
            "run.created",
            id,
            run_json(id, settled_run, 1, "completed", CLAUDE),
        )
        .await;
    fixture
        .put(
            "run.created",
            id,
            run_json(id, active_run, 2, "running", CLAUDE),
        )
        .await;
    fixture
        .put(
            "run-attempt.created",
            id,
            attempt_json(
                "attempt_recovery_background_active",
                active_run,
                "node_recovery_background_active",
                "running",
            ),
        )
        .await;
    fixture
        .put(
            "node.updated",
            id,
            node_json(
                id,
                "node_recovery_background_active",
                Some(active_run),
                "root_turn",
                "running",
                None,
            ),
        )
        .await;
    fixture
        .put(
            "turn-item.updated",
            id,
            command_item(
                id,
                "turn_item_recovery_background_stale",
                Some(settled_run),
                None,
                Some(idle_thread),
            ),
        )
        .await;
    fixture
        .put(
            "turn-item.updated",
            id,
            item(
                id,
                "turn_item_recovery_background_active",
                Some(active_run),
                Some("node_recovery_background_active"),
                Some(active_thread),
                "dynamic_tool",
                "running",
                json!({"toolName": "tool", "input": {}}),
            ),
        )
        .await;
    // Missing run: must attribute via providerThreadId, not the first provider thread.
    fixture
        .put(
            "turn-item.updated",
            id,
            command_item(
                id,
                "turn_item_recovery_background_null_run",
                None,
                None,
                Some(secondary_thread),
            ),
        )
        .await;
    // Missing run with a matching provider thread whose instance differs from
    // the subagent's own: the subagent's own providerInstanceId must win.
    let mut null_subagent = subagent_item(
        id,
        "turn_item_recovery_background_null_subagent",
        "-",
        "node:null-subagent",
        secondary_thread,
        "provider_native",
        "running",
    );
    null_subagent["runId"] = Value::Null;
    null_subagent["nodeId"] = Value::Null;
    null_subagent["providerInstanceId"] = json!(subagent_instance);
    fixture.put("turn-item.updated", id, null_subagent).await;

    let summary = fixture.reconcile(ReconcileTrigger::Startup).await;

    assert_eq!(summary.terminalized_runs, 1);
    let events = fixture.committed(id).await;
    let cancels = cancelled_ids(&events, "turn-item.updated");
    // Active-run item + settled-run stale + null-run command + null-run subagent.
    let mut expected = vec![
        "turn_item_recovery_background_active",
        "turn_item_recovery_background_null_run",
        "turn_item_recovery_background_null_subagent",
        "turn_item_recovery_background_stale",
    ];
    expected.sort();
    assert_eq!(cancels, expected);
    let instance_of = |item_id: &str| -> String {
        of_type(&events, "turn-item.updated")
            .into_iter()
            .find(|event| payload_id(event) == item_id)
            .map(|event| {
                wire(event)["providerInstanceId"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .unwrap()
    };
    assert_eq!(
        instance_of("turn_item_recovery_background_null_run"),
        secondary
    );
    assert_eq!(
        instance_of("turn_item_recovery_background_null_subagent"),
        subagent_instance
    );
    // A settled-run item still prefers the run's provider instance when present.
    assert_eq!(instance_of("turn_item_recovery_background_stale"), CLAUDE);

    // Only threads with active status or nonempty rosters are rewritten.
    let provider_threads = of_type(&events, "provider-thread.updated");
    assert_eq!(provider_threads.len(), 2);
    for event in &provider_threads {
        assert_eq!(wire(event)["payload"]["pendingBackgroundTasks"], json!([]));
        assert_eq!(status_of(event).as_deref(), Some("idle"));
    }
    let rewritten: Vec<String> = provider_threads
        .iter()
        .map(|event| payload_id(event))
        .collect();
    assert!(rewritten.contains(&idle_thread.to_owned()));
    assert!(rewritten.contains(&active_thread.to_owned()));
}

#[tokio::test]
async fn terminalizes_a_leftover_nonpersistent_dynamic_tool_on_a_settled_run_after_process_loss() {
    let fixture = fixture().await;
    let id = "thread_recovery_orphan_wait";
    let run_id = "run_recovery_orphan_wait_settled";
    let provider_thread = "provider_thread_recovery_orphan_wait";
    let wait_item =
        "turn-item:provider:codex:native-item:exec-4669f3bb-78c9-4af1-b44e-daa340d2c538";
    let monitor_item = "turn-item:provider:codex:native-item:exec-persistent-monitor";
    let wait_node = "node_recovery_orphan_wait";
    let monitor_node = "node_recovery_persistent_monitor";
    fixture.thread(id).await;
    fixture
        .put(
            "provider-thread.updated",
            id,
            provider_thread_json(provider_thread, id, CODEX, CODEX, "idle", json!([])),
        )
        .await;
    fixture
        .put(
            "run.created",
            id,
            run_json(id, run_id, 1, "completed", CODEX),
        )
        .await;
    for node in [wait_node, monitor_node] {
        fixture
            .put(
                "node.updated",
                id,
                node_json(id, node, Some(run_id), "tool_call", "running", None),
            )
            .await;
    }
    for (item_id, node, tool, input) in [
        (
            wait_item,
            wait_node,
            "t3-code.t3_thread_wait",
            json!({"threadId": "thread:delegated-task:x", "timeoutMs": 30000}),
        ),
        (
            monitor_item,
            monitor_node,
            "grok.monitor",
            json!({"persistent": true, "command": "tail -f"}),
        ),
    ] {
        fixture
            .put(
                "turn-item.updated",
                id,
                item(
                    id,
                    item_id,
                    Some(run_id),
                    Some(node),
                    Some(provider_thread),
                    "dynamic_tool",
                    "running",
                    json!({"toolName": tool, "input": input}),
                ),
            )
            .await;
    }

    let summary = fixture.reconcile(ReconcileTrigger::Startup).await;

    assert_eq!(summary.terminalized_runs, 0);
    let events = fixture.committed(id).await;
    // Process loss means the provider is gone, so even a persistent monitor
    // cannot still be alive. The leftover wait and the monitor both close.
    let mut items = vec![wait_item, monitor_item];
    items.sort();
    assert_eq!(cancelled_ids(&events, "turn-item.updated"), items);
    let mut nodes = vec![wait_node, monitor_node];
    nodes.sort();
    assert_eq!(cancelled_ids(&events, "node.updated"), nodes);
}

#[tokio::test]
async fn terminalizes_the_linked_subagent_and_node_for_a_stale_subagent_item_on_a_settled_run() {
    let fixture = fixture().await;
    let id = "thread_recovery_subagent";
    let run_id = "run_recovery_subagent_settled";
    let provider_thread = "provider_thread_recovery_subagent";
    let stale = "node_recovery_subagent_stale";
    let done = "node_recovery_subagent_done";
    fixture.thread(id).await;
    fixture
        .put(
            "provider-thread.updated",
            id,
            provider_thread_json(provider_thread, id, CLAUDE, CLAUDE, "idle", json!([])),
        )
        .await;
    // Settled run: the stale-item loop owns it, not the nonterminal loop.
    fixture
        .put(
            "run.created",
            id,
            run_json(id, run_id, 1, "completed", CLAUDE),
        )
        .await;
    fixture
        .put(
            "node.updated",
            id,
            node_json(id, stale, Some(run_id), "subagent", "running", None),
        )
        .await;
    fixture
        .put(
            "node.updated",
            id,
            node_json(id, done, Some(run_id), "subagent", "completed", None),
        )
        .await;
    fixture
        .put(
            "subagent.updated",
            id,
            subagent_json(
                id,
                stale,
                Some(run_id),
                "provider_native",
                CLAUDE,
                CLAUDE,
                "running",
                None,
            ),
        )
        .await;
    // Already finished with a real result: must never be overwritten.
    let mut finished = subagent_json(
        id,
        done,
        Some(run_id),
        "provider_native",
        CLAUDE,
        CLAUDE,
        "completed",
        None,
    );
    finished["result"] = json!("done");
    fixture.put("subagent.updated", id, finished).await;
    fixture
        .put(
            "turn-item.updated",
            id,
            subagent_item(
                id,
                "turn_item_recovery_subagent_stale",
                run_id,
                stale,
                provider_thread,
                "provider_native",
                "running",
            ),
        )
        .await;
    fixture
        .put(
            "turn-item.updated",
            id,
            subagent_item(
                id,
                "turn_item_recovery_subagent_done",
                run_id,
                done,
                provider_thread,
                "provider_native",
                "completed",
            ),
        )
        .await;

    fixture.reconcile(ReconcileTrigger::Startup).await;

    let events = fixture.committed(id).await;
    // Only the nonterminal subagent item is cancelled.
    assert_eq!(cancelled_ids(&events, "turn-item.updated").len(), 1);
    // The linked subagent entity is terminalized alongside its turn item.
    let subagents = of_type(&events, "subagent.updated");
    assert_eq!(subagents.len(), 1);
    assert_eq!(payload_id(subagents[0]), stale);
    assert_eq!(status_of(subagents[0]).as_deref(), Some("cancelled"));
    // So is its execution node, which no live process can terminalize.
    let nodes = of_type(&events, "node.updated");
    assert_eq!(nodes.len(), 1);
    assert_eq!(payload_id(nodes[0]), stale);
}

#[tokio::test]
async fn leaves_delegated_tasks_to_their_own_child_threads_after_process_loss() {
    let fixture = fixture().await;
    let id = "thread_recovery_delegation";
    let settled_run = "run_recovery_delegation_settled";
    let running_run = "run_recovery_delegation_running";
    let provider_thread = "provider_thread_recovery_delegation";
    let settled_task = "node_recovery_delegation_settled";
    let running_task = "node_recovery_delegation_running";
    let native = "node_recovery_native_subagent";
    fixture.thread_on(id, CLAUDE).await;
    fixture
        .put(
            "provider-thread.updated",
            id,
            provider_thread_json(provider_thread, id, CLAUDE, CLAUDE, "idle", json!([])),
        )
        .await;
    for (run_id, ordinal, status) in [(settled_run, 1, "completed"), (running_run, 2, "running")] {
        let mut value = run_json(id, run_id, ordinal, status, CLAUDE);
        value["providerThreadId"] = json!(provider_thread);
        fixture.put("run.created", id, value).await;
    }
    for (node, run_id) in [
        (settled_task, settled_run),
        (running_task, running_run),
        (native, settled_run),
    ] {
        fixture
            .put(
                "node.updated",
                id,
                node_json(id, node, Some(run_id), "subagent", "running", None),
            )
            .await;
    }
    for (task, run_id, origin) in [
        (settled_task, settled_run, "app_owned"),
        (running_task, running_run, "app_owned"),
        (native, settled_run, "provider_native"),
    ] {
        let child = (origin == "app_owned").then(|| format!("thread:delegated-task:{task}"));
        fixture
            .put(
                "subagent.updated",
                id,
                subagent_json(
                    id,
                    task,
                    Some(run_id),
                    origin,
                    CLAUDE,
                    CLAUDE,
                    "running",
                    child.as_deref(),
                ),
            )
            .await;
        fixture
            .put(
                "turn-item.updated",
                id,
                subagent_item(
                    id,
                    &format!("turn_item:{task}"),
                    run_id,
                    task,
                    provider_thread,
                    origin,
                    "running",
                ),
            )
            .await;
    }

    fixture.reconcile(ReconcileTrigger::Startup).await;

    let events = fixture.committed(id).await;
    let touched: Vec<String> = events
        .iter()
        .filter_map(|event| {
            let value = wire(event);
            match value["type"].as_str().unwrap() {
                "turn-item.updated" if value["payload"]["type"] == "subagent" => {
                    value["payload"]["subagentId"].as_str().map(str::to_owned)
                }
                "subagent.updated" | "node.updated" => {
                    value["payload"]["id"].as_str().map(str::to_owned)
                }
                _ => None,
            }
        })
        .collect();
    // The cut run itself is cancelled; only the provider-native subagent dies with it.
    assert!(of_type(&events, "run.updated").into_iter().any(|event| {
        payload_id(event) == running_run && status_of(event).as_deref() == Some("cancelled")
    }));
    assert!(!touched.contains(&settled_task.to_owned()));
    assert!(!touched.contains(&running_task.to_owned()));
    assert!(touched.contains(&native.to_owned()));
    // The note lists only provider-native work: delegated tasks report back on their own.
    let labels: Vec<String> = of_type(&events, "run.background-work-cancelled")
        .into_iter()
        .flat_map(|event| {
            wire(event)["payload"]["restartCancelledBackgroundWork"]
                .as_array()
                .unwrap()
                .iter()
                .map(|work| work["label"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(labels, vec![format!("subagent {native}")]);
}

/// The regression file's case.
#[tokio::test]
async fn uses_the_thread_provider_for_stale_background_work_without_provider_threads() {
    let fixture = fixture().await;
    let id = "thread_recovery_background_no_provider_threads";
    fixture.thread_on(id, CODEX).await;
    fixture
        .put(
            "run.created",
            id,
            run_json(
                id,
                "run_recovery_background_no_provider_threads",
                1,
                "completed",
                CODEX,
            ),
        )
        .await;
    fixture
        .put(
            "turn-item.updated",
            id,
            command_item(
                id,
                "turn_item_recovery_background_no_provider_threads",
                None,
                None,
                None,
            ),
        )
        .await;

    fixture.reconcile(ReconcileTrigger::Startup).await;

    let events = fixture.committed(id).await;
    let item_event = of_type(&events, "turn-item.updated")[0];
    assert_eq!(wire(item_event)["providerInstanceId"], CODEX);
    assert_eq!(status_of(item_event).as_deref(), Some("cancelled"));
}

// ---------------------------------------------------------------------------
// Restart continuation (no upstream case in this file; the pure selection is
// tested in `restart_continuation.rs`, and these cover the service's use of it)
// ---------------------------------------------------------------------------

const CONTINUATION_THREAD: &str = "thread_restart_continuation";
const CONTINUATION_RUN: &str = "run_restart_continuation";
const CONTINUATION_EFFECT: &str = "effect:restart-continuation:run_restart_continuation";

/// A running root turn that a restart continues: a strong native thread ref, a
/// live session and a running provider turn.
async fn seed_continuable_run(fixture: &Fixture) {
    let id = CONTINUATION_THREAD;
    fixture.thread(id).await;
    fixture
        .put(
            "provider-session.updated",
            id,
            session_json("provider_session_restart", CODEX, CODEX, "ready"),
        )
        .await;
    let mut provider_thread = provider_thread_json(
        "provider_thread_restart",
        id,
        CODEX,
        CODEX,
        "active",
        json!([]),
    );
    provider_thread["providerSessionId"] = json!("provider_session_restart");
    provider_thread["nativeThreadRef"] =
        json!({"driver": CODEX, "nativeId": "native-thread", "strength": "strong"});
    fixture
        .put("provider-thread.updated", id, provider_thread)
        .await;
    let mut running = run_json(id, CONTINUATION_RUN, 1, "running", CODEX);
    running["providerThreadId"] = json!("provider_thread_restart");
    running["activeAttemptId"] = json!("attempt_restart");
    fixture.put("run.created", id, running).await;
    fixture
        .put(
            "run-attempt.created",
            id,
            attempt_json(
                "attempt_restart",
                CONTINUATION_RUN,
                "node_restart",
                "running",
            ),
        )
        .await;
    fixture
        .put(
            "node.updated",
            id,
            node_json(
                id,
                "node_restart",
                Some(CONTINUATION_RUN),
                "root_turn",
                "running",
                None,
            ),
        )
        .await;
    fixture
        .put(
            "provider-turn.updated",
            id,
            provider_turn_json(
                "provider_turn_restart",
                "provider_thread_restart",
                "node_restart",
                "attempt_restart",
            ),
        )
        .await;
}

fn continuation_disabled() -> ServerSettings {
    let mut settings = default_server_settings();
    settings.continue_threads_after_server_update = false;
    settings
}

async fn continuation_effect(fixture: &Fixture) -> Option<crate::pipeline::effect_outbox::Effect> {
    fixture
        .sink
        .outbox()
        .get(CONTINUATION_EFFECT.to_owned())
        .await
        .expect("read effect")
}

#[tokio::test]
async fn startup_queues_a_continuation_for_the_run_it_cancels() {
    let fixture = fixture().await;
    seed_continuable_run(&fixture).await;

    let summary = fixture.reconcile(ReconcileTrigger::Startup).await;

    assert_eq!(summary.terminalized_runs, 1);
    let events = fixture.committed(CONTINUATION_THREAD).await;
    assert_eq!(
        status_of(of_type(&events, "run.updated")[0]).as_deref(),
        Some("cancelled")
    );
    let effect = continuation_effect(&fixture)
        .await
        .expect("continuation queued");
    assert_eq!(
        effect.status,
        crate::pipeline::effect_outbox::EffectStatus::Pending
    );
    assert!(matches!(
        effect.request,
        EffectRequest::ProviderRuntimeContinue { ref source_run_id }
            if source_run_id.as_str() == CONTINUATION_RUN
    ));
}

#[tokio::test]
async fn startup_queues_no_continuation_when_the_project_turns_it_off() {
    let fixture = fixture_with(continuation_disabled()).await;
    seed_continuable_run(&fixture).await;

    let summary = fixture.reconcile(ReconcileTrigger::Startup).await;

    assert_eq!(summary.terminalized_runs, 1);
    assert!(continuation_effect(&fixture).await.is_none());
}

#[tokio::test]
async fn shutdown_reconciliation_queues_no_continuation() {
    let fixture = fixture().await;
    seed_continuable_run(&fixture).await;

    let summary = fixture.reconcile(ReconcileTrigger::Shutdown).await;

    assert_eq!(summary.terminalized_runs, 1);
    assert!(continuation_effect(&fixture).await.is_none());
}

#[tokio::test]
async fn prepare_for_shutdown_records_intent_without_touching_the_run() {
    let fixture = fixture().await;
    seed_continuable_run(&fixture).await;

    fixture
        .service
        .prepare_for_shutdown()
        .await
        .expect("prepare");
    // Preparing twice leaves one effect: its id is the run's.
    fixture
        .service
        .prepare_for_shutdown()
        .await
        .expect("prepare again");

    assert!(continuation_effect(&fixture).await.is_some());
    assert!(
        of_type(&fixture.committed(CONTINUATION_THREAD).await, "run.updated").is_empty(),
        "providers are still live, so the run is not cancelled yet"
    );
}

#[tokio::test]
async fn prepare_for_shutdown_records_nothing_when_continuation_is_off() {
    let fixture = fixture_with(continuation_disabled()).await;
    seed_continuable_run(&fixture).await;

    fixture
        .service
        .prepare_for_shutdown()
        .await
        .expect("prepare");

    assert!(continuation_effect(&fixture).await.is_none());
}

// ---------------------------------------------------------------------------
// `RestartContinuation.test.ts` cases that drive this service
// ---------------------------------------------------------------------------

fn with_project_override(enabled: bool, override_: Option<bool>, project: &str) -> ServerSettings {
    let mut settings = default_server_settings();
    settings.continue_threads_after_server_update = enabled;
    if let Some(value) = override_ {
        settings.project_settings_overrides.insert(
            turin_contracts::ProjectId::new(project).unwrap(),
            turin_contracts::settings::ProjectSettingsOverrides {
                continue_threads_after_server_update: Some(value),
                ..Default::default()
            },
        );
    }
    settings
}

#[tokio::test]
async fn atomically_records_restart_intent_with_cancellation_for_each_opt_in_and_project_override()
{
    for (enabled, override_) in [
        (false, None),
        (true, None),
        (false, Some(true)),
        (true, Some(false)),
    ] {
        let fixture = fixture_with(with_project_override(enabled, override_, "project:test")).await;
        seed_continuable_run(&fixture).await;

        fixture.reconcile(ReconcileTrigger::Startup).await;

        let events = fixture.committed(CONTINUATION_THREAD).await;
        assert!(
            of_type(&events, "run.updated")
                .iter()
                .any(|event| status_of(event).as_deref() == Some("cancelled"))
        );
        let queued = continuation_effect(&fixture).await;
        assert_eq!(
            queued.is_some(),
            override_.unwrap_or(enabled),
            "enabled {enabled} override {override_:?}"
        );
        if let Some(effect) = queued {
            assert!(matches!(
                effect.request,
                EffectRequest::ProviderRuntimeContinue { ref source_run_id }
                    if source_run_id.as_str() == CONTINUATION_RUN
            ));
        }
    }
}

#[tokio::test]
async fn lets_a_cut_blocking_delegate_task_wake_its_parent_like_an_async_task() {
    let fixture = fixture().await;
    seed_continuable_run(&fixture).await;
    let id = CONTINUATION_THREAD;
    for (task, wake, status) in [
        ("task:waited", "settled_only", "running"),
        ("task:async", "always", "running"),
        ("task:finished", "settled_only", "completed"),
    ] {
        let mut row = subagent_json(
            id,
            task,
            Some(CONTINUATION_RUN),
            "app_owned",
            CODEX,
            CODEX,
            status,
            Some(&format!("thread:{task}")),
        );
        row["completionWake"] = json!(wake);
        fixture.put("subagent.updated", id, row).await;
    }

    fixture.reconcile(ReconcileTrigger::Startup).await;

    let events = fixture.committed(id).await;
    let upgraded: Vec<(String, Value)> = of_type(&events, "subagent.updated")
        .into_iter()
        .map(|event| {
            (
                payload_id(event),
                wire(event)["payload"]["completionWake"].clone(),
            )
        })
        .collect();
    assert_eq!(upgraded, vec![("task:waited".to_owned(), json!("always"))]);
}

/// A run that settled while a restart continuation of it is possible: a
/// background subagent or dev server it left open, on its own provider thread
/// or another's.
async fn seed_settled_run_with_background_work(
    fixture: &Fixture,
    status: &str,
    item_provider_thread: &str,
    background: &str,
) {
    let id = CONTINUATION_THREAD;
    seed_continuable_run(fixture).await;
    // Settled: the turn completed, and the run is no longer in flight.
    fixture
        .put(
            "provider-turn.updated",
            id,
            patch(
                provider_turn_json(
                    "provider_turn_restart",
                    "provider_thread_restart",
                    "node_restart",
                    "attempt_restart",
                ),
                json!({"status": "completed", "completedAt": T0}),
            ),
        )
        .await;
    let mut settled = run_json(id, CONTINUATION_RUN, 1, status, CODEX);
    settled["providerThreadId"] = json!("provider_thread_restart");
    settled["activeAttemptId"] = json!("attempt_restart");
    fixture.put("run.updated", id, settled).await;
    if background == "subagent" {
        fixture
            .put(
                "turn-item.updated",
                id,
                subagent_item(
                    id,
                    "turn-item:background-subagent",
                    CONTINUATION_RUN,
                    "subagent:background",
                    item_provider_thread,
                    "provider_native",
                    "running",
                ),
            )
            .await;
        fixture
            .put(
                "subagent.updated",
                id,
                subagent_json(
                    id,
                    "subagent:background",
                    Some(CONTINUATION_RUN),
                    "provider_native",
                    CODEX,
                    CODEX,
                    "running",
                    None,
                ),
            )
            .await;
    } else {
        let mut dev_server = command_item(
            id,
            "turn-item:dev-server",
            Some(CONTINUATION_RUN),
            None,
            Some(item_provider_thread),
        );
        dev_server["input"] = json!("vp run dev");
        fixture.put("turn-item.updated", id, dev_server).await;
    }
}

#[tokio::test]
async fn prepares_no_continuation_for_background_work_another_provider_thread_launched() {
    let fixture = fixture().await;
    // A Claude turn left a background subagent open, then the thread switched
    // to Codex and its turn settled. Recovery would cancel Claude's work.
    seed_settled_run_with_background_work(
        &fixture,
        "completed",
        "provider_thread_claude",
        "subagent",
    )
    .await;

    fixture
        .service
        .prepare_for_shutdown()
        .await
        .expect("prepare");
    assert!(continuation_effect(&fixture).await.is_none());

    // The same work on the settled run's own provider thread does wake it.
    fixture
        .put(
            "turn-item.updated",
            CONTINUATION_THREAD,
            subagent_item(
                CONTINUATION_THREAD,
                "turn-item:background-subagent",
                CONTINUATION_RUN,
                "subagent:background",
                "provider_thread_restart",
                "provider_native",
                "running",
            ),
        )
        .await;
    fixture
        .service
        .prepare_for_shutdown()
        .await
        .expect("prepare");
    assert!(continuation_effect(&fixture).await.is_some());
}

async fn cleans_up_background_work_and_wakes_only_for_awaited_work(
    status: &str,
    background: &str,
    wakes: bool,
) {
    for trigger in [ReconcileTrigger::Startup, ReconcileTrigger::Shutdown] {
        let fixture = fixture().await;
        seed_settled_run_with_background_work(
            &fixture,
            status,
            "provider_thread_restart",
            background,
        )
        .await;
        // Shutdown records the wake before it cancels the work; a startup after
        // a kill records it with the cancellation.
        if trigger == ReconcileTrigger::Shutdown {
            fixture
                .service
                .prepare_for_shutdown()
                .await
                .expect("prepare");
        }

        fixture.reconcile(trigger).await;

        assert_eq!(
            continuation_effect(&fixture).await.is_some(),
            wakes,
            "{status} {background} {trigger:?}"
        );
        let events = fixture.committed(CONTINUATION_THREAD).await;
        assert!(!cancelled_ids(&events, "turn-item.updated").is_empty());
        let notes = of_type(&events, "run.background-work-cancelled");
        assert_eq!(notes.len(), 1);
        let note = &wire(notes[0])["payload"];
        assert_eq!(note["runId"], CONTINUATION_RUN);
        let work = note["restartCancelledBackgroundWork"].as_array().unwrap();
        assert_eq!(work.len(), 1, "{work:?}");
        if background == "subagent" {
            assert_eq!(work[0]["kind"], "subagent");
            assert_eq!(work[0]["id"], "turn-item:background-subagent");
        } else {
            assert_eq!(work[0]["kind"], "shell");
            assert_eq!(work[0]["label"], "vp run dev");
            assert_eq!(work[0]["id"], "turn-item:dev-server");
        }
        if status == "completed" {
            assert!(of_type(&events, "run.updated").is_empty());
        }
    }
}

#[tokio::test]
async fn cleans_up_background_work_of_a_completed_run_and_wakes_it_for_an_awaited_subagent() {
    cleans_up_background_work_and_wakes_only_for_awaited_work("completed", "subagent", true).await;
}

#[tokio::test]
async fn cleans_up_background_work_of_a_waiting_run_and_wakes_it_for_an_awaited_subagent() {
    cleans_up_background_work_and_wakes_only_for_awaited_work("waiting", "subagent", true).await;
}

#[tokio::test]
async fn cleans_up_a_completed_runs_dev_server_without_waking_it() {
    cleans_up_background_work_and_wakes_only_for_awaited_work("completed", "shell", false).await;
}

#[tokio::test]
async fn cleans_up_a_waiting_runs_dev_server_without_waking_it() {
    cleans_up_background_work_and_wakes_only_for_awaited_work("waiting", "shell", false).await;
}

/// `ThreadManagementService` over the real store, for the continuation that
/// follows a recovery.
struct StoreThreads {
    store: Arc<ProjectionStore>,
    dispatched: parking_lot::Mutex<usize>,
}

#[async_trait::async_trait]
impl crate::core::restart_continuation_service::RestartThreads for StoreThreads {
    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        _fields: &[crate::pipeline::orchestrator_port::RecordField],
        _filter: &crate::pipeline::orchestrator_port::RecordFilter,
    ) -> Result<crate::pipeline::orchestrator_port::ThreadRecords, BoxError> {
        Ok(self.store.get_thread_projection(thread_id).await?)
    }

    async fn dispatch(
        &self,
        _command: turin_contracts::orchestration_v2::OrchestrationV2ServerCommand,
    ) -> Result<
        crate::pipeline::orchestrator_port::DispatchResult,
        crate::pipeline::thread_management_service::ThreadManagementError,
    > {
        *self.dispatched.lock() += 1;
        Ok(crate::pipeline::orchestrator_port::DispatchResult {
            sequence: 0,
            stored_events: Vec::new(),
        })
    }

    async fn recover_delegated_task(&self, _thread_id: &ThreadId, _source_run_id: &RunId) {}
}

#[tokio::test]
async fn does_not_cancel_or_resume_a_run_that_completes_while_shutdown_intent_commits() {
    let fixture = fixture().await;
    seed_continuable_run(&fixture).await;
    let id = CONTINUATION_THREAD;

    fixture
        .service
        .prepare_for_shutdown()
        .await
        .expect("prepare");
    assert!(continuation_effect(&fixture).await.is_some());
    // The provider finishes while the intent commits.
    let mut completed = run_json(id, CONTINUATION_RUN, 1, "completed", CODEX);
    completed["providerThreadId"] = json!("provider_thread_restart");
    completed["activeAttemptId"] = json!("attempt_restart");
    fixture.put("run.updated", id, completed).await;

    fixture.reconcile(ReconcileTrigger::Shutdown).await;

    assert!(
        of_type(&fixture.committed(id).await, "run.updated").is_empty(),
        "a completed run is not cancelled"
    );
    // Delivery rejects the source run that actually completed.
    let threads = Arc::new(StoreThreads {
        store: fixture.store.clone(),
        dispatched: parking_lot::Mutex::new(0),
    });
    let continuation = crate::core::restart_continuation_service::RestartContinuationService::new(
        Arc::new(StaticSettings(default_server_settings())),
        threads.clone(),
        fixture.store.clone(),
        fixture.sink.outbox().clone(),
        Arc::new(crate::core::restart_continuation_service::NoDelegatedTaskRecovery),
    );
    continuation
        .continue_restarted_run(&tid(id), &RunId::new(CONTINUATION_RUN).unwrap())
        .await
        .expect("continue");
    assert_eq!(*threads.dispatched.lock(), 0);
}

#[tokio::test]
async fn prepares_later_threads_continuations_when_one_thread_fails() {
    let fixture = fixture().await;
    let broken = "thread:broken";
    fixture.thread(broken).await;
    fixture
        .put(
            "run.created",
            broken,
            run_json(broken, "run:broken", 1, "running", CODEX),
        )
        .await;
    seed_continuable_run(&fixture).await;
    // The broken thread's row no longer decodes, so reading its recovery
    // projection fails.
    fixture
        .store
        .db()
        .write(|tx| {
            tx.execute(
                "UPDATE orchestration_v2_projection_threads SET payload_json = '{}' WHERE thread_id = 'thread:broken'",
                [],
            )
            .map_err(|error| turin_persistence::PersistenceError::sql("test", &error))?;
            Ok(())
        })
        .await
        .expect("corrupt the row");
    assert!(
        fixture
            .store
            .get_runtime_recovery_projection(&tid(broken))
            .await
            .is_err()
    );

    fixture
        .service
        .prepare_for_shutdown()
        .await
        .expect("prepare");

    assert!(continuation_effect(&fixture).await.is_some());
}
