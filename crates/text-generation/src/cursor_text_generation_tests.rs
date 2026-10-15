//! Port of upstream/packages/provider-cursor/src/server/textGeneration.test.ts, over ACP.
//!
//! The fake is a scripted JSON-RPC peer, not `turin_acp::AcpAgent`. AcpAgent answers only
//! `initialize`, `session/new`, and `session/prompt` through its core table. It has no way to
//! answer `session/set_config_option`, which carries the model selection, so the peer answers
//! every method itself and records the traffic. The real connector is covered by one test
//! that runs a shell script in place of `cursor-agent`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::Notify;
use turin_acp::AcpClient;
use turin_contracts::git::TextGenerationError;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_core::text_generation::{
    CommitMessageGenerationInput, ProviderTextGeneration, ThreadTitleGenerationInput,
};

use super::{
    CursorAcpConnector, CursorAgentProcess, CursorApiKeyResolver, CursorRunner,
    CursorTextGenerationOptions, WithoutInvestigation, cursor_config_updates,
    cursor_text_generation, cursor_text_generation_with_connector,
};
use crate::text_generation_operations::{
    Investigate, Operation, Request, Runner, RunnerTextGeneration,
};

const SESSION_ID: &str = "session-1";
const PERMISSION_REQUEST_ID: &str = "perm-1";
const TEST_KEY: &str = "test-cursor-key";
const SIGN_IN_DETAIL: &str = "Sign in with Cursor or add CURSOR_API_KEY in provider settings.";
const GENERIC_DETAIL: &str = "Cursor SDK text generation failed.";
const TIMEOUT_DETAIL: &str = "Cursor SDK request timed out.";
const CANCELLED_DETAIL: &str = "Cursor SDK request was cancelled.";
const ERROR_DETAIL: &str = "Cursor SDK request finished with an error.";
const EMPTY_DETAIL: &str = "Cursor SDK returned empty output.";
const SANDBOX_DETAIL: &str = "Cursor text generation cannot enforce workspace isolation with a custom ~/.cursor/sandbox.json. Use another text-generation provider.";
const DISABLED_DETAIL: &str = "Cursor is disabled in T3 Code settings.";

/// How the fake peer answers. The defaults make a successful `end_turn` run with no text.
#[derive(Clone)]
struct Script {
    chunks: Vec<String>,
    stop_reason: String,
    ask_permission: bool,
    hang_create: bool,
    fail_create: bool,
    hang_prompt: bool,
    fail_prompt: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            stop_reason: "end_turn".to_owned(),
            ask_permission: false,
            hang_create: false,
            fail_create: false,
            hang_prompt: false,
            fail_prompt: false,
        }
    }
}

impl Script {
    /// A run whose streamed text is `reply`.
    fn replying(reply: &str) -> Self {
        Self {
            chunks: vec![reply.to_owned()],
            ..Self::default()
        }
    }
}

/// One `connect` call: where the child would run, and what environment it gets.
struct Connect {
    cwd: PathBuf,
    cwd_was_empty: bool,
    env: BTreeMap<String, String>,
}

#[derive(Default)]
struct Calls {
    connects: Vec<Connect>,
    create_sessions: Vec<Value>,
    config_updates: Vec<Value>,
    modes: Vec<Value>,
    prompts: Vec<Value>,
    cancels: Vec<Value>,
    permission_replies: Vec<Value>,
}

#[derive(Default)]
struct Shared {
    calls: Mutex<Calls>,
    /// Permit-based, so a cancel that lands before anyone waits is not lost.
    cancel_seen: Notify,
}

struct FakeConnector {
    script: Script,
    shared: Arc<Shared>,
    fail: bool,
}

#[async_trait]
impl CursorAcpConnector for FakeConnector {
    async fn connect(
        &self,
        cwd: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<AcpClient, String> {
        {
            let mut calls = self.shared.calls.lock().unwrap();
            calls.connects.push(Connect {
                cwd: cwd.to_path_buf(),
                cwd_was_empty: std::fs::read_dir(cwd).unwrap().next().is_none(),
                env: environment.clone(),
            });
        }
        if self.fail {
            return Err("spawn failed".to_owned());
        }
        let (client_read, peer_write) = tokio::io::duplex(1 << 20);
        let (peer_read, client_write) = tokio::io::duplex(1 << 20);
        tokio::spawn(serve_peer(
            peer_read,
            peer_write,
            self.script.clone(),
            Arc::clone(&self.shared),
        ));
        Ok(AcpClient::connect(client_read, client_write))
    }
}

/// The peer side of the connection. Requests are answered in arrival order, and a permission
/// request blocks the prompt answer until the client replies, as the real agent does.
async fn serve_peer(
    read: DuplexStream,
    mut write: DuplexStream,
    script: Script,
    shared: Arc<Shared>,
) {
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Responses are only expected for the peer's own requests, which are read inline below.
        let Some(method) = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let id = frame.get("id").cloned();
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                send(
                    &mut write,
                    reply(id, json!({"protocolVersion": 1, "agentCapabilities": {}})),
                )
                .await;
            }
            "session/new" => {
                shared.calls.lock().unwrap().create_sessions.push(params);
                if script.hang_create {
                    continue;
                }
                let answer = if script.fail_create {
                    failure(id, "session/new failed")
                } else {
                    reply(id, json!({"sessionId": SESSION_ID, "configOptions": []}))
                };
                send(&mut write, answer).await;
            }
            "session/set_config_option" => {
                shared.calls.lock().unwrap().config_updates.push(params);
                send(&mut write, reply(id, json!({"configOptions": []}))).await;
            }
            "session/set_mode" => {
                shared.calls.lock().unwrap().modes.push(params);
                send(&mut write, reply(id, json!({}))).await;
            }
            "session/cancel" => {
                shared.calls.lock().unwrap().cancels.push(params);
                shared.cancel_seen.notify_one();
            }
            "session/prompt" => {
                shared.calls.lock().unwrap().prompts.push(params);
                if script.hang_prompt {
                    continue;
                }
                if script.fail_prompt {
                    send(&mut write, failure(id, "prompt failed")).await;
                    continue;
                }
                for chunk in &script.chunks {
                    send(
                        &mut write,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": SESSION_ID,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": chunk},
                                },
                            },
                        }),
                    )
                    .await;
                }
                if script.ask_permission {
                    send(
                        &mut write,
                        json!({
                            "jsonrpc": "2.0",
                            "id": PERMISSION_REQUEST_ID,
                            "method": "session/request_permission",
                            "params": {
                                "sessionId": SESSION_ID,
                                "toolCall": {"toolCallId": "tool-1", "title": "Run"},
                                "options": [
                                    {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                                    {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                                ],
                            },
                        }),
                    )
                    .await;
                    while let Ok(Some(next)) = lines.next_line().await {
                        let Ok(answer) = serde_json::from_str::<Value>(&next) else {
                            continue;
                        };
                        if answer.get("id") == Some(&json!(PERMISSION_REQUEST_ID))
                            && answer.get("method").is_none()
                        {
                            shared.calls.lock().unwrap().permission_replies.push(answer);
                            break;
                        }
                    }
                }
                send(
                    &mut write,
                    reply(id, json!({"stopReason": script.stop_reason})),
                )
                .await;
            }
            _ => {
                if id.is_some() {
                    send(
                        &mut write,
                        failure_with_code(id, -32601, "method not found"),
                    )
                    .await;
                }
            }
        }
    }
}

fn reply(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn failure(id: Option<Value>, message: &str) -> Value {
    failure_with_code(id, -32000, message)
}

fn failure_with_code(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

async fn send(write: &mut DuplexStream, frame: Value) {
    let mut bytes = serde_json::to_vec(&frame).expect("frame");
    bytes.push(b'\n');
    write.write_all(&bytes).await.expect("peer write");
}

/// One test's world: a fake connector with its script, and an empty home directory.
struct Harness {
    shared: Arc<Shared>,
    connector: Arc<FakeConnector>,
    home: TempDir,
}

impl Harness {
    fn new(script: Script) -> Self {
        Self::with_connect_failure(script, false)
    }

    fn with_connect_failure(script: Script, fail: bool) -> Self {
        let shared = Arc::new(Shared::default());
        Self {
            connector: Arc::new(FakeConnector {
                script,
                shared: Arc::clone(&shared),
                fail,
            }),
            shared,
            home: tempfile::tempdir().expect("home"),
        }
    }

    fn options(&self, environment: BTreeMap<String, String>) -> CursorTextGenerationOptions {
        CursorTextGenerationOptions {
            enabled: true,
            binary_path: String::new(),
            environment: Some(environment),
            resolve_api_key: None,
            home_dir: self.home_dir(),
        }
    }

    fn home_dir(&self) -> String {
        self.home.path().to_string_lossy().into_owned()
    }

    fn generation(
        &self,
        environment: BTreeMap<String, String>,
    ) -> RunnerTextGeneration<WithoutInvestigation<CursorRunner>> {
        cursor_text_generation_with_connector(self.options(environment), self.connector.clone())
    }

    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.shared.calls.lock().unwrap()
    }
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn key_env() -> BTreeMap<String, String> {
    env(&[("CURSOR_API_KEY", TEST_KEY)])
}

fn selection(model: &str, options: Value) -> ModelSelection {
    let mut value = json!({ "instanceId": "cursor", "model": model });
    if !options.is_null() {
        value["options"] = options;
    }
    serde_json::from_value(value).expect("model selection")
}

fn commit_input(model_selection: ModelSelection) -> CommitMessageGenerationInput {
    CommitMessageGenerationInput {
        cwd: "/real-workspace".to_owned(),
        branch: Some("feature/cursor".to_owned()),
        staged_summary: "M README.md".to_owned(),
        staged_patch: "diff --git a/README.md b/README.md".to_owned(),
        include_branch: false,
        model_selection,
    }
}

fn title_input(model_selection: ModelSelection) -> ThreadTitleGenerationInput {
    ThreadTitleGenerationInput {
        linked_context: None,
        cwd: "/real-workspace".to_owned(),
        message: "Fix the reconnect spinner after a resumed session.".to_owned(),
        previous_title: None,
        attachments: Vec::new(),
        model_selection,
    }
}

fn detail_of(error: TextGenerationError) -> (String, String) {
    let TextGenerationError::TextGenerationError {
        operation, detail, ..
    } = error;
    (operation, detail)
}

#[tokio::test]
async fn resolves_the_browser_credential_for_every_request() {
    let harness = Harness::new(Script::replying(
        r#"{"subject":"Add generated commit message","body":""}"#,
    ));
    let key = Arc::new(Mutex::new("first-browser-key".to_owned()));
    let resolver_key = Arc::clone(&key);
    let resolve: CursorApiKeyResolver = Arc::new(move || {
        let key = Arc::clone(&resolver_key);
        Box::pin(async move { Ok(key.lock().unwrap().clone()) })
    });
    let generation = cursor_text_generation_with_connector(
        CursorTextGenerationOptions {
            resolve_api_key: Some(resolve),
            ..harness.options(BTreeMap::new())
        },
        harness.connector.clone(),
    );
    let input = || commit_input(selection("auto", serde_json::Value::Null));

    generation.generate_commit_message(input()).await.unwrap();
    *key.lock().unwrap() = "second-browser-key".to_owned();
    generation.generate_commit_message(input()).await.unwrap();

    let calls = harness.calls();
    assert_eq!(calls.connects.len(), 2);
    assert_eq!(calls.connects[0].env["CURSOR_API_KEY"], "first-browser-key");
    assert_eq!(
        calls.connects[1].env["CURSOR_API_KEY"],
        "second-browser-key"
    );
}

#[tokio::test]
async fn forwards_model_parameters_and_the_api_key_over_acp() {
    let harness = Harness::new(Script::replying(
        r#"{"subject":"Add generated commit message","body":"- verify cursor acp text generation"}"#,
    ));
    let generation = harness.generation(key_env());
    let model = selection(
        "gpt-5.4",
        json!([
            {"id": "thinking", "value": "high"},
            {"id": "contextWindow", "value": "1m"},
            {"id": "fastMode", "value": true},
        ]),
    );

    let generated = generation
        .generate_commit_message(commit_input(model))
        .await
        .unwrap();

    assert_eq!(generated.subject, "Add generated commit message");
    assert_eq!(generated.body, "- verify cursor acp text generation");
    let calls = harness.calls();
    assert_eq!(calls.connects.len(), 1);
    let connect = &calls.connects[0];
    assert_eq!(connect.env["CURSOR_API_KEY"], TEST_KEY);
    assert!(connect.cwd_was_empty, "the agent workspace starts empty");
    assert!(
        connect
            .cwd
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("t3-cursor-text-")
    );
    assert_eq!(calls.create_sessions.len(), 1);
    assert_eq!(
        calls.create_sessions[0]["cwd"],
        connect.cwd.to_string_lossy().as_ref()
    );
    assert_eq!(calls.create_sessions[0]["mcpServers"], json!([]));
    let updates: Vec<(&str, &str)> = calls
        .config_updates
        .iter()
        .map(|update| {
            assert_eq!(update["sessionId"], SESSION_ID);
            (
                update["configId"].as_str().unwrap(),
                update["value"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        updates,
        [
            ("model", "gpt-5.4"),
            ("thinking", "high"),
            ("context", "1m"),
            ("fast", "true"),
        ]
    );
    assert_eq!(
        calls.modes,
        [json!({"sessionId": SESSION_ID, "modeId": "plan"})]
    );
    assert_eq!(calls.prompts.len(), 1);
    assert_eq!(calls.prompts[0]["sessionId"], SESSION_ID);
    let prompt_text = calls.prompts[0]["prompt"][0]["text"].as_str().unwrap();
    assert_eq!(calls.prompts[0]["prompt"][0]["type"], "text");
    assert!(prompt_text.contains("Staged patch:"));
}

#[test]
fn maps_the_auto_model_to_the_default_and_option_ids_to_cursor_parameters() {
    // Upstream sdkModel.ts: `auto` is the SDK's `default`; contextWindow and fastMode are renamed.
    let auto = cursor_config_updates(&selection("auto", Value::Null));
    assert_eq!(auto, [("model".to_owned(), "default".to_owned())]);
    let options = cursor_config_updates(&selection(
        "composer-2",
        json!([{"id": "effort", "value": "low"}, {"id": "fastMode", "value": false}]),
    ));
    assert_eq!(
        options,
        [
            ("model".to_owned(), "composer-2".to_owned()),
            ("effort".to_owned(), "low".to_owned()),
            ("fast".to_owned(), "false".to_owned()),
        ]
    );
}

#[tokio::test]
async fn accepts_json_objects_with_extra_assistant_text_around_them() {
    let harness = Harness::new(Script::replying(
        "Sure, here is the JSON:\n```json\n{\n  \"subject\": \"Update README dummy comment with attribution and date\",\n  \"body\": \"\"\n}\n```\nDone.",
    ));
    let generation = harness.generation(key_env());

    let generated = generation
        .generate_commit_message(commit_input(selection("composer-2", Value::Null)))
        .await
        .unwrap();

    assert_eq!(
        generated.subject,
        "Update README dummy comment with attribution and date"
    );
    assert_eq!(generated.body, "");
}

#[tokio::test]
async fn generates_thread_titles_through_cursor_acp() {
    let harness = Harness::new(Script::replying(
        r#"{"title":"\"Trim reconnect spinner status after resume.\""}"#,
    ));
    let generation = harness.generation(key_env());

    let generated = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap();

    assert_eq!(
        generated.title,
        "Trim reconnect spinner status after resume."
    );
}

#[tokio::test]
async fn rejects_error_and_cancelled_runs_even_with_valid_title_json() {
    for (stop_reason, expected) in [("error", ERROR_DETAIL), ("cancelled", CANCELLED_DETAIL)] {
        let harness = Harness::new(Script {
            stop_reason: stop_reason.to_owned(),
            ..Script::replying(r#"{"title":"Partial title from a failed run."}"#)
        });
        let generation = harness.generation(key_env());

        let error = generation
            .generate_thread_title(title_input(selection("composer-2", Value::Null)))
            .await
            .unwrap_err();

        assert_eq!(
            detail_of(error),
            ("generateThreadTitle".to_owned(), expected.to_owned()),
            "stop reason {stop_reason}"
        );
    }
}

#[tokio::test]
async fn an_end_turn_with_no_text_is_empty_output() {
    let harness = Harness::new(Script::default());
    let generation = harness.generation(key_env());

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), EMPTY_DETAIL.to_owned())
    );
}

#[tokio::test]
async fn refuses_when_a_custom_sandbox_policy_could_widen_write_access() {
    let harness = Harness::new(Script::replying(r#"{"title":"Title"}"#));
    std::fs::create_dir_all(harness.home.path().join(".cursor")).unwrap();
    std::fs::write(harness.home.path().join(".cursor/sandbox.json"), "{}").unwrap();
    let generation = harness.generation(key_env());

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), SANDBOX_DETAIL.to_owned())
    );
    assert!(harness.calls().connects.is_empty(), "nothing may start");
}

#[tokio::test(start_paused = true)]
async fn cancels_the_session_when_the_reply_times_out() {
    let harness = Harness::new(Script {
        hang_prompt: true,
        ..Script::default()
    });
    let generation = harness.generation(key_env());

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), TIMEOUT_DETAIL.to_owned())
    );
    // The cancel is written before the timeout returns; the peer reads it as the runtime idles.
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.shared.cancel_seen.notified(),
    )
    .await
    .expect("the peer received session/cancel");
    let calls = harness.calls();
    assert_eq!(calls.cancels, [json!({"sessionId": SESSION_ID})]);
}

#[tokio::test(start_paused = true)]
async fn times_out_a_pending_session_new_without_cancelling_a_run() {
    let harness = Harness::new(Script {
        hang_create: true,
        ..Script::default()
    });
    let generation = harness.generation(key_env());

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), TIMEOUT_DETAIL.to_owned())
    );
    assert!(harness.calls().cancels.is_empty());
    assert!(harness.calls().prompts.is_empty());
}

#[tokio::test]
async fn a_failed_connection_gives_the_generic_detail_and_is_not_retried() {
    let harness = Harness::with_connect_failure(Script::default(), true);
    let generation = harness.generation(key_env());

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), GENERIC_DETAIL.to_owned())
    );
    assert_eq!(harness.calls().connects.len(), 1);
}

#[tokio::test]
async fn a_failed_session_new_gives_the_generic_detail() {
    let harness = Harness::new(Script {
        fail_create: true,
        ..Script::default()
    });
    let generation = harness.generation(key_env());

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), GENERIC_DETAIL.to_owned())
    );
    assert_eq!(harness.calls().connects.len(), 1);
    assert!(harness.calls().prompts.is_empty());
}

#[tokio::test]
async fn a_failed_prompt_gives_the_generic_detail() {
    let harness = Harness::new(Script {
        fail_prompt: true,
        ..Script::default()
    });
    let generation = harness.generation(key_env());

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), GENERIC_DETAIL.to_owned())
    );
}

#[tokio::test]
async fn answers_permission_requests_with_the_reject_option() {
    let harness = Harness::new(Script {
        ask_permission: true,
        ..Script::replying(r#"{"title":"Title"}"#)
    });
    let generation = harness.generation(key_env());

    generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap();

    let calls = harness.calls();
    assert_eq!(calls.permission_replies.len(), 1);
    assert_eq!(
        calls.permission_replies[0]["result"],
        json!({"outcome": {"outcome": "selected", "optionId": "reject"}})
    );
}

#[tokio::test]
async fn requires_cursor_api_key_before_starting_anything() {
    let harness = Harness::new(Script::default());
    let generation = harness.generation(BTreeMap::new());

    let error = generation
        .generate_commit_message(commit_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        (
            "generateCommitMessage".to_owned(),
            SIGN_IN_DETAIL.to_owned()
        )
    );
    assert!(harness.calls().connects.is_empty());
}

#[tokio::test]
async fn a_blank_api_key_counts_as_missing() {
    let harness = Harness::new(Script::default());
    let generation = harness.generation(env(&[("CURSOR_API_KEY", "   ")]));

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), SIGN_IN_DETAIL.to_owned())
    );
}

#[tokio::test]
async fn a_disabled_provider_refuses_before_resolving_the_key() {
    let harness = Harness::new(Script::default());
    let resolve: CursorApiKeyResolver =
        Arc::new(|| Box::pin(async { panic!("a disabled provider must not resolve a key") }));
    let generation = cursor_text_generation_with_connector(
        CursorTextGenerationOptions {
            enabled: false,
            resolve_api_key: Some(resolve),
            ..harness.options(key_env())
        },
        harness.connector.clone(),
    );

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), DISABLED_DETAIL.to_owned())
    );
    assert!(harness.calls().connects.is_empty());
}

#[tokio::test]
async fn a_failed_key_resolver_gives_the_generic_detail() {
    let harness = Harness::new(Script::default());
    let resolve: CursorApiKeyResolver =
        Arc::new(|| Box::pin(async { Err("keychain locked".to_owned()) }));
    let generation = cursor_text_generation_with_connector(
        CursorTextGenerationOptions {
            resolve_api_key: Some(resolve),
            ..harness.options(BTreeMap::new())
        },
        harness.connector.clone(),
    );

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), GENERIC_DETAIL.to_owned())
    );
    assert!(harness.calls().connects.is_empty());
}

#[tokio::test]
async fn investigations_are_refused_without_starting_a_run() {
    let harness = Harness::new(Script::default());
    let runner = WithoutInvestigation {
        inner: CursorRunner {
            options: harness.options(key_env()),
            connector: harness.connector.clone(),
        },
    };
    let request = Request {
        operation: Operation::GenerateReviewGuide,
        cwd: "/real-workspace".to_owned(),
        prompt: String::new(),
        output_schema: json!({}),
        model_selection: selection("composer-2", Value::Null),
        attachments: Vec::new(),
        investigate: Some(Investigate {
            timeout: Duration::from_secs(60),
        }),
    };

    let error = runner.run(request).await.unwrap_err();

    assert_eq!(
        detail_of(error),
        (
            "generateReviewGuide".to_owned(),
            "Cursor cannot investigate a checkout read-only. Choose another review guide model, or source control writer model when none is set.".to_owned(),
        )
    );
    assert!(harness.calls().connects.is_empty());
}

/// The real connector, with a shell script standing in for `cursor-agent`. It records the
/// working directory, the API key it was given, and its first argument, then exits. The exit
/// ends the connection, so the run fails with the generic detail, and the file is the evidence.
#[cfg(unix)]
#[tokio::test]
async fn spawns_cursor_agent_acp_in_an_empty_directory_with_the_request_environment() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("record.txt");
    let script = dir.path().join("fake-cursor-agent");
    std::fs::write(
        &script,
        "#!/bin/sh\npwd > \"$OUT\"\necho \"$CURSOR_API_KEY\" >> \"$OUT\"\necho \"$1\" >> \"$OUT\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = tempfile::tempdir().unwrap();
    let generation = cursor_text_generation(CursorTextGenerationOptions {
        enabled: true,
        binary_path: script.to_string_lossy().into_owned(),
        environment: Some(env(&[
            ("CURSOR_API_KEY", TEST_KEY),
            ("OUT", out.to_str().unwrap()),
        ])),
        resolve_api_key: None,
        home_dir: home.path().to_string_lossy().into_owned(),
    });

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), GENERIC_DETAIL.to_owned())
    );
    let recorded = std::fs::read_to_string(&out).unwrap();
    let mut lines = recorded.lines();
    assert!(lines.next().unwrap().contains("t3-cursor-text-"));
    assert_eq!(lines.next(), Some(TEST_KEY));
    assert_eq!(lines.next(), Some("acp"));
}

#[tokio::test]
async fn a_missing_binary_gives_the_generic_detail() {
    let home = tempfile::tempdir().unwrap();
    let generation = cursor_text_generation_with_connector(
        CursorTextGenerationOptions {
            enabled: true,
            binary_path: String::new(),
            environment: Some(key_env()),
            resolve_api_key: None,
            home_dir: home.path().to_string_lossy().into_owned(),
        },
        Arc::new(CursorAgentProcess {
            binary_path: "/nonexistent/turin-cursor-agent".to_owned(),
        }),
    );

    let error = generation
        .generate_thread_title(title_input(selection("composer-2", Value::Null)))
        .await
        .unwrap_err();

    assert_eq!(
        detail_of(error),
        ("generateThreadTitle".to_owned(), GENERIC_DETAIL.to_owned())
    );
}
