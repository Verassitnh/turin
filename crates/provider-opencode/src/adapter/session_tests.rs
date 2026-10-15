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

#[tokio::test]
async fn v1_resubscribes_with_last_event_id_after_a_transport_error() {
    let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&opens);
    let mut server = FakeServer::spawn(move |request| {
        if request.method == "GET" && request.target.starts_with("/event") {
            if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                let mut response = FakeResponse::sse(
                    vec![
                        b"retry: 10\nid: 41\ndata: {\"type\":\"server.connected\",\"properties\":{}}\n\n"
                            .to_vec(),
                    ],
                    false,
                );
                response.truncated = true;
                return response;
            }
            return FakeResponse::sse(
                vec![sse(r#"{"type":"server.connected","properties":{}}"#)],
                true,
            );
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
        &server.url,
        None,
        Some("openai/gpt"),
    )
    .await
    .unwrap();
    let is_subscribe =
        |request: &Recorded| request.method == "GET" && request.target.starts_with("/event");
    let first = await_request(&mut server, is_subscribe).await;
    assert_eq!(first.last_event_id, None);
    let second = await_request(&mut server, is_subscribe).await;
    assert_eq!(second.last_event_id.as_deref(), Some("41"));
    session.close().await;
}

#[tokio::test]
async fn v2_unload_forgets_an_idle_thread_and_keeps_a_busy_one() {
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
    session
        .start_turn(turn(thread.clone(), "busy"))
        .await
        .unwrap();
    session
        .unload_thread(&thread)
        .expect("v2 implements unload_thread")
        .await
        .unwrap();
    assert!(
        session
            .has_pending_background_work()
            .unwrap()
            .await
            .unwrap(),
        "a running turn must keep its thread"
    );
    session.close().await;

    let session = OpenCodeV2Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        V2Client::connect(&script.server.url, "pw").unwrap(),
        None,
    )
    .unwrap();
    let idle = session
        .ensure_thread(ProviderAdapterV2EnsureThreadInput {
            thread_id: ThreadId::new("thread_2").unwrap(),
            title: None,
            model_selection: model(),
            runtime_policy: policy(RuntimeMode::FullAccess),
            provider_session_id: None,
            existing_provider_thread: None,
        })
        .await
        .unwrap();
    session.unload_thread(&idle).unwrap().await.unwrap();
    assert!(
        session
            .start_turn(turn(idle, "after unload"))
            .await
            .is_err(),
        "an unloaded thread must be gone"
    );
    session.close().await;
}

/// Events from a session until `done` matches one, or the deadline passes. Returns them as JSON.
async fn events_until(
    events: &mut ProviderEventStream,
    done: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.next()).await {
        let value = serde_json::to_value(event.expect("event")).expect("event json");
        let finished = done(&value);
        seen.push(value);
        if finished {
            break;
        }
    }
    seen
}

fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == kind)
        .collect()
}

#[tokio::test]
async fn v1_text_reasoning_tool_and_todo_emit_node_message_and_plan_records() {
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
    session.start_turn(turn(thread, "hi")).await.unwrap();
    await_request(&mut script.server, |request| {
        request.target.contains("prompt_async")
    })
    .await;

    let frames = [
        json!({"type":"message.updated","properties":{"info":{"id":"msg_u","sessionID":"ses_1","role":"user"}}}),
        // The user's echoed text is the prompt. It must not become an assistant message.
        json!({"type":"message.part.updated","properties":{"part":{"id":"p_user","sessionID":"ses_1","messageID":"msg_u","type":"text","text":"hi"}}}),
        json!({"type":"message.updated","properties":{"info":{"id":"msg_a","sessionID":"ses_1","role":"assistant"}}}),
        json!({"type":"message.part.updated","properties":{"part":{"id":"p1","sessionID":"ses_1","messageID":"msg_a","type":"text","text":"hel","time":{"start":1760500000000i64}}}}),
        json!({"type":"message.part.delta","properties":{"sessionID":"ses_1","messageID":"msg_a","partID":"p1","field":"text","delta":"lo"}}),
        // A delta on another field is not projected.
        json!({"type":"message.part.delta","properties":{"sessionID":"ses_1","messageID":"msg_a","partID":"p1","field":"other","delta":"!"}}),
        json!({"type":"message.part.updated","properties":{"part":{"id":"r1","sessionID":"ses_1","messageID":"msg_a","type":"reasoning","text":"thinking","time":{"start":1760500000000i64,"end":1760500000500i64}}}}),
        json!({"type":"message.part.updated","properties":{"part":{"id":"t1","sessionID":"ses_1","messageID":"msg_a","type":"tool","callID":"c1","tool":"bash","state":{"status":"completed","input":{"command":"ls"},"output":"x","title":"ls","time":{"start":1760500000000i64,"end":1760500001000i64}}}}}),
        json!({"type":"todo.updated","properties":{"sessionID":"ses_1","todos":[{"content":"a","status":"completed"},{"content":"b","status":"in_progress"}]}}),
        json!({"type":"message.updated","properties":{"info":{"id":"msg_a","sessionID":"ses_1","role":"assistant","time":{"created":1760500000000i64,"completed":1760500002000i64}}}}),
    ];
    for frame in frames {
        script.frames.send(Some(sse(&frame.to_string()))).unwrap();
    }
    // The item is the last of the three emissions for p1, after its node and message.
    let seen = events_until(&mut events, |value| {
        value["type"] == "turn_item.updated"
            && value["turn_item"]["nodeId"] == json!(derive_node("p1"))
            && value["turn_item"]["status"] == "completed"
    })
    .await;

    let p1_node = derive_node("p1");
    let p1_nodes: Vec<&Value> = of_type(&seen, "node.updated")
        .into_iter()
        .filter(|event| event["node"]["id"] == json!(p1_node))
        .collect();
    let statuses: Vec<&str> = p1_nodes
        .iter()
        .map(|event| event["node"]["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["running", "running", "completed"], "{seen:#?}");
    assert!(
        p1_nodes
            .iter()
            .all(|event| event["node"]["kind"] == "assistant_message")
    );
    assert!(
        p1_nodes
            .iter()
            .all(|event| event["node"]["countsForRun"] == false)
    );
    assert_eq!(p1_nodes[0]["node"]["parentNodeId"], json!(derive_root()));

    let messages: Vec<&Value> = of_type(&seen, "message.updated")
        .into_iter()
        .filter(|event| event["message"]["id"] == json!(derive_message("p1")))
        .collect();
    let texts: Vec<&str> = messages
        .iter()
        .map(|event| event["message"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["hel", "hello", "hello"], "{seen:#?}");
    assert_eq!(messages.last().unwrap()["message"]["streaming"], false);
    assert_eq!(messages[0]["message"]["role"], "assistant");
    assert_eq!(messages[0]["message"]["createdBy"], "agent");
    assert_eq!(messages[0]["message"]["creationSource"], "provider");
    assert!(
        of_type(&seen, "message.updated")
            .iter()
            .all(
                |event| event["message"]["id"] != json!(derive_message("p_user"))
                    && event["message"]["id"] != json!(derive_message("r1"))
            ),
        "user echo and reasoning must not become messages"
    );

    let p1_items: Vec<&Value> = of_type(&seen, "turn_item.updated")
        .into_iter()
        .filter(|event| event["turn_item"]["nodeId"] == json!(p1_node))
        .collect();
    assert_eq!(p1_items.len(), 3, "{seen:#?}");
    let ordinals: Vec<&Value> = p1_items
        .iter()
        .map(|event| &event["turn_item"]["ordinal"])
        .collect();
    assert!(
        ordinals.windows(2).all(|pair| pair[0] == pair[1]),
        "ordinal renumbered: {ordinals:?}"
    );

    let reasoning = of_type(&seen, "node.updated")
        .into_iter()
        .find(|event| event["node"]["id"] == json!(derive_node("r1")))
        .expect("reasoning node");
    assert_eq!(reasoning["node"]["kind"], "reasoning");
    assert_eq!(reasoning["node"]["status"], "completed");

    let tool = of_type(&seen, "node.updated")
        .into_iter()
        .find(|event| event["node"]["id"] == json!(derive_node("t1")))
        .expect("tool node");
    assert_eq!(tool["node"]["kind"], "tool_call");
    assert_eq!(tool["node"]["status"], "completed");

    let todo_node = of_type(&seen, "node.updated")
        .into_iter()
        .find(|event| event["node"]["kind"] == "todo_list")
        .expect("todo node");
    assert_eq!(todo_node["node"]["status"], "running");
    let plan = of_type(&seen, "plan.updated")
        .into_iter()
        .find(|event| event["plan"]["kind"] == "todo_list")
        .expect("plan");
    assert_eq!(plan["plan"]["status"], "active");
    assert_eq!(plan["plan"]["nodeId"], todo_node["node"]["id"]);
    let steps = plan["plan"]["steps"].as_array().unwrap();
    assert_eq!(steps[0]["status"], "completed");
    assert_eq!(steps[1]["status"], "running");
    assert_eq!(steps[1]["text"], "b");
    let todo_item = of_type(&seen, "turn_item.updated")
        .into_iter()
        .find(|event| event["turn_item"]["type"] == "todo_list")
        .expect("todo item");
    assert_eq!(todo_item["turn_item"]["planId"], plan["plan"]["id"]);
    session.close().await;
}

fn derive_node(native: &str) -> String {
    turin_provider_core::id_allocator::derive::node_from_provider_item(
        &crate::adapter::logic::opencode_driver(),
        native,
    )
    .as_str()
    .to_owned()
}

fn derive_message(native: &str) -> String {
    turin_provider_core::id_allocator::derive::message_from_provider_item(
        &crate::adapter::logic::opencode_driver(),
        native,
    )
    .as_str()
    .to_owned()
}

fn derive_root() -> String {
    "node_1".to_owned()
}

#[tokio::test]
async fn v2_text_blocks_and_tools_emit_node_and_message_records() {
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
    session.start_turn(turn(thread, "hi")).await.unwrap();

    let frames = [
        json!({"id":"e1","type":"session.text.started","data":{"sessionID":"ses_1","assistantMessageID":"msg_a","ordinal":0}}),
        json!({"id":"e2","type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_a","ordinal":0,"delta":"hel"}}),
        json!({"id":"e3","type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_a","ordinal":0,"delta":"lo"}}),
        json!({"id":"e4","type":"session.reasoning.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_a","ordinal":1,"delta":"hm"}}),
        json!({"id":"e5","type":"session.tool.success","data":{"sessionID":"ses_1","id":"call_1","name":"bash","status":"completed","input":{"command":"ls"},"output":"x"}}),
        json!({"id":"e6","type":"session.text.ended","data":{"sessionID":"ses_1","assistantMessageID":"msg_a","ordinal":0,"text":"hello"}}),
    ];
    for frame in frames {
        script.frames.send(Some(sse(&frame.to_string()))).unwrap();
    }
    let text_node = derive_node("msg_a:text:0");
    let seen = events_until(&mut events, |value| {
        value["type"] == "turn_item.updated"
            && value["turn_item"]["nodeId"] == json!(text_node)
            && value["turn_item"]["status"] == "completed"
    })
    .await;

    let text_nodes: Vec<&Value> = of_type(&seen, "node.updated")
        .into_iter()
        .filter(|event| event["node"]["id"] == json!(text_node))
        .collect();
    let statuses: Vec<&str> = text_nodes
        .iter()
        .map(|event| event["node"]["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["running", "running", "completed"], "{seen:#?}");
    let messages: Vec<&str> = of_type(&seen, "message.updated")
        .into_iter()
        .filter(|event| event["message"]["id"] == json!(derive_message("msg_a:text:0")))
        .map(|event| event["message"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(messages, ["hel", "hello", "hello"], "{seen:#?}");
    let ended_message = of_type(&seen, "message.updated")
        .into_iter()
        .rev()
        .find(|event| event["message"]["id"] == json!(derive_message("msg_a:text:0")))
        .unwrap();
    assert_eq!(ended_message["message"]["streaming"], false);

    let reasoning = of_type(&seen, "node.updated")
        .into_iter()
        .find(|event| event["node"]["id"] == json!(derive_node("msg_a:reasoning:1")))
        .expect("reasoning node");
    assert_eq!(reasoning["node"]["kind"], "reasoning");
    assert!(
        of_type(&seen, "message.updated")
            .iter()
            .all(|event| event["message"]["id"] != json!(derive_message("msg_a:reasoning:1"))),
        "reasoning is not a message"
    );

    let tool = of_type(&seen, "node.updated")
        .into_iter()
        .find(|event| event["node"]["id"] == json!(derive_node("call_1")))
        .expect("tool node");
    assert_eq!(tool["node"]["kind"], "tool_call");
    assert_eq!(tool["node"]["status"], "completed");
    session.close().await;
}

#[tokio::test]
async fn v2_approval_request_emits_a_waiting_node_at_the_approval_id() {
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
    let session = OpenCodeV2Session::open(
        ProviderInstanceId::new("opencode").unwrap(),
        ProviderSessionId::new("provider-session").unwrap(),
        "/work",
        V2Client::connect(&script.server.url, "pw").unwrap(),
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
    session.start_turn(turn(thread, "hi")).await.unwrap();
    script
        .frames
        .send(Some(sse(
            &json!({"id":"e1","type":"permission.asked","data":{"sessionID":"ses_1","id":"perm_1","permission":"bash","patterns":["ls"]}}).to_string(),
        )))
        .unwrap();
    let seen = events_until(&mut events, |value| {
        value["type"] == "runtime_request.updated"
    })
    .await;
    let node = of_type(&seen, "node.updated")
        .into_iter()
        .find(|event| event["node"]["kind"] == "approval_request")
        .expect("approval node");
    assert_eq!(node["node"]["status"], "waiting");
    let request = of_type(&seen, "runtime_request.updated")
        .into_iter()
        .next()
        .expect("request");
    assert_eq!(request["runtime_request"]["nodeId"], node["node"]["id"]);
    session.close().await;
}
