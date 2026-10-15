//! Port of @anthropic-ai/claude-agent-sdk@0.3.276 sdk.mjs Query control dispatcher and ProcessTransport.
use crate::{
    frame::{Frame, UserMessage},
    launch::LaunchOptions,
};
use anyhow::{Context, Result};
use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Notify, mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;

pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send>>;
/// `Ok(None)` means "no callback registered" and gets the SDK's default reply for the
/// subtype. Only `request_user_dialog` stays unanswered; `elicitation` is declined and
/// every other subtype gets an error response so the CLI never waits forever.
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

/// Status collected after the CLI process is reaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum SessionEvent {
    Frame(Frame),
    Stderr(String),
    Error(String),
    /// Stdout closed. [`SessionEvent::Exit`] carries the reaped status and may arrive before or after this.
    Eof,
    Exit(ProcessExit),
}
enum WriteCommand {
    Json(Value),
    Close,
}
struct Pending {
    sender: oneshot::Sender<Value>,
    subtype: String,
}
struct PendingTable {
    closed: bool,
    requests: HashMap<String, Pending>,
}
impl PendingTable {
    fn fail_all(&mut self) {
        self.closed = true;
        for (id, pending) in self.requests.drain() {
            let _ = pending.sender.send(json!({
                "subtype": "error",
                "request_id": id,
                "error": "claude process closed"
            }));
        }
    }
}
struct Shared {
    writer: mpsc::UnboundedSender<WriteCommand>,
    events: mpsc::UnboundedSender<SessionEvent>,
    pending: Mutex<PendingTable>,
    unmatched: Mutex<VecDeque<(String, Value)>>,
    inbound: Mutex<HashMap<String, CancellationToken>>,
    handler: Arc<dyn ControlHandler>,
    exit_status: Mutex<Option<ProcessExit>>,
    exited: Notify,
    stderr_tail: Mutex<String>,
    /// Process-group id. Taken after the leader is reaped so the group can still be signaled.
    pgid: Mutex<Option<u32>>,
}
pub struct ClaudeSession {
    state: Arc<Shared>,
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
            pending: Mutex::new(PendingTable {
                closed: false,
                requests: HashMap::new(),
            }),
            unmatched: Mutex::new(VecDeque::new()),
            inbound: Mutex::new(HashMap::new()),
            handler,
            exit_status: Mutex::new(None),
            exited: Notify::new(),
            stderr_tail: Mutex::new(String::new()),
            pgid: Mutex::new(pid),
        });
        let mut tasks = JoinSet::new();
        tasks.spawn(reaper(child, state.clone()));
        tasks.spawn(write_loop(stdin, writes, state.clone()));
        tasks.spawn(read_loop(stdout, state.clone()));
        tasks.spawn(stderr_loop(stderr, state.clone()));
        let mut session = Self {
            state,
            pid,
            events,
            tasks,
            initialization: Value::Null,
        };
        // SDK initializes immediately, before any prompt. system/init is emitted only when a turn begins.
        session.initialization = match session.initialize(options.initialize_request()).await {
            Ok(value) => value,
            Err(error) => {
                let text = error.to_string();
                if text.contains("claude process closed")
                    || text.contains("channel closed")
                    || text.contains("stdin closed")
                {
                    let exit = session.wait_for_exit().await;
                    let tail = session.state.stderr_tail.lock().unwrap().clone();
                    anyhow::bail!("{}", exit_error(&exit, &tail));
                }
                return Err(error);
            }
        };
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
        if self.state.pending.lock().unwrap().closed {
            anyhow::bail!("claude process closed");
        }
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
            let mut pending = self.state.pending.lock().unwrap();
            if pending.closed {
                anyhow::bail!("claude process closed");
            }
            pending.requests.insert(
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
                _ = token.cancelled() => { self.state.pending.lock().unwrap().requests.remove(&id); self.send_raw(json!({"type":"control_cancel_request", "request_id":id}))?; anyhow::bail!("control request cancelled"); }
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
        {
            let mut pending = self.state.pending.lock().unwrap();
            if pending.closed {
                anyhow::bail!("claude process closed");
            }
            pending.requests.insert(
                request_id.into(),
                Pending {
                    sender,
                    subtype: "await_control_response".into(),
                },
            );
        }
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
    pub async fn shutdown(mut self) -> Result<ProcessExit> {
        let _ = self.state.writer.send(WriteCommand::Close);
        let exit = self.wait_for_exit_with_signals().await;
        self.pid = None;
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
        Ok(exit)
    }
    async fn wait_for_exit(&self) -> ProcessExit {
        if self.state.exit_status.lock().unwrap().is_none() {
            let _ =
                tokio::time::timeout(Duration::from_secs(2), self.state.exited.notified()).await;
        }
        self.state
            .exit_status
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(ProcessExit {
                code: None,
                signal: None,
            })
    }
    async fn wait_for_exit_with_signals(&self) -> ProcessExit {
        if self.state.exit_status.lock().unwrap().is_some() {
            return self.wait_for_exit().await;
        }
        let wait = self.state.exited.notified();
        tokio::pin!(wait);
        if tokio::time::timeout(Duration::from_secs(2), &mut wait)
            .await
            .is_err()
        {
            signal_group(self.pid, term_signal());
            if tokio::time::timeout(Duration::from_secs(5), &mut wait)
                .await
                .is_err()
            {
                signal_group(self.pid, kill_signal());
                let _ = tokio::time::timeout(Duration::from_secs(1), &mut wait).await;
            }
        }
        self.wait_for_exit().await
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
fn process_exit(status: Option<std::process::ExitStatus>) -> ProcessExit {
    let Some(status) = status else {
        return ProcessExit {
            code: None,
            signal: None,
        };
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ProcessExit {
                code: None,
                signal: Some(signal),
            };
        }
    }
    ProcessExit {
        code: status.code(),
        signal: None,
    }
}
fn exit_error(exit: &ProcessExit, tail: &str) -> String {
    let mut message = if let Some(signal) = exit.signal {
        format!("Claude Code process exited with signal {signal}")
    } else {
        format!(
            "Claude Code process exited with code {}",
            exit.code.unwrap_or(0)
        )
    };
    let tail = tail.trim();
    if !tail.is_empty() {
        message.push_str(". stderr: ");
        message.push_str(tail);
    }
    message
}
fn push_stderr_tail(tail: &mut String, chunk: &str) {
    tail.push_str(chunk);
    let extra = tail.chars().count().saturating_sub(2048);
    if extra > 0 {
        let start = tail
            .char_indices()
            .nth(extra)
            .map(|(index, _)| index)
            .unwrap_or(tail.len());
        tail.replace_range(..start, "");
    }
}
async fn reaper(mut child: tokio::process::Child, state: Arc<Shared>) {
    let status = child.wait().await.ok();
    let pgid = state.pgid.lock().unwrap().take();
    signal_group(pgid, kill_signal());
    let exit = process_exit(status);
    *state.exit_status.lock().unwrap() = Some(exit.clone());
    state.exited.notify_one();
    let _ = state.events.send(SessionEvent::Exit(exit));
}
async fn write_loop(
    mut stdin: tokio::process::ChildStdin,
    mut writes: mpsc::UnboundedReceiver<WriteCommand>,
    state: Arc<Shared>,
) {
    while let Some(command) = writes.recv().await {
        match command {
            WriteCommand::Json(value) => match serde_json::to_vec(&value) {
                Ok(mut bytes) => {
                    bytes.push(b'\n');
                    if let Err(e) = stdin.write_all(&bytes).await {
                        let _ = state
                            .events
                            .send(SessionEvent::Error(format!("stdin: {e}")));
                        state.pending.lock().unwrap().fail_all();
                        break;
                    }
                }
                Err(e) => {
                    let _ = state
                        .events
                        .send(SessionEvent::Error(format!("serialize: {e}")));
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
        while handlers.try_join_next().is_some() {}
        match lines.next().await {
            Some(Ok(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
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
                            if let Some(pending) = state.pending.lock().unwrap().requests.remove(id)
                            {
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
    state.pending.lock().unwrap().fail_all();
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
        let outcome = AssertUnwindSafe(async { state.handler.handle(request, token).await })
            .catch_unwind()
            .await
            .unwrap_or_else(|panic| Err(anyhow::anyhow!(panic_message(panic.as_ref()))));
        state.inbound.lock().unwrap().remove(&id);
        // A cancel aborts the handler's token. The SDK still writes whatever the
        // handler settled (including an error). Only an explicit None stays silent.
        let outcome = match outcome {
            Ok(Some(mut payload)) => {
                if subtype == "can_use_tool"
                    && let Some(tool_id) = body.get("tool_use_id").and_then(Value::as_str)
                    && let Some(object) = payload.as_object_mut()
                {
                    object.insert("toolUseID".into(), tool_id.into());
                }
                Ok(payload)
            }
            Ok(None) => match unhandled_reply(&body) {
                Some(reply) => reply,
                None => return,
            },
            Err(e) => Err(e.to_string()),
        };
        let response = match outcome {
            Ok(payload) => json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":payload}}),
            Err(error) => json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":error}}),
        };
        let _ = state.writer.send(WriteCommand::Json(response));
    });
}
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    let detail = panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into());
    format!("control handler panicked: {detail}")
}
/// The SDK's reply when no callback is registered for a subtype. `None` means stay silent.
fn unhandled_reply(body: &Value) -> Option<Result<Value, String>> {
    let field = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or("undefined");
    let error = |message: String| Some(Err(message));
    match body.get("subtype").and_then(Value::as_str) {
        Some("request_user_dialog") => None,
        Some("elicitation") => Some(Ok(json!({"action": "decline"}))),
        Some("can_use_tool") => error("canUseTool callback is not provided.".into()),
        Some("hook_callback") => error(format!(
            "No hook callback found for ID: {}",
            field("callback_id")
        )),
        Some("mcp_message") => error(format!(
            "SDK MCP server not found: {}",
            field("server_name")
        )),
        Some("oauth_token_refresh") => error("getOAuthToken callback is not provided.".into()),
        Some("host_auth_token_refresh") => {
            error("getHostAuthToken callback is not provided.".into())
        }
        Some("remote_control_work_secret") => {
            error("refreshWorkSecret callback is not provided.".into())
        }
        _ => error(format!(
            "Unsupported control request subtype: {}",
            field("subtype")
        )),
    }
}
async fn stderr_loop(mut stderr: tokio::process::ChildStderr, state: Arc<Shared>) {
    let mut chunk = [0_u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&chunk[..n]).into_owned();
                push_stderr_tail(&mut state.stderr_tail.lock().unwrap(), &text);
                let _ = state.events.send(SessionEvent::Stderr(text));
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
    fn script(dir: &std::path::Path, body: &str) -> LaunchOptions {
        let path = dir.join("fake-claude");
        fs::write(&path, body).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        let mut env = BTreeMap::new();
        if let Some(path_env) = std::env::var_os("PATH") {
            env.insert("PATH".into(), path_env.to_string_lossy().into_owned());
        }
        LaunchOptions {
            binary: path,
            env: Some(env),
            ..Default::default()
        }
    }
    struct Accept;
    impl ControlHandler for Accept {
        fn handle(&self, _: ControlRequest, _: CancellationToken) -> HandlerFuture {
            Box::pin(async { Ok(Some(json!({}))) })
        }
    }
    #[tokio::test]
    async fn control_request_after_stdout_eof_fails_instead_of_hanging() {
        let dir =
            std::env::temp_dir().join(format!("turin-claude-closed-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let options = script(
            &dir,
            r#"#!/usr/bin/env python3
import json,os,sys
for line in sys.stdin:
    value=json.loads(line)
    if value.get("type")=="control_request" and value["request"]["subtype"]=="initialize":
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"models":[]}}}),flush=True)
        os.close(1)
        sys.stdin.read()
"#,
        );
        let mut session = ClaudeSession::spawn(options, Arc::new(Accept))
            .await
            .unwrap();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), session.next_event())
                .await
                .unwrap()
                .unwrap()
            {
                SessionEvent::Eof => break,
                SessionEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        let error = tokio::time::timeout(Duration::from_secs(2), session.interrupt(false))
            .await
            .expect("interrupt hung after stdout eof")
            .unwrap_err();
        assert!(
            error.to_string().contains("claude process closed"),
            "{error}"
        );
        let error = session.get_mcp_status().await.unwrap_err();
        assert!(
            error.to_string().contains("claude process closed"),
            "{error}"
        );
        session.shutdown().await.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn stdin_write_failure_fails_pending_control_requests() {
        let dir = std::env::temp_dir().join(format!("turin-claude-stdin-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let options = script(
            &dir,
            r#"#!/usr/bin/env python3
import json,os,sys,time
for line in sys.stdin:
    value=json.loads(line)
    if value.get("type")=="control_request" and value["request"]["subtype"]=="initialize":
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"models":[]}}}),flush=True)
        print(json.dumps({"type":"system","subtype":"status","status":"stdin-closed"}),flush=True)
        os.close(0)
        time.sleep(30)
"#,
        );
        let mut session = ClaudeSession::spawn(options, Arc::new(Accept))
            .await
            .unwrap();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), session.next_event())
                .await
                .unwrap()
                .unwrap()
            {
                SessionEvent::Frame(frame) if frame.raw["status"] == "stdin-closed" => break,
                SessionEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        let error = tokio::time::timeout(Duration::from_secs(2), session.get_mcp_status())
            .await
            .expect("request hung after stdin closed")
            .unwrap_err();
        assert!(
            error.to_string().contains("claude process closed"),
            "{error}"
        );
        session.shutdown().await.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    /// A fake CLI that sends `requests` after initialize and echoes each control_response
    /// back as a `system` frame so the test can read the replies.
    fn replier(dir: &std::path::Path, requests: Value) -> LaunchOptions {
        script(
            dir,
            &format!(
                r#"#!/usr/bin/env python3
import json,sys
requests=json.loads({:?})
for line in sys.stdin:
    value=json.loads(line)
    if value.get("type")=="control_request" and value["request"]["subtype"]=="initialize":
        print(json.dumps({{"type":"control_response","response":{{"subtype":"success","request_id":value["request_id"],"response":{{"models":[]}}}}}}),flush=True)
        for request in requests:
            print(json.dumps(request),flush=True)
    elif value.get("type")=="control_response":
        print(json.dumps({{"type":"system","subtype":"status","reply":value["response"]}}),flush=True)
"#,
                requests.to_string()
            ),
        )
    }
    async fn replies(session: &mut ClaudeSession, count: usize) -> HashMap<String, Value> {
        let mut replies = HashMap::new();
        while replies.len() < count {
            match tokio::time::timeout(Duration::from_secs(3), session.next_event())
                .await
                .expect("control request left unanswered")
                .unwrap()
            {
                SessionEvent::Frame(frame) => {
                    let reply = frame.raw["reply"].clone();
                    replies.insert(reply["request_id"].as_str().unwrap().to_owned(), reply);
                }
                SessionEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        replies
    }
    fn control(id: &str, request: Value) -> Value {
        json!({"type":"control_request","request_id":id,"request":request})
    }
    struct Silent;
    impl ControlHandler for Silent {
        fn handle(&self, _: ControlRequest, _: CancellationToken) -> HandlerFuture {
            Box::pin(async { Ok(None) })
        }
    }
    #[tokio::test]
    async fn unanswered_control_requests_get_sdk_default_replies() {
        let dir =
            std::env::temp_dir().join(format!("turin-claude-defaults-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let options = replier(
            &dir,
            json!([
                control(
                    "d1",
                    json!({"subtype":"request_user_dialog","dialog_kind":"x","payload":{}})
                ),
                control(
                    "e1",
                    json!({"subtype":"elicitation","mcp_server_name":"s","message":"m"})
                ),
                control("f1", json!({"subtype":"future_thing"})),
                control(
                    "c1",
                    json!({"subtype":"can_use_tool","tool_name":"Bash","input":{},"tool_use_id":"t"})
                ),
                control(
                    "h1",
                    json!({"subtype":"hook_callback","callback_id":"cb9","input":{}})
                ),
            ]),
        );
        let mut session = ClaudeSession::spawn(options, Arc::new(Silent))
            .await
            .unwrap();
        let replies = replies(&mut session, 4).await;
        assert_eq!(replies["e1"]["subtype"], "success");
        assert_eq!(replies["e1"]["response"], json!({"action":"decline"}));
        assert_eq!(replies["f1"]["subtype"], "error");
        assert_eq!(
            replies["f1"]["error"],
            "Unsupported control request subtype: future_thing"
        );
        assert_eq!(
            replies["c1"]["error"],
            "canUseTool callback is not provided."
        );
        assert_eq!(replies["h1"]["error"], "No hook callback found for ID: cb9");
        assert!(
            tokio::time::timeout(Duration::from_millis(300), session.next_event())
                .await
                .is_err(),
            "request_user_dialog must stay unanswered"
        );
        session.shutdown().await.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    struct PanicOnce(std::sync::atomic::AtomicBool);
    impl ControlHandler for PanicOnce {
        fn handle(&self, _: ControlRequest, _: CancellationToken) -> HandlerFuture {
            let first = !self.0.swap(true, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if first {
                    panic!("boom");
                }
                Ok(Some(json!({"behavior":"allow","updatedInput":{}})))
            })
        }
    }
    #[tokio::test]
    async fn panicking_handler_replies_with_an_error_and_frees_the_request_id() {
        let dir = std::env::temp_dir().join(format!("turin-claude-panic-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let options = script(
            &dir,
            r#"#!/usr/bin/env python3
import json,sys
request={"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{},"tool_use_id":"t1"}}
for line in sys.stdin:
    value=json.loads(line)
    if value.get("type")=="control_request" and value["request"]["subtype"]=="initialize":
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"models":[]}}}),flush=True)
        print(json.dumps(request),flush=True)
    elif value.get("type")=="control_response":
        print(json.dumps({"type":"system","subtype":"status","reply":value["response"]}),flush=True)
        if value["response"]["subtype"]=="error":
            print(json.dumps(request),flush=True)
"#,
        );
        let handler = Arc::new(PanicOnce(std::sync::atomic::AtomicBool::new(false)));
        let mut session = ClaudeSession::spawn(options, handler).await.unwrap();
        let mut seen = vec![];
        while seen.len() < 2 {
            match tokio::time::timeout(Duration::from_secs(3), session.next_event())
                .await
                .expect("panicking handler left the request unanswered")
                .unwrap()
            {
                SessionEvent::Frame(frame) => seen.push(frame.raw["reply"].clone()),
                SessionEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert_eq!(seen[0]["subtype"], "error");
        assert_eq!(seen[0]["error"], "control handler panicked: boom");
        assert_eq!(seen[1]["subtype"], "success");
        assert_eq!(seen[1]["response"]["toolUseID"], "t1");
        session.shutdown().await.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn blank_stdout_lines_are_skipped() {
        let dir = std::env::temp_dir().join(format!("turin-claude-blank-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let options = script(
            &dir,
            r#"#!/usr/bin/env python3
import json,sys
for line in sys.stdin:
    value=json.loads(line)
    if value.get("type")=="control_request" and value["request"]["subtype"]=="initialize":
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"models":[]}}}),flush=True)
        sys.stdout.write("\n   \n\t\n")
        print(json.dumps({"type":"system","subtype":"status","status":"after-blank"}),flush=True)
"#,
        );
        let mut session = ClaudeSession::spawn(options, Arc::new(Accept))
            .await
            .unwrap();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), session.next_event())
                .await
                .unwrap()
                .unwrap()
            {
                SessionEvent::Frame(frame) => {
                    assert_eq!(frame.raw["status"], "after-blank");
                    break;
                }
                SessionEvent::Error(error) => panic!("blank line surfaced as an error: {error}"),
                _ => {}
            }
        }
        session.shutdown().await.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn spawn_reports_exit_code_when_the_cli_dies_before_initialize() {
        let dir = std::env::temp_dir().join(format!("turin-claude-exit-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let options = script(
            &dir,
            "#!/usr/bin/env python3\nimport sys\nsys.stderr.write('Error: not logged in\\n')\nsys.stderr.flush()\nraise SystemExit(3)\n",
        );
        let error = match ClaudeSession::spawn(options, Arc::new(Accept)).await {
            Err(error) => error,
            Ok(_) => panic!("spawn succeeded"),
        };
        let text = error.to_string();
        assert!(
            text.contains("Claude Code process exited with code 3"),
            "{text}"
        );
        assert!(text.contains("Error: not logged in"), "{text}");
        fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn exit_event_and_shutdown_surface_the_status_after_init() {
        let dir = std::env::temp_dir().join(format!("turin-claude-exit7-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let options = script(
            &dir,
            r#"#!/usr/bin/env python3
import json,sys
for line in sys.stdin:
    value=json.loads(line)
    if value.get("type")=="control_request" and value["request"]["subtype"]=="initialize":
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"models":[]}}}),flush=True)
        sys.stderr.write("boom\n")
        sys.stderr.flush()
        raise SystemExit(7)
"#,
        );
        let mut session = ClaudeSession::spawn(options, Arc::new(Accept))
            .await
            .unwrap();
        let mut saw_stderr = false;
        let mut exit = None;
        let mut saw_eof = false;
        while !saw_stderr || exit.is_none() || !saw_eof {
            match tokio::time::timeout(Duration::from_secs(2), session.next_event())
                .await
                .unwrap()
                .unwrap()
            {
                SessionEvent::Stderr(text) => {
                    if text.contains("boom") {
                        saw_stderr = true;
                    }
                }
                SessionEvent::Exit(status) => exit = Some(status),
                SessionEvent::Eof => saw_eof = true,
                SessionEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert_eq!(
            exit.unwrap(),
            ProcessExit {
                code: Some(7),
                signal: None
            }
        );
        let status = session.shutdown().await.unwrap();
        assert_eq!(status.code, Some(7));
        fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn shutdown_kills_grandchildren_after_the_leader_exits() {
        let dir = std::env::temp_dir().join(format!("turin-claude-group-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let grandchild = dir.join("grandchild");
        let mut options = script(
            &dir,
            r#"#!/usr/bin/env python3
import json,os,sys
for line in sys.stdin:
    value=json.loads(line)
    if value.get("type")=="control_request" and value["request"]["subtype"]=="initialize":
        print(json.dumps({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"models":[]}}}),flush=True)
        pid=os.fork()
        if pid==0:
            os.execvp("sleep", ["sleep", "47"])
        handle=open(os.environ["GRANDCHILD"], "w")
        handle.write(str(pid))
        handle.flush()
        os._exit(0)
"#,
        );
        options
            .env
            .as_mut()
            .unwrap()
            .insert("GRANDCHILD".into(), grandchild.display().to_string());
        let session = ClaudeSession::spawn(options, Arc::new(Accept))
            .await
            .unwrap();
        let pid = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&grandchild).await {
                    if let Ok(pid) = text.trim().parse::<i32>() {
                        return pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("grandchild pid was not written");
        let status = session.shutdown().await.unwrap();
        assert_eq!(status.code, Some(0));
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "grandchild {pid} still alive"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
