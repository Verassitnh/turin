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
    ProviderAdapterV2RuntimePolicy, ProviderAdapterV2SteerInput, ProviderAdapterV2TurnInput,
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
        if request.get("subtype") == "interrupt":
            consume("query.interrupt")
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
