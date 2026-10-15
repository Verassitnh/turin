//! Port of @anthropic-ai/claude-agent-sdk@0.3.276 sdk.mjs Query control dispatcher and ProcessTransport.
use crate::{
    frame::{Frame, UserMessage},
    launch::LaunchOptions,
};
use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;

pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send>>;
/// Return None to deliberately leave a dialog unanswered, as the SDK does when no renderer is registered.
pub trait ControlHandler: Send + Sync + 'static {
    fn handle(&self, request: ControlRequest, cancel: CancellationToken) -> HandlerFuture;
}
#[derive(Debug, Clone)]
pub struct ControlRequest {
    pub request_id: String,
    pub subtype: String,
    pub body: Value,
    pub kind: InboundControl,
}
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum InboundControl {
    CanUseTool {
        tool_name: String,
        input: Value,
        tool_use_id: String,
        agent_id: Option<String>,
        permission_suggestions: Option<Value>,
        decision_reason: Option<String>,
        blocked_path: Option<String>,
        title: Option<String>,
        display_name: Option<String>,
        description: Option<String>,
    },
    HookCallback {
        callback_id: String,
        input: Value,
        tool_use_id: Option<String>,
    },
    RequestUserDialog {
        dialog_kind: String,
        payload: Value,
        tool_use_id: Option<String>,
    },
    McpMessage {
        server_name: String,
        message: Value,
    },
    Other(Value),
}
impl InboundControl {
    fn from_body(body: &Value) -> Self {
        let string = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_owned);
        match body.get("subtype").and_then(Value::as_str) {
            Some("can_use_tool") => match (
                string("tool_name"),
                body.get("input"),
                string("tool_use_id"),
            ) {
                (Some(tool_name), Some(input), Some(tool_use_id)) => Self::CanUseTool {
                    tool_name,
                    input: input.clone(),
                    tool_use_id,
                    agent_id: string("agent_id"),
                    permission_suggestions: body.get("permission_suggestions").cloned(),
                    decision_reason: string("decision_reason"),
                    blocked_path: string("blocked_path"),
                    title: string("title"),
                    display_name: string("display_name"),
                    description: string("description"),
                },
                _ => Self::Other(body.clone()),
            },
            Some("hook_callback") => match (string("callback_id"), body.get("input")) {
                (Some(callback_id), Some(input)) => Self::HookCallback {
                    callback_id,
                    input: input.clone(),
                    tool_use_id: string("tool_use_id"),
                },
                _ => Self::Other(body.clone()),
            },
            Some("request_user_dialog") => match (string("dialog_kind"), body.get("payload")) {
                (Some(dialog_kind), Some(payload)) => Self::RequestUserDialog {
                    dialog_kind,
                    payload: payload.clone(),
                    tool_use_id: string("tool_use_id"),
                },
                _ => Self::Other(body.clone()),
            },
            Some("mcp_message") => match (string("server_name"), body.get("message")) {
                (Some(server_name), Some(message)) => Self::McpMessage {
                    server_name,
                    message: message.clone(),
                },
                _ => Self::Other(body.clone()),
            },
            _ => Self::Other(body.clone()),
        }
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum SessionEvent {
    Frame(Frame),
    Stderr(String),
    Error(String),
    Eof,
}
enum WriteCommand {
    Json(Value),
    Close,
}
struct Pending {
    sender: oneshot::Sender<Value>,
    subtype: String,
}
struct Shared {
    writer: mpsc::UnboundedSender<WriteCommand>,
    events: mpsc::UnboundedSender<SessionEvent>,
    pending: Mutex<HashMap<String, Pending>>,
    unmatched: Mutex<VecDeque<(String, Value)>>,
    inbound: Mutex<HashMap<String, CancellationToken>>,
    handler: Arc<dyn ControlHandler>,
}
pub struct ClaudeSession {
    state: Arc<Shared>,
    child: Child,
    pid: Option<u32>,
    events: mpsc::UnboundedReceiver<SessionEvent>,
    tasks: JoinSet<()>,
    initialization: Value,
}
impl ClaudeSession {
    pub async fn spawn(options: LaunchOptions, handler: Arc<dyn ControlHandler>) -> Result<Self> {
        let spec = options.spec()?;
        let mut command = Command::new(&spec.binary);
        command
            .args(&spec.args)
            .env_clear()
            .envs(&spec.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn {}", spec.binary.display()))?;
        let pid = child.id();
        let stdin = child.stdin.take().context("claude stdin missing")?;
        let stdout = child.stdout.take().context("claude stdout missing")?;
        let stderr = child.stderr.take().context("claude stderr missing")?;
        let (writer, writes) = mpsc::unbounded_channel();
        let (event_tx, events) = mpsc::unbounded_channel();
        let state = Arc::new(Shared {
            writer,
            events: event_tx,
            pending: Mutex::new(HashMap::new()),
            unmatched: Mutex::new(VecDeque::new()),
            inbound: Mutex::new(HashMap::new()),
            handler,
        });
        let mut tasks = JoinSet::new();
        tasks.spawn(write_loop(stdin, writes, state.events.clone()));
        tasks.spawn(read_loop(stdout, state.clone()));
        tasks.spawn(stderr_loop(stderr, state.events.clone()));
        let mut session = Self {
            state,
            child,
            pid,
            events,
            tasks,
            initialization: Value::Null,
        };
        // SDK initializes immediately, before any prompt. system/init is emitted only when a turn begins.
        session.initialization = session.initialize(options.initialize_request()).await?;
        Ok(session)
    }
    /// Inner `response` object from the initialize control response (commands, models, account, …).
    pub fn initialization(&self) -> &Value {
        &self.initialization
    }
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        self.events.recv().await
    }
    pub fn send(&self, message: UserMessage) -> Result<()> {
        self.send_raw(serde_json::to_value(message)?)
    }
    pub fn send_raw(&self, value: Value) -> Result<()> {
        self.state
            .writer
            .send(WriteCommand::Json(value))
            .map_err(|_| anyhow::anyhow!("claude stdin closed"))
    }
    pub async fn request(&self, body: Value) -> Result<Value> {
        self.request_with_cancel(body, None).await
    }
    pub async fn request_with_cancel(
        &self,
        body: Value,
        cancel: Option<CancellationToken>,
    ) -> Result<Value> {
        let subtype = body
            .get("subtype")
            .and_then(Value::as_str)
            .context("control request missing subtype")?
            .to_owned();
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        {
            self.state.pending.lock().unwrap().insert(
                id.clone(),
                Pending {
                    sender: tx,
                    subtype: subtype.clone(),
                },
            );
        }
        self.send_raw(json!({"type":"control_request", "request_id":id, "request":body}))?;
        let response = if let Some(token) = cancel {
            tokio::select! {
                value = rx => value.context("claude control response channel closed")?,
                _ = token.cancelled() => { self.state.pending.lock().unwrap().remove(&id); self.send_raw(json!({"type":"control_cancel_request", "request_id":id}))?; anyhow::bail!("control request cancelled"); }
            }
        } else {
            rx.await.context("claude control response channel closed")?
        };
        parse_control_response(response, &subtype)
    }
    pub async fn await_control_response(&self, request_id: &str) -> Result<Value> {
        let early = {
            let mut unmatched = self.state.unmatched.lock().unwrap();
            unmatched
                .iter()
                .position(|(id, _)| id == request_id)
                .map(|index| unmatched.remove(index).unwrap().1)
        };
        if let Some(response) = early {
            return parse_control_response(response, "await_control_response");
        }
        let (sender, receiver) = oneshot::channel();
        self.state.pending.lock().unwrap().insert(
            request_id.into(),
            Pending {
                sender,
                subtype: "await_control_response".into(),
            },
        );
        let response = receiver
            .await
            .context("claude control response channel closed")?;
        parse_control_response(response, "await_control_response")
    }
    pub async fn initialize(&self, body: Value) -> Result<Value> {
        self.request(body).await
    }
    pub async fn interrupt(&self, cancel_queued: bool) -> Result<Value> {
        self.request(if cancel_queued {
            json!({"subtype":"interrupt","cancel_queued":true})
        } else {
            json!({"subtype":"interrupt"})
        })
        .await
    }
    pub async fn set_permission_mode(&self, mode: &str) -> Result<()> {
        self.request(json!({"subtype":"set_permission_mode","mode":mode}))
            .await
            .map(|_| ())
    }
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.request(json!({"subtype":"set_model","model":model}))
            .await
            .map(|_| ())
    }
    pub async fn apply_flag_settings(&self, settings: Value) -> Result<()> {
        self.request(json!({"subtype":"apply_flag_settings","settings":settings}))
            .await
            .map(|_| ())
    }
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.request(json!({"subtype":"stop_task","task_id":task_id}))
            .await
            .map(|_| ())
    }
    pub async fn set_mcp_servers(&self, servers: Value) -> Result<Value> {
        self.request(json!({"subtype":"mcp_set_servers","servers":servers}))
            .await
    }
    pub async fn get_usage(&self, skip_behaviors: bool) -> Result<Value> {
        self.request(if skip_behaviors {
            json!({"subtype":"get_usage","skip_behaviors":true})
        } else {
            json!({"subtype":"get_usage"})
        })
        .await
    }
    pub async fn get_mcp_status(&self) -> Result<Value> {
        self.request(json!({"subtype":"mcp_status"})).await
    }
    pub async fn mcp_message(&self, server_name: &str, message: Value) -> Result<Value> {
        self.request(json!({"subtype":"mcp_message","server_name":server_name,"message":message}))
            .await
    }
    pub async fn shutdown(mut self) -> Result<()> {
        let _ = self.state.writer.send(WriteCommand::Close);
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            signal_group(self.pid, term_signal());
            if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
                .await
                .is_err()
            {
                signal_group(self.pid, kill_signal());
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
            }
        }
        self.pid = None;
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
        Ok(())
    }
}
fn parse_control_response(response: Value, subtype: &str) -> Result<Value> {
    if response.get("subtype").and_then(Value::as_str) == Some("error") {
        anyhow::bail!(
            "claude {subtype}: {}",
            response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    anyhow::ensure!(
        response.get("subtype").and_then(Value::as_str) == Some("success"),
        "unexpected control response: {response}"
    );
    Ok(response.get("response").cloned().unwrap_or(Value::Null))
}
impl Drop for ClaudeSession {
    fn drop(&mut self) {
        signal_group(self.pid, kill_signal());
        self.tasks.abort_all();
    }
}
#[cfg(unix)]
fn term_signal() -> i32 {
    libc::SIGTERM
}
#[cfg(unix)]
fn kill_signal() -> i32 {
    libc::SIGKILL
}
#[cfg(not(unix))]
fn term_signal() -> i32 {
    15
}
#[cfg(not(unix))]
fn kill_signal() -> i32 {
    9
}
#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: i32) {
    if let Some(pid) = pid {
        unsafe {
            libc::killpg(pid as i32, signal);
        }
    }
}
#[cfg(not(unix))]
fn signal_group(_: Option<u32>, _: i32) {}
async fn write_loop(
    mut stdin: tokio::process::ChildStdin,
    mut writes: mpsc::UnboundedReceiver<WriteCommand>,
    events: mpsc::UnboundedSender<SessionEvent>,
) {
    while let Some(command) = writes.recv().await {
        match command {
            WriteCommand::Json(value) => match serde_json::to_vec(&value) {
                Ok(mut bytes) => {
                    bytes.push(b'\n');
                    if let Err(e) = stdin.write_all(&bytes).await {
                        let _ = events.send(SessionEvent::Error(format!("stdin: {e}")));
                        break;
                    }
                }
                Err(e) => {
                    let _ = events.send(SessionEvent::Error(format!("serialize: {e}")));
                }
            },
            WriteCommand::Close => break,
        }
    }
    let _ = stdin.shutdown().await;
}
const MAX_STDOUT_LINE: usize = 32 * 1024 * 1024;

async fn read_loop(stdout: tokio::process::ChildStdout, state: Arc<Shared>) {
    let mut lines = FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_STDOUT_LINE));
    let mut handlers = JoinSet::new();
    loop {
        match lines.next().await {
            Some(Ok(line)) => {
                let value: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = state
                            .events
                            .send(SessionEvent::Error(format!("stdout JSON: {e}")));
                        continue;
                    }
                };
                match value.get("type").and_then(Value::as_str) {
                    Some("control_response") => {
                        let response = value.get("response").cloned().unwrap_or(Value::Null);
                        if let Some(id) = response.get("request_id").and_then(Value::as_str) {
                            if let Some(pending) = state.pending.lock().unwrap().remove(id) {
                                if pending.subtype == "initialize"
                                    && response.get("subtype").and_then(Value::as_str)
                                        == Some("success")
                                {
                                    replay_pending(&response, &state, &mut handlers);
                                }
                                let _ = pending.sender.send(response);
                            } else {
                                let mut unmatched = state.unmatched.lock().unwrap();
                                if unmatched.len() >= 1024 {
                                    unmatched.pop_front();
                                }
                                unmatched.push_back((id.into(), response));
                            }
                        }
                    }
                    Some("control_request") => dispatch(value, &state, &mut handlers),
                    Some("control_cancel_request") => {
                        if let Some(id) = value.get("request_id").and_then(Value::as_str)
                            && let Some(token) = state.inbound.lock().unwrap().remove(id)
                        {
                            token.cancel();
                        }
                    }
                    Some("keep_alive" | "transcript_mirror") => {}
                    _ => match Frame::decode(value) {
                        Ok(frame) => {
                            let _ = state.events.send(SessionEvent::Frame(frame));
                        }
                        Err(e) => {
                            let _ = state
                                .events
                                .send(SessionEvent::Error(format!("invalid known frame: {e}")));
                            break;
                        }
                    },
                }
            }
            Some(Err(e)) => {
                let _ = state
                    .events
                    .send(SessionEvent::Error(format!("stdout: {e}")));
                break;
            }
            None => break,
        }
    }
    handlers.abort_all();
    for (_, pending) in state.pending.lock().unwrap().drain() {
        drop(pending.sender);
    }
    let _ = state.events.send(SessionEvent::Eof);
}
fn replay_pending(response: &Value, state: &Arc<Shared>, tasks: &mut JoinSet<()>) {
    for name in [
        "pending_permission_requests",
        "pending_user_dialog_requests",
    ] {
        if let Some(items) = response.get(name).and_then(Value::as_array) {
            for item in items {
                if matches!(
                    item.pointer("/request/subtype").and_then(Value::as_str),
                    Some("can_use_tool" | "request_user_dialog")
                ) {
                    dispatch(item.clone(), state, tasks);
                }
            }
        }
    }
}
fn dispatch(value: Value, state: &Arc<Shared>, tasks: &mut JoinSet<()>) {
    let Some(id) = value.get("request_id").and_then(Value::as_str) else {
        return;
    };
    let id = id.to_owned();
    let body = value.get("request").cloned().unwrap_or(Value::Null);
    let subtype = body
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let token = CancellationToken::new();
    {
        let mut inbound = state.inbound.lock().unwrap();
        if inbound.contains_key(&id) {
            return;
        }
        inbound.insert(id.clone(), token.clone());
    }
    let state = state.clone();
    tasks.spawn(async move {
        let request = ControlRequest { request_id: id.clone(), subtype: subtype.clone(), kind: InboundControl::from_body(&body), body: body.clone() };
        let outcome = state.handler.handle(request, token).await;
        state.inbound.lock().unwrap().remove(&id);
        // A cancel aborts the handler's token. The SDK still writes whatever the
        // handler settled (including an error). Only an explicit None stays silent.
        let response = match outcome {
            Ok(Some(mut payload)) => {
                if subtype == "can_use_tool"
                    && let Some(tool_id) = body.get("tool_use_id").and_then(Value::as_str)
                    && let Some(object) = payload.as_object_mut()
                {
                    object.insert("toolUseID".into(), tool_id.into());
                }
                json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":payload}}) },
            Ok(None) => return,
            Err(e) => json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":e.to_string()}}),
        };
        let _ = state.writer.send(WriteCommand::Json(response));
    });
}
async fn stderr_loop(
    mut stderr: tokio::process::ChildStderr,
    events: mpsc::UnboundedSender<SessionEvent>,
) {
    let mut chunk = [0_u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let _ = events.send(SessionEvent::Stderr(
                    String::from_utf8_lossy(&chunk[..n]).into_owned(),
                ));
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};
    struct Handler {
        entered: Arc<tokio::sync::Notify>,
    }
    impl ControlHandler for Handler {
        fn handle(&self, request: ControlRequest, cancel: CancellationToken) -> HandlerFuture {
            let entered = self.entered.clone();
            Box::pin(async move {
                if request.subtype == "can_use_tool" {
                    entered.notify_one();
                    cancel.cancelled().await;
                    Ok(Some(
                        json!({"behavior":"deny","message":"User cancelled tool execution."}),
                    ))
                } else {
                    Ok(Some(json!({})))
                }
            })
        }
    }
    #[tokio::test]
    async fn fake_cli_replays_initialize_pending_and_streams_while_approval_waits() {
        let dir = std::env::temp_dir().join(format!("turin-claude-fake-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let script = dir.join("fake-claude");
        let seen = dir.join("seen.json");
        fs::write(&script, r##"#!/usr/bin/env python3
import json,os,sys
seen=os.environ["FAKE_SEEN"]
for line in sys.stdin:
    value=json.loads(line)
    request=value.get("request") or {}
    if value.get("type")=="control_request" and request.get("subtype")=="initialize":
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"models":[]},"pending_permission_requests":[{"type":"control_request","request_id":"approval-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"pwd"},"tool_use_id":"toolu_1"}}]}}),flush=True)
    elif value.get("type")=="control_request" and request.get("subtype")=="get_usage":
        open(seen,"a").write(json.dumps(request)+"\n")
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"usage":{}}}}),flush=True)
    elif value.get("type")=="user":
        print(json.dumps({"type":"system","subtype":"init","session_id":"test-session","model":"claude-sonnet-4-6","tools":[]}),flush=True)
        print(json.dumps({"type":"stream_event","session_id":"test-session","uuid":"event-1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}}),flush=True)
        print(json.dumps({"type":"control_cancel_request","request_id":"approval-1"}),flush=True)
        print(json.dumps({"type":"result","subtype":"success","session_id":"test-session","uuid":"result-1","is_error":False,"result":"hello"}),flush=True)
        reply=sys.stdin.readline()
        open(seen,"a").write(reply)
"##).unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&script, permissions).unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let handler = Arc::new(Handler {
            entered: entered.clone(),
        });
        let mut env = BTreeMap::from([("FAKE_SEEN".into(), seen.display().to_string())]);
        if let Some(path) = std::env::var_os("PATH") {
            env.insert("PATH".into(), path.to_string_lossy().into_owned());
        }
        let options = LaunchOptions {
            binary: script,
            env: Some(env),
            ..Default::default()
        };
        let mut session = ClaudeSession::spawn(options, handler).await.unwrap();
        let usage = session.get_usage(true).await.unwrap();
        assert_eq!(usage["usage"], json!({}));
        entered.notified().await;
        session
            .send(UserMessage::text("hello", Some("prompt-1".into()), false))
            .unwrap();
        let mut types = vec![];
        while types.len() < 3 {
            match tokio::time::timeout(Duration::from_secs(2), session.next_event())
                .await
                .unwrap()
                .unwrap()
            {
                SessionEvent::Frame(frame) => {
                    types.push(frame.raw["type"].as_str().unwrap().to_owned())
                }
                SessionEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        assert_eq!(types, ["system", "stream_event", "result"]);
        session.shutdown().await.unwrap();
        let recorded = fs::read_to_string(&seen).unwrap();
        assert!(
            recorded.contains("\"skip_behaviors\": true")
                || recorded.contains("\"skip_behaviors\":true")
        );
        let reply: serde_json::Value =
            serde_json::from_str(recorded.lines().last().unwrap()).unwrap();
        assert_eq!(reply["response"]["request_id"], "approval-1");
        assert_eq!(reply["response"]["response"]["toolUseID"], "toolu_1");
        fs::remove_dir_all(dir).unwrap();
    }
}
