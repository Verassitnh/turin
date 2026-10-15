//! Port of upstream/packages/provider-opencode/src/server/v2/textGeneration.ts (OpenCode 2.x).
//!
//! Text generation on an OpenCode 2 server. Both of its `generate` endpoints answer 403
//! on OpenCode's free tier, so each request runs as a prompt in a temporary session, as
//! 1.x does, and the session is removed afterwards. The reply arrives on the event
//! stream, which is subscribed before the session is created.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::git::TextGenerationError;
use turin_contracts::js_trim;
use turin_contracts::model::ProviderOptionSelectionValue;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_opencode::{
    EventStreamError, OpenCode2Server, OpenCodeRuntimeError, V2Client, V2Event, V2StreamEvent,
    open_code_runtime_error_detail,
};

use crate::codex_text_generation::AttachmentPathResolver;
use crate::opencode_text_generation::parse_open_code_model_slug;
use crate::text_generation_operations::{
    Operation, Request, Runner, RunnerTextGeneration, investigation_unsupported,
};

/// A generation that takes this long is stuck; its session is stopped and removed.
const GENERATION_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// Upstream `Effect.timeout("5 seconds")` on the session removal in the finalizer.
const SESSION_REMOVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Reply to every permission ask. Nothing in a text generation needs a tool.
const TOOLS_REJECTED_MESSAGE: &str = "Tools are not available while generating text.";
const STREAM_LOST_DETAIL: &str = "The OpenCode event stream was lost.";

/// The event stream a session subscription yields (upstream `connection.events`).
pub type OpenCode2EventStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<V2StreamEvent, EventStreamError>> + Send>>;

/// The OpenCode 2 calls a generation makes. `V2Client` implements it for production;
/// tests replay a recorded transcript through it.
#[async_trait]
pub trait OpenCode2Api: Send + Sync {
    /// Upstream `client.session.create`. Returns the session body.
    async fn create_session(&self, body: &Value) -> Result<Value, OpenCodeRuntimeError>;
    /// Upstream `client.session.prompt`.
    async fn prompt_session(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError>;
    /// Upstream `client.session.interrupt`.
    async fn interrupt_session(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError>;
    /// Upstream `client.session.remove`.
    async fn remove_session(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError>;
    /// Upstream `client.permission.reply`.
    async fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError>;
    /// Upstream `connection.events`: a stream that is live once the subscription is accepted.
    async fn subscribe(&self) -> Result<OpenCode2EventStream, OpenCodeRuntimeError>;
}

#[async_trait]
impl OpenCode2Api for V2Client {
    async fn create_session(&self, body: &Value) -> Result<Value, OpenCodeRuntimeError> {
        V2Client::create_session(self, body).await
    }

    async fn prompt_session(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError> {
        V2Client::prompt(self, session_id, body).await
    }

    async fn interrupt_session(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        V2Client::interrupt(self, session_id, None)
            .await
            .map(|_| ())
    }

    /// BLOCKED on turin-provider-opencode: `V2Client` has no `DELETE /api/session/{id}`
    /// yet, and the local server password is not exposed to callers, so this cannot
    /// remove the session. The finalizer ignores the error, as upstream ignores its
    /// failure, so each generation leaves its temporary session behind until the client
    /// gains a remove method.
    async fn remove_session(&self, _session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        Err(OpenCodeRuntimeError::new(
            "session.remove",
            "OpenCode 2 session removal is not available in this build.",
        ))
    }

    async fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        V2Client::reply_permission(self, session_id, request_id, body).await
    }

    async fn subscribe(&self) -> Result<OpenCode2EventStream, OpenCodeRuntimeError> {
        V2Client::events(self).await
    }
}

/// How a generation ended. A failure's cause is the provider's own error, kept out of `detail`.
#[derive(Debug, Clone)]
enum Outcome {
    Text(String),
    Failed {
        detail: String,
        cause: Option<Value>,
    },
}

/// Upstream `Deferred<Outcome>`: the first settle wins, later ones are ignored.
#[derive(Clone)]
struct Deferred {
    sender: Arc<Mutex<Option<oneshot::Sender<Outcome>>>>,
}

impl Deferred {
    fn new() -> (Self, oneshot::Receiver<Outcome>) {
        let (sender, receiver) = oneshot::channel();
        (
            Self {
                sender: Arc::new(Mutex::new(Some(sender))),
            },
            receiver,
        )
    }

    fn settle(&self, outcome: Outcome) {
        let sender = self.sender.lock().expect("deferred lock").take();
        if let Some(sender) = sender {
            let _ = sender.send(outcome);
        }
    }
}

/// What the event consumer records while the generation runs.
#[derive(Default)]
struct Transcript {
    session_id: Option<String>,
    /// Upstream `Map<ordinal key, text>`: insertion order kept, updates keep the slot.
    texts: Vec<(String, String)>,
}

/// Errors inside a generation. `Failed` is upstream's `TextGenerationError`, which passes
/// through; `Runtime` is any other failure, which upstream reports with the generic detail.
#[derive(Debug)]
enum OpenCode2Error {
    Failed {
        detail: String,
        cause: Option<Value>,
    },
    Runtime(OpenCodeRuntimeError),
}

impl From<OpenCodeRuntimeError> for OpenCode2Error {
    fn from(error: OpenCodeRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// The `Runner` for OpenCode 2.x: one temporary session per request, on a lent connection.
pub struct OpenCode2Runner {
    server: Arc<OpenCode2Server>,
    resolve_attachment_path: AttachmentPathResolver,
}

/// Upstream `make()` for OpenCode 2. The caller owns the server. Each generation borrows
/// its connection for as long as its session lives, so a spawned server cannot go idle
/// while a generation runs.
pub fn open_code2_text_generation(
    server: Arc<OpenCode2Server>,
    resolve_attachment_path: AttachmentPathResolver,
) -> RunnerTextGeneration<OpenCode2Runner> {
    RunnerTextGeneration::new(
        "OpenCode",
        OpenCode2Runner {
            server,
            resolve_attachment_path,
        },
    )
}

#[async_trait]
impl Runner for OpenCode2Runner {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
        // Upstream `withoutInvestigation("OpenCode", ...)`.
        if request.investigate.is_some() {
            return Err(investigation_unsupported(request.operation, "OpenCode"));
        }
        let operation = request.operation;
        let request = &request;
        let resolve_attachment_path = &self.resolve_attachment_path;
        self.server
            .with_connection(|connection| {
                let api: Arc<dyn OpenCode2Api> = Arc::new(connection.client);
                async move { run_on_api(api, request, resolve_attachment_path).await }
            })
            .await
            .map_err(|error| to_text_generation_error(operation, error))
    }
}

/// Upstream `runOnServer`: the raw reply text, with the session removed on every path after creation.
async fn run_on_api(
    api: Arc<dyn OpenCode2Api>,
    request: &Request,
    resolve_attachment_path: &AttachmentPathResolver,
) -> Result<String, OpenCode2Error> {
    let model =
        parse_open_code_model_slug(request.model_selection.model.as_str()).ok_or_else(|| {
            OpenCode2Error::Failed {
                detail: "OpenCode model selection must use the 'provider/model' format.".to_owned(),
                cause: None,
            }
        })?;
    let variant = string_option(&request.model_selection, "variant");
    let (outcome, outcome_receiver) = Deferred::new();
    let transcript = Arc::new(Mutex::new(Transcript::default()));

    // Subscribed first: the stream is volatile, and the reply arrives on it.
    let events = api.subscribe().await?;
    let consumer = tokio::spawn(consume_events(
        events,
        Arc::clone(&api),
        Arc::clone(&transcript),
        outcome.clone(),
    ));
    // Upstream `forkScoped`: the consumer stops when this generation's scope ends.
    let _consumer = AbortOnDrop(consumer);

    let mut model_ref = json!({ "providerID": model.provider_id, "id": model.model_id });
    if let Some(variant) = variant {
        model_ref["variant"] = json!(variant);
    }
    let session = api
        .create_session(&json!({
            "title": format!("T3 Code {}", request.operation.as_str()),
            "location": { "directory": request.cwd },
            "model": model_ref,
            "permissions": [{ "action": "*", "resource": "*", "effect": "ask" }],
        }))
        .await?;
    let Some(session_id) = session.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return Err(OpenCode2Error::Runtime(OpenCodeRuntimeError::new(
            "session.create",
            "OpenCode 2 session.create returned no session id.",
        )));
    };
    transcript.lock().expect("transcript lock").session_id = Some(session_id.clone());

    // Upstream `addFinalizer`: removal runs after the generation on success and failure alike.
    let result = generate_in_session(
        &api,
        &session_id,
        request,
        resolve_attachment_path,
        outcome_receiver,
    )
    .await;
    let _ = tokio::time::timeout(SESSION_REMOVE_TIMEOUT, api.remove_session(&session_id)).await;
    result
}

/// Prompts the session and waits for the outcome, interrupting it after the timeout.
async fn generate_in_session(
    api: &Arc<dyn OpenCode2Api>,
    session_id: &str,
    request: &Request,
    resolve_attachment_path: &AttachmentPathResolver,
    outcome_receiver: oneshot::Receiver<Outcome>,
) -> Result<String, OpenCode2Error> {
    let images: Vec<Value> = request
        .attachments
        .iter()
        .filter_map(|attachment| {
            let ChatAttachment::Image(image) = attachment else {
                return None;
            };
            let path = resolve_attachment_path(attachment)?;
            Some(json!({
                "uri": format!("file://{}", path.to_string_lossy()),
                "name": image.name.as_str(),
            }))
        })
        .collect();
    let mut body = json!({ "text": request.prompt });
    if !images.is_empty() {
        body["files"] = json!(images);
    }
    api.prompt_session(session_id, &body).await?;

    let outcome = match tokio::time::timeout(GENERATION_TIMEOUT, outcome_receiver).await {
        Ok(Ok(outcome)) => outcome,
        // The consumer settles before it ends. A dropped sender means nothing ever will.
        Ok(Err(_)) => Outcome::Failed {
            detail: STREAM_LOST_DETAIL.to_owned(),
            cause: None,
        },
        Err(_) => {
            // Upstream `Effect.ignore`: a failed interrupt does not change the outcome.
            let _ = api.interrupt_session(session_id).await;
            Outcome::Failed {
                detail: "OpenCode did not finish generating in time.".to_owned(),
                cause: None,
            }
        }
    };
    match outcome {
        Outcome::Failed { detail, cause } => Err(OpenCode2Error::Failed { detail, cause }),
        Outcome::Text(text) if text.is_empty() => Err(OpenCode2Error::Failed {
            detail: "OpenCode returned empty output.".to_owned(),
            cause: None,
        }),
        Outcome::Text(text) => Ok(text),
    }
}

/// Upstream's `Stream.runForEach` fiber. Every end of the stream, clean or not, settles the outcome.
async fn consume_events(
    mut events: OpenCode2EventStream,
    api: Arc<dyn OpenCode2Api>,
    transcript: Arc<Mutex<Transcript>>,
    outcome: Deferred,
) {
    while let Some(item) = events.next().await {
        match item {
            Ok(event) => handle_event(event, api.as_ref(), &transcript, &outcome).await,
            Err(error) => {
                // The reply only arrives on this stream, so a lost stream ends the wait.
                outcome.settle(Outcome::Failed {
                    detail: STREAM_LOST_DETAIL.to_owned(),
                    cause: Some(Value::String(error.to_string())),
                });
                return;
            }
        }
    }
    outcome.settle(Outcome::Failed {
        detail: STREAM_LOST_DETAIL.to_owned(),
        cause: Some(Value::String("The stream ended.".to_owned())),
    });
}

async fn handle_event(
    event: V2StreamEvent,
    api: &dyn OpenCode2Api,
    transcript: &Mutex<Transcript>,
    outcome: &Deferred,
) {
    match event {
        V2StreamEvent::UnreadableExecutionEnded {
            execution_type,
            session_id,
        } => {
            // An execution end a newer server sent in a shape this build cannot read still
            // ends the generation, as it ends a turn in the adapter.
            if current_session(transcript).as_deref() != Some(session_id.as_str()) {
                return;
            }
            outcome.settle(if execution_type == "session.execution.succeeded" {
                Outcome::Text(joined_text(transcript))
            } else {
                Outcome::Failed {
                    detail: "OpenCode ended the generation in a way this version cannot read."
                        .to_owned(),
                    cause: Some(json!({
                        "type": execution_type,
                        "sessionID": session_id,
                    })),
                }
            });
        }
        // Upstream ignores the unreadable start, as it ignores any event it does not handle.
        V2StreamEvent::UnreadableExecutionStarted { .. } => {}
        V2StreamEvent::Event(event) => handle_typed_event(*event, api, transcript, outcome).await,
    }
}

async fn handle_typed_event(
    event: V2Event,
    api: &dyn OpenCode2Api,
    transcript: &Mutex<Transcript>,
    outcome: &Deferred,
) {
    // Events before the session id is known, and events for other sessions, are not ours.
    let Some(session_id) = current_session(transcript) else {
        return;
    };
    if event.data.get("sessionID").and_then(Value::as_str) != Some(session_id.as_str()) {
        return;
    }
    match event.event_type.as_str() {
        "session.text.ended" => {
            let key = format!(
                "{}:{}",
                js_display(event.data.get("assistantMessageID")),
                js_display(event.data.get("ordinal")),
            );
            let text = event
                .data
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            set_text(transcript, key, text);
        }
        "permission.asked" => {
            let request_id = js_display(event.data.get("id"));
            let body = json!({ "decision": "reject", "message": TOOLS_REJECTED_MESSAGE });
            // Upstream `Effect.ignore({ log: true })`.
            if let Err(error) = api.reply_permission(&session_id, &request_id, &body).await {
                tracing::warn!(
                    detail = %open_code_runtime_error_detail(&error),
                    "Could not reject an OpenCode permission ask during text generation."
                );
            }
        }
        "session.execution.succeeded" => {
            outcome.settle(Outcome::Text(joined_text(transcript)));
        }
        "session.execution.failed" => outcome.settle(Outcome::Failed {
            detail: "OpenCode could not generate the text.".to_owned(),
            cause: event.data.get("error").cloned(),
        }),
        "session.execution.interrupted" => outcome.settle(Outcome::Failed {
            detail: "OpenCode stopped the generation.".to_owned(),
            cause: None,
        }),
        _ => {}
    }
}

fn current_session(transcript: &Mutex<Transcript>) -> Option<String> {
    transcript
        .lock()
        .expect("transcript lock")
        .session_id
        .clone()
}

fn set_text(transcript: &Mutex<Transcript>, key: String, text: String) {
    let mut transcript = transcript.lock().expect("transcript lock");
    match transcript
        .texts
        .iter_mut()
        .find(|(existing, _)| *existing == key)
    {
        Some(entry) => entry.1 = text,
        None => transcript.texts.push((key, text)),
    }
}

/// Upstream `[...texts.values()].join("\n").trim()`.
fn joined_text(transcript: &Mutex<Transcript>) -> String {
    let transcript = transcript.lock().expect("transcript lock");
    let joined = transcript
        .texts
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    js_trim(&joined).to_owned()
}

/// JS string conversion for the event fields that build a key or id. A missing field reads
/// as `undefined`, as it does in upstream's template literal.
fn js_display(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) => "null".to_owned(),
        Some(other) => other.to_string(),
    }
}

/// `getModelSelectionStringOptionValue`.
fn string_option<'a>(selection: &'a ModelSelection, id: &str) -> Option<&'a str> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|option| option.id.as_str() == id)
        .and_then(|option| match &option.value {
            ProviderOptionSelectionValue::Str(value) => Some(value.as_str()),
            ProviderOptionSelectionValue::Bool(_) => None,
        })
}

fn failure(operation: Operation, detail: &str, cause: Option<Value>) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: detail.to_owned(),
        cause,
    }
}

/// Upstream `mapError`: a `TextGenerationError` passes through, and any other failure gets
/// the generic detail with the runtime error as its cause.
fn to_text_generation_error(operation: Operation, error: OpenCode2Error) -> TextGenerationError {
    match error {
        OpenCode2Error::Failed { detail, cause } => failure(operation, &detail, cause),
        OpenCode2Error::Runtime(cause) => failure(
            operation,
            "OpenCode text generation failed.",
            Some(json!({
                "_tag": "OpenCodeRuntimeError",
                "operation": cause.operation,
                "detail": open_code_runtime_error_detail(&cause),
            })),
        ),
    }
}

/// Aborts a spawned task when its owner goes out of scope.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
#[path = "opencode2_text_generation_tests.rs"]
mod tests;
