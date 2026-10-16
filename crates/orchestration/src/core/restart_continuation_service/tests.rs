//! Upstream `RestartContinuation.test.ts`, the effectful cases that drive
//! `continueRestartedRun`. Upstream mocks `ThreadManagementService` so that
//! `getThreadRecords` answers every read with one projection; `FakeThreads`
//! does the same, and records the dispatched commands. The cases that drive
//! `ProviderRuntimeRecoveryService` are in its own test module. The pure
//! selection cases are in `restart_continuation.rs`.
//!
//! The outbox is the real one, in memory, for the `RestartRecovery` reads.

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};
use turin_contracts::{
    ProjectId, orchestration_v2::OrchestrationV2ThreadProjection,
    provider_instance::ProviderInstanceId, settings::default_server_settings,
};
use turin_persistence::Db;

use super::*;
use crate::{
    pipeline::{
        effect_outbox::{EffectRequest, PendingEffect},
        test_support::StaticSettings,
    },
    projection::test_support::{self, message, run, thread, turn_item},
};

const THREAD: &str = "thread:restart";
const RUN: &str = "run:restart";
const INSTANCE: &str = "codex";
const PROVIDER_THREAD: &str = "provider-thread:restart";
const SESSION: &str = "session:restart";
const ATTEMPT: &str = "attempt:restart";
const T0: &str = "2026-10-03T12:00:00.000Z";
const CONTINUE: &str = "Continue where you left off.";

// ---------------------------------------------------------------------------
// Fixtures: upstream's `makeProjection`, as the wire shape
// ---------------------------------------------------------------------------

fn session_json(status: &str) -> Value {
    json!({
        "id": SESSION, "driver": INSTANCE, "providerInstanceId": INSTANCE, "status": status,
        "cwd": "/repo", "model": null, "capabilities": test_support::capabilities(),
        "createdAt": T0, "updatedAt": T0, "lastError": null,
    })
}

fn provider_thread_json() -> Value {
    json!({
        "id": PROVIDER_THREAD, "driver": INSTANCE, "providerInstanceId": INSTANCE,
        "providerSessionId": SESSION, "appThreadId": THREAD, "ownerNodeId": null,
        "nativeThreadRef": {"driver": INSTANCE, "nativeId": "native-thread", "strength": "strong"},
        "nativeConversationHeadRef": null, "status": "active", "firstRunOrdinal": null,
        "lastRunOrdinal": null, "handoffIds": [], "forkedFrom": null,
        "createdAt": T0, "updatedAt": T0,
    })
}

fn provider_turn_json(status: &str) -> Value {
    json!({
        "id": "turn:restart", "providerThreadId": PROVIDER_THREAD, "nodeId": "node:restart",
        "runAttemptId": ATTEMPT, "nativeTurnRef": null, "ordinal": 1, "status": status,
        "startedAt": T0, "completedAt": null, "tokenUsage": null, "turnTokenUsage": null,
    })
}

fn run_json(id: &str, ordinal: u64, status: &str) -> Value {
    let mut value = run(THREAD, id, ordinal, status);
    value["providerInstanceId"] = json!(INSTANCE);
    value["modelSelection"] = json!({"instanceId": INSTANCE, "model": "gpt-6"});
    value["providerThreadId"] = json!(PROVIDER_THREAD);
    value["activeAttemptId"] = json!(ATTEMPT);
    value["userMessageId"] = json!("message:user");
    value
}

/// A running root run with a live native thread, a ready session and an
/// in-flight provider turn.
fn base() -> Value {
    let mut thread = thread(THREAD);
    thread["projectId"] = json!("restart-project");
    thread["providerInstanceId"] = json!(INSTANCE);
    json!({
        "thread": thread,
        "runs": [run_json(RUN, 1, "running")],
        "attempts": [], "nodes": [], "subagents": [],
        "providerSessions": [session_json("running")],
        "providerThreads": [provider_thread_json()],
        "providerTurns": [provider_turn_json("running")],
        "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
        "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
        "contextTransfers": [], "visibleTurnItems": [], "updatedAt": T0,
    })
}

/// A run the restart cut: cancelled, with its provider turn cancelled too.
fn cut_mid_turn(extra: Value) -> Value {
    let mut value = base();
    value["runs"][0]["status"] = json!("cancelled");
    value["runs"][0]["userMessageId"] = json!("message:user");
    for (key, field) in extra.as_object().cloned().unwrap_or_default() {
        value["runs"][0][key] = field;
    }
    value["providerTurns"][0]["status"] = json!("cancelled");
    value
}

fn queued_follow_up() -> Value {
    let mut queued = run_json("run:queued", 2, "queued");
    queued["queueHeld"] = json!(true);
    queued
}

fn user_message(text: &str) -> Value {
    message(THREAD, "message:user", Some(RUN), "user", text, T0)
}

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

enum DispatchFailure {
    InstanceDisabled,
    Other,
}

/// `ThreadManagementService` as upstream's tests mock it.
struct FakeThreads {
    projection: Mutex<Value>,
    commands: Mutex<Vec<OrchestrationV2ServerCommand>>,
    recovered: Mutex<Vec<(String, String)>>,
    failure: Mutex<Option<DispatchFailure>>,
    reads: Mutex<usize>,
}

impl FakeThreads {
    fn new(projection: Value) -> Arc<Self> {
        Arc::new(Self {
            projection: Mutex::new(projection),
            commands: Mutex::default(),
            recovered: Mutex::default(),
            failure: Mutex::default(),
            reads: Mutex::default(),
        })
    }

    fn dispatches(&self) -> Vec<Value> {
        self.commands
            .lock()
            .iter()
            .map(|command| serde_json::to_value(command).expect("command encodes"))
            .collect()
    }

    fn texts(&self) -> Vec<String> {
        self.dispatches()
            .iter()
            .map(|command| command["text"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[async_trait]
impl RestartThreads for FakeThreads {
    async fn get_thread_records(
        &self,
        _thread_id: &ThreadId,
        _fields: &[RecordField],
        _filter: &RecordFilter,
    ) -> Result<ThreadRecords, BoxError> {
        *self.reads.lock() += 1;
        let value = self.projection.lock().clone();
        Ok(serde_json::from_value::<OrchestrationV2ThreadProjection>(
            value,
        )?)
    }

    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError> {
        let failure = self.failure.lock().take();
        match failure {
            Some(DispatchFailure::InstanceDisabled) => {
                Err(ThreadManagementError::ProviderInstanceDisabled {
                    project_id: ProjectId::new("restart-project").unwrap(),
                    instance_id: ProviderInstanceId::new(INSTANCE).unwrap(),
                })
            }
            Some(DispatchFailure::Other) => Err(ThreadManagementError::ProjectionLoad {
                project_id: ProjectId::new("restart-project").unwrap(),
                thread_id: ThreadId::new(THREAD).unwrap(),
                cause: "boom".into(),
            }),
            None => {
                // The dispatch commits the continuation's message.
                let mut projection = self.projection.lock();
                if let OrchestrationV2ServerCommand::Client(
                    turin_contracts::orchestration_v2::OrchestrationV2Command::MessageDispatch {
                        message_id,
                        ..
                    },
                ) = &command
                {
                    let mut messages = projection["messages"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    messages.push(message(
                        THREAD,
                        message_id.as_str(),
                        None,
                        "user",
                        "continuation",
                        T0,
                    ));
                    projection["messages"] = Value::Array(messages);
                }
                self.commands.lock().push(command);
                Ok(DispatchResult {
                    sequence: 0,
                    stored_events: Vec::new(),
                })
            }
        }
    }

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId) {
        self.recovered
            .lock()
            .push((thread_id.to_string(), source_run_id.to_string()));
    }
}

struct Delegated {
    swept: Mutex<usize>,
    settled: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl DelegatedTaskRecovery for Delegated {
    async fn recover_delegated_tasks(&self) {
        *self.swept.lock() += 1;
    }

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId) {
        self.settled
            .lock()
            .push((thread_id.to_string(), source_run_id.to_string()));
    }
}

struct Harness {
    service: RestartContinuationService,
    threads: Arc<FakeThreads>,
    delegated: Arc<Delegated>,
    outbox: EffectOutbox,
}

async fn harness_with(projection: Value, enabled: bool) -> Harness {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let store = Arc::new(ProjectionStore::new(db.clone()));
    let outbox = EffectOutbox::new(db);
    let threads = FakeThreads::new(projection);
    let delegated = Arc::new(Delegated {
        swept: Mutex::default(),
        settled: Mutex::default(),
    });
    let mut settings = default_server_settings();
    settings.continue_threads_after_server_update = enabled;
    Harness {
        service: RestartContinuationService::new(
            Arc::new(StaticSettings(settings)),
            threads.clone(),
            store,
            outbox.clone(),
            delegated.clone(),
        ),
        threads,
        delegated,
        outbox,
    }
}

async fn harness(projection: Value) -> Harness {
    harness_with(projection, true).await
}

fn thread_id() -> ThreadId {
    ThreadId::new(THREAD).unwrap()
}

fn run_id(id: &str) -> RunId {
    RunId::new(id).unwrap()
}

async fn continue_run(harness: &Harness) {
    harness
        .service
        .continue_restarted_run(&thread_id(), &run_id(RUN))
        .await
        .expect("continue");
}

/// The prompts dispatched for a projection (upstream `continuationTexts`).
async fn continuation_texts(projection: Value) -> Vec<String> {
    let harness = harness(projection).await;
    continue_run(&harness).await;
    harness.threads.texts()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

async fn ignores_a_pending_continuation_for_a_settled_turn(status: &str) {
    let mut projection = base();
    projection["runs"][0]["status"] = json!(status);
    projection["runs"][0]["restartCancelledBackgroundWork"] =
        json!([{"kind": "shell", "label": "sleep 25 && echo DONE"}]);
    projection["providerTurns"][0]["status"] = json!("completed");
    assert!(continuation_texts(projection).await.is_empty());
}

#[tokio::test]
async fn ignores_a_pending_restart_continuation_for_a_completed_run_whose_provider_turn_settled() {
    ignores_a_pending_continuation_for_a_settled_turn("completed").await;
}

#[tokio::test]
async fn ignores_a_pending_restart_continuation_for_a_waiting_run_whose_provider_turn_settled() {
    ignores_a_pending_continuation_for_a_settled_turn("waiting").await;
}

#[tokio::test]
async fn ignores_a_pending_restart_continuation_for_a_cancelled_run_whose_provider_turn_settled() {
    ignores_a_pending_continuation_for_a_settled_turn("cancelled").await;
}

async fn wakes_a_run_whose_monitor_the_restart_cancelled(status: &str) {
    let mut projection = base();
    projection["runs"][0]["status"] = json!(status);
    projection["runs"][0]["restartCancelledBackgroundWork"] = json!([
        {"kind": "monitor", "label": "Watch CI run 42"},
        {"kind": "shell", "label": "vp run dev"},
    ]);
    projection["providerTurns"][0]["status"] = json!("completed");
    let harness = harness(projection).await;

    continue_run(&harness).await;

    let dispatches = harness.threads.dispatches();
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0]["type"], "message.dispatch");
    assert_eq!(dispatches[0]["restartContinuationOfRunId"], RUN);
    let text = dispatches[0]["text"].as_str().unwrap();
    assert!(text.contains("- monitor: Watch CI run 42"), "{text}");
    assert!(text.contains("- shell: vp run dev"), "{text}");
}

#[tokio::test]
async fn wakes_a_completed_run_whose_monitor_the_restart_cancelled_with_the_note_as_its_prompt() {
    wakes_a_run_whose_monitor_the_restart_cancelled("completed").await;
}

#[tokio::test]
async fn wakes_a_waiting_run_whose_monitor_the_restart_cancelled_with_the_note_as_its_prompt() {
    wakes_a_run_whose_monitor_the_restart_cancelled("waiting").await;
}

#[tokio::test]
async fn wakes_a_cancelled_run_whose_monitor_the_restart_cancelled_with_the_note_as_its_prompt() {
    wakes_a_run_whose_monitor_the_restart_cancelled("cancelled").await;
}

#[tokio::test]
async fn does_not_continue_a_failed_run_that_lost_background_work() {
    let mut projection = base();
    projection["runs"][0]["status"] = json!("failed");
    projection["runs"][0]["restartCancelledBackgroundWork"] =
        json!([{"kind": "shell", "label": "sleep 25"}]);
    projection["providerTurns"][0]["status"] = json!("failed");
    assert!(continuation_texts(projection).await.is_empty());
}

#[tokio::test]
async fn does_not_duplicate_delivery_and_yields_to_newer_user_work_or_opt_out() {
    let mut projection = base();
    projection["runs"][0]["status"] = json!("cancelled");
    let harness_on = harness(projection.clone()).await;

    continue_run(&harness_on).await;
    continue_run(&harness_on).await;

    let dispatches = harness_on.threads.dispatches();
    assert_eq!(dispatches.len(), 1);
    assert!(
        dispatches[0]["commandId"]
            .as_str()
            .unwrap()
            .ends_with("run:restart")
    );
    assert_eq!(dispatches[0]["restartContinuationOfRunId"], RUN);

    // A user run that ran after the cut one takes precedence.
    let mut newer = projection.clone();
    // Like the cut run it has no end time, so the ordinal decides which ran later.
    let user_run = run_json("run:user-newer", 2, "completed");
    newer["runs"].as_array_mut().unwrap().push(user_run);
    assert!(continuation_texts(newer).await.is_empty());

    // The project turned continuation off since.
    let off = harness_with(projection, false).await;
    continue_run(&off).await;
    assert!(off.threads.dispatches().is_empty());
}

#[tokio::test]
async fn continues_a_cut_run_past_queued_follow_ups_which_stay_held() {
    let mut cut = cut_mid_turn(json!({}));
    cut["runs"].as_array_mut().unwrap().push(queued_follow_up());
    assert_eq!(continuation_texts(cut).await, vec![CONTINUE.to_owned()]);
}

#[tokio::test]
async fn continues_a_resumed_queued_run_that_ran_after_an_earlier_continuation() {
    // The continuation (ordinal 3) ran ahead of the held queue, then the user
    // resumed the queued run (ordinal 1 here) and the server restarted again.
    let mut finished = run_json("run:earlier-continuation", 3, "completed");
    finished["completedAt"] = json!("2026-10-03T10:00:00.000Z");
    let mut cut = cut_mid_turn(json!({"completedAt": "2026-10-03T10:05:00.000Z"}));
    cut["runs"].as_array_mut().unwrap().push(finished);
    assert_eq!(continuation_texts(cut).await, vec![CONTINUE.to_owned()]);
}

#[tokio::test]
async fn does_not_continue_a_run_the_user_asked_to_stop_before_the_restart() {
    let mut cut = cut_mid_turn(json!({}));
    cut["turnItems"] = json!([turn_item(
        THREAD,
        "turn-item:interrupt",
        Some(RUN),
        1,
        "run_interrupt_request",
        "completed",
        json!({"message": "Stop"}),
    )]);
    assert!(continuation_texts(cut).await.is_empty());
}

#[tokio::test]
async fn does_not_continue_a_cut_compact_or_logout_turn() {
    for text in ["/compact", " /LOGOUT "] {
        let mut cut = cut_mid_turn(json!({}));
        cut["messages"] = json!([user_message(text)]);
        assert!(continuation_texts(cut).await.is_empty(), "{text}");
    }
    let mut cut = cut_mid_turn(json!({}));
    cut["messages"] = json!([user_message("/compact later")]);
    assert_eq!(continuation_texts(cut).await.len(), 1);
}

#[tokio::test]
async fn tells_a_turn_cut_mid_way_about_the_background_work_it_lost() {
    let cut = cut_mid_turn(json!({
        "restartCancelledBackgroundWork": [{"kind": "subagent", "label": "Background reviewer"}]
    }));
    let texts = continuation_texts(cut).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("Background reviewer"));
    assert!(texts[0].ends_with(CONTINUE));
}

#[tokio::test]
async fn shows_a_continuation_as_a_t3_code_notice_whose_detail_is_the_prompt() {
    let harness = harness(cut_mid_turn(json!({}))).await;
    continue_run(&harness).await;
    assert_eq!(
        harness.threads.dispatches()[0]["notification"],
        json!({
            "source": {"kind": "system"},
            "outcome": "updated",
            "summary": "T3 Code restarted and resumed this turn",
            "detail": CONTINUE,
        })
    );
}

#[tokio::test]
async fn carries_the_note_forward_when_its_continuation_was_cut_before_reaching_the_provider() {
    let mut original = run_json("run:original", 1, "completed");
    original["activeAttemptId"] = json!("attempt:original");
    original["restartCancelledBackgroundWork"] =
        json!([{"kind": "shell", "label": "sleep 25 && echo DONE"}]);
    let mut continuation = run_json(RUN, 2, "cancelled");
    continuation["restartContinuationOfRunId"] = json!("run:original");
    let mut projection = base();
    projection["runs"] = json!([original, continuation]);
    // The original turn settled; the continuation's attempt never started one.
    projection["providerTurns"][0]["runAttemptId"] = json!("attempt:original");
    projection["providerTurns"][0]["status"] = json!("completed");
    projection["attempts"] = json!([{
        "id": "attempt:original", "runId": "run:original", "attemptOrdinal": 1,
        "rootNodeId": "node:original", "providerInstanceId": INSTANCE,
        "providerThreadId": PROVIDER_THREAD, "providerTurnId": null, "reason": "initial",
        "status": "completed", "startedAt": T0, "completedAt": T0,
    }]);

    let texts = continuation_texts(projection).await;

    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("sleep 25 && echo DONE"));
    assert!(!texts[0].contains("Continue where"));
}

#[tokio::test]
async fn stops_continuing_after_three_automatic_restart_continuations_in_a_row() {
    let mut cut = cut_mid_turn(json!({"restartContinuationOfRunId": "run:second-continuation"}));
    let mut second = cut["runs"][0].clone();
    second["id"] = json!("run:second-continuation");
    second["restartContinuationOfRunId"] = json!("run:first-continuation");
    let mut first = cut["runs"][0].clone();
    first["id"] = json!("run:first-continuation");
    first
        .as_object_mut()
        .unwrap()
        .remove("restartContinuationOfRunId");
    cut["runs"].as_array_mut().unwrap().extend([second, first]);
    assert_eq!(continuation_texts(cut.clone()).await.len(), 1);

    cut["runs"][2]["restartContinuationOfRunId"] = json!("run:root");
    assert!(continuation_texts(cut).await.is_empty());
}

#[tokio::test]
async fn a_runtime_request_decides_whether_a_cut_run_continues() {
    // (kind, status, capability, continues)
    let rows = [
        ("user_input", "pending", "live", false),
        ("user_input", "expired", "not_resumable", false),
        ("user_input", "cancelled", "not_resumable", false),
        ("command", "pending", "live", false),
        ("command", "expired", "not_resumable", false),
        ("user_input", "resolved", "live", true),
        ("command", "cancelled", "live", true),
        ("user_input", "pending", "message", true),
        ("dynamic_tool_call", "pending", "live", true),
        ("auth_refresh", "pending", "live", true),
    ];
    for (kind, status, capability, continues) in rows {
        let capability_json = match capability {
            "live" => json!({"type": "live", "providerSessionId": SESSION}),
            "not_resumable" => json!({"type": "not_resumable", "reason": "The server restarted."}),
            _ => json!({"type": "message"}),
        };
        let requests = json!([{
            "id": "request:restart", "threadId": THREAD, "nodeId": "node:restart-question",
            "providerTurnId": "turn:restart", "nativeRequestRef": null, "kind": kind,
            "status": status, "responseCapability": capability_json,
            "createdAt": T0, "resolvedAt": null,
        }]);
        let mut live = base();
        live["runtimeRequests"] = requests.clone();
        let live: OrchestrationV2ThreadProjection = serde_json::from_value(live).unwrap();
        assert_eq!(
            crate::core::restart_continuation::restart_continuation_run(&live, chrono::Utc::now())
                .is_some(),
            continues,
            "{kind} {status} {capability}"
        );
        let mut cut = cut_mid_turn(json!({}));
        cut["runtimeRequests"] = requests;
        assert_eq!(
            continuation_texts(cut).await.len(),
            usize::from(continues),
            "{kind} {status} {capability}"
        );
    }
}

#[tokio::test]
async fn does_not_wake_a_snoozed_or_settled_thread_but_continues_once_a_snooze_elapsed() {
    for (field, value) in [
        ("snoozedUntil", json!("2099-01-01T00:00:00.000Z")),
        ("settledOverride", json!("settled")),
    ] {
        let mut cut = cut_mid_turn(json!({}));
        cut["thread"][field] = value;
        assert!(continuation_texts(cut).await.is_empty(), "{field}");
    }
    let mut cut = cut_mid_turn(json!({}));
    cut["thread"]["snoozedUntil"] = json!("2000-01-01T00:00:00.000Z");
    assert_eq!(continuation_texts(cut).await.len(), 1);
}

#[tokio::test]
async fn does_not_continue_a_run_waiting_on_a_secret_the_restart_closed() {
    let completed_at = "2026-10-03T10:05:00.000Z";
    let secret = |status: &str, at: &str| {
        let mut item = turn_item(
            THREAD,
            "turn-item:secret",
            Some(RUN),
            1,
            "secret_request",
            "cancelled",
            json!({"secretStatus": status, "label": "GitHub token", "reason": "Used as GH_TOKEN."}),
        );
        item["completedAt"] = json!(at);
        item
    };
    let mut cut = cut_mid_turn(json!({"completedAt": completed_at}));
    cut["turnItems"] = json!([secret("cancelled", completed_at)]);
    assert!(continuation_texts(cut.clone()).await.is_empty());
    // A secret the user dismissed earlier in the run does not hold it.
    cut["turnItems"] = json!([secret("cancelled", "2026-10-03T10:04:59.000Z")]);
    assert_eq!(continuation_texts(cut).await.len(), 1);
}

#[tokio::test]
async fn does_not_continue_when_the_provider_turn_ended_before_the_app_run_settled() {
    for status in ["completed", "failed", "interrupted"] {
        let mut cut = cut_mid_turn(json!({}));
        cut["providerTurns"][0]["status"] = json!(status);
        assert!(continuation_texts(cut).await.is_empty(), "{status}");
    }
}

// --- Own cases: the effect's contract with the delegated-task settle ---

#[tokio::test]
async fn settles_a_delegated_task_after_it_continues_and_after_it_declines() {
    let harness_a = harness(cut_mid_turn(json!({}))).await;
    continue_run(&harness_a).await;
    assert_eq!(harness_a.threads.dispatches().len(), 1);
    assert_eq!(
        *harness_a.threads.recovered.lock(),
        vec![(THREAD.to_owned(), RUN.to_owned())]
    );

    // Opted out: no turn resumes, and the settle still runs.
    let harness_b = harness_with(cut_mid_turn(json!({})), false).await;
    continue_run(&harness_b).await;
    assert!(harness_b.threads.dispatches().is_empty());
    assert_eq!(harness_b.threads.recovered.lock().len(), 1);
}

#[tokio::test]
async fn a_disabled_provider_instance_declines_but_still_settles() {
    let harness = harness(cut_mid_turn(json!({}))).await;
    *harness.threads.failure.lock() = Some(DispatchFailure::InstanceDisabled);

    continue_run(&harness).await;

    assert!(harness.threads.dispatches().is_empty());
    assert_eq!(harness.threads.recovered.lock().len(), 1);
}

#[tokio::test]
async fn any_other_dispatch_failure_fails_the_effect_without_settling() {
    let harness = harness(cut_mid_turn(json!({}))).await;
    *harness.threads.failure.lock() = Some(DispatchFailure::Other);

    let result = harness
        .service
        .continue_restarted_run(&thread_id(), &run_id(RUN))
        .await;

    assert!(result.is_err());
    // The effect worker retries, and settles after its last attempt.
    assert!(harness.threads.recovered.lock().is_empty());
}

// --- Own cases: the restart reads the orchestrator answers ---

async fn queue_continuation(harness: &Harness, id: &str) {
    harness
        .outbox
        .enqueue(vec![PendingEffect {
            id: format!("effect:restart-continuation:{id}"),
            command_id: turin_contracts::CommandId::new(format!("command:effect:{id}")).unwrap(),
            thread_id: thread_id(),
            request: EffectRequest::ProviderRuntimeContinue {
                source_run_id: run_id(id),
            },
            available_at: None,
        }])
        .await
        .expect("enqueue");
}

fn decoded_run(value: Value) -> OrchestrationV2Run {
    serde_json::from_value(value).expect("run")
}

#[tokio::test]
async fn a_run_awaits_its_continuation_while_the_effect_is_pending() {
    let harness = harness(base()).await;
    let cut = decoded_run(run_json(RUN, 1, "cancelled"));
    assert!(!harness.service.restart_continuation_pending(&cut).await);

    queue_continuation(&harness, RUN).await;
    assert!(harness.service.restart_continuation_pending(&cut).await);

    let claimed = harness
        .outbox
        .claim_next("worker".to_owned(), 60_000, false)
        .await
        .unwrap()
        .expect("claimed");
    harness
        .outbox
        .succeed(claimed.id, "worker".to_owned())
        .await
        .unwrap();
    assert!(!harness.service.restart_continuation_pending(&cut).await);
    // Already settled: waiting returns at once.
    harness.service.await_restart_continuation(&cut).await;
}

#[tokio::test]
async fn awaiting_a_continuation_wakes_when_the_effect_settles() {
    let harness = Arc::new(harness(base()).await);
    queue_continuation(&harness, RUN).await;
    let cut = decoded_run(run_json(RUN, 1, "cancelled"));
    let waiter = {
        let harness = harness.clone();
        let cut = cut.clone();
        tokio::spawn(async move { harness.service.await_restart_continuation(&cut).await })
    };
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    let claimed = harness
        .outbox
        .claim_next("worker".to_owned(), 60_000, false)
        .await
        .unwrap()
        .expect("claimed");
    harness
        .outbox
        .succeed(claimed.id, "worker".to_owned())
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .expect("woken")
        .expect("joined");
}

/// The child projection `delegated_task_result_pending` reads is the real
/// store's, so the child thread and its runs are seeded there.
async fn child_harness() -> (Harness, Arc<ProjectionStore>) {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let store = Arc::new(ProjectionStore::new(db.clone()));
    let outbox = EffectOutbox::new(db);
    let delegated = Arc::new(Delegated {
        swept: Mutex::default(),
        settled: Mutex::default(),
    });
    let threads = FakeThreads::new(base());
    let harness = Harness {
        service: RestartContinuationService::new(
            Arc::new(StaticSettings(default_server_settings())),
            threads.clone(),
            store.clone(),
            outbox.clone(),
            delegated.clone(),
        ),
        threads,
        delegated,
        outbox,
    };
    (harness, store)
}

#[tokio::test]
async fn a_delegated_result_is_held_while_a_continuation_of_its_run_is_pending() {
    let (harness, store) = child_harness().await;
    let child = "thread:delegated-child";
    store
        .apply(test_support::event(
            "thread.created",
            child,
            T0,
            thread(child),
        ))
        .await
        .unwrap();
    let mut finished = run(child, "run:child", 1, "interrupted");
    finished["delegatedTaskId"] = json!("task:child");
    finished["completedAt"] = json!(T0);
    store
        .apply(test_support::event("run.created", child, T0, finished))
        .await
        .unwrap();
    let child_id = ThreadId::new(child).unwrap();
    let task = NodeId::new("task:child").unwrap();

    // No continuation is queued: the result is final.
    assert!(
        !harness
            .service
            .delegated_task_result_pending(&child_id, Some(&task))
            .await
            .unwrap()
    );
    assert!(
        !harness
            .service
            .delegated_task_result_pending(&child_id, None)
            .await
            .unwrap()
    );

    // A restart continuation of the result run is pending: it is not.
    queue_continuation(&harness, "run:child").await;
    assert!(
        harness
            .service
            .delegated_task_result_pending(&child_id, Some(&task))
            .await
            .unwrap()
    );
    assert!(
        harness
            .service
            .delegated_task_result_pending(&child_id, None)
            .await
            .unwrap()
    );

    // A task with no run yet has no result.
    let other = NodeId::new("task:other").unwrap();
    assert!(
        harness
            .service
            .delegated_task_result_pending(&child_id, Some(&other))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_delegated_result_is_not_final_while_the_child_still_works() {
    let (harness, store) = child_harness().await;
    let child = "thread:delegated-working";
    store
        .apply(test_support::event(
            "thread.created",
            child,
            T0,
            thread(child),
        ))
        .await
        .unwrap();
    let mut working = run(child, "run:child-working", 1, "running");
    working["delegatedTaskId"] = json!("task:working");
    store
        .apply(test_support::event("run.created", child, T0, working))
        .await
        .unwrap();
    let child_id = ThreadId::new(child).unwrap();

    assert!(
        harness
            .service
            .delegated_task_result_pending(&child_id, None)
            .await
            .unwrap()
    );
    assert!(
        harness
            .service
            .delegated_task_result_pending(&child_id, Some(&NodeId::new("task:working").unwrap()))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn the_delegated_task_sweep_and_settle_go_to_the_orchestrators_finalizer() {
    let harness = harness(base()).await;
    harness.service.recover_delegated_tasks().await;
    harness
        .service
        .recover_delegated_task(&thread_id(), &run_id(RUN))
        .await;
    assert_eq!(*harness.delegated.swept.lock(), 1);
    assert_eq!(
        *harness.delegated.settled.lock(),
        vec![(THREAD.to_owned(), RUN.to_owned())]
    );
}
