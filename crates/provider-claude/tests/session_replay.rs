//! Replays upstream's recorded Claude transcripts through the `claude`
//! stream-json protocol (a fake CLI script) and the provider-core session
//! runtime. No paid turns run.
#![cfg(unix)]

use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use turin_contracts::{
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2ExecutionNodeKind, OrchestrationV2PlanArtifact,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
    provider_instance::ProviderInstanceId,
    provider_policy::{ProviderInteractionMode, RuntimeMode},
    settings::ClaudeSettings,
};
use turin_provider_claude::{
    claude_model_catalog::ClaudeModelCatalog, claude_provider_adapter::ClaudeProviderAdapter,
};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Event,
    ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
    ProviderAdapterV2RollbackTarget, ProviderAdapterV2RollbackThreadInput,
    ProviderAdapterV2RuntimePolicy, ProviderAdapterV2SteerInput,
    ProviderAdapterV2StopSubagentInput, ProviderAdapterV2SubagentOutcome,
    ProviderAdapterV2SubagentOutcomeStatus, ProviderAdapterV2TurnInput,
    ProviderAdapterV2TurnMessage, ProviderAdapterV2TurnTerminalStatus, ProviderEventStream,
    ProviderSessionRuntime,
};

/// Plays the recorded session in order. A prompt (`user`) consumes the next
/// `prompt.offer`, then replays inbound frames up to the next outbound entry.
/// An interrupt control request consumes `query.interrupt` the same way. Other
/// control requests get a success reply. `runtime_exit` ends the process.
const FAKE_CLI: &str = r#"#!/usr/bin/env python3
import json, os, sys
# Each launch appends its argv, so tests can check flags such as resume-at.
with open(os.environ["FAKE_ARGV"], "a") as log:
    log.write(json.dumps(sys.argv) + "\n")
pending = [json.loads(line) for line in open(os.environ["FAKE_FIXTURE"]) if line.strip()]

def emit(value):
    print(json.dumps(value), flush=True)

def wait_response(request_id):
    # The CLI blocks a turn on a permission callback until the host answers.
    for line in sys.stdin:
        if line.strip() and json.loads(line).get("type") == "control_response":
            if json.loads(line)["response"].get("request_id") == request_id:
                return

def drain():
    while pending and pending[0]["type"] != "expect_outbound":
        entry = pending.pop(0)
        if entry["type"] == "emit_inbound":
            emit(entry["frame"])
            if entry["frame"].get("type") == "control_request":
                wait_response(entry["frame"]["request_id"])
        elif entry["type"] == "runtime_exit":
            sys.exit(0)

def consume(prefix):
    if pending and pending[0]["type"] == "expect_outbound" and pending[0]["label"].startswith(prefix):
        pending.pop(0)
        drain()

while pending and not (pending[0]["type"] == "expect_outbound" and pending[0]["label"].startswith("query.open")):
    pending.pop(0)
if pending:
    pending.pop(0)

for line in sys.stdin:
    if not line.strip():
        continue
    message = json.loads(line)
    if message.get("type") == "user":
        consume("prompt.offer")
    elif message.get("type") == "control_request":
        request = message.get("request") or {}
        body = {"models": [], "commands": [], "account": {}} if request.get("subtype") == "initialize" else {}
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": message["request_id"], "response": body}})
        # An interrupt or a task stop is followed by the frames the CLI sends for it.
        if request.get("subtype") in ("interrupt", "stop_task"):
            consume("query." + request["subtype"])
"#;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../upstream/apps/server/src/orchestration-v2/testkit/fixtures")
        .join(name)
        .join("claude_transcript.ndjson")
}

fn id<T: DeserializeOwned>(value: &str) -> T {
    serde_json::from_value(json!(value)).expect("valid id")
}

struct Harness {
    dir: PathBuf,
    adapter: ClaudeProviderAdapter,
}

impl Harness {
    /// Replays a bundled upstream fixture.
    fn fixture(name: &str) -> Self {
        Self::with_transcript(fixture(name).display().to_string(), None)
    }

    /// Replays inline NDJSON entries (for shapes the fixtures do not cover).
    fn lines(entries: &[Value]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("turin-claude-replay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript.ndjson");
        let body: String = entries.iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(&path, body).unwrap();
        Self::with_transcript(path.display().to_string(), Some(dir))
    }

    fn with_transcript(transcript: String, dir: Option<PathBuf>) -> Self {
        let dir = dir.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("turin-claude-replay-{}", uuid::Uuid::new_v4()))
        });
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-claude");
        std::fs::write(&script, FAKE_CLI).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        env.insert("FAKE_FIXTURE".into(), transcript);
        env.insert(
            "FAKE_ARGV".into(),
            dir.join("argv.log").display().to_string(),
        );
        let settings: ClaudeSettings =
            serde_json::from_value(json!({ "binaryPath": script.display().to_string() })).unwrap();
        let adapter = ClaudeProviderAdapter::new(
            id::<ProviderInstanceId>("claudeAgent"),
            settings,
            ClaudeModelCatalog::default(),
            Some(env),
            None,
            String::new(),
        );
        Self { dir, adapter }
    }

    fn model(&self) -> ModelSelection {
        serde_json::from_value(json!({"instanceId": "claudeAgent", "model": "claude-sonnet-4-6"}))
            .unwrap()
    }

    fn policy(&self) -> ProviderAdapterV2RuntimePolicy {
        ProviderAdapterV2RuntimePolicy {
            runtime_mode: RuntimeMode::Auto,
            interaction_mode: ProviderInteractionMode::Default,
            cwd: Some(self.dir.display().to_string()),
            approval_policy: None,
            sandbox_policy: None,
            reasoning_effort: None,
        }
    }

    fn app_thread(&self) -> OrchestrationV2AppThread {
        serde_json::from_value(json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": "thread-1",
            "projectId": "project-1",
            "title": "replay",
            "providerInstanceId": "claudeAgent",
            "modelSelection": {"instanceId": "claudeAgent", "model": "claude-sonnet-4-6"},
            "runtimeMode": "auto",
            "interactionMode": "default",
            "lineage": {"rootThreadId": "thread-1"},
            "forkedFrom": null,
            "createdAt": "2026-10-15T00:00:00.000Z",
            "updatedAt": "2026-10-15T00:00:00.000Z",
            "archivedAt": null,
        }))
        .expect("app thread fixture")
    }

    async fn open(&self) -> Arc<dyn ProviderSessionRuntime> {
        self.adapter
            .open_session(ProviderAdapterV2OpenSessionInput {
                thread_id: id("thread-1"),
                provider_session_id: id("session-1"),
                model_selection: self.model(),
                runtime_policy: self.policy(),
                resume_from_session: None,
                initial_native_thread_id: None,
                initial_provider_item_identity_version: None,
            })
            .await
            .expect("open session")
    }

    async fn ensure(&self, runtime: &dyn ProviderSessionRuntime) -> OrchestrationV2ProviderThread {
        runtime
            .ensure_thread(ProviderAdapterV2EnsureThreadInput {
                thread_id: id("thread-1"),
                title: Some("replay".into()),
                model_selection: self.model(),
                runtime_policy: self.policy(),
                provider_session_id: Some(id("session-1")),
                existing_provider_thread: None,
            })
            .await
            .expect("ensure thread")
    }

    fn turn_input(
        &self,
        provider_thread: OrchestrationV2ProviderThread,
        text: &str,
    ) -> ProviderAdapterV2TurnInput {
        ProviderAdapterV2TurnInput {
            app_thread: self.app_thread(),
            thread_id: id("thread-1"),
            run_id: id("run-1"),
            run_ordinal: 1,
            native_thread_has_turns: Some(false),
            provider_turn_ordinal: 1,
            restart_continuation_of_run_id: None,
            attempt_id: id("attempt-1"),
            root_node_id: id("node-1"),
            provider_thread,
            message: message("message-1", text),
            model_selection: self.model(),
            runtime_policy: self.policy(),
            app_context: None,
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn message(id_text: &str, text: &str) -> ProviderAdapterV2TurnMessage {
    ProviderAdapterV2TurnMessage {
        message_id: id(id_text),
        text: text.into(),
        attachments: vec![],
        created_by: serde_json::from_value(json!("user")).unwrap(),
        creation_source: serde_json::from_value(json!("web")).unwrap(),
        scheduled_task_id: None,
        sender_thread_id: None,
    }
}

/// Waits for the turn terminal event. The timeout only guards against a hung
/// replay; the replay itself drives the sequence.
async fn until_terminal(
    events: &mut ProviderEventStream,
) -> (
    Vec<ProviderAdapterV2Event>,
    ProviderAdapterV2TurnTerminalStatus,
) {
    let mut seen = vec![];
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.next())
            .await
            .expect("replay produced events in time")
            .expect("event stream open")
            .expect("event is not an error");
        if let ProviderAdapterV2Event::TurnTerminal { status, .. } = &event {
            let status = *status;
            seen.push(event);
            return (seen, status);
        }
        seen.push(event);
    }
}

/// Reads until the turn starts and returns its provider turn.
async fn started_turn(events: &mut ProviderEventStream) -> OrchestrationV2ProviderTurn {
    loop {
        let event = events.next().await.unwrap().unwrap();
        if let ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } = event {
            return provider_turn;
        }
    }
}

fn assistant_texts(events: &[ProviderAdapterV2Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::MessageUpdated { message, .. } => Some(message.text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn simple_fixture_completes_one_turn() {
    let harness = Harness::fixture("simple");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(
            harness.turn_input(thread, "Respond with the following text: fixture simple ok"),
        )
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert!(
        assistant_texts(&seen)
            .iter()
            .any(|t| t == "fixture simple ok")
    );
}

#[tokio::test]
async fn error_result_fixture_recovers_on_the_next_turn() {
    // Recorded as: a 401 error result, then a successful result.
    let harness = Harness::fixture("claude_result_is_error");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "fail please"))
        .await
        .expect("start turn");

    let (_, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Failed);
}

#[tokio::test]
async fn exit_before_result_fails_the_turn() {
    let harness = Harness::lines(&[
        json!({"type": "expect_outbound", "label": "query.open", "frame": {}}),
        json!({"type": "expect_outbound", "label": "prompt.offer:1", "frame": {}}),
        json!({"type": "runtime_exit", "status": "failed"}),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "will never answer"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Failed);
    let failure = seen.iter().find_map(|event| match event {
        ProviderAdapterV2Event::TurnTerminal { failure, .. } => failure.clone(),
        _ => None,
    });
    let text = format!("{failure:?}");
    assert!(
        text.contains("Claude Code process exited before turn result"),
        "{text}"
    );
}

#[tokio::test]
async fn steering_replaces_the_active_prompt_and_completes() {
    let harness = Harness::fixture("message_steering");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(
            thread.clone(),
            "Respond with exactly: steering fixture initial response",
        ))
        .await
        .expect("start turn");
    let turn = started_turn(&mut events).await;

    runtime
        .steer_turn(ProviderAdapterV2SteerInput {
            thread_id: id("thread-1"),
            run_id: id("run-1"),
            provider_thread: thread,
            provider_turn_id: turn.id,
            message: message(
                "message-2",
                "Actually, respond with exactly: steering fixture observed",
            ),
        })
        .await
        .expect("steer");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert!(
        assistant_texts(&seen)
            .iter()
            .any(|t| t == "steering fixture observed")
    );
}

#[tokio::test]
async fn interrupt_reaches_the_cli_and_marks_the_turn_interrupted() {
    let harness = Harness::fixture("turn_interrupt");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread.clone(), "Do not answer immediately."))
        .await
        .expect("start turn");
    let turn = started_turn(&mut events).await;

    runtime
        .interrupt_turn(ProviderAdapterV2InterruptInput {
            provider_thread: thread,
            provider_turn_id: turn.id,
            request_runtime_restart: None,
        })
        .await
        .expect("interrupt");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Interrupted);
    let turn = seen.iter().rev().find_map(|event| match event {
        ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } => {
            Some(provider_turn.status)
        }
        _ => None,
    });
    assert_eq!(turn, Some(OrchestrationV2ProviderTurnStatus::Interrupted));
}

fn turn_items(events: &[ProviderAdapterV2Event]) -> Vec<OrchestrationV2TurnItem> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => Some(turn_item.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn read_only_tool_fixture_emits_tool_items_and_completes() {
    let harness = Harness::fixture("tool_call_read_only");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Read two files"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert!(
        assistant_texts(&seen)
            .iter()
            .any(|t| t == "read only tool fixture complete")
    );

    // Each Read starts running, then completes with its file content.
    let reads: Vec<_> = turn_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            OrchestrationV2TurnItem::DynamicTool {
                tool_name,
                status,
                output,
                ..
            } if tool_name.as_ref().map(|name| name.as_str()) == Some("Read") => {
                Some((status, output))
            }
            _ => None,
        })
        .collect();
    assert_eq!(reads.len(), 4, "two running and two completed updates");
    let completed: Vec<_> = reads
        .iter()
        .filter(|(status, _)| *status == OrchestrationV2TurnItemStatus::Completed)
        .collect();
    assert_eq!(completed.len(), 2);
    assert!(completed.iter().all(|(_, output)| output.is_some()));

    let tool_nodes = seen
        .iter()
        .filter(|event| {
            matches!(event, ProviderAdapterV2Event::NodeUpdated { node, .. }
            if node.kind == OrchestrationV2ExecutionNodeKind::ToolCall)
        })
        .count();
    assert_eq!(tool_nodes, 4);
}

#[tokio::test]
async fn exit_plan_mode_capture_emits_the_plan_and_completes() {
    let harness = Harness::lines(&[
        json!({"type": "expect_outbound", "label": "query.open", "frame": {}}),
        json!({"type": "expect_outbound", "label": "prompt.offer:1", "frame": {}}),
        json!({"type": "emit_inbound", "label": "control", "frame": {
            "type": "control_request",
            "request_id": "plan-request-1",
            "uuid": "00000000-0000-4000-8000-000000000001",
            "session_id": "plan-session",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "ExitPlanMode",
                "tool_use_id": "toolu_plan_1",
                "input": {"plan": "1. Read the config\n2. Change the default"}
            }
        }}),
        json!({"type": "emit_inbound", "label": "result", "frame": {
            "type": "result", "subtype": "success", "is_error": false,
            "result": "plan captured", "terminal_reason": "completed",
            "session_id": "plan-session", "usage": {},
            "uuid": "00000000-0000-4000-8000-000000000002"
        }}),
        json!({"type": "runtime_exit", "status": "success"}),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Plan the change"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(
        status,
        ProviderAdapterV2TurnTerminalStatus::Completed,
        "{seen:?}"
    );
    let plans: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::PlanUpdated { plan, .. } => Some(plan.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(plans.len(), 1);
    match &plans[0] {
        OrchestrationV2PlanArtifact::ProposedPlan { markdown, .. } => {
            assert_eq!(markdown, "1. Read the config\n2. Change the default");
        }
        other => panic!("expected a proposed plan, got {other:?}"),
    }
    // The plan is also a turn item, so the timeline shows it in place.
    let proposed: Vec<_> = turn_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            OrchestrationV2TurnItem::ProposedPlan { markdown, .. } => Some(markdown),
            _ => None,
        })
        .collect();
    assert_eq!(proposed, ["1. Read the config\n2. Change the default"]);
    let plan = serde_json::to_value(&plans[0]).unwrap();
    assert_eq!(plan["status"], "active");
}

/// Inline transcript for one prompt: the given frames, then a success result.
fn one_turn(frames: Vec<Value>) -> Harness {
    let mut entries = vec![
        json!({"type": "expect_outbound", "label": "query.open", "frame": {}}),
        json!({"type": "expect_outbound", "label": "prompt.offer:1", "frame": {}}),
    ];
    entries.extend(
        frames
            .into_iter()
            .map(|frame| json!({"type": "emit_inbound", "label": "frame", "frame": frame})),
    );
    entries.push(json!({"type": "emit_inbound", "label": "result", "frame": {
        "type": "result", "subtype": "success", "is_error": false,
        "result": "tasks done", "terminal_reason": "completed",
        "session_id": "todo-session", "usage": {},
        "uuid": "00000000-0000-4000-8000-0000000000ff"
    }}));
    entries.push(json!({"type": "runtime_exit", "status": "success"}));
    Harness::lines(&entries)
}

fn assistant_tool_use(
    uuid: &str,
    parent: Option<&str>,
    id: &str,
    name: &str,
    input: Value,
) -> Value {
    json!({
        "type": "assistant",
        "message": {"id": format!("msg_{uuid}"), "role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": name, "input": input}
        ]},
        "uuid": uuid, "session_id": "todo-session", "parent_tool_use_id": parent,
    })
}

fn user_tool_result(
    uuid: &str,
    parent: Option<&str>,
    id: &str,
    text: &str,
    structured: Value,
) -> Value {
    json!({
        "type": "user",
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": text}
        ]},
        "uuid": uuid, "session_id": "todo-session", "parent_tool_use_id": parent,
        "tool_use_result": structured,
    })
}

fn todo_plan_values(events: &[ProviderAdapterV2Event]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::PlanUpdated { plan, .. }
                if matches!(plan, OrchestrationV2PlanArtifact::TodoList { .. }) =>
            {
                serde_json::to_value(plan).ok()
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn todo_write_projects_one_todo_list_with_index_ids() {
    let harness = one_turn(vec![
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000011",
            None,
            "toolu_todo_1",
            "TodoWrite",
            json!({"todos": [
                {"content": "Read config", "status": "completed", "activeForm": "Reading"},
                {"content": "Edit default", "status": "in_progress", "activeForm": "Editing"},
                {"content": "Ship", "status": "pending", "activeForm": "Shipping"},
            ]}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000012",
            None,
            "toolu_todo_1",
            "Todos have been modified successfully.",
            json!({"oldTodos": [], "newTodos": []}),
        ),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Plan with todos"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);

    let plans = todo_plan_values(&seen);
    assert_eq!(plans.len(), 1, "{plans:?}");
    assert_eq!(plans[0]["status"], "active");
    assert_eq!(
        plans[0]["steps"],
        json!([
            {"id": "todo-0", "text": "Read config", "status": "completed"},
            {"id": "todo-1", "text": "Edit default", "status": "running"},
            {"id": "todo-2", "text": "Ship", "status": "pending"},
        ])
    );
    let todo_items: Vec<_> = turn_items(&seen)
        .into_iter()
        .filter(|item| matches!(item, OrchestrationV2TurnItem::TodoList { .. }))
        .collect();
    assert_eq!(todo_items.len(), 1);
}

#[tokio::test]
async fn task_tools_keep_one_list_and_ignore_subagent_task_calls() {
    let harness = one_turn(vec![
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000021",
            None,
            "toolu_create_1",
            "TaskCreate",
            json!({"subject": "Write tests", "description": "unit"}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000022",
            None,
            "toolu_create_1",
            "Task #1 created",
            json!({"task": {"id": "1", "subject": "Write tests"}}),
        ),
        // A subagent's task calls never change the parent's list.
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000023",
            Some("toolu_agent_1"),
            "toolu_sub_create",
            "TaskCreate",
            json!({"subject": "Subagent only"}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000024",
            Some("toolu_agent_1"),
            "toolu_sub_create",
            "Task #2 created",
            json!({"task": {"id": "2", "subject": "Subagent only"}}),
        ),
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000025",
            None,
            "toolu_update_1",
            "TaskUpdate",
            json!({"taskId": "1", "status": "in_progress"}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000026",
            None,
            "toolu_update_1",
            "Updated task #1",
            json!({"success": true, "taskId": "1"}),
        ),
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000027",
            None,
            "toolu_list_1",
            "TaskList",
            json!({}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000028",
            None,
            "toolu_list_1",
            "#1 [completed] Write tests",
            json!({"tasks": [{"id": "1", "subject": "Write tests", "status": "completed"}]}),
        ),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Track tasks"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);

    // Create, update, and list each re-project the same list (one plan id).
    let plans = todo_plan_values(&seen);
    assert_eq!(plans.len(), 3, "{plans:?}");
    assert!(plans.iter().all(|plan| plan["id"] == plans[0]["id"]));
    assert_eq!(
        plans[0]["steps"],
        json!([{"id": "task-1", "text": "Write tests", "status": "pending"}])
    );
    assert_eq!(plans[1]["steps"][0]["status"], "running");
    let last = plans.last().unwrap();
    assert_eq!(last["status"], "completed");
    assert_eq!(
        last["steps"],
        json!([{"id": "task-1", "text": "Write tests", "status": "completed"}])
    );
}

fn assistant_text(uuid: &str, text: &str) -> Value {
    json!({
        "type": "assistant",
        "message": {"id": format!("msg_{uuid}"), "role": "assistant",
            "content": [{"type": "text", "text": text}]},
        "uuid": uuid, "session_id": "rollback-session",
    })
}

fn result_ok(uuid: &str, text: &str) -> Value {
    json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": text, "terminal_reason": "completed",
        "session_id": "rollback-session", "usage": {}, "uuid": uuid,
    })
}

#[tokio::test]
async fn rollback_resumes_the_next_turn_at_the_recorded_cursor() {
    const FIRST: &str = "00000000-0000-4000-8000-0000000000c1";
    const SECOND: &str = "00000000-0000-4000-8000-0000000000c2";
    let harness = Harness::lines(&[
        json!({"type": "expect_outbound", "label": "query.open:1", "frame": {}}),
        json!({"type": "expect_outbound", "label": "prompt.offer:1", "frame": {}}),
        json!({"type": "emit_inbound", "label": "assistant", "frame": assistant_text(FIRST, "first answer")}),
        json!({"type": "emit_inbound", "label": "result", "frame": result_ok("00000000-0000-4000-8000-0000000000d1", "first answer")}),
        json!({"type": "runtime_exit", "status": "success"}),
        json!({"type": "expect_outbound", "label": "query.open:2", "frame": {}}),
        json!({"type": "expect_outbound", "label": "prompt.offer:2", "frame": {}}),
        json!({"type": "emit_inbound", "label": "assistant", "frame": assistant_text(SECOND, "second answer")}),
        json!({"type": "emit_inbound", "label": "result", "frame": result_ok("00000000-0000-4000-8000-0000000000d2", "second answer")}),
        json!({"type": "runtime_exit", "status": "success"}),
    ]);
    let capabilities =
        serde_json::to_value(harness.adapter.get_capabilities().await.unwrap()).unwrap();
    assert_eq!(capabilities["threads"]["canRollbackThread"], true);

    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread.clone(), "first"))
        .await
        .expect("start turn");
    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    // The terminal turn records the assistant cursor as its native ref.
    let turn = seen
        .iter()
        .rev()
        .find_map(|event| match event {
            ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } => {
                Some(provider_turn.clone())
            }
            _ => None,
        })
        .expect("terminal turn");
    let cursor = turn
        .native_turn_ref
        .as_ref()
        .and_then(|r| r.native_id.as_ref());
    assert_eq!(cursor.map(|id| id.as_str()), Some(FIRST));

    let rolled = runtime
        .rollback_thread(ProviderAdapterV2RollbackThreadInput {
            provider_thread: thread,
            target: ProviderAdapterV2RollbackTarget::ProviderTurn {
                checkpoint_id: id("checkpoint-1"),
                app_run_ordinal: 1,
                provider_turn: turn.clone(),
            },
            provider_thread_turns: vec![turn],
        })
        .await
        .expect("rollback");
    let head = rolled.provider_thread.native_conversation_head_ref.as_ref();
    assert_eq!(
        head.and_then(|r| r.native_id.as_ref())
            .map(|id| id.as_str()),
        Some(FIRST)
    );

    runtime
        .start_turn(harness.turn_input(rolled.provider_thread, "second"))
        .await
        .expect("start turn after rollback");
    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    // The completed turn consumes the head, so later turns resume normally.
    let cleared = seen.iter().any(|event| match event {
        ProviderAdapterV2Event::ProviderThreadUpdated {
            provider_thread, ..
        } => provider_thread.native_conversation_head_ref.is_none(),
        _ => false,
    });
    assert!(cleared);

    let launches = std::fs::read_to_string(harness.dir.join("argv.log")).unwrap();
    let launches: Vec<&str> = launches.lines().collect();
    assert_eq!(launches.len(), 2, "{launches:?}");
    assert!(
        !launches[0].contains("resume-session-at"),
        "{}",
        launches[0]
    );
    assert!(
        launches[1].contains(&format!("--resume-session-at={FIRST}")),
        "{}",
        launches[1]
    );
}

#[tokio::test]
async fn thinking_block_becomes_a_reasoning_item_before_the_answer() {
    let thinking = json!({
        "type": "assistant",
        "message": {"id": "msg_think", "role": "assistant", "content": [
            {"type": "thinking", "thinking": "Answer directly.", "signature": "sig"}
        ]},
        "uuid": "00000000-0000-4000-8000-0000000000e1", "session_id": "reasoning-session",
    });
    let harness = one_turn(vec![
        thinking,
        json!({
            "type": "assistant",
            "message": {"id": "msg_think", "role": "assistant", "content": [
                {"type": "text", "text": "done"}
            ]},
            "uuid": "00000000-0000-4000-8000-0000000000e2", "session_id": "reasoning-session",
        }),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Think first"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    let reasoning: Vec<_> = turn_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            OrchestrationV2TurnItem::Reasoning {
                text,
                title,
                status,
                streaming,
                native_item_ref,
                ..
            } => Some((text, title, status, streaming, native_item_ref)),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning.len(), 1, "{reasoning:?}");
    let (text, title, status, streaming, native) = &reasoning[0];
    assert_eq!(text, "Answer directly.");
    assert_eq!(title.as_deref(), Some("Thinking"));
    assert_eq!(*status, OrchestrationV2TurnItemStatus::Completed);
    assert!(!streaming);
    assert_eq!(
        native
            .as_ref()
            .and_then(|r| r.native_id.as_ref())
            .map(|id| id.as_str()),
        Some("msg_think:thinking:0")
    );
}

#[tokio::test]
async fn compact_boundary_becomes_a_context_compacted_item() {
    let harness = one_turn(vec![json!({
        "type": "system", "subtype": "compact_boundary",
        "compact_metadata": {"trigger": "auto", "pre_tokens": 1000, "post_tokens": 250},
        "uuid": "00000000-0000-4000-8000-0000000000f1", "session_id": "compact-session",
    })]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Compact"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    let compaction = turn_items(&seen)
        .into_iter()
        .find_map(|item| match item {
            OrchestrationV2TurnItem::Compaction {
                title,
                before_token_count,
                after_token_count,
                status,
                ..
            } => Some((title, before_token_count, after_token_count, status)),
            _ => None,
        })
        .expect("compaction item");
    assert_eq!(compaction.0.as_deref(), Some("Context compacted"));
    assert_eq!(compaction.1, Some(1000));
    assert_eq!(compaction.2, Some(250));
    assert_eq!(compaction.3, OrchestrationV2TurnItemStatus::Completed);
}

#[tokio::test]
async fn tool_items_take_their_upstream_titles() {
    let mcp_frame = json!({
        "type": "assistant",
        "message": {"id": "msg_mcp", "role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_mcp", "name": "mcp__linear__create_issue", "input": {}}
        ]},
        "uuid": "00000000-0000-4000-8000-000000000305", "session_id": "title-session",
        "tool_use_meta": [{"id": "toolu_mcp", "display_name": "Create issue",
            "server_display_name": "Linear"}],
    });
    let harness = one_turn(vec![
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000301",
            None,
            "toolu_read",
            "Read",
            json!({"file_path": " src/lib.rs "}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000302",
            None,
            "toolu_read",
            "fn main() {}",
            Value::Null,
        ),
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000303",
            None,
            "toolu_grep",
            "Grep",
            json!({"pattern": "TODO", "path": "/repo/src/"}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000304",
            None,
            "toolu_grep",
            "a.rs:1",
            Value::Null,
        ),
        mcp_frame,
        user_tool_result(
            "00000000-0000-4000-8000-000000000306",
            None,
            "toolu_mcp",
            "created",
            Value::Null,
        ),
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000307",
            None,
            "toolu_skill",
            "Skill",
            json!({"skill": "review"}),
        ),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Use tools"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    let titles: BTreeMap<String, (Option<String>, Option<String>)> = turn_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            OrchestrationV2TurnItem::DynamicTool {
                native_item_ref,
                title,
                tool_source,
                ..
            } => {
                let native = native_item_ref?.native_id?.as_str().to_owned();
                let source = tool_source.map(|source| source.name.as_str().to_owned());
                Some((native, (title, source)))
            }
            _ => None,
        })
        .collect();
    let title = |native: &str| titles.get(native).map(|(title, _)| title.clone());
    assert_eq!(title("toolu_read"), Some(Some("Read src/lib.rs".into())));
    assert_eq!(
        title("toolu_grep"),
        Some(Some("Searched TODO in src".into()))
    );
    assert_eq!(title("toolu_mcp"), Some(Some("Create issue".into())));
    assert_eq!(title("toolu_skill"), Some(Some("Skill: review".into())));
    assert_eq!(titles["toolu_mcp"].1.as_deref(), Some("Linear"));
}

/// Subagent frames carry `parent_tool_use_id`. Their text and tool calls stay
/// out of the parent turn. The subagent's own lifecycle reaches its record.
#[tokio::test]
async fn subagent_frames_stay_out_of_the_parent_turn() {
    let harness = one_turn(vec![
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000401",
            None,
            "toolu_agent",
            "Agent",
            json!({"description": "Agent A", "prompt": "Reply A", "model": "haiku"}),
        ),
        json!({
            "type": "system", "subtype": "task_started", "task_id": "task_a",
            "tool_use_id": "toolu_agent", "description": "Agent A",
            "task_type": "local_agent", "session_id": "todo-session",
            "uuid": "00000000-0000-4000-8000-000000000402",
        }),
        json!({
            "type": "assistant",
            "message": {"id": "msg_child", "role": "assistant", "content": [
                {"type": "text", "text": "child narration"},
                {"type": "tool_use", "id": "toolu_child_bash", "name": "Bash", "input": {"command": "echo hi"}}
            ]},
            "uuid": "00000000-0000-4000-8000-000000000403", "session_id": "todo-session",
            "parent_tool_use_id": "toolu_agent",
        }),
        user_tool_result(
            "00000000-0000-4000-8000-000000000404",
            Some("toolu_agent"),
            "toolu_child_bash",
            "hi",
            Value::Null,
        ),
        json!({
            "type": "system", "subtype": "task_progress", "task_id": "task_a",
            "tool_use_id": "toolu_agent", "summary": "running echo",
            "session_id": "todo-session", "uuid": "00000000-0000-4000-8000-000000000405",
        }),
        json!({
            "type": "system", "subtype": "task_notification", "task_id": "task_a",
            "tool_use_id": "toolu_agent", "status": "completed", "summary": "A_FIRST",
            "session_id": "todo-session", "uuid": "00000000-0000-4000-8000-000000000406",
        }),
        user_tool_result(
            "00000000-0000-4000-8000-000000000407",
            None,
            "toolu_agent",
            "A_FIRST",
            Value::Null,
        ),
        assistant_text("00000000-0000-4000-8000-000000000408", "parent done"),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Launch a subagent"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    // The parent turn's messages carry its run; the subagent's go to its child thread.
    let parent_texts: Vec<String> = seen
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::MessageUpdated { message, .. } if message.run_id.is_some() => {
                Some(message.text.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(parent_texts, vec!["parent done".to_owned()]);
    let child_id = seen
        .iter()
        .find_map(|event| match event {
            ProviderAdapterV2Event::SubagentUpdated { subagent, .. } => {
                subagent.child_thread_id.clone()
            }
            _ => None,
        })
        .expect("the subagent has a child thread");
    let child_texts: Vec<(String, String)> = seen
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::MessageUpdated { message, .. } if message.run_id.is_none() => {
                Some((message.thread_id.as_str().to_owned(), message.text.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(
        child_texts
            .iter()
            .any(|(thread, text)| thread == child_id.as_str() && text == "child narration"),
        "{child_texts:?}"
    );
    assert!(
        turn_items(&seen)
            .iter()
            .all(|item| !matches!(item, OrchestrationV2TurnItem::DynamicTool { .. })),
        "the Agent call and the subagent's Bash must not become parent tool items"
    );
    let subagents: Vec<Value> = seen
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::SubagentUpdated { subagent, .. } => {
                serde_json::to_value(subagent).ok()
            }
            _ => None,
        })
        .collect();
    let statuses: Vec<&Value> = subagents.iter().map(|s| &s["status"]).collect();
    assert_eq!(
        statuses,
        vec![&json!("running"), &json!("running"), &json!("completed")]
    );
    let last = subagents.last().unwrap();
    assert_eq!(last["result"], "A_FIRST");
    assert_eq!(last["title"], "Agent A");
    assert_eq!(last["model"], "haiku");
    assert_eq!(subagents[1]["progress"], "running echo");
}

/// A `/goal` is read from the transcript: its synthetic output sets it, and a
/// checked turn that completes with no background work completes it.
#[tokio::test]
async fn goal_set_by_command_output_completes_after_a_checked_turn() {
    let harness = one_turn(vec![
        json!({
            "type": "assistant",
            "message": {"id": "msg_goal_set", "model": "<synthetic>", "role": "assistant",
                "content": [{"type": "text", "text": "Goal set: ship the parser"}]},
            "uuid": "00000000-0000-4000-8000-000000000701", "session_id": "todo-session",
        }),
        assistant_text("00000000-0000-4000-8000-000000000702", "working on it"),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Set a goal"))
        .await
        .expect("start turn");
    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);

    let goals: Vec<Value> = seen
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::ProviderThreadUpdated {
                provider_thread, ..
            } => serde_json::to_value(&provider_thread.goal).ok(),
            _ => None,
        })
        .collect();
    assert!(
        goals
            .iter()
            .any(|goal| goal["objective"] == "ship the parser" && goal["status"] == "active"),
        "{goals:?}"
    );
    let last = goals.last().expect("the thread carries its goal");
    assert_eq!(last["objective"], "ship the parser");
    assert_eq!(last["status"], "complete", "{goals:?}");
}

/// A `local_bash` task is background work on the thread's roster until its
/// notification. Each change re-emits the thread with the roster.
#[tokio::test]
async fn background_bash_task_is_on_the_roster_until_it_notifies() {
    let harness = one_turn(vec![
        json!({
            "type": "system", "subtype": "background_tasks_changed",
            "tasks": [{"task_id": "bg-1", "task_type": "local_bash", "description": "npm test"}],
            "session_id": "todo-session", "uuid": "00000000-0000-4000-8000-000000000601",
        }),
        json!({
            "type": "system", "subtype": "task_notification", "task_id": "bg-1",
            "status": "completed", "summary": "ok", "session_id": "todo-session",
            "uuid": "00000000-0000-4000-8000-000000000602",
        }),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Run the tests in the background"))
        .await
        .expect("start turn");
    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);

    let rosters: Vec<Value> = seen
        .iter()
        .filter_map(|event| match event {
            ProviderAdapterV2Event::ProviderThreadUpdated {
                provider_thread, ..
            } => serde_json::to_value(&provider_thread.pending_background_tasks).ok(),
            _ => None,
        })
        .collect();
    // The turn also re-emits the thread with an empty roster; the edges are the
    // listed roster and the cleared one after the notification.
    let listed = rosters
        .iter()
        .position(|roster| roster != &json!([]))
        .expect("the roster lists bg-1");
    assert_eq!(rosters[listed][0]["taskId"], "bg-1");
    assert_eq!(rosters[listed][0]["kind"], "command");
    assert_eq!(rosters[listed][0]["description"], "npm test");
    assert_eq!(rosters.last(), Some(&json!([])), "{rosters:?}");
}

/// A Stop is confirmed by the task's native `stopped` frame, which the CLI
/// sends after `stop_task`. A repeat Stop answers from that confirmed outcome.
#[tokio::test]
async fn stop_subagent_is_confirmed_by_the_native_stopped_frame() {
    let entries = vec![
        json!({"type": "expect_outbound", "label": "query.open", "frame": {}}),
        json!({"type": "expect_outbound", "label": "prompt.offer:1", "frame": {}}),
        json!({"type": "emit_inbound", "label": "frame", "frame": assistant_tool_use(
            "00000000-0000-4000-8000-000000000501", None, "toolu_agent", "Agent",
            json!({"description": "Agent S", "prompt": "Count", "model": "haiku"}),
        )}),
        json!({"type": "emit_inbound", "label": "frame", "frame": {
            "type": "system", "subtype": "task_started", "task_id": "task_s",
            "tool_use_id": "toolu_agent", "description": "Agent S",
            "task_type": "local_agent", "session_id": "todo-session",
            "uuid": "00000000-0000-4000-8000-000000000502",
        }}),
        json!({"type": "emit_inbound", "label": "result", "frame": {
            "type": "result", "subtype": "success", "is_error": false,
            "result": "parent done", "terminal_reason": "completed",
            "session_id": "todo-session", "usage": {},
            "uuid": "00000000-0000-4000-8000-000000000503"
        }}),
        // The CLI answers stop_task with this frame.
        json!({"type": "expect_outbound", "label": "query.stop_task", "frame": {}}),
        json!({"type": "emit_inbound", "label": "frame", "frame": {
            "type": "system", "subtype": "task_notification", "task_id": "task_s",
            "tool_use_id": "toolu_agent", "status": "stopped", "summary": "stopped early",
            "session_id": "todo-session", "uuid": "00000000-0000-4000-8000-000000000504",
        }}),
    ];
    let harness = Harness::lines(&entries);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread.clone(), "Count in the background"))
        .await
        .expect("start turn");
    let (_seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);

    let stop = |thread: OrchestrationV2ProviderThread| ProviderAdapterV2StopSubagentInput {
        provider_thread: thread,
        native_task_id: "task_s".into(),
        requested_at: None,
    };
    let outcome: Option<ProviderAdapterV2SubagentOutcome> = runtime
        .stop_subagent(stop(thread.clone()))
        .expect("stopping a subagent is supported")
        .await
        .expect("the stop is confirmed");
    let outcome = outcome.expect("the native frame confirms the stop");
    assert_eq!(
        outcome.status,
        ProviderAdapterV2SubagentOutcomeStatus::Cancelled
    );
    assert_eq!(outcome.result.as_deref(), Some("stopped early"));

    let again = runtime
        .stop_subagent(stop(thread))
        .expect("stopping a subagent is supported")
        .await
        .expect("a repeat stop answers");
    assert_eq!(again, Some(outcome));
}

/// Bash output is the structured `stdout` and `stderr`, as upstream reads it.
/// A background run has empty streams, so its content acknowledgement shows.
#[tokio::test]
async fn bash_output_comes_from_the_structured_streams() {
    let harness = one_turn(vec![
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000501",
            None,
            "toolu_bash",
            "Bash",
            json!({"command": "ls"}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000502",
            None,
            "toolu_bash",
            "out",
            json!({"stdout": "out", "stderr": "warn", "interrupted": false}),
        ),
        assistant_tool_use(
            "00000000-0000-4000-8000-000000000503",
            None,
            "toolu_bg",
            "Bash",
            json!({"command": "sleep 1", "run_in_background": true}),
        ),
        user_tool_result(
            "00000000-0000-4000-8000-000000000504",
            None,
            "toolu_bg",
            "Command started in background",
            json!({"stdout": "", "stderr": "", "backgroundTaskId": "b1"}),
        ),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Run commands"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    let outputs: Vec<Value> = turn_items(&seen)
        .iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .filter(|item| item["type"] == "command_execution" && item["status"] == "completed")
        .map(|item| item["output"].clone())
        .collect();
    assert_eq!(
        outputs,
        vec![json!("out\nwarn"), json!("Command started in background")]
    );
}

/// Thinking deltas coalesce on the 50 ms flush (upstream `flushIntervalMs: 50`).
/// A block that completes inside the window writes only its completed text, and
/// that replaces the block's text under the same native id.
#[tokio::test]
async fn thinking_deltas_coalesce_into_the_completed_reasoning_item() {
    let stream = |event: Value| {
        json!({
            "type": "stream_event", "event": event, "parent_tool_use_id": null,
            "session_id": "todo-session", "uuid": "00000000-0000-4000-8000-000000000601",
        })
    };
    let harness = one_turn(vec![
        stream(json!({"type": "message_start", "message": {"id": "msg_think"}})),
        stream(json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "thinking", "thinking": ""}})),
        stream(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "Plan"}})),
        stream(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": " it"}})),
        stream(json!({"type": "content_block_stop", "index": 0})),
        json!({
            "type": "assistant",
            "message": {"id": "msg_think", "role": "assistant", "content": [
                {"type": "thinking", "thinking": "Plan it"}
            ]},
            "uuid": "00000000-0000-4000-8000-000000000602", "session_id": "todo-session",
        }),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Think"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    let reasoning: Vec<(String, bool, String)> = turn_items(&seen)
        .iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .filter(|item| item["type"] == "reasoning")
        .map(|item| {
            (
                item["text"].as_str().unwrap_or_default().to_owned(),
                item["streaming"] == true,
                item["status"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let at =
        |text: &str, streaming: bool, status: &str| (text.to_owned(), streaming, status.to_owned());
    assert_eq!(
        reasoning,
        vec![
            at("Plan it", false, "completed"),
            at("Plan it", false, "completed"),
        ]
    );
}

/// A block still open when the turn's result arrives writes its buffered text
/// before the turn ends, rather than waiting for the flush timer.
#[tokio::test]
async fn thinking_text_still_open_at_the_result_is_written_before_the_turn_ends() {
    let stream = |event: Value| {
        json!({
            "type": "stream_event", "event": event, "parent_tool_use_id": null,
            "session_id": "todo-session", "uuid": "00000000-0000-4000-8000-000000000611",
        })
    };
    let harness = one_turn(vec![
        stream(json!({"type": "message_start", "message": {"id": "msg_open"}})),
        stream(json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "thinking", "thinking": ""}})),
        stream(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "Still "}})),
        stream(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "thinking"}})),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Think, then stop"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    let reasoning: Vec<(String, String)> = turn_items(&seen)
        .iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .filter(|item| item["type"] == "reasoning")
        .map(|item| {
            (
                item["text"].as_str().unwrap_or_default().to_owned(),
                item["status"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        reasoning.last(),
        Some(&("Still thinking".to_owned(), "running".to_owned())),
        "{reasoning:?}"
    );
}

/// On Claude 5.1+, a tool call right after a second thinking block takes that
/// block's text as its preamble. The first thinking block stays reasoning.
#[tokio::test]
async fn thinking_before_a_tool_call_becomes_the_preamble_on_claude_5_1() {
    let harness = one_turn(vec![
        json!({
            "type": "assistant",
            "message": {"id": "msg_pre", "role": "assistant", "model": "claude-sonnet-5-5", "content": [
                {"type": "thinking", "thinking": "First thought"},
                {"type": "thinking", "thinking": "Reading the file now"},
                {"type": "tool_use", "id": "toolu_pre", "name": "Read", "input": {"file_path": "a.rs"}}
            ]},
            "uuid": "00000000-0000-4000-8000-000000000701", "session_id": "todo-session",
        }),
        user_tool_result(
            "00000000-0000-4000-8000-000000000702",
            None,
            "toolu_pre",
            "fn a() {}",
            Value::Null,
        ),
    ]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Preamble"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert_eq!(
        assistant_texts(&seen),
        vec!["Reading the file now".to_owned()]
    );
    let kinds: Vec<String> = turn_items(&seen)
        .iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .map(|item| item["type"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(kinds.contains(&"reasoning".to_owned()), "{kinds:?}");
}

/// Points a thread at the session id a transcript reports in its `init` frame.
fn with_transcript_session(
    mut thread: OrchestrationV2ProviderThread,
    session: &str,
) -> OrchestrationV2ProviderThread {
    thread.native_thread_ref = serde_json::from_value(json!({
        "driver": "claudeAgent", "nativeId": session, "strength": "strong",
    }))
    .ok();
    thread
}

/// The upstream MCP presentation fixture: two Firecrawl tools, each titled
/// and sourced from the MCP server it names.
#[tokio::test]
async fn fixture_mcp_tools_take_their_presentation() {
    let harness = Harness::fixture("claude_mcp_tool_presentation");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(
            thread,
            "Do exactly this, one tool call at a time, with no other tool calls.",
        ))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert!(
        assistant_texts(&seen)
            .iter()
            .any(|t| t == "mcp presentation fixture complete")
    );
    let completed: Vec<Value> = turn_items(&seen)
        .iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .filter(|item| item["type"] == "dynamic_tool" && item["status"] == "completed")
        .collect();
    let names: Vec<&str> = completed
        .iter()
        .map(|item| item["toolName"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec![
            "mcp__claude_ai_Firecrawl__firecrawl_scrape",
            "mcp__claude_ai_Firecrawl__firecrawl_map",
        ]
    );
    let [scrape, map] = [&completed[0], &completed[1]];
    assert_eq!(scrape["title"], "Firecrawl scrape");
    assert_eq!(scrape["toolSource"]["key"], "mcp:firecrawl");
    assert_eq!(scrape["toolSource"]["name"], "Firecrawl");
    assert_eq!(scrape["toolSource"]["kind"], "integration");
    assert_eq!(scrape["toolSource"]["icon"]["_tag"], "themed-logo");
    assert_eq!(scrape["toolIcon"], scrape["toolSource"]["icon"]);
    assert_eq!(map["title"], "firecrawl map");
    assert_eq!(map["toolSource"]["key"], "mcp:firecrawl");
}

/// The upstream no-echo compaction fixture. A CLI that never echoes cannot
/// show the `/compact` turn is not the peer's, so the peer's reply settles it.
#[tokio::test]
async fn fixture_compact_after_peer_turn_without_echo_settles_each_turn() {
    let harness = Harness::fixture("claude_compact_after_peer_turn_no_echo");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    // The transcript's own session id must be the thread's, or each turn relaunches the CLI.
    let thread = with_transcript_session(thread, "c0de0c0d-5e55-4c0a-9a7c-0000000000c1");
    runtime
        .start_turn(harness.turn_input(
            thread.clone(),
            "Reply with exactly: compact probe first turn",
        ))
        .await
        .expect("start first turn");
    let (first, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert_eq!(
        assistant_texts(&first),
        vec!["compact probe first turn".to_owned()]
    );

    runtime
        .start_turn(harness.turn_input(thread, "/compact"))
        .await
        .expect("start compact turn");
    let (second, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert_eq!(assistant_texts(&second), vec!["PEER_ACK".to_owned()]);
    assert!(
        turn_items(&second)
            .iter()
            .all(|item| !matches!(item, OrchestrationV2TurnItem::Compaction { .. })),
        "no compaction frame is in this transcript"
    );
}

/// The compaction frame from the upstream compaction fixture, read from that
/// transcript, becomes a context-compacted item with its before and after sizes.
#[tokio::test]
async fn fixture_compact_boundary_frame_projects_its_sizes() {
    let transcript = std::fs::read_to_string(fixture("claude_compact_after_peer_turn")).unwrap();
    let boundary: Value = transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "emit_inbound")
        .map(|entry| entry["frame"].clone())
        .find(|frame| frame["subtype"] == "compact_boundary")
        .expect("fixture has a compact_boundary frame");
    let harness = one_turn(vec![boundary]);
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    runtime
        .start_turn(harness.turn_input(thread, "Compact"))
        .await
        .expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    let compaction = turn_items(&seen)
        .into_iter()
        .find_map(|item| match item {
            OrchestrationV2TurnItem::Compaction {
                before_token_count,
                after_token_count,
                ..
            } => Some((before_token_count, after_token_count)),
            _ => None,
        })
        .expect("compaction item");
    assert_eq!(compaction, (Some(27445), Some(1192)));
    // The context window is T10d's catalog, so the used tokens carry no max yet.
    let usage = seen.iter().find_map(|event| match event {
        ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } => provider_turn
            .token_usage
            .as_ref()
            .and_then(|usage| serde_json::to_value(usage).ok()),
        _ => None,
    });
    let usage = usage.expect("turn usage after compaction");
    assert_eq!(usage["usedTokens"], 1192);
    assert!(usage["maxTokens"].is_null());
}

/// The upstream nested-subagent fixture. The outer subagent hangs off the
/// turn's root node. Its own Agent call starts a nested subagent, which hangs
/// off the outer one. Both finish with their result text.
/// Not asserted: the model names. Turin does not resolve aliases to catalog
/// slugs yet, so the model is left as the launch wrote it.
#[tokio::test]
async fn fixture_nested_subagent_is_parented_to_its_owner() {
    const OUTER_TASK: &str = "a457ca677bc1ecd82";
    const NESTED_TASK: &str = "aac1d7a2af5371c7d";
    let harness = Harness::fixture("claude_nested_subagent_model");
    let runtime = harness.open().await;
    let mut events = runtime.events();
    let thread = harness.ensure(runtime.as_ref()).await;
    let input = harness.turn_input(
        thread,
        "Live-test nested subagents. Do exactly this, with no extra steps.",
    );
    let root = serde_json::to_value(&input.root_node_id).unwrap();
    runtime.start_turn(input).await.expect("start turn");

    let (seen, status) = until_terminal(&mut events).await;
    assert_eq!(status, ProviderAdapterV2TurnTerminalStatus::Completed);
    assert!(
        assistant_texts(&seen)
            .iter()
            .any(|t| t.contains("NESTED_LEAF_OK"))
    );
    // Each subagent's last record is its final one.
    let mut latest: BTreeMap<String, Value> = BTreeMap::new();
    for event in &seen {
        if let ProviderAdapterV2Event::SubagentUpdated { subagent, .. } = event {
            let value = serde_json::to_value(subagent).unwrap();
            let task = value["nativeTaskRef"]["nativeId"]
                .as_str()
                .unwrap()
                .to_owned();
            latest.insert(task, value);
        }
    }
    let outer = &latest[OUTER_TASK];
    let nested = &latest[NESTED_TASK];
    assert_eq!(outer["status"], "completed");
    assert_eq!(nested["status"], "completed");
    assert_eq!(outer["parentNodeId"], root);
    assert_eq!(nested["parentNodeId"], outer["id"]);
    assert!(
        nested["result"]
            .as_str()
            .unwrap_or_default()
            .contains("NESTED_LEAF_OK")
    );
    // Each subagent has its own child thread, and the nested one is not the outer one's.
    let outer_child = outer["childThreadId"].as_str().expect("outer child thread");
    let nested_child = nested["childThreadId"]
        .as_str()
        .expect("nested child thread");
    assert_ne!(outer_child, nested_child);
}
