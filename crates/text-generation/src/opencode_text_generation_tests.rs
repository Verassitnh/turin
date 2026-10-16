//! Port of upstream/packages/provider-opencode/src/server/textGeneration.test.ts.
//!
//! The upstream test replaces the runtime with a double. Here the text generator
//! runs over real HTTP against an in-process fake OpenCode 1.x server, so request
//! bodies, Basic auth, and version checks all go through `V1Client` and the connect
//! path. Tests that need a spawned binary or the owner's idle timer are skipped; see
//! the list at the bottom of this file.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_core::text_generation::{
    CommitMessageGenerationInput, ProviderTextGeneration, ThreadTitleGenerationInput,
};
use turin_provider_opencode::{OpenCodeServerOwner, OwnerConfig};

use super::{
    OpenCodeRunner, OpenCodeTextGenerationSettings, ServerEndpoint, open_code_text_generation,
    parse_open_code_model_slug, run_against_server,
};
use crate::codex_text_generation::AttachmentPathResolver;
use crate::text_generation_operations::{Operation, Request, RunnerTextGeneration};

const DEFAULT_TEST_MODEL: &str = "openai/gpt-5";
const CWD: &str = "/work/repo";
/// `opencode:secret-password` in Basic auth.
const SECRET_AUTH: &str = "Basic b3BlbmNvZGU6c2VjcmV0LXBhc3N3b3Jk";

fn selection(model: &str, options: Value) -> ModelSelection {
    let mut value = json!({ "instanceId": "opencode", "model": model });
    if !options.is_null() {
        value["options"] = options;
    }
    serde_json::from_value(value).expect("model selection")
}

fn default_selection() -> ModelSelection {
    selection(DEFAULT_TEST_MODEL, Value::Null)
}

fn image(id: &str, name: &str) -> ChatAttachment {
    serde_json::from_value(json!({
        "type": "image",
        "id": id,
        "name": name,
        "mimeType": "image/png",
        "sizeBytes": 3,
    }))
    .expect("image attachment")
}

fn file(id: &str, name: &str) -> ChatAttachment {
    serde_json::from_value(json!({
        "type": "file",
        "id": id,
        "name": name,
        "mimeType": "application/pdf",
        "sizeBytes": 42,
    }))
    .expect("file attachment")
}

fn commit_input() -> CommitMessageGenerationInput {
    CommitMessageGenerationInput {
        cwd: CWD.to_owned(),
        branch: Some("feature/opencode-reuse".to_owned()),
        staged_summary: "M README.md".to_owned(),
        staged_patch: "diff --git a/README.md b/README.md".to_owned(),
        include_branch: false,
        model_selection: default_selection(),
    }
}

fn commit_message_reply() -> Value {
    json!({
        "parts": [{
            "type": "text",
            "text": json!({
                "subject": "Improve OpenCode reuse",
                "body": "Reuse one server for the full action.",
            })
            .to_string(),
        }],
    })
}

/// The attachment path the composition root would resolve: `/tmp/attachments/<name>`.
fn resolver() -> AttachmentPathResolver {
    Arc::new(|attachment| match attachment {
        ChatAttachment::Image(image) => {
            Some(PathBuf::from("/tmp/attachments").join(image.name.as_str()))
        }
        _ => None,
    })
}

/// A server whose owner is never spawned: every test here uses a configured URL or a lent endpoint.
fn owner() -> OpenCodeServerOwner {
    OpenCodeServerOwner::new(OwnerConfig {
        binary_path: "fake-opencode".to_owned(),
        directory: CWD.to_owned(),
        server_password: None,
        environment: None,
        idle: None,
        shutdown_grace: None,
        verify: None,
        ledger: None,
    })
}

fn text_generation(
    server_url: &str,
    server_password: Option<&str>,
) -> RunnerTextGeneration<OpenCodeRunner> {
    open_code_text_generation(
        OpenCodeTextGenerationSettings {
            binary_path: "fake-opencode".to_owned(),
            server_url: server_url.to_owned(),
            server_password: server_password.map(str::to_owned),
        },
        owner(),
        resolver(),
    )
}

fn raw_request(model_selection: ModelSelection, prompt: &str) -> Request {
    Request {
        operation: Operation::GenerateCommitMessage,
        cwd: CWD.to_owned(),
        prompt: prompt.to_owned(),
        output_schema: json!({}),
        model_selection,
        attachments: Vec::new(),
        investigate: None,
    }
}

// --- In-process fake OpenCode 1.x server -------------------------------------------

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Value,
}

#[derive(Debug, Clone)]
struct Reply {
    status: u16,
    body: Value,
}

fn reply(status: u16, body: Value) -> Reply {
    Reply { status, body }
}

/// What the fake answers for each route. Unlisted routes are 404.
#[derive(Clone)]
struct Script {
    version: &'static str,
    session: Reply,
    prompt: Reply,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            version: "1.14.19",
            session: reply(200, json!({ "id": "ses_test" })),
            prompt: reply(200, commit_message_reply()),
        }
    }
}

impl Script {
    fn answer(&self, request: &Recorded) -> Reply {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/global/health") => {
                reply(200, json!({ "healthy": true, "version": self.version }))
            }
            ("POST", "/session") => self.session.clone(),
            ("POST", path) if path.starts_with("/session/") && path.ends_with("/message") => {
                self.prompt.clone()
            }
            _ => reply(404, json!({ "name": "NotFound" })),
        }
    }
}

struct FakeOpenCode {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl FakeOpenCode {
    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("requests").clone()
    }

    /// Bodies of `POST /session/{id}/message`, in order.
    fn prompt_bodies(&self) -> Vec<Value> {
        self.requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.path.ends_with("/message"))
            .map(|request| request.body)
            .collect()
    }

    fn session_creates(&self) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.method == "POST" && request.path == "/session")
            .count()
    }
}

async fn serve(script: Script) -> FakeOpenCode {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let script = Arc::new(script);
    let recorded = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let script = Arc::clone(&script);
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move { handle_connection(stream, script, recorded).await });
        }
    });
    FakeOpenCode { url, requests }
}

async fn handle_connection(
    mut stream: TcpStream,
    script: Arc<Script>,
    recorded: Arc<Mutex<Vec<Recorded>>>,
) {
    let mut buffer = Vec::new();
    loop {
        let Some(header_end) = read_until(&mut stream, &mut buffer, b"\r\n\r\n").await else {
            return;
        };
        let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
        let body_start = header_end + 4;
        let content_length = head
            .lines()
            .skip(1)
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buffer.len() < body_start + content_length {
            let mut chunk = [0u8; 4096];
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        let body_bytes = buffer[body_start..body_start + content_length].to_vec();
        buffer.drain(..body_start + content_length);

        let mut request_line = head.lines().next().unwrap_or("").split_whitespace();
        let method = request_line.next().unwrap_or("").to_owned();
        let path = request_line
            .next()
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("")
            .to_owned();
        let authorization = head.lines().skip(1).find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_owned())
        });
        let request = Recorded {
            method,
            path,
            authorization,
            body: serde_json::from_slice(&body_bytes).unwrap_or(Value::Null),
        };
        recorded.lock().expect("recorded").push(request.clone());

        let answer = script.answer(&request);
        let payload = answer.body.to_string();
        let response = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            answer.status,
            if answer.status == 200 { "OK" } else { "Error" },
            payload.len(),
            payload
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// Reads until `marker` is in the buffer; returns its offset, or `None` when the peer closed.
async fn read_until(stream: &mut TcpStream, buffer: &mut Vec<u8>, marker: &[u8]) -> Option<usize> {
    loop {
        if let Some(position) = buffer
            .windows(marker.len())
            .position(|window| window == marker)
        {
            return Some(position);
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

// --- Tests ----------------------------------------------------------------------------

#[tokio::test]
async fn excludes_generic_files_from_thread_title_generation() {
    let fake = serve(Script {
        prompt: reply(
            200,
            json!({ "parts": [{ "type": "text", "text": "{\"title\":\"Review uploaded report\"}" }] }),
        ),
        ..Script::default()
    })
    .await;
    let generation = text_generation(&fake.url, None);

    generation
        .generate_thread_title(ThreadTitleGenerationInput {
            linked_context: None,
            cwd: CWD.to_owned(),
            message: "Review these attachments.".to_owned(),
            previous_title: None,
            attachments: vec![
                image("thread-image-attachment", "screenshot.png"),
                file("thread-report-attachment-pdf", "report.pdf"),
            ],
            model_selection: default_selection(),
        })
        .await
        .expect("title");

    let prompts = fake.prompt_bodies();
    let parts = prompts[0]["parts"].as_array().expect("parts");
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[1]["type"], "file");
    assert_eq!(parts[1]["filename"], "screenshot.png");
    assert_eq!(parts[1]["mime"], "image/png");
    assert_eq!(parts[1]["url"], "file:///tmp/attachments/screenshot.png");
}

/// Ported with a lent endpoint instead of a spawned binary. The spawned branch
/// only differs in where the URL and password come from (the owner's process).
#[tokio::test]
async fn passes_configured_authentication_to_a_locally_spawned_server() {
    let fake = serve(Script::default()).await;
    let model = parse_open_code_model_slug(DEFAULT_TEST_MODEL).expect("model");

    run_against_server(
        ServerEndpoint {
            url: &fake.url,
            server_password: Some("secret-password"),
        },
        &raw_request(default_selection(), "prompt"),
        &model,
        &[],
    )
    .await
    .expect("reply");

    let auth: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|request| request.method == "POST")
        .map(|request| request.authorization)
        .collect();
    assert_eq!(
        auth,
        vec![Some(SECRET_AUTH.to_owned()), Some(SECRET_AUTH.to_owned())]
    );
}

#[tokio::test]
async fn preserves_the_sdk_cause_when_session_creation_fails() {
    let fake = serve(Script {
        session: reply(500, json!({ "name": "UnknownError" })),
        ..Script::default()
    })
    .await;

    let error = text_generation(&fake.url, None)
        .generate_commit_message(commit_input())
        .await
        .expect_err("session failure");

    let (operation, detail, cause) = text_generation_error(error);
    assert_eq!(operation, "generateCommitMessage");
    assert_eq!(detail, "OpenCode session.create request failed.");
    let cause = cause.expect("cause");
    assert_eq!(cause["_tag"], "OpenCodeTextGenerationSessionRequestError");
    assert_eq!(cause["operation"], "generateCommitMessage");
    assert_eq!(cause["cwd"], CWD);
    // The SDK's own failure stays under `cause`, as upstream keeps the original error.
    assert_eq!(cause["cause"]["_tag"], "OpenCodeRuntimeError");
    assert!(
        cause["cause"]["detail"]
            .as_str()
            .unwrap_or("")
            .contains("500"),
        "{cause}"
    );
}

#[tokio::test]
async fn reports_a_missing_session_payload_without_manufacturing_a_cause() {
    let fake = serve(Script {
        session: reply(200, json!({})),
        ..Script::default()
    })
    .await;

    let error = text_generation(&fake.url, None)
        .generate_commit_message(commit_input())
        .await
        .expect_err("missing payload");

    let (_, detail, cause) = text_generation_error(error);
    assert_eq!(
        detail,
        "OpenCode session.create returned no session payload."
    );
    let cause = cause.expect("cause");
    assert_eq!(cause["_tag"], "OpenCodeTextGenerationSessionPayloadError");
    assert_eq!(cause["operation"], "generateCommitMessage");
    assert_eq!(cause["cwd"], CWD);
    assert!(cause.get("cause").is_none(), "{cause}");
}

#[tokio::test]
async fn preserves_the_sdk_cause_and_request_context_when_prompting_fails() {
    let fake = serve(Script {
        prompt: reply(500, json!({ "name": "UnknownError" })),
        ..Script::default()
    })
    .await;

    let error = text_generation(&fake.url, None)
        .generate_commit_message(commit_input())
        .await
        .expect_err("prompt failure");

    let (operation, detail, cause) = text_generation_error(error);
    assert_eq!(operation, "generateCommitMessage");
    assert_eq!(detail, "OpenCode session.prompt request failed.");
    let cause = cause.expect("cause");
    assert_eq!(cause["_tag"], "OpenCodeTextGenerationPromptRequestError");
    assert_eq!(cause["operation"], "generateCommitMessage");
    assert_eq!(cause["cwd"], CWD);
    assert_eq!(cause["sessionId"], "ses_test");
    assert_eq!(cause["providerId"], "openai");
    assert_eq!(cause["modelId"], "gpt-5");
    assert_eq!(cause["cause"]["_tag"], "OpenCodeRuntimeError");
    assert!(
        cause["cause"]["detail"]
            .as_str()
            .unwrap_or("")
            .contains("500"),
        "{cause}"
    );
}

#[tokio::test]
async fn returns_a_typed_empty_output_error_for_malformed_and_blank_response_parts() {
    let fake = serve(Script {
        prompt: reply(
            200,
            json!({ "parts": [null, { "type": "tool" }, { "type": "text", "text": "   " }] }),
        ),
        ..Script::default()
    })
    .await;

    let error = text_generation(&fake.url, None)
        .generate_commit_message(commit_input())
        .await
        .expect_err("empty output");

    let (_, detail, cause) = text_generation_error(error);
    assert_eq!(detail, "OpenCode returned empty output.");
    let cause = cause.expect("cause");
    assert_eq!(cause["_tag"], "OpenCodeTextGenerationEmptyOutputError");
    assert_eq!(cause["operation"], "generateCommitMessage");
    assert_eq!(cause["cwd"], CWD);
    assert_eq!(cause["sessionId"], "ses_test");
    assert_eq!(cause["providerId"], "openai");
    assert_eq!(cause["modelId"], "gpt-5");
    assert_eq!(cause["responsePartCount"], 3);
    assert_eq!(cause["textPartCount"], 1);
    assert!(cause.get("cause").is_none(), "{cause}");
}

#[tokio::test]
async fn parses_json_returned_as_plain_text_output() {
    let fake = serve(Script {
        prompt: reply(
            200,
            json!({
                "parts": [{
                    "type": "text",
                    "text": "Here is the result:\n{\"subject\":\"Tighten OpenCode parsing\",\"body\":\"Handle JSON text output locally.\"}",
                }],
            }),
        ),
        ..Script::default()
    })
    .await;

    let result = text_generation(&fake.url, None)
        .generate_commit_message(commit_input())
        .await
        .expect("commit message");

    assert_eq!(result.subject, "Tighten OpenCode parsing");
    assert_eq!(result.body, "Handle JSON text output locally.");
}

#[tokio::test]
async fn surfaces_the_upstream_opencode_structured_output_error_message() {
    let fake = serve(Script {
        prompt: reply(
            200,
            json!({
                "info": {
                    "error": {
                        "name": "StructuredOutputError",
                        "data": {
                            "message": "Model did not produce structured output",
                            "retries": 2,
                        },
                    },
                },
            }),
        ),
        ..Script::default()
    })
    .await;

    let error = text_generation(&fake.url, None)
        .generate_commit_message(commit_input())
        .await
        .expect_err("structured output error");

    let (_, detail, cause) = text_generation_error(error);
    assert_eq!(detail, "Model did not produce structured output");
    let cause = cause.expect("cause");
    assert_eq!(cause["_tag"], "OpenCodeTextGenerationPromptResponseError");
    assert_eq!(cause["operation"], "generateCommitMessage");
    assert_eq!(cause["cwd"], CWD);
    assert_eq!(cause["sessionId"], "ses_test");
    assert_eq!(cause["providerId"], "openai");
    assert_eq!(cause["modelId"], "gpt-5");
    assert_eq!(cause["providerErrorName"], "StructuredOutputError");
    assert_eq!(
        cause["providerMessage"],
        "Model did not produce structured output"
    );
    assert!(cause.get("cause").is_none(), "{cause}");
}

/// Upstream runs this with `OPENCODE_SERVER_PASSWORD` set in the environment. The
/// Rust options carry no environment, and a configured server never reads one, so the
/// check here is only that no Authorization header is sent without a configured password.
#[tokio::test]
async fn does_not_send_a_local_environment_password_to_a_configured_server() {
    let fake = serve(Script::default()).await;

    text_generation(&fake.url, None)
        .generate_commit_message(commit_input())
        .await
        .expect("commit message");

    let auth: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|request| request.method == "POST")
        .map(|request| request.authorization)
        .collect();
    assert_eq!(auth, vec![None, None]);
}

#[tokio::test]
async fn does_not_create_a_session_when_the_server_version_is_unsupported() {
    let fake = serve(Script {
        version: "1.14.18",
        ..Script::default()
    })
    .await;

    let error = text_generation(&fake.url, Some("secret-password"))
        .generate_commit_message(commit_input())
        .await
        .expect_err("unsupported version");

    let (_, detail, _) = text_generation_error(error);
    assert!(detail.contains("v1.14.18 is too old"), "{detail}");
    assert_eq!(fake.session_creates(), 0);
}

#[tokio::test]
async fn reuses_a_configured_opencode_server_url_for_back_to_back_requests() {
    let fake = serve(Script::default()).await;
    let generation = text_generation(&fake.url, Some("secret-password"));

    generation
        .generate_commit_message(commit_input())
        .await
        .expect("first");
    generation
        .generate_commit_message(commit_input())
        .await
        .expect("second");

    let prompts = fake.prompt_bodies();
    assert_eq!(prompts.len(), 2);
    let auth: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|request| request.method == "POST" && request.path.ends_with("/message"))
        .map(|request| request.authorization)
        .collect();
    assert_eq!(
        auth,
        vec![Some(SECRET_AUTH.to_owned()), Some(SECRET_AUTH.to_owned())]
    );
}

/// Pulls `(operation, detail, cause)` out of a `TextGenerationError`.
fn text_generation_error(
    error: turin_contracts::git::TextGenerationError,
) -> (String, String, Option<Value>) {
    match error {
        turin_contracts::git::TextGenerationError::TextGenerationError {
            operation,
            detail,
            cause,
        } => (operation, detail, cause),
    }
}

// Upstream tests not ported here, and why:
// - "uses an environment-only password for a locally spawned server" and
//   "uses settings auth when the local environment password differs": they check how
//   `OPENCODE_SERVER_PASSWORD` is resolved when the runtime spawns a process. That is
//   `start_open_code_server_process` in turin-provider-opencode, not text generation.
// - "reuses a warm server across back-to-back requests and closes it after idling" and
//   "starts a new server after the warm server idles out": they test
//   `OpenCodeServerOwner`'s spawn, borrow count, and 30s idle timer. The owner lives in
//   turin-provider-opencode and these tests would need a real `opencode` binary.
// - The idle-TTL half of "reuses a configured OpenCode server URL ..." is also owner
//   behavior and is not repeated here. The URL-reuse and auth half is ported above.
