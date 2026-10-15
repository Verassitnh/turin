//! Fake HTTP/SSE sequences for both OpenCode generations.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use turin_contracts::{
    MessageId, NodeId, ProviderSessionId, RunAttemptId, RunId, ThreadId,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2CreationSource,
    },
    provider_instance::ProviderInstanceId,
    provider_policy::{ProviderApprovalDecision, ProviderInteractionMode, RuntimeMode},
};

use crate::OpenCodeGeneration;
use crate::adapter::api::*;
use crate::adapter::v1::OpenCodeV1Session;
use crate::adapter::v2::OpenCodeV2Session;
use crate::test_support::{FakeResponse, FakeServer, Recorded};
use crate::v2_client::V2Client;

fn sse(payload: &str) -> Vec<u8> {
    format!("data: {payload}\n\n").into_bytes()
}

fn app_thread() -> OrchestrationV2AppThread {
    serde_json::from_value(json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": "thread_1",
        "projectId": "project_1",
        "title": "Thread",
        "providerInstanceId": "opencode",
        "modelSelection": {"instanceId": "opencode", "model": "openai/gpt"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread_1"},
        "forkedFrom": null,
        "createdAt": "2026-10-15T00:00:00.000Z",
        "updatedAt": "2026-10-15T00:00:00.000Z",
        "archivedAt": null,
        "deletedAt": null
    }))
    .expect("app thread")
}

fn model() -> ModelSelection {
    ModelSelection {
        instance_id: ProviderInstanceId::new("opencode").unwrap(),
        model: turin_contracts::TrimmedNonEmptyString::new("openai/gpt").unwrap(),
        options: None,
    }
}

fn policy(mode: RuntimeMode) -> ProviderAdapterV2RuntimePolicy {
    ProviderAdapterV2RuntimePolicy {
        runtime_mode: mode,
        interaction_mode: ProviderInteractionMode::Default,
        cwd: Some("/work".to_owned()),
        approval_policy: None,
        sandbox_policy: None,
        reasoning_effort: None,
    }
}

fn turn(
    thread: turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
    text: &str,
) -> ProviderAdapterV2TurnInput {
    ProviderAdapterV2TurnInput {
        app_thread: app_thread(),
        thread_id: ThreadId::new("thread_1").unwrap(),
        run_id: RunId::new("run_1").unwrap(),
        run_ordinal: 1,
        native_thread_has_turns: None,
        provider_turn_ordinal: 1,
        restart_continuation_of_run_id: None,
        attempt_id: RunAttemptId::new("attempt_1").unwrap(),
        root_node_id: NodeId::new("node_1").unwrap(),
        provider_thread: thread,
        message: ProviderAdapterV2TurnMessage {
            message_id: MessageId::new("message_1").unwrap(),
            text: text.to_owned(),
            attachments: Vec::new(),
            created_by: OrchestrationV2Actor::User,
            creation_source: OrchestrationV2CreationSource::Web,
            scheduled_task_id: None,
            sender_thread_id: None,
        },
        model_selection: model(),
        runtime_policy: policy(RuntimeMode::FullAccess),
        app_context: None,
    }
}

struct Script {
    server: FakeServer,
    frames: mpsc::UnboundedSender<Option<Vec<u8>>>,
}

async fn script(reply: impl Fn(Recorded) -> FakeResponse + Send + Sync + 'static) -> Script {
    let (frames, rx) = mpsc::unbounded_channel();
    let rx = Arc::new(Mutex::new(Some(rx)));
    let server = FakeServer::spawn(move |request| {
        if request.method == "GET"
            && (request.target.contains("/event") || request.target.contains("/api/event"))
        {
            let mut response = FakeResponse::sse(Vec::new(), true);
            response.follow = rx.lock().take();
            return response;
        }
        reply(request)
    })
    .await;
    Script { server, frames }
}

async fn await_request(server: &mut FakeServer, pred: impl Fn(&Recorded) -> bool) -> Recorded {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let next = tokio::time::timeout_at(deadline, server.requests.recv())
            .await
            .expect("request")
            .expect("open");
        if pred(&next) {
            return next;
        }
    }
}

fn body(recorded: &Recorded) -> Value {
    serde_json::from_str(&recorded.body).unwrap_or(Value::Null)
}

#[tokio::test]
async fn v1_prompt_sends_system_and_model() {
    let mut script = script(|request| {
        if request.method == "POST" && request.target.contains("prompt_async") {
            return FakeResponse::empty(204);
        }
        if request.method == "POST" && request.target.contains("/session") {
            return FakeResponse::json(200, r#"{"id":"ses_1"}"#);
        }
        FakeResponse::json(200, "{}")
    })
    .await;
    let session = OpenCodeV1Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        &script.server.url,
        None,
        Some("openai/gpt"),
    )
    .await
    .unwrap();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::FullAccess),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.start_turn(turn(thread, "hello")).await.unwrap();
    let prompt = await_request(&mut script.server, |request| {
        request.target.contains("prompt_async")
    })
    .await;
    let body = body(&prompt);
    assert_eq!(body["model"]["providerID"], "openai");
    assert_eq!(body["model"]["modelID"], "gpt");
    assert!(body["system"].as_str().unwrap().contains("OpenCode"));
    assert!(body.get("system").is_some());
    session.close().await;
}

#[tokio::test]
async fn v1_slash_command_posts_the_command_route() {
    let mut script = script(|request| {
        if request.target.contains("/command") && !request.target.contains("prompt") {
            if request.method == "GET" {
                return FakeResponse::json(200, r#"[{"name":"review"}]"#);
            }
            return FakeResponse::json(200, r#"{"id":"cmd"}"#);
        }
        if request.method == "POST" && request.target.contains("/session") {
            return FakeResponse::json(200, r#"{"id":"ses_1"}"#);
        }
        FakeResponse::json(200, "{}")
    })
    .await;
    let session = OpenCodeV1Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        &script.server.url,
        None,
        None,
    )
    .await
    .unwrap();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::Auto),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session
        .start_turn(turn(thread, "/review the diff"))
        .await
        .unwrap();
    let command = await_request(&mut script.server, |request| {
        request.method == "POST" && request.target.contains("/command")
    })
    .await;
    assert_eq!(body(&command)["command"], "review");
    session.close().await;
}

#[tokio::test]
async fn v1_compact_posts_summarize() {
    let mut script = script(|request| {
        if request.target.contains("/summarize") {
            return FakeResponse::json(200, "true");
        }
        if request.method == "POST" && request.target.contains("/session") {
            return FakeResponse::json(200, r#"{"id":"ses_1"}"#);
        }
        FakeResponse::json(200, "{}")
    })
    .await;
    let session = OpenCodeV1Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        &script.server.url,
        None,
        None,
    )
    .await
    .unwrap();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::Auto),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.start_turn(turn(thread, "/compact")).await.unwrap();
    let summarize = await_request(&mut script.server, |request| {
        request.target.contains("/summarize")
    })
    .await;
    assert_eq!(body(&summarize)["providerID"], "openai");
    session.close().await;
}

#[tokio::test]
async fn v1_accept_for_session_replies_always() {
    let mut script = script(|request| {
        if request.target.contains("prompt_async") {
            return FakeResponse::empty(204);
        }
        if request.target.contains("/permission/") {
            return FakeResponse::json(200, "true");
        }
        if request.method == "POST" && request.target.contains("/session") {
            return FakeResponse::json(200, r#"{"id":"ses_1"}"#);
        }
        FakeResponse::json(200, "{}")
    })
    .await;
    let session = OpenCodeV1Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        &script.server.url,
        None,
        None,
    )
    .await
    .unwrap();
    let mut events = session.events();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::Auto),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.start_turn(turn(thread, "run it")).await.unwrap();
    script
        .frames
        .send(Some(sse(
            r#"{"type":"permission.asked","properties":{"id":"perm_1","sessionID":"ses_1","permission":"bash"}}"#,
        )))
        .unwrap();
    let request_id = next_request(&mut events).await;
    session
        .respond_to_runtime_request(ProviderAdapterV2RuntimeRequestResponseInput {
            request_id,
            decision: Some(ProviderApprovalDecision::AcceptForSession),
            answers: None,
            response: None,
        })
        .await
        .unwrap();
    let reply = await_request(&mut script.server, |request| {
        request.target.contains("/permission/")
    })
    .await;
    assert_eq!(body(&reply)["reply"], "always");
    session.close().await;
}

#[tokio::test]
async fn v2_prompt_writes_instructions_and_has_no_system_field() {
    let mut script = script(|request| {
        if request.method == "PUT" && request.target.contains("instructions/entries/t3-code") {
            return FakeResponse::empty(204);
        }
        if request.method == "PATCH" {
            return FakeResponse::empty(204);
        }
        if request.target.contains("/prompt") {
            return FakeResponse::json(200, r#"{"data":{}}"#);
        }
        if request.method == "GET" && request.target.contains("/api/model") {
            return FakeResponse::json(200, r#"{"location":{},"data":[]}"#);
        }
        if request.method == "POST" && request.target.contains("/api/session") {
            return FakeResponse::json(200, r#"{"data":{"id":"ses_1"}}"#);
        }
        FakeResponse::json(200, r#"{"data":{}}"#)
    })
    .await;
    let client = V2Client::connect(&script.server.url, "pw").unwrap();
    let session = OpenCodeV2Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        client,
        Some("openai/gpt"),
    )
    .unwrap();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: Some("Thread".to_owned()),
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::Auto),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.start_turn(turn(thread, "hello")).await.unwrap();
    let instructions = await_request(&mut script.server, |request| {
        request.target.contains("instructions/entries")
    })
    .await;
    assert!(
        body(&instructions)["value"]
            .as_str()
            .unwrap()
            .contains("OpenCode")
    );
    let prompt = await_request(&mut script.server, |request| {
        request.target.contains("/prompt")
    })
    .await;
    let body = body(&prompt);
    assert!(body.get("system").is_none());
    assert!(
        body["id"]
            .as_str()
            .unwrap()
            .starts_with("msg_t3_turn_ses_1:")
    );
    assert_eq!(body["text"], "hello");
    session.close().await;
}

#[tokio::test]
async fn v2_decline_posts_the_note_then_rejects_with_the_message() {
    let mut script = script(|request| {
        if request.method == "PUT" || request.method == "PATCH" {
            return FakeResponse::empty(204);
        }
        if request.target.contains("/synthetic") || request.target.contains("/prompt") {
            return FakeResponse::json(200, r#"{"data":{}}"#);
        }
        if request.target.contains("/reply") {
            return FakeResponse::empty(204);
        }
        if request.method == "GET" && request.target.contains("/api/model") {
            return FakeResponse::json(200, r#"{"location":{},"data":[]}"#);
        }
        if request.method == "POST" && request.target.contains("/api/session") {
            return FakeResponse::json(200, r#"{"data":{"id":"ses_1"}}"#);
        }
        FakeResponse::json(200, r#"{"data":{}}"#)
    })
    .await;
    let client = V2Client::connect(&script.server.url, "pw").unwrap();
    let session = OpenCodeV2Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        client,
        None,
    )
    .unwrap();
    let mut events = session.events();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::Auto),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.start_turn(turn(thread, "edit")).await.unwrap();
    script
        .frames
        .send(Some(sse(
            r#"{"id":"evt1","type":"permission.asked","data":{"id":"perm_1","sessionID":"ses_1","action":"edit","resources":["a.ts"],"save":["*"]}}"#,
        )))
        .unwrap();
    let request_id = next_request(&mut events).await;
    session
        .respond_to_runtime_request(ProviderAdapterV2RuntimeRequestResponseInput {
            request_id,
            decision: Some(ProviderApprovalDecision::Decline),
            answers: None,
            response: None,
        })
        .await
        .unwrap();
    let synthetic = await_request(&mut script.server, |request| {
        request.target.contains("/synthetic")
    })
    .await;
    assert!(
        body(&synthetic)["text"]
            .as_str()
            .unwrap()
            .contains("declined")
    );
    assert_eq!(body(&synthetic)["resume"], false);
    let reply = await_request(&mut script.server, |request| {
        request.target.contains("/reply")
    })
    .await;
    assert_eq!(body(&reply)["decision"], "reject");
    assert_eq!(body(&reply)["message"], "The user declined this request.");
    session.close().await;
}

#[tokio::test]
async fn v2_accept_for_session_writes_a_grant_and_replies_once() {
    let mut script = script(|request| {
        if request.method == "PUT" || request.method == "PATCH" {
            return FakeResponse::empty(204);
        }
        if request.target.contains("/prompt") {
            return FakeResponse::json(200, r#"{"data":{}}"#);
        }
        if request.target.contains("/reply") {
            return FakeResponse::empty(204);
        }
        if request.method == "GET" && request.target.contains("/api/model") {
            return FakeResponse::json(200, r#"{"location":{},"data":[]}"#);
        }
        if request.method == "POST" && request.target.contains("/api/session") {
            return FakeResponse::json(200, r#"{"data":{"id":"ses_1"}}"#);
        }
        FakeResponse::json(200, r#"{"data":{}}"#)
    })
    .await;
    let client = V2Client::connect(&script.server.url, "pw").unwrap();
    let session = OpenCodeV2Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        client,
        None,
    )
    .unwrap();
    let mut events = session.events();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::Auto),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.start_turn(turn(thread, "edit")).await.unwrap();
    script
        .frames
        .send(Some(sse(
            r#"{"id":"evt1","type":"permission.asked","data":{"id":"perm_1","sessionID":"ses_1","action":"edit","resources":["a.ts"],"save":["*"]}}"#,
        )))
        .unwrap();
    let request_id = next_request(&mut events).await;
    session
        .respond_to_runtime_request(ProviderAdapterV2RuntimeRequestResponseInput {
            request_id,
            decision: Some(ProviderApprovalDecision::AcceptForSession),
            answers: None,
            response: None,
        })
        .await
        .unwrap();
    let patch = await_request(&mut script.server, |request| {
        if request.method != "PATCH" {
            return false;
        }
        let parsed = body(request);
        parsed
            .get("permissions")
            .and_then(Value::as_array)
            .is_some_and(|rules| {
                rules
                    .iter()
                    .any(|rule| rule["action"] == "edit" && rule["effect"] == "allow")
            })
    })
    .await;
    let parsed = body(&patch);
    assert!(
        parsed["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["action"] == "edit" && rule["effect"] == "allow")
    );
    let reply = await_request(&mut script.server, |request| {
        request.target.contains("/reply")
    })
    .await;
    assert_eq!(body(&reply)["decision"], "once");
    session.close().await;
}

#[tokio::test]
async fn v2_session_created_links_a_subagent() {
    let script = script(|request| {
        if request.method == "PUT" || request.method == "PATCH" {
            return FakeResponse::empty(204);
        }
        if request.method == "GET" && request.target.contains("/api/model") {
            return FakeResponse::json(200, r#"{"location":{},"data":[]}"#);
        }
        if request.method == "POST" && request.target.contains("/api/session") {
            return FakeResponse::json(200, r#"{"data":{"id":"ses_1"}}"#);
        }
        FakeResponse::json(200, r#"{"data":{}}"#)
    })
    .await;
    let client = V2Client::connect(&script.server.url, "pw").unwrap();
    let session = OpenCodeV2Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        client,
        None,
    )
    .unwrap();
    let mut events = session.events();
    let thread = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::FullAccess),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.start_turn(turn(thread, "delegate")).await.unwrap();
    script
        .frames
        .send(Some(sse(
            r#"{"id":"evt2","type":"session.created","data":{"id":"child_1","parentID":"ses_1"}}"#,
        )))
        .unwrap();
    let found = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = events.next().await {
            if let Ok(ProviderAdapterV2Event::SubagentUpdated { subagent, .. }) = event {
                return subagent
                    .native_task_ref
                    .as_ref()
                    .and_then(|native| native.native_id.as_ref())
                    .map(|id| id.as_str().to_owned());
            }
        }
        None
    })
    .await
    .unwrap();
    assert_eq!(found.as_deref(), Some("child_1"));
    session.close().await;
}

#[tokio::test]
async fn a_failed_probe_selects_v1_capabilities() {
    let adapter = crate::OpenCodeAdapter::new(
        ProviderInstanceId::new("opencode").unwrap(),
        crate::OpenCodeSettings::default(),
    );
    let caps = adapter.get_capabilities().await.unwrap();
    assert!(!caps.sessions.supports_multiple_provider_threads_per_session);
    let forced = crate::OpenCodeAdapter::connecting(
        ProviderInstanceId::new("opencode").unwrap(),
        OpenCodeGeneration::V2,
        "http://127.0.0.1:9",
        "pw",
    );
    assert!(
        forced
            .get_capabilities()
            .await
            .unwrap()
            .sessions
            .supports_multiple_provider_threads_per_session
    );
}

async fn next_request(events: &mut ProviderEventStream) -> turin_contracts::RuntimeRequestId {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = events.next().await {
            if let Ok(ProviderAdapterV2Event::RuntimeRequestUpdated {
                runtime_request, ..
            }) = event
            {
                return runtime_request.id;
            }
        }
        panic!("event stream ended");
    })
    .await
    .expect("runtime request")
}

#[tokio::test]
async fn adapter_live_smoke_creates_a_session_without_a_prompt() {
    if std::env::var("TURIN_OPENCODE_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let binary = std::env::var("OPENCODE_BIN").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{home}/.opencode/bin/opencode")
    });
    let directory =
        std::env::temp_dir().join(format!("t3-opencode-adapter-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let adapter = crate::OpenCodeAdapter::new(
        ProviderInstanceId::new("opencode").unwrap(),
        crate::OpenCodeSettings {
            enabled: true,
            binary_path: binary,
            server_url: String::new(),
            server_password: String::new(),
        },
    );
    let runtime = adapter
        .open_session(ProviderAdapterV2OpenSessionInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            provider_session_id: ProviderSessionId::new("provider-session").unwrap(),
            model_selection: model(),
            runtime_policy: ProviderAdapterV2RuntimePolicy {
                cwd: Some(directory.to_string_lossy().into_owned()),
                ..policy(RuntimeMode::FullAccess)
            },
            resume_from_session: None,
            initial_native_thread_id: None,
            initial_provider_item_identity_version: None,
        })
        .await
        .unwrap();
    let thread = runtime
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_1").unwrap(),
            title: Some("smoke".to_owned()),
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::FullAccess),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    assert!(
        thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .is_some()
    );
    drop(runtime);
}
