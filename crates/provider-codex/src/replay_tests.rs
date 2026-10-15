//! Replays upstream Codex fixtures through the `ProviderSessionRuntime`
//! boundary, so assertions are on `ProviderAdapterV2Event`s the way upstream's
//! `CodexReplayFixtures.integration.test.ts` checks orchestration output.
//! Outbound frames are checked by the replay driver; the turn counts and
//! approval counts mirror `scenarioExpectations` there.
use crate::{
    adapter::CodexEvent,
    orchestration_adapter::CodexProviderAdapter,
    projection::{RuntimeState, TurnState, project_event},
    runtime::CodexRuntimeConfig,
};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use turin_codex_app_server::replay::{
    ReplayDriver, ReplayEntry, ReplayTranscript, make_replay_client,
};
use turin_contracts::{
    NodeId,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2CreationSource,
        OrchestrationV2ExecutionNodeKind, OrchestrationV2ExecutionNodeStatus,
        OrchestrationV2PlanArtifact, OrchestrationV2PlanArtifactStatus,
        OrchestrationV2PlanStepStatus, OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn,
        OrchestrationV2RuntimeRequest, OrchestrationV2RuntimeRequestKind,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2Subagent,
        OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
    provider_instance::ProviderInstanceId,
    provider_policy::{ProviderInteractionMode, ProviderUserInputAnswers, RuntimeMode},
};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Event,
    ProviderAdapterV2ForkThreadInput, ProviderAdapterV2InterruptInput,
    ProviderAdapterV2OpenSessionInput, ProviderAdapterV2ResumeThreadInput,
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
    let (model, runtime, events, driver) = open_session(transcript).await;
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

/// A second session on the rest of a transcript, after the first session's runtime
/// exited. It reattaches to the first session's native thread instead of starting one.
async fn open_restarted(transcript: ReplayTranscript, first: &Replay) -> Replay {
    let (model, runtime, events, driver) = open_session(transcript).await;
    let thread = runtime
        .resume_thread(ProviderAdapterV2ResumeThreadInput {
            provider_thread: first.thread.clone(),
            thread_id: Some(id("thread-1")),
            model_selection: Some(selection(&model)),
            runtime_policy: Some(policy(
                RuntimeMode::FullAccess,
                ProviderInteractionMode::Default,
                None,
                None,
            )),
        })
        .await
        .unwrap();
    Replay {
        model,
        runtime,
        events,
        driver,
        thread,
        runs: first.runs,
    }
}

async fn open_session(
    transcript: ReplayTranscript,
) -> (
    String,
    Arc<dyn ProviderSessionRuntime>,
    ProviderEventStream,
    ReplayDriver,
) {
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
    (model, runtime, events, driver)
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

/// Completed assistant texts only: streaming updates are partial by design.
fn assistant_texts(events: &[ProviderAdapterV2Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::TurnItemUpdated {
                turn_item:
                    OrchestrationV2TurnItem::AssistantMessage {
                        text,
                        status: OrchestrationV2TurnItemStatus::Completed,
                        ..
                    },
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn node_kinds(events: &[ProviderAdapterV2Event]) -> Vec<OrchestrationV2ExecutionNodeKind> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::NodeUpdated { node, .. } => Some(node.kind),
            _ => None,
        })
        .collect()
}

fn turn_items(events: &[ProviderAdapterV2Event]) -> Vec<&OrchestrationV2TurnItem> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => Some(turn_item),
            _ => None,
        })
        .collect()
}

fn plan_artifacts(events: &[ProviderAdapterV2Event]) -> Vec<&OrchestrationV2PlanArtifact> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::PlanUpdated { plan, .. } => Some(plan),
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

/// The recorded `fileChange` item (one `add`) maps to a completed `file_change`
/// turn item: first change's path and diff, with its tool-call node completed.
#[tokio::test]
async fn file_change_replay_maps_first_change_to_file_change_item() {
    let transcript = fixture("tool_call_restricted_granular");
    let text = recorded_text(&transcript, 0);
    let policy = recorded_policy(&transcript, RuntimeMode::ApprovalRequired);
    let mut replay = open(transcript).await;
    replay.start(&text, policy).await;
    let before = replay
        .next_until(|event| !runtime_requests(std::slice::from_ref(event)).is_empty())
        .await;
    let request = runtime_requests(&before).remove(0);
    replay
        .runtime
        .respond_to_runtime_request(accept(&request))
        .await
        .unwrap();
    let after = replay.next_until(is_terminal).await;
    let (file_name, diff_str, status, node_id) = turn_items(&after)
        .into_iter()
        .find_map(|item| match item {
            OrchestrationV2TurnItem::FileChange {
                file_name,
                diff_str,
                status,
                node_id,
                ..
            } => Some((
                file_name.clone(),
                diff_str.clone(),
                *status,
                node_id.clone(),
            )),
            _ => None,
        })
        .expect("file change item");
    assert_eq!(
        file_name.as_str(),
        "/home/replay-user/t3code/packages/effect-codex-app-server/.codex-probe-write-action.txt"
    );
    assert_eq!(
        diff_str.as_deref(),
        Some("codex app-server approval fixture.\n")
    );
    assert_eq!(status, OrchestrationV2TurnItemStatus::Completed);
    assert!(after.iter().any(|event| matches!(
        event,
        ProviderAdapterV2Event::NodeUpdated { node, .. }
            if Some(&node.id) == node_id.as_ref()
                && node.kind == OrchestrationV2ExecutionNodeKind::ToolCall
                && node.status == OrchestrationV2ExecutionNodeStatus::Completed
    )));
    assert_eq!(
        terminal_status(&after),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

/// Restart: the first session ends with `runtime_exit` after one turn. A second
/// session runs `thread/resume` with `excludeTurns` on the same native thread,
/// then a second turn on it.
#[tokio::test]
async fn provider_thread_resume_replays_second_turn_after_restart() {
    let transcript = fixture("provider_thread_resume");
    // Entry 32 is the first session's `runtime_exit`; entry 33 starts the second.
    let mut first_transcript = transcript.clone();
    let second_entries = first_transcript.entries.split_off(33);
    let mut second_transcript = transcript.clone();
    second_transcript.entries = second_entries;

    let first_text = recorded_text(&transcript, 0);
    let second_text = recorded_text(&transcript, 1);
    let mut first = open(first_transcript).await;
    first
        .start(
            &first_text,
            recorded_policy(&transcript, RuntimeMode::FullAccess),
        )
        .await;
    assert_eq!(
        first.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    let mut second = open_restarted(second_transcript, &first).await;
    second
        .start(
            &second_text,
            recorded_policy(&transcript, RuntimeMode::FullAccess),
        )
        .await;
    let seen = second.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    assert!(!assistant_texts(&seen).is_empty());
    assert_eq!(
        second.thread.native_thread_ref,
        first.thread.native_thread_ref
    );
    second.driver.verify_complete().unwrap();
}

/// Restart, then rollback. The restarted session reads the thread, resumes it because
/// it is not loaded, counts turns, and reverts. Upstream `ProviderSessionManager`
/// then reattaches the thread with `thread/resume` before the next turn.
#[tokio::test]
async fn thread_rollback_after_restart_reverts_then_continues() {
    let transcript = fixture("thread_rollback_after_restart");
    // Entry 45 is the first session's `runtime_exit`; entry 46 starts the restarted one.
    let mut first_transcript = transcript.clone();
    let second_entries = first_transcript.entries.split_off(46);
    let mut second_transcript = transcript.clone();
    second_transcript.entries = second_entries;

    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut first = open(first_transcript).await;
    let mut turns = Vec::new();
    for n in 0..2 {
        first
            .start(&recorded_text(&transcript, n), policy.clone())
            .await;
        turns.push(last_provider_turn(&first.next_until(is_terminal).await));
    }

    let (model, runtime, events, driver) = open_session(second_transcript).await;
    let mut second = Replay {
        model,
        runtime,
        events,
        driver,
        thread: first.thread.clone(),
        runs: first.runs,
    };
    let snapshot = second
        .runtime
        .rollback_thread(ProviderAdapterV2RollbackThreadInput {
            provider_thread: second.thread.clone(),
            target: ProviderAdapterV2RollbackTarget::ProviderTurn {
                checkpoint_id: id("checkpoint-1"),
                app_run_ordinal: 1,
                provider_turn: turns[0].clone(),
            },
            provider_thread_turns: turns,
        })
        .await
        .unwrap();
    second.thread = second
        .runtime
        .resume_thread(ProviderAdapterV2ResumeThreadInput {
            provider_thread: snapshot.provider_thread,
            thread_id: Some(id("thread-1")),
            model_selection: Some(selection(&second.model)),
            runtime_policy: Some(policy.clone()),
        })
        .await
        .unwrap();

    second.start(&recorded_text(&transcript, 2), policy).await;
    assert_eq!(
        second.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    second.driver.verify_complete().unwrap();
}

/// Rollback to a stopped turn. The boundary is the interrupted turn, so the count is
/// the one completed turn after it. `thread/turns/list` returns that turn, and
/// `thread/revert` names it as the cut.
#[tokio::test]
async fn thread_rollback_to_stopped_turn_reverts_turns_after_boundary() {
    let transcript = fixture("thread_rollback_to_stopped_turn");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    let mut turns = Vec::new();

    replay
        .start(&recorded_text(&transcript, 0), policy.clone())
        .await;
    turns.push(last_provider_turn(&replay.next_until(is_terminal).await));

    // The second turn runs a long command and is interrupted while it runs.
    replay
        .start(&recorded_text(&transcript, 1), policy.clone())
        .await;
    let native_turn = provider_turn_of(&replay.next_until(is_turn_running).await);
    replay
        .next_until(|event| {
            matches!(
                event,
                ProviderAdapterV2Event::TurnItemUpdated {
                    turn_item: OrchestrationV2TurnItem::CommandExecution {
                        status: OrchestrationV2TurnItemStatus::Running,
                        ..
                    },
                    ..
                }
            )
        })
        .await;
    replay
        .runtime
        .interrupt_turn(ProviderAdapterV2InterruptInput {
            provider_thread: replay.thread.clone(),
            provider_turn_id: native_turn,
            request_runtime_restart: None,
        })
        .await
        .unwrap();
    let stopped = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&stopped),
        ProviderAdapterV2TurnTerminalStatus::Interrupted
    );
    turns.push(last_provider_turn(&stopped));

    replay
        .start(&recorded_text(&transcript, 2), policy.clone())
        .await;
    turns.push(last_provider_turn(&replay.next_until(is_terminal).await));

    let snapshot = replay
        .runtime
        .rollback_thread(ProviderAdapterV2RollbackThreadInput {
            provider_thread: replay.thread.clone(),
            target: ProviderAdapterV2RollbackTarget::ProviderTurn {
                checkpoint_id: id("checkpoint-1"),
                app_run_ordinal: 2,
                provider_turn: turns[1].clone(),
            },
            provider_thread_turns: turns,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.provider_thread.id, replay.thread.id);

    replay.start(&recorded_text(&transcript, 3), policy).await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

/// Forks `replay`'s thread at `boundary`, as the orchestration does for a native fork.
async fn fork_at(
    replay: &Replay,
    turns: &[OrchestrationV2ProviderTurn],
    boundary: &OrchestrationV2ProviderTurn,
    target: &str,
    policy: &ProviderAdapterV2RuntimePolicy,
) -> OrchestrationV2ProviderThread {
    replay
        .runtime
        .fork_thread(ProviderAdapterV2ForkThreadInput {
            source_provider_thread: replay.thread.clone(),
            source_provider_turns: Some(turns.to_vec()),
            provider_turn_id: Some(boundary.id.clone()),
            target_thread_id: id(target),
            owner_node_id: None,
            model_selection: Some(selection(&replay.model)),
            runtime_policy: Some(policy.clone()),
        })
        .await
        .unwrap()
}

/// A fork at a native turn continues on its own thread. Both turns after the fork
/// run on the fork, and the source is not written to again.
#[tokio::test]
async fn thread_fork_native_continue_runs_turns_on_fork() {
    let transcript = fixture("thread_fork_native_continue");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay
        .start(&recorded_text(&transcript, 0), policy.clone())
        .await;
    let turns = vec![last_provider_turn(&replay.next_until(is_terminal).await)];

    replay.thread = fork_at(&replay, &turns, &turns[0], "thread-2", &policy).await;
    replay
        .start(&recorded_text(&transcript, 1), policy.clone())
        .await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.start(&recorded_text(&transcript, 2), policy).await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

/// Two forks from the same source turn are independent: each runs its own turn.
#[tokio::test]
async fn thread_fork_native_siblings_keep_forks_separate() {
    let transcript = fixture("thread_fork_native_siblings");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay
        .start(&recorded_text(&transcript, 0), policy.clone())
        .await;
    let turns = vec![last_provider_turn(&replay.next_until(is_terminal).await)];
    let source = replay.thread.clone();

    replay.thread = fork_at(&replay, &turns, &turns[0], "thread-2", &policy).await;
    replay
        .start(&recorded_text(&transcript, 1), policy.clone())
        .await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    replay.thread = source;
    replay.thread = fork_at(&replay, &turns, &turns[0], "thread-3", &policy).await;
    replay.start(&recorded_text(&transcript, 2), policy).await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.driver.verify_complete().unwrap();
}

/// Merge-back: the fork runs a turn, then the source runs the handoff text that
/// carries the fork's delta, then the source answers the final question.
#[tokio::test]
async fn thread_merge_back_continue_hands_fork_delta_to_source() {
    let transcript = fixture("thread_merge_back_continue");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay
        .start(&recorded_text(&transcript, 0), policy.clone())
        .await;
    let turns = vec![last_provider_turn(&replay.next_until(is_terminal).await)];
    let source = replay.thread.clone();

    replay.thread = fork_at(&replay, &turns, &turns[0], "thread-2", &policy).await;
    replay
        .start(&recorded_text(&transcript, 1), policy.clone())
        .await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    replay.thread = source;
    for n in 2..4 {
        replay
            .start(&recorded_text(&transcript, n), policy.clone())
            .await;
        assert_eq!(
            replay.finish_turn().await,
            ProviderAdapterV2TurnTerminalStatus::Completed
        );
    }
    replay.driver.verify_complete().unwrap();
}

/// Merge-back from two siblings: each fork's delta is handed back to the source in turn,
/// and the source answers with both markers.
#[tokio::test]
async fn thread_merge_back_siblings_hands_both_forks_back() {
    let transcript = fixture("thread_merge_back_siblings");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay
        .start(&recorded_text(&transcript, 0), policy.clone())
        .await;
    let turns = vec![last_provider_turn(&replay.next_until(is_terminal).await)];
    let source = replay.thread.clone();

    replay.thread = fork_at(&replay, &turns, &turns[0], "thread-2", &policy).await;
    replay
        .start(&recorded_text(&transcript, 1), policy.clone())
        .await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    replay.thread = source.clone();
    replay.thread = fork_at(&replay, &turns, &turns[0], "thread-3", &policy).await;
    replay
        .start(&recorded_text(&transcript, 2), policy.clone())
        .await;
    assert_eq!(
        replay.finish_turn().await,
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    replay.thread = source;
    for n in 3..6 {
        replay
            .start(&recorded_text(&transcript, n), policy.clone())
            .await;
        assert_eq!(
            replay.finish_turn().await,
            ProviderAdapterV2TurnTerminalStatus::Completed
        );
    }
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

#[tokio::test]
async fn todo_list_replay_tracks_plan_steps_and_command() {
    let transcript = fixture("todo_list");
    let text = recorded_text(&transcript, 0);
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript).await;
    replay.start(&text, policy).await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    assert_eq!(
        assistant_texts(&seen),
        vec!["todo list fixture complete".to_owned()]
    );
    let kinds = node_kinds(&seen);
    assert!(kinds.contains(&OrchestrationV2ExecutionNodeKind::TodoList));
    assert!(kinds.contains(&OrchestrationV2ExecutionNodeKind::ToolCall));
    let items = turn_items(&seen);
    assert!(
        items
            .iter()
            .any(|item| matches!(item, OrchestrationV2TurnItem::CommandExecution { .. }))
    );
    let todo = plan_artifacts(&seen)
        .into_iter()
        .rev()
        .find_map(|plan| match plan {
            OrchestrationV2PlanArtifact::TodoList { steps, .. } => Some(steps.clone()),
            _ => None,
        })
        .expect("todo list plan");
    assert_eq!(todo.len(), 3);
    assert!(
        todo.iter()
            .all(|step| step.status == OrchestrationV2PlanStepStatus::Completed)
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn web_search_replay_reports_completed_search_item() {
    let transcript = fixture("web_search");
    let text = recorded_text(&transcript, 0);
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript).await;
    replay.start(&text, policy).await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    assert_eq!(
        assistant_texts(&seen).last().map(String::as_str),
        Some("web search fixture complete")
    );
    assert!(runtime_requests(&seen).is_empty());
    let searches: Vec<_> = turn_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            OrchestrationV2TurnItem::WebSearch {
                status, patterns, ..
            } => Some((*status, patterns.clone().unwrap_or_default())),
            _ => None,
        })
        .collect();
    let (status, patterns) = searches.last().expect("web search item");
    assert_eq!(*status, OrchestrationV2TurnItemStatus::Completed);
    assert!(
        patterns
            .iter()
            .any(|p| p == "FIFA World Cup 2026 ticket prices official")
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn proposed_plan_replay_emits_active_plan_artifact() {
    let transcript = fixture("proposed_plan");
    let text = recorded_text(&transcript, 0);
    let params = recorded_turn_params(&transcript, 0);
    let mut replay = open(transcript).await;
    let policy = policy(
        RuntimeMode::FullAccess,
        ProviderInteractionMode::Plan,
        None,
        Some(params["sandboxPolicy"].clone()),
    );
    replay.start(&text, policy).await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    let proposed = plan_artifacts(&seen)
        .into_iter()
        .rev()
        .find_map(|plan| match plan {
            OrchestrationV2PlanArtifact::ProposedPlan {
                status, markdown, ..
            } => Some((*status, markdown.clone())),
            _ => None,
        })
        .expect("proposed plan");
    assert_eq!(proposed.0, OrchestrationV2PlanArtifactStatus::Active);
    assert!(proposed.1.contains("replay") && proposed.1.contains("fixture"));
    let finished = turn_items(&seen)
        .into_iter()
        .rev()
        .find_map(|item| match item {
            OrchestrationV2TurnItem::ProposedPlan {
                streaming, status, ..
            } => Some((*streaming, *status)),
            _ => None,
        });
    assert_eq!(
        finished,
        Some((false, OrchestrationV2TurnItemStatus::Completed))
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn mid_tool_interrupt_terminates_background_terminal() {
    let transcript = fixture("turn_interrupt_mid_tool");
    let text = recorded_text(&transcript, 0);
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript).await;
    replay.start(&text, policy).await;
    let started = replay.next_until(is_turn_running).await;
    let native_turn = provider_turn_of(&started);
    replay
        .next_until(|event| {
            matches!(
                event,
                ProviderAdapterV2Event::TurnItemUpdated {
                    turn_item: OrchestrationV2TurnItem::CommandExecution {
                        status: OrchestrationV2TurnItemStatus::Running,
                        ..
                    },
                    ..
                }
            )
        })
        .await;
    replay
        .runtime
        .interrupt_turn(ProviderAdapterV2InterruptInput {
            provider_thread: replay.thread.clone(),
            provider_turn_id: native_turn,
            request_runtime_restart: None,
        })
        .await
        .unwrap();
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Interrupted
    );
    assert!(turn_items(&seen).iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::CommandExecution {
            status: OrchestrationV2TurnItemStatus::Interrupted,
            ..
        }
    )));
    replay.driver.verify_complete().unwrap();
}

/// The latest state of each subagent the replay reported, in first-seen order.
fn subagents(events: &[ProviderAdapterV2Event]) -> Vec<OrchestrationV2Subagent> {
    let mut latest: Vec<OrchestrationV2Subagent> = vec![];
    for event in events {
        if let ProviderAdapterV2Event::SubagentUpdated { subagent, .. } = event {
            match latest.iter_mut().find(|known| known.id == subagent.id) {
                Some(slot) => *slot = subagent.clone(),
                None => latest.push(subagent.clone()),
            }
        }
    }
    latest
}

/// The latest state of each turn item, as JSON so a test can select by type and thread.
fn latest_items(events: &[ProviderAdapterV2Event]) -> Vec<Value> {
    let mut latest: Vec<Value> = vec![];
    for event in events {
        if let ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } = event {
            let item = serde_json::to_value(turn_item).expect("turn item json");
            match latest.iter_mut().find(|known| known["id"] == item["id"]) {
                Some(slot) => *slot = item,
                None => latest.push(item),
            }
        }
    }
    latest
}

/// Items of one thread, selected by type, in first-seen order.
fn items_of<'a>(items: &'a [Value], thread: &str, kind: &str) -> Vec<&'a Value> {
    items
        .iter()
        .filter(|item| item["threadId"] == thread && item["type"] == kind)
        .collect()
}

/// Whether the replay created an app thread with this id.
fn has_app_thread(events: &[ProviderAdapterV2Event], child: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ProviderAdapterV2Event::AppThreadCreated { app_thread, .. }
                if app_thread.id.as_str() == child
        )
    })
}

#[tokio::test]
async fn subagent_replay_runs_spawned_children_and_settles_their_results() {
    let transcript = fixture("subagent");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay.start(&recorded_text(&transcript, 0), policy).await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    let subagents = subagents(&seen);
    assert_eq!(subagents.len(), 2);
    for subagent in &subagents {
        assert_eq!(
            subagent.origin,
            OrchestrationV2SubagentOrigin::ProviderNative
        );
        assert_eq!(subagent.created_by, OrchestrationV2Actor::Agent);
        assert_eq!(subagent.driver.as_str(), "codex");
        assert_eq!(subagent.status, OrchestrationV2SubagentStatus::Completed);
        assert!(subagent.child_thread_id.is_some());
        assert!(subagent.native_task_ref.is_some());
        assert!(subagent.completed_at.is_some());
    }
    let result_for = |needle: &str| {
        subagents
            .iter()
            .find(|subagent| subagent.prompt.contains(needle))
            .and_then(|subagent| subagent.result.clone())
            .unwrap_or_default()
    };
    assert!(result_for("package.json").contains("effect-codex-app-server"));
    assert!(!result_for("package.json").contains("tsconfig.base.json"));
    assert!(result_for("tsconfig.json").contains("../../tsconfig.base.json"));

    let items = latest_items(&seen);
    let root = "thread-1";
    assert!(
        items_of(&items, root, "command_execution").is_empty(),
        "subagent commands stay in the child thread"
    );
    assert_eq!(items_of(&items, root, "subagent").len(), 2);
    for subagent in &subagents {
        let child = subagent.child_thread_id.as_ref().expect("child").as_str();
        assert!(has_app_thread(&seen, child), "child thread is created");
        assert!(
            items_of(&items, child, "user_message")
                .iter()
                .any(|item| item["text"] == subagent.prompt.as_str()),
            "child thread holds its prompt"
        );
        assert!(
            !items_of(&items, child, "command_execution").is_empty(),
            "child thread holds its command"
        );
        let head: String = subagent
            .result
            .iter()
            .flat_map(|r| r.chars())
            .take(40)
            .collect();
        assert!(
            items_of(&items, child, "assistant_message")
                .iter()
                .any(|item| item["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&head))),
            "child thread holds its answer"
        );
    }
    replay.driver.verify_complete().unwrap();
}

/// The app threads the replay created, as JSON, so a test can read lineage by name.
fn app_threads(events: &[ProviderAdapterV2Event]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::AppThreadCreated { app_thread, .. } => {
                serde_json::to_value(app_thread).ok()
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn subagent_v2_replay_maps_activity_to_one_child_with_its_answer() {
    let transcript = fixture("subagent_v2");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay.start(&recorded_text(&transcript, 0), policy).await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    let subagents = subagents(&seen);
    assert_eq!(subagents.len(), 1);
    let subagent = &subagents[0];
    assert_eq!(
        subagent.origin,
        OrchestrationV2SubagentOrigin::ProviderNative
    );
    assert_eq!(subagent.created_by, OrchestrationV2Actor::Agent);
    assert_eq!(subagent.driver.as_str(), "codex");
    assert_eq!(subagent.title.as_deref(), Some("/root/say_hello"));
    assert_eq!(subagent.status, OrchestrationV2SubagentStatus::Completed);
    assert_eq!(subagent.result.as_deref(), Some("Hello."));
    assert!(subagent.provider_thread_id.is_some());
    assert!(subagent.native_task_ref.is_some());
    assert!(subagent.completed_at.is_some());

    let items = latest_items(&seen);
    let root = "thread-1";
    let parent_items = items_of(&items, root, "subagent");
    assert_eq!(parent_items.len(), 1);
    assert_eq!(parent_items[0]["subagentId"], json!(subagent.id.as_str()));
    assert_eq!(parent_items[0]["result"], json!("Hello."));

    let child = subagent
        .child_thread_id
        .as_ref()
        .expect("child thread")
        .as_str();
    assert!(
        items_of(&items, child, "assistant_message")
            .iter()
            .any(|item| item["text"] == "Hello."),
        "child thread holds the subagent's answer"
    );
    let child_app = app_threads(&seen)
        .into_iter()
        .find(|app| app["id"] == child)
        .expect("child app thread");
    assert_eq!(child_app["lineage"]["parentThreadId"], root);
    assert_eq!(child_app["lineage"]["relationshipToParent"], "subagent");
    assert!(
        seen.iter().any(|event| matches!(
            event,
            ProviderAdapterV2Event::ProviderTurnUpdated { thread_id: Some(thread), provider_turn, .. }
                if thread.as_str() == child
                    && provider_turn.status == turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Completed
        )),
        "child provider turn completes"
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn subagent_continue_replay_reopens_the_child_on_a_later_run() {
    let transcript = fixture("subagent_continue");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay
        .start(&recorded_text(&transcript, 0), policy.clone())
        .await;
    let mut seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    replay.start(&recorded_text(&transcript, 1), policy).await;
    let second = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&second),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    seen.extend(second);

    let subagents = subagents(&seen);
    assert_eq!(subagents.len(), 1);
    let subagent = &subagents[0];
    assert_eq!(subagent.status, OrchestrationV2SubagentStatus::Completed);
    assert_eq!(
        subagent.result.as_deref(),
        Some("continued subagent response")
    );
    assert_eq!(
        serde_json::to_value(subagent).unwrap()["runId"],
        json!("run-2"),
        "the continued subagent belongs to the run that resumed it"
    );

    let child = subagent
        .child_thread_id
        .as_ref()
        .expect("child thread")
        .as_str();
    let items = latest_items(&seen);
    let child_answers: Vec<&str> = items_of(&items, child, "assistant_message")
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect();
    assert!(child_answers.contains(&"initial subagent response"));
    assert!(child_answers.contains(&"continued subagent response"));
    assert!(
        items_of(&items, child, "user_message").len() >= 2,
        "the child keeps its spawn prompt and the message that resumed it"
    );
    let child_turns: std::collections::BTreeSet<String> = seen
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::ProviderTurnUpdated {
                thread_id: Some(thread),
                provider_turn,
                ..
            } if thread.as_str() == child => Some(provider_turn.id.as_str().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(child_turns.len(), 2, "the child has two provider turns");
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn subagent_v2_approval_replay_asks_on_the_parent_turn_under_the_subagent() {
    let transcript = fixture("subagent_v2_approval");
    let policy = recorded_policy(&transcript, RuntimeMode::ApprovalRequired);
    let mut replay = open(transcript.clone()).await;
    replay.start(&recorded_text(&transcript, 0), policy).await;
    let before = replay
        .next_until(|event| !runtime_requests(std::slice::from_ref(event)).is_empty())
        .await;
    let request = runtime_requests(&before).remove(0);
    assert_eq!(request.kind, OrchestrationV2RuntimeRequestKind::Command);
    let subagent = subagents(&before).remove(0);

    // The approval is the parent's: its run, its root provider turn, its thread.
    let root_turn = before
        .iter()
        .find_map(|event| match event {
            ProviderAdapterV2Event::ProviderTurnUpdated {
                thread_id: Some(thread),
                provider_turn,
                ..
            } if thread.as_str() == "thread-1" => Some(provider_turn.id.clone()),
            _ => None,
        })
        .expect("root provider turn");
    assert_eq!(request.provider_turn_id, Some(root_turn));
    let node = before
        .iter()
        .find_map(|event| match event {
            ProviderAdapterV2Event::NodeUpdated { node, .. } if node.id == request.node_id => {
                Some(node.clone())
            }
            _ => None,
        })
        .expect("approval node");
    assert_eq!(node.kind, OrchestrationV2ExecutionNodeKind::ApprovalRequest);
    assert_eq!(node.parent_node_id, Some(subagent.id.clone()));
    assert_eq!(node.thread_id, id("thread-1"));
    assert_eq!(node.run_id, Some(id("run-1")));
    assert_eq!(node.status, OrchestrationV2ExecutionNodeStatus::Waiting);

    replay
        .runtime
        .respond_to_runtime_request(accept(&request))
        .await
        .unwrap();
    let mut seen = before;
    seen.extend(replay.next_until(is_terminal).await);
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    assert_eq!(
        runtime_requests(&seen).len(),
        1,
        "the child asks nothing itself"
    );
    let settled = subagents(&seen).remove(0);
    assert_eq!(settled.result.as_deref(), Some("Written."));
    assert_eq!(settled.status, OrchestrationV2SubagentStatus::Completed);
    let child = settled
        .child_thread_id
        .as_ref()
        .expect("child thread")
        .as_str();
    let items = latest_items(&seen);
    assert!(
        items_of(&items, child, "command_execution")
            .iter()
            .any(|item| item["status"] == "completed"),
        "the child runs its own command after approval"
    );
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn subagent_v2_nested_replay_chains_subagents_to_the_leaf() {
    let transcript = fixture("subagent_v2_nested");
    let policy = recorded_policy(&transcript, RuntimeMode::FullAccess);
    let mut replay = open(transcript.clone()).await;
    replay.start(&recorded_text(&transcript, 0), policy).await;
    let seen = replay.next_until(is_terminal).await;
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );

    let subagents = subagents(&seen);
    assert_eq!(subagents.len(), 3);
    let first = subagents
        .iter()
        .find(|subagent| subagent.thread_id == id("thread-1"))
        .expect("first subagent under the root");
    let second = subagents
        .iter()
        .find(|subagent| Some(&subagent.thread_id) == first.child_thread_id.as_ref())
        .expect("second subagent under the first child");
    let third = subagents
        .iter()
        .find(|subagent| Some(&subagent.thread_id) == second.child_thread_id.as_ref())
        .expect("third subagent under the second child");
    for (subagent, parent_title) in [
        (first, None),
        (second, first.title.as_deref()),
        (third, second.title.as_deref()),
    ] {
        assert_eq!(subagent.status, OrchestrationV2SubagentStatus::Completed);
        assert_eq!(subagent.result.as_deref(), Some("Hello."));
        assert_eq!(
            subagent.origin,
            OrchestrationV2SubagentOrigin::ProviderNative
        );
        let title = subagent.title.as_deref().expect("agent path title");
        match parent_title {
            None => assert!(title.starts_with("/root/") && title.matches('/').count() == 2),
            Some(parent) => assert!(
                title.starts_with(&format!("{parent}/"))
                    && title[parent.len() + 1..].find('/').is_none(),
                "{title} extends {parent} by one segment"
            ),
        }
    }

    let items = latest_items(&seen);
    let leaf = third
        .child_thread_id
        .as_ref()
        .expect("leaf thread")
        .as_str();
    assert!(
        items_of(&items, leaf, "subagent").is_empty(),
        "the leaf spawns nothing"
    );
    assert!(
        items_of(&items, leaf, "assistant_message")
            .iter()
            .any(|item| item["text"] == "Hello."),
        "the leaf holds its answer"
    );
    let apps = app_threads(&seen);
    let second_child = second
        .child_thread_id
        .as_ref()
        .expect("second child")
        .as_str();
    let second_app = apps
        .iter()
        .find(|app| app["id"] == second_child)
        .expect("second child app thread");
    assert_eq!(
        second_app["lineage"]["parentThreadId"],
        first
            .child_thread_id
            .as_ref()
            .expect("first child")
            .as_str()
    );
    assert_eq!(second_app["lineage"]["rootThreadId"], "thread-1");
    replay.driver.verify_complete().unwrap();
}

#[tokio::test]
async fn subagent_v2_nested_approval_replay_asks_on_the_root_under_the_top_subagent() {
    let transcript = fixture("subagent_v2_nested_approval");
    let policy = recorded_policy(&transcript, RuntimeMode::ApprovalRequired);
    let mut replay = open(transcript.clone()).await;
    replay.start(&recorded_text(&transcript, 0), policy).await;
    let before = replay
        .next_until(|event| !runtime_requests(std::slice::from_ref(event)).is_empty())
        .await;
    let request = runtime_requests(&before).remove(0);
    assert_eq!(request.kind, OrchestrationV2RuntimeRequestKind::Command);

    // The grandchild asked. The approval hangs off the top-level subagent on the root.
    let top = subagents(&before)
        .into_iter()
        .find(|subagent| subagent.thread_id == id("thread-1"))
        .expect("top-level subagent");
    let node = before
        .iter()
        .find_map(|event| match event {
            ProviderAdapterV2Event::NodeUpdated { node, .. } if node.id == request.node_id => {
                Some(node.clone())
            }
            _ => None,
        })
        .expect("approval node");
    assert_eq!(node.kind, OrchestrationV2ExecutionNodeKind::ApprovalRequest);
    assert_eq!(node.thread_id, id("thread-1"));
    assert_eq!(node.run_id, Some(id("run-1")));
    assert_eq!(node.parent_node_id, Some(top.id.clone()));

    replay
        .runtime
        .respond_to_runtime_request(accept(&request))
        .await
        .unwrap();
    let mut seen = before;
    seen.extend(replay.next_until(is_terminal).await);
    assert_eq!(
        terminal_status(&seen),
        ProviderAdapterV2TurnTerminalStatus::Completed
    );
    assert_eq!(runtime_requests(&seen).len(), 1);
    let subagents = subagents(&seen);
    assert_eq!(
        subagents.len(),
        2,
        "the top-level subagent and the grandchild"
    );
    let grandchild = subagents
        .iter()
        .find(|subagent| Some(&subagent.thread_id) == top.child_thread_id.as_ref())
        .expect("grandchild under the top-level subagent");
    assert_eq!(grandchild.status, OrchestrationV2SubagentStatus::Completed);
    assert_eq!(grandchild.result.as_deref(), Some("Written."));
    let top = subagents
        .iter()
        .find(|subagent| subagent.thread_id == id("thread-1"))
        .expect("top-level subagent");
    assert_eq!(top.result.as_deref(), Some("Written."));
    let leaf = grandchild
        .child_thread_id
        .as_ref()
        .expect("grandchild thread")
        .as_str();
    let items = latest_items(&seen);
    assert!(
        items_of(&items, leaf, "command_execution")
            .iter()
            .any(|item| item["status"] == "completed"),
        "the grandchild runs its command after approval"
    );
    replay.driver.verify_complete().unwrap();
}

/// A running turn with no replay behind it, for projection rules that no fixture reaches.
fn detached_turn(native_turn: &str) -> TurnState {
    let provider_thread: OrchestrationV2ProviderThread = serde_json::from_value(json!({
        "id": "provider-thread-1",
        "driver": "codex",
        "providerInstanceId": "codex",
        "status": "idle",
        "handoffIds": [],
        "pendingBackgroundTasks": [],
        "createdAt": "2026-10-15T00:00:00.000Z",
        "updatedAt": "2026-10-15T00:00:00.000Z",
    }))
    .expect("provider thread fixture");
    TurnState::new(
        native_turn.into(),
        ProviderAdapterV2TurnInput {
            app_thread: app_thread("gpt-6-luna"),
            thread_id: id("thread-1"),
            run_id: id("run-1"),
            run_ordinal: 1,
            native_thread_has_turns: None,
            provider_turn_ordinal: 1,
            restart_continuation_of_run_id: None,
            attempt_id: id("attempt-1"),
            root_node_id: id("root-1"),
            provider_thread,
            message: ProviderAdapterV2TurnMessage {
                message_id: id("message-1"),
                text: "run a tool".into(),
                attachments: vec![],
                created_by: OrchestrationV2Actor::User,
                creation_source: OrchestrationV2CreationSource::Web,
                scheduled_task_id: None,
                sender_thread_id: None,
            },
            model_selection: selection("gpt-6-luna"),
            runtime_policy: policy(
                RuntimeMode::FullAccess,
                ProviderInteractionMode::Default,
                None,
                None,
            ),
            app_context: None,
        },
    )
}

/// Feeds one Codex notification through the projection, as the pump does.
fn notify(state: &mut RuntimeState, method: &str, params: Value) -> Vec<ProviderAdapterV2Event> {
    project_event(CodexEvent::from_notification(method.into(), params), state)
}

/// The id of the node a native item projects to.
fn item_node(native: &str) -> NodeId {
    turin_provider_core::id_allocator::derive::node_from_provider_item(
        &crate::orchestration_adapter::driver(),
        native,
    )
}

/// The node statuses a projection step reported for one node id.
fn node_status_of(
    events: &[ProviderAdapterV2Event],
    node: &NodeId,
) -> Vec<OrchestrationV2ExecutionNodeStatus> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::NodeUpdated { node: updated, .. } if updated.id == *node => {
                Some(updated.status)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn persistent_dynamic_tool_outlives_a_clean_turn_and_completes_later() {
    let mut state = RuntimeState::default();
    state
        .turns
        .insert("native-turn-1".into(), detached_turn("native-turn-1"));
    let tool = json!({
        "type": "dynamicToolCall",
        "id": "tool-1",
        "tool": "watch_build",
        "arguments": {"persistent": true},
        "status": "inProgress",
    });
    let node = item_node("tool-1");
    notify(
        &mut state,
        "item/started",
        json!({"threadId": "thread-1", "turnId": "native-turn-1", "startedAtMs": 1, "item": tool}),
    );

    let ended = notify(
        &mut state,
        "turn/completed",
        json!({"threadId": "thread-1", "turn": {"id": "native-turn-1", "status": "completed", "completedAt": 2}}),
    );
    assert!(
        node_status_of(&ended, &node).is_empty(),
        "a persistent tool is not terminalized by a clean turn end"
    );
    assert!(
        state.settled.contains_key("native-turn-1"),
        "the turn is retained"
    );

    let mut done = tool.clone();
    done["status"] = json!("completed");
    let completed = notify(
        &mut state,
        "item/completed",
        json!({"threadId": "thread-1", "turnId": "native-turn-1", "item": done}),
    );
    assert_eq!(
        node_status_of(&completed, &node),
        vec![OrchestrationV2ExecutionNodeStatus::Completed]
    );
    assert!(
        !state.settled.contains_key("native-turn-1"),
        "the turn is released once its tool completes"
    );
}

#[test]
fn running_command_outlives_a_clean_turn_and_completes_later() {
    let mut state = RuntimeState::default();
    state
        .turns
        .insert("native-turn-1".into(), detached_turn("native-turn-1"));
    let command = json!({
        "type": "commandExecution",
        "id": "cmd-1",
        "command": "sleep 5",
        "status": "inProgress",
        "aggregatedOutput": null,
    });
    let node = item_node("cmd-1");
    notify(
        &mut state,
        "item/started",
        json!({"threadId": "thread-1", "turnId": "native-turn-1", "startedAtMs": 1, "item": command}),
    );
    let ended = notify(
        &mut state,
        "turn/completed",
        json!({"threadId": "thread-1", "turn": {"id": "native-turn-1", "status": "completed", "completedAt": 2}}),
    );
    assert!(node_status_of(&ended, &node).is_empty());
    assert!(state.settled.contains_key("native-turn-1"));

    let mut done = command.clone();
    done["status"] = json!("completed");
    done["exitCode"] = json!(0);
    let completed = notify(
        &mut state,
        "item/completed",
        json!({"threadId": "thread-1", "turnId": "native-turn-1", "item": done}),
    );
    assert_eq!(
        node_status_of(&completed, &node),
        vec![OrchestrationV2ExecutionNodeStatus::Completed]
    );
    assert!(!state.settled.contains_key("native-turn-1"));
}

#[test]
fn context_compaction_is_one_turn_item_that_runs_then_completes() {
    let mut state = RuntimeState::default();
    state
        .turns
        .insert("native-turn-1".into(), detached_turn("native-turn-1"));
    let item = json!({"type": "contextCompaction", "id": "compact-1"});
    let started = notify(
        &mut state,
        "item/started",
        json!({"threadId": "thread-1", "turnId": "native-turn-1", "startedAtMs": 1, "item": item}),
    );
    assert!(
        !started
            .iter()
            .any(|event| matches!(event, ProviderAdapterV2Event::NodeUpdated { .. })),
        "a compaction has no execution node"
    );
    let running = latest_items(&started);
    assert_eq!(running.len(), 1);
    assert_eq!(running[0]["type"], "compaction");
    assert_eq!(running[0]["status"], "running");
    assert_eq!(running[0]["title"], "Compacting context");

    let completed = notify(
        &mut state,
        "item/completed",
        json!({"threadId": "thread-1", "turnId": "native-turn-1", "item": item}),
    );
    let done = latest_items(&completed);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["id"], running[0]["id"], "the same item completes");
    assert_eq!(done[0]["status"], "completed");
    assert_eq!(done[0]["title"], "Context compacted");
    assert!(done[0]["completedAt"].is_string());
}
