//! Port of upstream/packages/effect-codex-app-server/src/replay.ts
#![allow(clippy::collapsible_if)] // Keep upstream normalization branches easy to compare.
use crate::client::Client;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    task::JoinHandle,
    time::{Duration, sleep},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplayEntry {
    ExpectOutbound {
        #[serde(default)]
        label: Option<String>,
        frame: Value,
    },
    EmitInbound {
        #[serde(default)]
        label: Option<String>,
        frame: Value,
        #[serde(rename = "afterMs", default)]
        after_ms: Option<f64>,
    },
    RuntimeExit {
        status: ExitStatus,
        #[serde(default)]
        error: Option<Value>,
    },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExitStatus {
    Success,
    Error,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayTranscript {
    pub provider: String,
    pub protocol: String,
    pub version: String,
    pub scenario: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub entries: Vec<ReplayEntry>,
}
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum ReplayError {
    #[error("failed to parse outbound replay frame at cursor {cursor}: {detail}")]
    JsonParse { cursor: usize, detail: String },
    #[error("replay transcript exhausted before outbound frame {cursor}")]
    Exhausted { cursor: usize },
    #[error("unexpected outbound frame at replay cursor {cursor}")]
    UnexpectedOutbound { cursor: usize },
    #[error(
        "outbound replay frame mismatch at cursor {cursor}: expected {expected}, received {actual}"
    )]
    FrameMismatch {
        cursor: usize,
        expected: Box<Value>,
        actual: Box<Value>,
    },
    #[error("replay exited with status {status:?} at cursor {cursor}")]
    RuntimeExit { cursor: usize, status: ExitStatus },
    #[error("replay ended with {remaining} unconsumed entries")]
    Incomplete { cursor: usize, remaining: usize },
}
#[derive(Debug, Clone, Default)]
pub struct ReplayState {
    pub cursor: usize,
    pub failure: Option<ReplayError>,
}
pub struct ReplayDriver {
    pub transcript: ReplayTranscript,
    pub state: Arc<Mutex<ReplayState>>,
    task: JoinHandle<()>,
}
impl Drop for ReplayDriver {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl ReplayDriver {
    pub fn verify_complete(&self) -> Result<(), ReplayError> {
        let state = self.state.lock();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        let remaining = self.transcript.entries.len().saturating_sub(state.cursor);
        if remaining > 0 {
            Err(ReplayError::Incomplete {
                cursor: state.cursor,
                remaining,
            })
        } else {
            Ok(())
        }
    }
}
pub fn make_replay_client(transcript: ReplayTranscript) -> (Client, ReplayDriver) {
    let (client_read, replay_write) = tokio::io::duplex(1024 * 1024);
    let (replay_read, client_write) = tokio::io::duplex(1024 * 1024);
    let client = Client::from_io(client_read, client_write);
    let state = Arc::new(Mutex::new(ReplayState::default()));
    let task_state = state.clone();
    let task_transcript = transcript.clone();
    let task = tokio::spawn(async move {
        run_replay(task_transcript, replay_read, replay_write, task_state).await;
    });
    (
        client,
        ReplayDriver {
            transcript,
            state,
            task,
        },
    )
}
async fn run_replay(
    transcript: ReplayTranscript,
    read: DuplexStream,
    mut write: DuplexStream,
    state: Arc<Mutex<ReplayState>>,
) {
    let ignored = transcript
        .metadata
        .as_ref()
        .and_then(|v| v.get("recorderThreadConfigKeys"))
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut lines = BufReader::new(read).lines();
    loop {
        let cursor = state.lock().cursor;
        let Some(entry) = transcript.entries.get(cursor) else {
            break;
        };
        match entry {
            ReplayEntry::EmitInbound {
                frame, after_ms, ..
            } => {
                if let Some(ms) = after_ms.filter(|ms| *ms > 0.0) {
                    sleep(Duration::from_secs_f64(ms / 1000.0)).await;
                }
                let frame = normalize_legacy_inbound(frame.clone());
                let mut bytes = serde_json::to_vec(&frame).expect("JSON frame");
                bytes.push(b'\n');
                if write.write_all(&bytes).await.is_err() {
                    break;
                }
                state.lock().cursor += 1;
            }
            ReplayEntry::RuntimeExit { status, .. } => {
                state.lock().cursor += 1;
                if *status != ExitStatus::Success {
                    state.lock().failure = Some(ReplayError::RuntimeExit {
                        cursor,
                        status: *status,
                    });
                }
                break;
            }
            ReplayEntry::ExpectOutbound { frame, .. } => match lines.next_line().await {
                Ok(Some(line)) if line.trim().is_empty() => continue,
                Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                    Ok(actual) if same_frame(frame, &actual, &ignored) => {
                        state.lock().cursor += 1;
                    }
                    Ok(actual) => {
                        state.lock().failure = Some(ReplayError::FrameMismatch {
                            cursor,
                            expected: Box::new(frame.clone()),
                            actual: Box::new(actual),
                        });
                        break;
                    }
                    Err(error) => {
                        state.lock().failure = Some(ReplayError::JsonParse {
                            cursor,
                            detail: error.to_string(),
                        });
                        break;
                    }
                },
                Ok(None) => break,
                Err(error) => {
                    state.lock().failure = Some(ReplayError::JsonParse {
                        cursor,
                        detail: error.to_string(),
                    });
                    break;
                }
            },
        }
    }
    if state.lock().failure.is_none() {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_millis(10), lines.next_line()).await
        {
            if !line.trim().is_empty() {
                let cursor = state.lock().cursor;
                state.lock().failure = Some(ReplayError::Exhausted { cursor });
            }
        }
    }
}
fn normalize_legacy_inbound(mut value: Value) -> Value {
    match &mut value {
        Value::Array(array) => {
            for item in array {
                *item = normalize_legacy_inbound(item.take());
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                *item = normalize_legacy_inbound(item.take());
            }
            if map.get("modelProvider").is_some() && map.get("status").is_some() {
                if let Some(id) = map.get("id").cloned() {
                    map.entry("sessionId").or_insert(id);
                    map.entry("projectId").or_insert(Value::Null);
                }
            }
            let method = map.get("method").and_then(Value::as_str).map(str::to_owned);
            if let Some(Value::Object(params)) = map.get_mut("params") {
                match method.as_deref() {
                    Some("item/started") => {
                        params.entry("startedAtMs").or_insert(Value::from(0));
                    }
                    Some("item/completed") => {
                        params.entry("completedAtMs").or_insert(Value::from(0));
                    }
                    Some("item/tool/requestUserInput") => {
                        params.entry("isBlocking").or_insert(Value::Bool(true));
                    }
                    Some(m) if m.ends_with("/requestApproval") => {
                        params.entry("startedAtMs").or_insert(Value::from(0));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    value
}
fn normalize(mut value: Value, ignored: &BTreeSet<String>) -> Value {
    match &mut value {
        Value::Array(array) => {
            for item in array {
                *item = normalize(item.take(), ignored);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                *item = normalize(item.take(), ignored);
            }
            let method = map.get("method").and_then(Value::as_str).map(str::to_owned);
            if method.as_deref() == Some("initialize") {
                if let Some(Value::Object(params)) = map.get_mut("params") {
                    if let Some(Value::Object(info)) = params.get_mut("clientInfo") {
                        info.insert("version".into(), Value::String("<ignored>".into()));
                    }
                }
            }
            if method.as_deref() == Some("turn/start") {
                if let Some(Value::Object(params)) = map.get_mut("params") {
                    if params.get("approvalPolicy") == Some(&Value::String("never".into())) {
                        params.remove("approvalPolicy");
                    }
                    if params.get("sandboxPolicy").and_then(|v| v.get("type"))
                        == Some(&Value::String("dangerFullAccess".into()))
                    {
                        params.remove("sandboxPolicy");
                    }
                    if let Some(Value::Object(mode)) = params.get_mut("collaborationMode") {
                        if let Some(Value::Object(settings)) = mode.get_mut("settings") {
                            settings.insert(
                                "developer_instructions".into(),
                                Value::String("<ignored>".into()),
                            );
                        }
                    }
                }
            }
            if matches!(
                method.as_deref(),
                Some("thread/start" | "thread/resume" | "thread/fork")
            ) {
                if let Some(Value::Object(params)) = map.get_mut("params") {
                    params.remove("cwd");
                    params.remove("model");
                    if let Some(Value::Object(config)) = params.get_mut("config") {
                        config.remove("mcp_servers");
                        for key in ignored {
                            config.remove(key);
                        }
                    }
                }
            }
            if let Some(Value::String(text)) = map.get_mut("text") {
                *text = normalize_handoff(text);
            }
        }
        Value::String(text) => *text = normalize_handoff(text),
        _ => {}
    }
    value
}
fn normalize_handoff(text: &str) -> String {
    if !text.starts_with("Context handoff (") {
        return text.into();
    }
    let Some(user) = text.find("\n\nUser message:\n") else {
        return text.into();
    };
    let Some(header) = text.find(":\n") else {
        return text.into();
    };
    if header >= user {
        return text.into();
    }
    format!("{}<dynamic-summary>{}", &text[..header + 2], &text[user..])
}
fn same_frame(expected: &Value, actual: &Value, ignored: &BTreeSet<String>) -> bool {
    normalize(expected.clone(), ignored) == normalize(actual.clone(), ignored)
}
