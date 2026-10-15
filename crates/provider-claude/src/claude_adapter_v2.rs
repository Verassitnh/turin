//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts.
//! The protocol session owns the CLI; this layer maps its controls and frames
//! into the provider-facing lifecycle. Orchestration owns persistence and UI IDs.
use crate::claude_permission::{
    ApprovalDecision, auto_approved, cancelled_question, captured_plan,
    permission_result_from_decision,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use turin_claude_protocol::{
    ClaudeSession, ControlHandler, ControlRequest, Frame, FrameKind, InboundControl, LaunchOptions,
    ProcessExit, SessionEvent, SessionInfo, SessionMessage, TranscriptOptions, UserMessage,
    UserPriority, fork_session, get_session_messages, get_subagent_messages, list_sessions,
};

pub const PROVIDER: &str = "claudeAgent";
pub const QUERY_PROTOCOL: &str = "claude-agent-sdk.query";

/// The orchestrator answers requests while the protocol reader keeps draining
/// stdout. Dropping a response sender cancels the corresponding interaction.
pub enum ClaudeHostRequest {
    Approval {
        request: ControlRequest,
        reply: oneshot::Sender<ApprovalDecision>,
    },
    Question {
        request: ControlRequest,
        reply: oneshot::Sender<Option<Value>>,
    },
    Dialog {
        request: ControlRequest,
        reply: oneshot::Sender<Option<Value>>,
    },
    Hook {
        request: ControlRequest,
        reply: oneshot::Sender<Option<Value>>,
    },
    /// `ack` is answered once the plan is recorded, so the CLI cannot finish
    /// the turn before the plan is visible.
    ProposedPlan {
        tool_use_id: String,
        markdown: String,
        ack: oneshot::Sender<()>,
    },
}

struct HostHandler {
    sender: mpsc::UnboundedSender<ClaudeHostRequest>,
    require_approval: bool,
}
impl ControlHandler for HostHandler {
    fn handle(
        &self,
        request: ControlRequest,
        cancel: CancellationToken,
    ) -> turin_claude_protocol::session::HandlerFuture {
        let sender = self.sender.clone();
        let require_approval = self.require_approval;
        Box::pin(async move {
            match &request.kind {
                InboundControl::CanUseTool {
                    tool_name,
                    input,
                    tool_use_id,
                    permission_suggestions,
                    ..
                } => {
                    if tool_name == "ExitPlanMode" {
                        if let Some(plan) = proposed_plan(input) {
                            let (ack, recorded) = oneshot::channel();
                            if sender
                                .send(ClaudeHostRequest::ProposedPlan {
                                    tool_use_id: tool_use_id.clone(),
                                    markdown: plan,
                                    ack,
                                })
                                .is_ok()
                            {
                                tokio::select! { _ = recorded => {}, _ = cancel.cancelled() => {} }
                            }
                        }
                        return Ok(Some(captured_plan(tool_use_id)));
                    }
                    if tool_name == "AskUserQuestion" {
                        let (tx, rx) = oneshot::channel();
                        if sender
                            .send(ClaudeHostRequest::Question {
                                request: request.clone(),
                                reply: tx,
                            })
                            .is_err()
                        {
                            return Ok(Some(cancelled_question()));
                        }
                        let answers = tokio::select! { answer = rx => answer.ok().flatten(), _ = cancel.cancelled() => None };
                        return Ok(Some(match answers {
                            Some(answers) => {
                                json!({"behavior":"allow","updatedInput":{"questions":input.get("questions").cloned().unwrap_or(Value::Null),"answers":normalize_question_answers(&answers)},"toolUseID":tool_use_id})
                            }
                            None => cancelled_question(),
                        }));
                    }
                    if !require_approval {
                        return Ok(Some(auto_approved(input, tool_use_id)));
                    }
                    let (tx, rx) = oneshot::channel();
                    if sender
                        .send(ClaudeHostRequest::Approval {
                            request: request.clone(),
                            reply: tx,
                        })
                        .is_err()
                    {
                        return Ok(Some(permission_result_from_decision(
                            tool_name,
                            ApprovalDecision::Cancel,
                            input,
                            tool_use_id,
                            None,
                        )));
                    }
                    let decision = tokio::select! { decision = rx => decision.unwrap_or(ApprovalDecision::Cancel), _ = cancel.cancelled() => ApprovalDecision::Cancel };
                    let suggestions = permission_suggestions
                        .as_ref()
                        .and_then(Value::as_array)
                        .map(Vec::as_slice);
                    Ok(Some(permission_result_from_decision(
                        tool_name,
                        decision,
                        input,
                        tool_use_id,
                        suggestions,
                    )))
                }
                InboundControl::RequestUserDialog { .. } => {
                    let (tx, rx) = oneshot::channel();
                    if sender
                        .send(ClaudeHostRequest::Dialog {
                            request: request.clone(),
                            reply: tx,
                        })
                        .is_err()
                    {
                        return Ok(Some(json!({"behavior":"cancelled"})));
                    }
                    Ok(Some(
                        tokio::select! { value = rx => value.ok().flatten(), _ = cancel.cancelled() => None }
                            .unwrap_or_else(|| json!({"behavior":"cancelled"})),
                    ))
                }
                InboundControl::HookCallback { .. } => {
                    let (tx, rx) = oneshot::channel();
                    if sender
                        .send(ClaudeHostRequest::Hook {
                            request: request.clone(),
                            reply: tx,
                        })
                        .is_err()
                    {
                        return Ok(None);
                    }
                    Ok(
                        tokio::select! { value = rx => value.ok().flatten(), _ = cancel.cancelled() => None },
                    )
                }
                _ => Ok(None),
            }
        })
    }
}

pub fn process_exit_message(exit: &ProcessExit) -> String {
    match exit.signal {
        Some(signal) => format!("Claude Code process exited with signal {signal}"),
        None => format!(
            "Claude Code process exited with code {}",
            exit.code.unwrap_or(0)
        ),
    }
}

pub fn proposed_plan(input: &Value) -> Option<String> {
    let value = if input.get("type").and_then(Value::as_str) == Some("record") {
        input.get("value").unwrap_or(input)
    } else {
        input
    };
    value
        .get("plan")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn normalize_question_answers(answers: &Value) -> Value {
    let Some(map) = answers.as_object() else {
        return Value::Object(Default::default());
    };
    Value::Object(
        map.iter()
            .map(|(question, answer)| {
                let text = match answer {
                    Value::String(s) => s.clone(),
                    Value::Array(values) => values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                (question.clone(), Value::String(text))
            })
            .collect(),
    )
}

/// Events remain lossless through `raw`; the typed variants identify the
/// boundaries the orchestrator must handle specially.
#[derive(Debug)]
pub enum ClaudeAdapterEvent {
    Init {
        session_id: String,
        capabilities: Vec<String>,
        frame: Frame,
    },
    Assistant(Frame),
    User(Frame),
    Stream(Frame),
    Result(Frame),
    Compaction(Frame),
    RateLimit(Frame),
    Background(Frame),
    Hook(Frame),
    Other(Frame),
    Stderr(String),
    Error(String),
    Eof,
}

pub fn project_frame(frame: Frame) -> ClaudeAdapterEvent {
    match &frame.kind {
        FrameKind::Init(init) => ClaudeAdapterEvent::Init {
            session_id: init.session_id.clone(),
            capabilities: init.capabilities.clone(),
            frame,
        },
        FrameKind::Assistant(_) => ClaudeAdapterEvent::Assistant(frame),
        FrameKind::User(_) => ClaudeAdapterEvent::User(frame),
        FrameKind::StreamEvent(_) => ClaudeAdapterEvent::Stream(frame),
        FrameKind::Result(_) => ClaudeAdapterEvent::Result(frame),
        FrameKind::RateLimitEvent(_) => ClaudeAdapterEvent::RateLimit(frame),
        FrameKind::System(system) => match &system.subtype {
            turin_claude_protocol::frame::SystemSubtype::CompactBoundary => {
                ClaudeAdapterEvent::Compaction(frame)
            }
            turin_claude_protocol::frame::SystemSubtype::TaskStarted
            | turin_claude_protocol::frame::SystemSubtype::TaskUpdated
            | turin_claude_protocol::frame::SystemSubtype::TaskProgress
            | turin_claude_protocol::frame::SystemSubtype::TaskNotification
            | turin_claude_protocol::frame::SystemSubtype::BackgroundTasksChanged => {
                ClaudeAdapterEvent::Background(frame)
            }
            turin_claude_protocol::frame::SystemSubtype::HookStarted
            | turin_claude_protocol::frame::SystemSubtype::HookProgress
            | turin_claude_protocol::frame::SystemSubtype::HookResponse => {
                ClaudeAdapterEvent::Hook(frame)
            }
            _ => ClaudeAdapterEvent::Other(frame),
        },
        _ => ClaudeAdapterEvent::Other(frame),
    }
}

pub struct ClaudeAdapterV2 {
    session: Option<ClaudeSession>,
    requests: Option<mpsc::UnboundedReceiver<ClaudeHostRequest>>,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    pending_prompts: usize,
    pub latest_turn_capabilities: Vec<String>,
}

impl ClaudeAdapterV2 {
    pub async fn open(mut options: LaunchOptions, require_approval: bool) -> Result<Self> {
        // External MCP server secrets must not appear in `--mcp-config` argv.
        let mcp_servers = std::mem::take(&mut options.mcp_servers);
        options.can_use_tool = true;
        options.include_partial_messages = true;
        options.initialize.supported_dialog_kinds = Some(vec!["resume_return".into()]);
        let cwd = options.cwd.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        let handler = Arc::new(HostHandler {
            sender: tx,
            require_approval,
        });
        let session = match ClaudeSession::spawn(options.clone(), handler.clone()).await {
            Ok(session) => session,
            Err(error) if options.resume.is_some() => {
                tracing::warn!(%error, "Claude resume rejected; starting a fresh session");
                options.resume = None;
                options.resume_session_at = None;
                options.session_id = Some(uuid::Uuid::new_v4().to_string());
                ClaudeSession::spawn(options.clone(), handler).await?
            }
            Err(error) => return Err(error),
        };
        if !mcp_servers.is_empty() {
            tokio::time::timeout(
                std::time::Duration::from_secs(90),
                session.set_mcp_servers(serde_json::to_value(mcp_servers)?),
            )
            .await
            .context("Claude MCP registration timed out")??;
        }
        let session_id = options
            .resume
            .clone()
            .or_else(|| options.session_id.clone());
        Ok(Self {
            session: Some(session),
            requests: Some(rx),
            session_id,
            cwd,
            pending_prompts: 0,
            latest_turn_capabilities: vec![],
        })
    }
    pub fn take_requests(&mut self) -> Option<mpsc::UnboundedReceiver<ClaudeHostRequest>> {
        self.requests.take()
    }
    pub fn initialization(&self) -> Option<&Value> {
        self.session.as_ref().map(ClaudeSession::initialization)
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    pub fn send_prompt(
        &mut self,
        text: impl Into<String>,
        uuid: Option<String>,
        priority: Option<UserPriority>,
    ) -> Result<()> {
        let message = UserMessage::text_with_priority(text, uuid, priority);
        self.send_message(message)
    }
    pub fn send_blocks(
        &mut self,
        blocks: Vec<Value>,
        uuid: Option<String>,
        priority: Option<UserPriority>,
    ) -> Result<()> {
        let mut message = UserMessage::blocks(blocks, uuid, false);
        message.priority = priority;
        self.send_message(message)
    }
    pub fn steer(&mut self, text: impl Into<String>, uuid: Option<String>) -> Result<()> {
        self.send_prompt(text, uuid, Some(UserPriority::Now))
    }
    fn send_message(&mut self, message: UserMessage) -> Result<()> {
        self.session
            .as_ref()
            .context("Claude session closed")?
            .send(message)?;
        self.pending_prompts += 1;
        Ok(())
    }
    pub async fn next_event(&mut self) -> Option<ClaudeAdapterEvent> {
        let event = self.session.as_mut()?.next_event().await?;
        Some(match event {
            SessionEvent::Frame(frame) => {
                if let FrameKind::Init(init) = &frame.kind {
                    self.session_id = Some(init.session_id.clone());
                    self.latest_turn_capabilities = init.capabilities.clone();
                }
                if matches!(frame.kind, FrameKind::Result(_)) {
                    self.pending_prompts = self.pending_prompts.saturating_sub(1);
                }
                project_frame(frame)
            }
            SessionEvent::Stderr(value) => ClaudeAdapterEvent::Stderr(value),
            SessionEvent::Error(value) => ClaudeAdapterEvent::Error(value),
            SessionEvent::Eof => {
                if self.pending_prompts > 0 {
                    self.pending_prompts = 0;
                    ClaudeAdapterEvent::Error(
                        "Claude Code process exited before turn result".into(),
                    )
                } else {
                    ClaudeAdapterEvent::Eof
                }
            }
            SessionEvent::Exit(exit) => {
                if self.pending_prompts > 0 || exit.code.is_some_and(|code| code != 0) {
                    self.pending_prompts = 0;
                    ClaudeAdapterEvent::Error(process_exit_message(&exit))
                } else {
                    ClaudeAdapterEvent::Eof
                }
            }
        })
    }
    pub async fn interrupt(&self) -> Result<Value> {
        self.session
            .as_ref()
            .context("Claude session closed")?
            .interrupt(false)
            .await
    }
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.session
            .as_ref()
            .context("Claude session closed")?
            .set_model(model)
            .await
    }
    pub async fn set_permission_mode(&self, mode: &str) -> Result<()> {
        self.session
            .as_ref()
            .context("Claude session closed")?
            .set_permission_mode(mode)
            .await
    }
    pub async fn apply_flag_settings(&self, settings: Value) -> Result<()> {
        self.session
            .as_ref()
            .context("Claude session closed")?
            .apply_flag_settings(settings)
            .await
    }
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.session
            .as_ref()
            .context("Claude session closed")?
            .stop_task(task_id)
            .await
    }
    pub async fn close(&mut self) -> Result<()> {
        if let Some(session) = self.session.take() {
            session.shutdown().await?;
        }
        Ok(())
    }
    /// SDK-local transcript fork. Close the source CLI before reading its JSONL
    /// so buffered transcript writes cannot be omitted from the fork.
    pub async fn fork(
        &mut self,
        up_to_message_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<String> {
        anyhow::ensure!(
            self.pending_prompts == 0,
            "cannot fork during an active turn"
        );
        let id = self
            .session_id
            .clone()
            .context("Claude session ID not known")?;
        self.close().await?;
        let options = TranscriptOptions {
            dir: self.cwd.clone(),
            ..Default::default()
        };
        fork_session(&id, &options, up_to_message_id, title).await
    }
    /// Async replacement for `claudeHistoryWorker`'s isolated Node process.
    pub async fn history(&self, include_system_messages: bool) -> Result<Vec<SessionMessage>> {
        let id = self
            .session_id
            .as_deref()
            .context("Claude session ID not known")?;
        let options = TranscriptOptions {
            dir: self.cwd.clone(),
            include_system_messages,
            ..Default::default()
        };
        get_session_messages(id, &options).await
    }
    pub async fn subagent_history(&self, agent_id: &str) -> Result<Vec<SessionMessage>> {
        let id = self
            .session_id
            .as_deref()
            .context("Claude session ID not known")?;
        let options = TranscriptOptions {
            dir: self.cwd.clone(),
            ..Default::default()
        };
        get_subagent_messages(id, agent_id, &options).await
    }
    pub async fn list_history(cwd: Option<PathBuf>) -> Result<Vec<SessionInfo>> {
        list_sessions(&TranscriptOptions {
            dir: cwd,
            ..Default::default()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn plan_and_question_projection() {
        assert_eq!(
            proposed_plan(&json!({"type":"record","value":{"plan":"  Go  "}})),
            Some("Go".into())
        );
        assert_eq!(
            normalize_question_answers(&json!({"Q":["A","B",3]}))["Q"],
            "A, B"
        );
    }
    #[test]
    fn fixture_frames_project_losslessly() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../upstream/apps/server/src/orchestration-v2/testkit/fixtures");
        for scenario in [
            "simple",
            "message_steering",
            "claude_background_subagent_lifecycle",
            "claude_compact_after_peer_turn",
        ] {
            let source =
                std::fs::read_to_string(root.join(scenario).join("claude_transcript.ndjson"))
                    .unwrap();
            let mut frames = 0;
            for line in source.lines() {
                let entry: Value = serde_json::from_str(line).unwrap();
                if entry["type"] != "emit_inbound" {
                    continue;
                }
                let raw = entry["frame"].clone();
                let frame = Frame::decode(raw.clone()).unwrap();
                let event = project_frame(frame);
                let projected = match event {
                    ClaudeAdapterEvent::Init { frame, .. }
                    | ClaudeAdapterEvent::Assistant(frame)
                    | ClaudeAdapterEvent::User(frame)
                    | ClaudeAdapterEvent::Stream(frame)
                    | ClaudeAdapterEvent::Result(frame)
                    | ClaudeAdapterEvent::Compaction(frame)
                    | ClaudeAdapterEvent::RateLimit(frame)
                    | ClaudeAdapterEvent::Background(frame)
                    | ClaudeAdapterEvent::Hook(frame)
                    | ClaudeAdapterEvent::Other(frame) => frame.raw,
                    _ => unreachable!(),
                };
                assert_eq!(projected, raw);
                frames += 1;
            }
            assert!(frames > 0, "{scenario}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_cli_replays_upstream_simple_fixture_without_paid_turn() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../upstream/apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_transcript.ndjson");
        let dir =
            std::env::temp_dir().join(format!("turin-provider-claude-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let script = dir.join("fake-claude");
        std::fs::write(&script, r##"#!/usr/bin/env python3
import json,os,sys
for line in sys.stdin:
    value=json.loads(line)
    request=value.get('request') or {}
    if value.get('type')=='control_request' and request.get('subtype')=='initialize':
        print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':{'models':[],'commands':[],'account':{}}}}),flush=True)
    elif value.get('type')=='user':
        for line in open(os.environ['FAKE_FIXTURE']):
            entry=json.loads(line)
            if entry.get('type')=='emit_inbound':
                print(json.dumps(entry['frame']),flush=True)
"##).unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).unwrap();
        let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        env.insert("FAKE_FIXTURE".into(), root.display().to_string());
        let options = LaunchOptions {
            binary: script,
            env: Some(env),
            ..Default::default()
        };
        let mut adapter = ClaudeAdapterV2::open(options, false).await.unwrap();
        adapter
            .send_prompt("fixture simple ok", Some("prompt-1".into()), None)
            .unwrap();
        let mut saw_assistant = false;
        let mut saw_rate_limit = false;
        loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(5), adapter.next_event())
                    .await
                    .unwrap()
                    .unwrap();
            match event {
                ClaudeAdapterEvent::Assistant(frame) => {
                    saw_assistant =
                        frame.raw["message"]["content"][0]["text"] == "fixture simple ok";
                }
                ClaudeAdapterEvent::RateLimit(_) => saw_rate_limit = true,
                ClaudeAdapterEvent::Result(frame) => {
                    assert_eq!(frame.raw["result"], "fixture simple ok");
                    break;
                }
                ClaudeAdapterEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(saw_assistant && saw_rate_limit);
        adapter.close().await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
