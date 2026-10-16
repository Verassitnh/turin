//! Port of upstream/apps/server/src/textGeneration/OpenCode2TextGeneration.test.ts.
//!
//! The recorded transcript is `testing/opencode2_title_generation.json`, converted from
//! `OpenCode2TextGeneration.fixture.ts` (2.0.18, `opencode/big-pickle`, recorded live).
//! Upstream replays it at the HTTP and SSE boundary. Here it replays at `OpenCode2Api`,
//! the seam over `V2Client`. SSE frames still go through `decode_v2_event`, so the
//! decoder's handling of each frame is the real one.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::git::TextGenerationError;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_core::text_generation::{ProviderTextGeneration, ThreadTitleGenerationInput};
use turin_provider_opencode::{EventStreamError, OpenCodeRuntimeError, SseFrame, decode_v2_event};

use super::{OpenCode2Api, OpenCode2EventStream, run_on_api, to_text_generation_error};
use crate::codex_text_generation::AttachmentPathResolver;
use crate::text_generation_operations::{Request, Runner, RunnerTextGeneration};

const RECORDED: &str = include_str!("../testing/opencode2_title_generation.json");
const SESSION_ID: &str = "ses_f0ec85cf6ffebfalpvV2E9nocu";
const TITLE_MESSAGE: &str = "fix the login redirect loop after oauth";
const CWD: &str = "/work/repo";

// --- Transcript replay ---------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Entry {
    ExpectOutbound {
        frame: Outbound,
    },
    EmitInbound {
        frame: Inbound,
    },
    /// The server stops. Its event stream ends; later requests still reach the transcript.
    RuntimeExit {},
}

#[derive(Deserialize)]
struct Outbound {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Inbound {
    #[serde(rename = "sdk.event")]
    Event { event: Value },
    #[serde(rename = "sdk.response")]
    Response { data: Value },
}

/// Replays a transcript at the `OpenCode2Api` seam.
///
/// Each call pops the next outbound expectation and checks it, then plays inbound
/// entries until the next expectation. The first `sdk.response` answers the call, and
/// `sdk.event` frames go to the event stream, which ends at `runtime_exit`.
struct Replay {
    state: Mutex<ReplayState>,
}

#[derive(Default)]
struct ReplayState {
    entries: VecDeque<Entry>,
    /// Raw SSE `data` of each event, sent to the subscription that is open.
    events: Option<mpsc::UnboundedSender<String>>,
}

fn replay(entries: Vec<Value>) -> Arc<Replay> {
    let entries = entries
        .into_iter()
        .map(|entry| serde_json::from_value(entry).expect("replay entry"))
        .collect();
    Arc::new(Replay {
        state: Mutex::new(ReplayState {
            entries,
            events: None,
        }),
    })
}

/// Upstream `<any>` is a wildcard for any value. Objects must have the same keys.
fn matches(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::String(expected), _) if expected == "<any>" => true,
        (Value::Object(expected), Value::Object(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .all(|(key, value)| actual.get(key).is_some_and(|other| matches(value, other)))
        }
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected.iter().zip(actual).all(|(e, a)| matches(e, a))
        }
        _ => expected == actual,
    }
}

impl Replay {
    /// Checks the next outbound request, then plays inbound entries. Returns the first `sdk.response`.
    fn exchange(&self, kind: &str, input: Option<Value>) -> Option<Value> {
        let mut state = self.state.lock().expect("replay lock");
        match state.entries.pop_front() {
            Some(Entry::ExpectOutbound { frame }) => {
                assert_eq!(frame.kind, kind, "replayed request type");
                if let Some(expected) = &frame.input {
                    let actual = input.unwrap_or(Value::Null);
                    assert!(
                        matches(expected, &actual),
                        "replayed {kind} input: expected {expected}, got {actual}"
                    );
                }
            }
            _ => panic!("replay expected an outbound request before {kind}"),
        }
        let mut response = None;
        while !matches!(
            state.entries.front(),
            None | Some(Entry::ExpectOutbound { .. })
        ) {
            match state.entries.pop_front() {
                Some(Entry::EmitInbound {
                    frame: Inbound::Event { event },
                }) => {
                    if let Some(events) = &state.events {
                        let _ = events.send(event.to_string());
                    }
                }
                Some(Entry::EmitInbound {
                    frame: Inbound::Response { data },
                }) => {
                    response.get_or_insert(data);
                }
                Some(Entry::RuntimeExit {}) => state.events = None,
                _ => unreachable!("the loop stops at an expectation"),
            }
        }
        response
    }

    /// Upstream checks at scope close that the transcript ran out.
    fn assert_exhausted(&self) {
        let state = self.state.lock().expect("replay lock");
        assert!(
            state.entries.is_empty(),
            "{} replayed entries left",
            state.entries.len()
        );
    }
}

/// The `session.create` and `session.create.response` answer, unwrapped as `V2Client` does.
fn unwrap_data(response: Option<Value>) -> Value {
    response
        .and_then(|response| response.get("data").cloned())
        .unwrap_or(Value::Null)
}

#[async_trait]
impl OpenCode2Api for Replay {
    async fn create_session(&self, body: &Value) -> Result<Value, OpenCodeRuntimeError> {
        Ok(unwrap_data(
            self.exchange("session.create", Some(body.clone())),
        ))
    }

    async fn prompt_session(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError> {
        let mut input = body.clone();
        input["sessionID"] = json!(session_id);
        Ok(unwrap_data(self.exchange("session.prompt", Some(input))))
    }

    async fn interrupt_session(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        self.exchange(
            "session.interrupt",
            Some(json!({ "sessionID": session_id })),
        );
        Ok(())
    }

    async fn remove_session(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        self.exchange("session.remove", Some(json!({ "sessionID": session_id })));
        Ok(())
    }

    async fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        let mut input = body.clone();
        input["sessionID"] = json!(session_id);
        input["requestID"] = json!(request_id);
        self.exchange("permission.reply", Some(input));
        Ok(())
    }

    async fn subscribe(&self) -> Result<OpenCode2EventStream, OpenCodeRuntimeError> {
        let (sender, receiver) = mpsc::unbounded_channel();
        {
            let mut state = self.state.lock().expect("replay lock");
            state.events = Some(sender);
        }
        self.exchange("event.subscribe", None);
        // Frames go through the production decoder; a frame it skips is never delivered.
        let stream = futures::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|data| (data, receiver))
        })
        .filter_map(|data| {
            std::future::ready(
                decode_v2_event(&SseFrame {
                    data,
                    event: None,
                    id: None,
                    retry: None,
                })
                .map(Ok::<_, EventStreamError>),
            )
        });
        Ok(Box::pin(stream))
    }
}

/// The `Runner` under test: the same core that `OpenCode2Runner` calls under `with_connection`.
struct ReplayRunner {
    api: Arc<Replay>,
    resolve: AttachmentPathResolver,
}

#[async_trait]
impl Runner for ReplayRunner {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
        let api: Arc<dyn OpenCode2Api> = self.api.clone();
        run_on_api(api, &request, &self.resolve)
            .await
            .map_err(|error| to_text_generation_error(request.operation, error))
    }
}

fn generation(api: &Arc<Replay>) -> RunnerTextGeneration<ReplayRunner> {
    RunnerTextGeneration::new(
        "OpenCode",
        ReplayRunner {
            api: Arc::clone(api),
            resolve: Arc::new(|_| None),
        },
    )
}

fn title_input(model: &str) -> ThreadTitleGenerationInput {
    ThreadTitleGenerationInput {
        linked_context: None,
        cwd: CWD.to_owned(),
        message: TITLE_MESSAGE.to_owned(),
        previous_title: None,
        attachments: Vec::<ChatAttachment>::new(),
        model_selection: serde_json::from_value::<ModelSelection>(json!({
            "instanceId": "opencode",
            "model": model,
        }))
        .expect("model selection"),
    }
}

// --- Transcript pieces ---------------------------------------------------------------

/// The recorded transcript, as JSON entries.
fn recorded() -> Vec<Value> {
    serde_json::from_str(RECORDED).expect("recorded transcript")
}

/// Subscribe, connect, create, and create's response: the opening of every recording.
fn opening() -> Vec<Value> {
    recorded()[..4].to_vec()
}

/// The prompt's expectation, and the prompt's response.
fn prompt_entries() -> Vec<Value> {
    vec![
        json!({
            "type": "expect_outbound",
            "frame": { "type": "session.prompt", "input": { "sessionID": SESSION_ID, "text": "<any>" } },
        }),
        json!({
            "type": "emit_inbound",
            "frame": {
                "type": "sdk.response",
                "operation": "session.prompt",
                "data": { "data": {
                    "id": "msg_1",
                    "sessionID": SESSION_ID,
                    "time": { "created": 1 },
                    "type": "user",
                    "payload": { "text": "t" },
                    "delivery": "steer",
                } },
            },
        }),
    ]
}

fn remove_entries() -> Vec<Value> {
    vec![
        json!({
            "type": "expect_outbound",
            "frame": { "type": "session.remove", "input": { "sessionID": SESSION_ID } },
        }),
        json!({
            "type": "emit_inbound",
            "frame": { "type": "sdk.response", "operation": "session.remove", "data": null },
        }),
    ]
}

fn exit() -> Value {
    json!({ "type": "runtime_exit", "status": "success" })
}

fn sdk_event(event: Value) -> Value {
    json!({ "type": "emit_inbound", "frame": { "type": "sdk.event", "event": event } })
}

fn concat(parts: Vec<Vec<Value>>) -> Vec<Value> {
    parts.into_iter().flatten().collect()
}

// --- Tests ----------------------------------------------------------------------------

#[tokio::test]
async fn generates_a_title_in_a_temporary_session_on_the_free_tier_and_removes_it() {
    let mut entries = recorded();
    entries.push(exit());
    let api = replay(entries);

    let title = generation(&api)
        .generate_thread_title(title_input("opencode/big-pickle"))
        .await
        .expect("title");

    assert_eq!(title.title, "Fix OAuth Login Redirect Loop");
    api.assert_exhausted();
}

#[tokio::test]
async fn fails_at_once_when_the_event_stream_drops_before_the_reply() {
    // The server goes away before the reply; the temporary session is still removed.
    let api = replay(concat(vec![
        opening(),
        prompt_entries(),
        vec![exit()],
        remove_entries(),
    ]));

    let failure = generation(&api)
        .generate_thread_title(title_input("opencode/big-pickle"))
        .await
        .expect_err("stream lost");

    let TextGenerationError::TextGenerationError { detail, .. } = failure;
    assert_eq!(detail, "The OpenCode event stream was lost.");
    api.assert_exhausted();
}

#[tokio::test]
async fn keeps_a_providers_own_error_message_out_of_the_caller_visible_detail() {
    let refusal = json!({
        "type": "provider.auth",
        "message": "Error from provider (Console): OpenCode's free tier can only be used from within OpenCode",
        "status": 403,
    });
    let api = replay(concat(vec![
        opening(),
        prompt_entries(),
        vec![sdk_event(json!({
            "id": "evt_0eb7f9ae0001VCNDHVLnqTfoPG",
            "created": 1790657403616u64,
            "type": "session.execution.failed",
            "data": { "sessionID": SESSION_ID, "error": refusal },
            "durable": { "aggregateID": SESSION_ID, "seq": 7, "version": 1 },
        }))],
        remove_entries(),
        vec![exit()],
    ]));

    let failure = generation(&api)
        .generate_thread_title(title_input("opencode/big-pickle"))
        .await
        .expect_err("refused");

    let TextGenerationError::TextGenerationError { detail, cause, .. } = failure;
    assert_eq!(detail, "OpenCode could not generate the text.");
    assert_eq!(cause, Some(refusal));
    api.assert_exhausted();
}

#[tokio::test]
async fn ends_at_once_on_an_execution_end_this_build_cannot_decode() {
    // A reason added after 2.0.18. The Rust envelope accepts the data as a value, so the
    // frame reads as a normal interruption; the envelope-less variant below covers the
    // unreadable path.
    let api = replay(concat(vec![
        opening(),
        prompt_entries(),
        vec![sdk_event(json!({
            "id": "evt_0eb7f9ae0001VCNDHVLnqTfoPG",
            "created": 1790657403616u64,
            "type": "session.execution.interrupted",
            "data": { "sessionID": SESSION_ID, "reason": "budget" },
            "durable": { "aggregateID": SESSION_ID, "seq": 7, "version": 1 },
        }))],
        remove_entries(),
        vec![exit()],
    ]));

    let failure = generation(&api)
        .generate_thread_title(title_input("opencode/big-pickle"))
        .await
        .expect_err("interrupted");

    assert!(matches!(
        failure,
        TextGenerationError::TextGenerationError { .. }
    ));
    api.assert_exhausted();
}

/// Extra case (not upstream): an execution end with no `id` is unreadable to this build,
/// so the generation ends on it as it would on a failure, reading the text received so far.
#[tokio::test]
async fn ends_on_an_unreadable_execution_end_with_the_text_received_so_far() {
    let api = replay(concat(vec![
        opening(),
        prompt_entries(),
        vec![sdk_event(json!({
            "id": "evt_text",
            "type": "session.text.ended",
            "data": {
                "sessionID": SESSION_ID,
                "assistantMessageID": "msg_a",
                "ordinal": 0,
                "text": "{\"title\": \"Fix OAuth Login Redirect Loop\"}",
            },
        }))],
        vec![sdk_event(json!({
            "type": "session.execution.succeeded",
            "data": { "sessionID": SESSION_ID },
        }))],
        remove_entries(),
        vec![exit()],
    ]));

    let title = generation(&api)
        .generate_thread_title(title_input("opencode/big-pickle"))
        .await
        .expect("title");

    assert_eq!(title.title, "Fix OAuth Login Redirect Loop");
    api.assert_exhausted();
}

#[tokio::test]
async fn rejects_a_model_selection_without_a_provider_prefix() {
    let api = replay(Vec::new());

    let failure = generation(&api)
        .generate_thread_title(title_input("big-pickle"))
        .await
        .expect_err("bad slug");

    let TextGenerationError::TextGenerationError { detail, .. } = failure;
    assert_eq!(
        detail,
        "OpenCode model selection must use the 'provider/model' format."
    );
}
