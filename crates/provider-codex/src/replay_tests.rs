//! Replays upstream Codex fixtures through the `ProviderSessionRuntime`
//! boundary, so assertions are on `ProviderAdapterV2Event`s the way upstream's
//! `CodexReplayFixtures.integration.test.ts` checks orchestration output.
//! Outbound frames are checked by the replay driver; the turn counts and
//! approval counts mirror `scenarioExpectations` there.
use crate::{orchestration_adapter::CodexProviderAdapter, runtime::CodexRuntimeConfig};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use turin_codex_app_server::replay::{
    ReplayDriver, ReplayEntry, ReplayTranscript, make_replay_client,
};
use turin_contracts::{
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2CreationSource,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
        OrchestrationV2TurnItem,
    },
    provider_instance::ProviderInstanceId,
    provider_policy::{ProviderInteractionMode, ProviderUserInputAnswers, RuntimeMode},
};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Event,
    ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
    ProviderAdapterV2RollbackTarget, ProviderAdapterV2RollbackThreadInput,
    ProviderAdapterV2RuntimePolicy, ProviderAdapterV2RuntimeRequestResponseInput,
    ProviderAdapterV2SteerInput, ProviderAdapterV2TurnInput, ProviderAdapterV2TurnMessage,
    ProviderAdapterV2TurnTerminalStatus, ProviderEventStream, ProviderSessionRuntime,
};

const CLIENT_VERSION: &str = "0.0.44";

/// Loads a transcript from the pinned upstream fixture tree. Turin cuts MCP
/// apps, so the initialize capability extension is removed from the expected
/// frame, and the recorder's cwd is pinned to the workspace the replay uses.
fn fixture(name: &str) -> ReplayTranscript {
    let path = format!(
        "../../upstream/apps/server/src/orchestration-v2/testkit/fixtures/{name}/codex_transcript.ndjson"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let head: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut entries: Vec<ReplayEntry> = lines
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for entry in &mut entries {
        if let ReplayEntry::ExpectOutbound { frame, .. } = entry {
            match frame.get("method").and_then(Value::as_str) {
                Some("initialize") => {
                    frame
                        .pointer_mut("/params/capabilities")
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .remove("extensions");
                }
                Some("turn/start") => frame["params"]["cwd"] = json!("/workspace"),
                _ => {}
            }
        }
    }
    ReplayTranscript {
        provider: "codex".into(),
        protocol: "codex.app-server".into(),
        version: head["version"].as_str().unwrap().into(),
        scenario: name.into(),
        metadata: head.get("metadata").cloned(),
        entries,
    }
}

/// `params` of the nth outbound `turn/start` the upstream recorder sent.
fn recorded_turn_params(transcript: &ReplayTranscript, nth: usize) -> Value {
    transcript
        .entries
        .iter()
        .filter_map(|entry| match entry {
            ReplayEntry::ExpectOutbound { frame, .. }
                if frame.get("method").and_then(Value::as_str) == Some("turn/start") =>
            {
                frame.get("params").cloned()
            }
            _ => None,
        })
        .nth(nth)
        .expect("recorded turn/start")
}

fn id<T: DeserializeOwned>(value: &str) -> T {
    serde_json::from_value(json!(value)).expect("typed id")
}

fn selection(model: &str) -> ModelSelection {
    serde_json::from_value(json!({"instanceId": "codex", "model": model})).expect("selection")
}

fn policy(
    runtime_mode: RuntimeMode,
    interaction_mode: ProviderInteractionMode,
    approval_policy: Option<Value>,
    sandbox_policy: Option<Value>,
) -> ProviderAdapterV2RuntimePolicy {
    ProviderAdapterV2RuntimePolicy {
        runtime_mode,
        interaction_mode,
        cwd: Some("/workspace".into()),
        approval_policy,
        sandbox_policy,
        reasoning_effort: None,
    }
}

fn app_thread(model: &str) -> OrchestrationV2AppThread {
    serde_json::from_value(json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": "thread-1",
        "projectId": "project-1",
        "title": "fixture",
        "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": model},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "activeProviderThreadId": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1"},
        "forkedFrom": null,
        "createdAt": "2026-10-15T00:00:00.000Z",
        "updatedAt": "2026-10-15T00:00:00.000Z",
        "archivedAt": null,
    }))
    .expect("app thread fixture")
}

struct Replay {
    model: String,
    runtime: Arc<dyn ProviderSessionRuntime>,
    events: ProviderEventStream,
    driver: ReplayDriver,
    thread: OrchestrationV2ProviderThread,
    runs: usize,
}

async fn open(transcript: ReplayTranscript) -> Replay {
    // The replay runs on the model the upstream recorder used for this scenario.
    let model = recorded_turn_params(&transcript, 0)["model"]
        .as_str()
        .expect("recorded model")
        .to_owned();
    let (client, driver) = make_replay_client(transcript);
    let config = CodexRuntimeConfig {
        binary: "codex".into(),
        cwd: "/workspace".into(),
        home_path: None,
        launch_args: vec![],
        environment: BTreeMap::new(),
        client_version: CLIENT_VERSION.into(),
    };
    let adapter = CodexProviderAdapter::with_client(
        ProviderInstanceId::new("codex").unwrap(),
        config,
        client,
    );
    let runtime = adapter
        .open_session(ProviderAdapterV2OpenSessionInput {
            thread_id: id("thread-1"),
            provider_session_id: id("session-1"),
            model_selection: selection(&model),
            runtime_policy: policy(
                RuntimeMode::FullAccess,
                ProviderInteractionMode::Default,
                None,
                None,
            ),
            resume_from_session: None,
            initial_native_thread_id: None,
            initial_provider_item_identity_version: None,
        })
        .await
        .unwrap();
    let events = runtime.events();
    let thread = runtime
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: id("thread-1"),
            title: None,
            model_selection: selection(&model),
            runtime_policy: policy(
                RuntimeMode::FullAccess,
                ProviderInteractionMode::Default,
                None,
                None,
            ),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    Replay {
        model,
        runtime,
        events,
        driver,
        thread,
        runs: 0,
    }
}

impl Replay {
    fn turn_input(
        &mut self,
        text: &str,
        policy: ProviderAdapterV2RuntimePolicy,
    ) -> ProviderAdapterV2TurnInput {
        self.runs += 1;
        let run = self.runs;
        ProviderAdapterV2TurnInput {
            app_thread: app_thread(&self.model),
            thread_id: id("thread-1"),
            run_id: id(&format!("run-{run}")),
            run_ordinal: run as i64,
            native_thread_has_turns: None,
            provider_turn_ordinal: run as i64,
            restart_continuation_of_run_id: None,
            attempt_id: id(&format!("attempt-{run}")),
            root_node_id: id(&format!("root-{run}")),
            provider_thread: self.thread.clone(),
            message: ProviderAdapterV2TurnMessage {
                message_id: id(&format!("message-{run}")),
                text: text.into(),
                attachments: vec![],
                created_by: OrchestrationV2Actor::User,
                creation_source: OrchestrationV2CreationSource::Web,
                scheduled_task_id: None,
                sender_thread_id: None,
            },
            model_selection: selection(&self.model),
            runtime_policy: policy,
            app_context: None,
        }
    }

    async fn start(&mut self, text: &str, policy: ProviderAdapterV2RuntimePolicy) {
        let input = self.turn_input(text, policy);
        self.runtime.start_turn(input).await.unwrap();
    }

    async fn next_until(
        &mut self,
        done: impl Fn(&ProviderAdapterV2Event) -> bool,
    ) -> Vec<ProviderAdapterV2Event> {
        let mut seen = Vec::new();
        loop {
            let event = self
                .events
                .next()
                .await
                .expect("event stream ended")
                .expect("event error");
            let stop = done(&event);
            seen.push(event);
            if stop {
                return seen;
            }
        }
    }

    /// Consumes events through the next terminal turn and returns its status.
    async fn finish_turn(&mut self) -> ProviderAdapterV2TurnTerminalStatus {
        let seen = self.next_until(is_terminal).await;
        terminal_status(&seen)
    }
}

fn is_terminal(event: &ProviderAdapterV2Event) -> bool {
    matches!(event, ProviderAdapterV2Event::TurnTerminal { .. })
}

fn terminal_status(events: &[ProviderAdapterV2Event]) -> ProviderAdapterV2TurnTerminalStatus {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            ProviderAdapterV2Event::TurnTerminal { status, .. } => Some(*status),
            _ => None,
        })
        .expect("terminal event")
}

fn assistant_texts(events: &[ProviderAdapterV2Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::TurnItemUpdated {
                turn_item: OrchestrationV2TurnItem::AssistantMessage { text, .. },
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn runtime_requests(
    events: &[ProviderAdapterV2Event],
) -> Vec<turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequest> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::RuntimeRequestUpdated {
                runtime_request, ..
            } if runtime_request.status == OrchestrationV2RuntimeRequestStatus::Pending => {
                Some(runtime_request.clone())
            }
            _ => None,
        })
        .collect()
}

fn read_only_policy(transcript: &ReplayTranscript) -> ProviderAdapterV2RuntimePolicy {
    let params = recorded_turn_params(transcript, 0);
    policy(
        RuntimeMode::ApprovalRequired,
        ProviderInteractionMode::Default,
        Some(json!("on-request")),
        Some(params["sandboxPolicy"].clone()),
    )
}

#[tokio::test]
async fn simple_replay_emits_assistant_text_and_completes() {
    let transcript = fixture("simple");
    let policy = policy(
        RuntimeMode::FullAccess,
        ProviderInteractionMode::Default,
        None,
        None,
    );
    let mut replay = open(transcript).await;
    replay
        .start("Respond with the following text: fixture simple ok", policy)
        .await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    assert_eq!(assistant_texts(&seen), vec!["fixture simple ok".to_owned()]);
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn multi_turn_replay_completes_two_turns() {
    let mut replay = open(fixture("multi_turn")).await;
    let policy = policy(
        RuntimeMode::FullAccess,
        ProviderInteractionMode::Default,
        None,
        None,
    );
    replay
        .start(
            "Respond with exactly: first fixture turn complete",
            policy.clone(),
        )
        .await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay
        .start("Respond with exactly: second fixture turn complete", policy)
        .await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn steering_replay_accepts_steer_before_completion() {
    let mut replay = open(fixture("message_steering")).await;
    let policy = policy(
        RuntimeMode::FullAccess,
        ProviderInteractionMode::Default,
        None,
        None,
    );
    replay
        .start(
            "Respond with exactly: steering fixture initial response",
            policy,
        )
        .await;
    let started = replay.next_until(is_turn_running).await;
    let native_turn = started
        .iter()
        .find_map(|event| match event {
            ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } => {
                Some(provider_turn.id.clone())
            }
            _ => None,
        })
        .expect("provider turn");
    replay
        .runtime
        .steer_turn(ProviderAdapterV2SteerInput {
            thread_id: id("thread-1"),
            run_id: id("run-1"),
            provider_thread: replay.thread.clone(),
            provider_turn_id: native_turn,
            message: ProviderAdapterV2TurnMessage {
                message_id: id("message-steer"),
                text: "Actually, respond with exactly: steering fixture observed".into(),
                attachments: vec![],
                created_by: OrchestrationV2Actor::User,
                creation_source: OrchestrationV2CreationSource::Web,
                scheduled_task_id: None,
                sender_thread_id: None,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn interrupt_replay_terminates_turn_as_interrupted() {
    let mut replay = open(fixture("turn_interrupt")).await;
    let policy = policy(
        RuntimeMode::FullAccess,
        ProviderInteractionMode::Default,
        None,
        Some(json!({"networkAccess": false, "type": "workspaceWrite", "writableRoots": []})),
    );
    replay
        .start(
            "Do not answer immediately. First run the local shell command `sleep 30`, then respond with exactly: interrupt fixture should not finish naturally.",
            policy,
        )
        .await;
    let started = replay.next_until(is_turn_running).await;
    let native_turn = provider_turn_of(&started);
    replay
        .runtime
        .interrupt_turn(ProviderAdapterV2InterruptInput {
            provider_thread: replay.thread.clone(),
            provider_turn_id: native_turn,
            request_runtime_restart: None,
        })
        .await
        .unwrap();
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Interrupted
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn read_only_approval_replay_requests_and_accepts_command_approval() {
    let transcript = fixture("tool_call_read_only_on_request");
    let policy = read_only_policy(&transcript);
    let text = recorded_turn_params(&transcript, 0)["input"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut replay = open(transcript).await;
    replay.start(&text, policy).await;
    let seen = replay
        .next_until(|event| !runtime_requests(std::slice::from_ref(event)).is_empty())
        .await;
    let request = runtime_requests(&seen).remove(0);
    assert_eq!(request.kind, OrchestrationV2RuntimeRequestKind::Command);
    replay
        .runtime
        .respond_to_runtime_request(accept(&request))
        .await
        .unwrap();
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

/// Policy with the approval and sandbox settings the upstream recorder sent, so
/// the replay's outbound frame check sees the same `turn/start`.
fn recorded_policy(
    transcript: &ReplayTranscript,
    runtime_mode: RuntimeMode,
) -> ProviderAdapterV2RuntimePolicy {
    let params = recorded_turn_params(transcript, 0);
    policy(
        runtime_mode,
        ProviderInteractionMode::Default,
        params.get("approvalPolicy").cloned(),
        params.get("sandboxPolicy").cloned(),
    )
}

fn recorded_text(transcript: &ReplayTranscript, nth: usize) -> String {
    recorded_turn_params(transcript, nth)["input"][0]["text"]
        .as_str()
        .expect("recorded text")
        .to_owned()
}

fn last_provider_turn(events: &[ProviderAdapterV2Event]) -> OrchestrationV2ProviderTurn {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } => {
                Some(provider_turn.clone())
            }
            _ => None,
        })
        .expect("provider turn update")
}

#[tokio::test]
async fn thread_rollback_replay_reverts_later_turn_and_continues() {
    let transcript = fixture("thread_rollback");
    let texts: Vec<String> = (0..3).map(|n| recorded_text(&transcript, n)).collect();
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript).await;
    let mut turns = Vec::new();
    for text in &texts[..2] {
        replay.start(text, policy.clone()).await;
        let seen = replay.next_until(is_terminal).await;
        turns.push(last_provider_turn(&seen));
    }
    let snapshot = replay
        .runtime
        .rollback_thread(ProviderAdapterV2RollbackThreadInput {
            provider_thread: replay.thread.clone(),
            target: ProviderAdapterV2RollbackTarget::ProviderTurn {
                checkpoint_id: id("checkpoint-1"),
                app_run_ordinal: 1,
                provider_turn: turns[0].clone(),
            },
            provider_thread_turns: turns,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.provider_thread.id, replay.thread.id);
    replay.start(&texts[2], policy).await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn queued_turn_replay_runs_turns_in_order() {
    let transcript = fixture("queued_turn");
    let texts = [recorded_text(&transcript, 0), recorded_text(&transcript, 1)];
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript).await;
    for text in &texts {
        replay.start(text, policy.clone()).await;
        assert_eq!(
            replay.finish_turn().await,
            ProviderAdapterV2TurnTerminalStatus::Completed
        );
    }
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn workspace_never_replay_completes_without_approval() {
    let transcript = fixture("tool_call_workspace_never");
    let text = recorded_text(&transcript, 0);
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript).await;
    replay.start(&text, policy).await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    assert!(runtime_requests(&seen).is_empty());
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn restricted_granular_replay_requests_and_accepts_file_change_approval() {
    let transcript = fixture("tool_call_restricted_granular");
    let text = recorded_text(&transcript, 0);
    let policy = recorded_policy(&transcript, RuntimeMode::ApprovalRequired);
    let mut replay = open(transcript).await;
    replay.start(&text, policy).await;
    let seen = replay
        .next_until(|event| !runtime_requests(std::slice::from_ref(event)).is_empty())
        .await;
    let request = runtime_requests(&seen).remove(0);
    assert_eq!(request.kind, OrchestrationV2RuntimeRequestKind::FileChange);
    replay
        .runtime
        .respond_to_runtime_request(accept(&request))
        .await
        .unwrap();
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn plan_questions_replay_answers_user_input_request() {
    let transcript = fixture("plan_questions");
    let params = recorded_turn_params(&transcript, 0);
    let mut replay = open(transcript).await;
    let policy = policy(
        RuntimeMode::FullAccess,
        ProviderInteractionMode::Plan,
        None,
        Some(params["sandboxPolicy"].clone()),
    );
    replay
        .start(params["input"][0]["text"].as_str().unwrap(), policy)
        .await;
    let seen = replay
        .next_until(|event| !runtime_requests(std::slice::from_ref(event)).is_empty())
        .await;
    let request = runtime_requests(&seen).remove(0);
    assert_eq!(request.kind, OrchestrationV2RuntimeRequestKind::UserInput);
    let answers: ProviderUserInputAnswers =
        serde_json::from_value(json!({"schema_preference": "Strict schemas (Recommended)"}))
            .unwrap();
    replay
        .runtime
        .respond_to_runtime_request(ProviderAdapterV2RuntimeRequestResponseInput {
            request_id: request.id.clone(),
            decision: None,
            answers: Some(answers),
            response: None,
        })
        .await
        .unwrap();
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

fn accept(request: &OrchestrationV2RuntimeRequest) -> ProviderAdapterV2RuntimeRequestResponseInput {
    ProviderAdapterV2RuntimeRequestResponseInput {
        request_id: request.id.clone(),
        decision: Some(turin_contracts::provider_policy::ProviderApprovalDecision::Accept),
        answers: None,
        response: None,
    }
}

fn is_turn_running(event: &ProviderAdapterV2Event) -> bool {
    matches!(
        event,
        ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. }
            if provider_turn.status
                == turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Running
    )
}

fn provider_turn_of(events: &[ProviderAdapterV2Event]) -> turin_contracts::ProviderTurnId {
    events
        .iter()
        .find_map(|event| match event {
            ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } => {
                Some(provider_turn.id.clone())
            }
            _ => None,
        })
        .expect("provider turn")
}
