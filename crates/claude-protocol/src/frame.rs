//! Port of @anthropic-ai/claude-agent-sdk@0.3.276 sdk.d.ts SDKMessage and input envelopes.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub raw: Value,
}
#[derive(Debug, Clone, PartialEq)]
pub enum FrameKind {
    Init(InitFrame),
    Assistant(AssistantFrame),
    User(UserFrame),
    StreamEvent(StreamEventFrame),
    Result(ResultFrame),
    RateLimitEvent(RateLimitFrame),
    System(SystemFrame),
    CommandLifecycle(CommandLifecycleFrame),
    ToolProgress(ToolProgressFrame),
    AuthStatus(AuthStatusFrame),
    ConversationReset(Value),
    Unknown {
        kind: String,
        subtype: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InitFrame {
    pub session_id: String,
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub agents: Vec<Value>,
    #[serde(default)]
    pub slash_commands: Vec<Value>,
    #[serde(default)]
    pub skills: Vec<Value>,
    #[serde(default)]
    pub plugins: Vec<Value>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub output_style: Option<String>,
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: Option<String>,
    #[serde(default, rename = "apiKeySource")]
    pub api_key_source: Option<String>,
    #[serde(default)]
    pub claude_code_version: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub fast_mode_state: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantFrame {
    pub message: Value,
    pub uuid: String,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub user_message_uuid: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserFrame {
    pub message: Value,
    pub uuid: String,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_use_result: Option<Value>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamEventFrame {
    pub event: Value,
    pub uuid: String,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub user_message_uuid: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResultFrame {
    pub subtype: String,
    pub is_error: bool,
    pub uuid: String,
    pub session_id: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub terminal_reason: Option<TerminalReason>,
    #[serde(default)]
    pub api_error_status: Option<u16>,
    #[serde(default)]
    pub num_turns: Option<u64>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default, rename = "modelUsage")]
    pub model_usage: Option<Value>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub permission_denials: Vec<Value>,
    #[serde(default)]
    pub user_message_uuid: Option<String>,
    #[serde(default)]
    pub user_message_uuids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RateLimitFrame {
    pub rate_limit_info: Value,
    pub uuid: String,
    pub session_id: String,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AuthStatusFrame {
    #[serde(rename = "isAuthenticating")]
    pub is_authenticating: bool,
    #[serde(default)]
    pub output: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub uuid: String,
    pub session_id: String,
}
impl ResultFrame {
    /// Upstream drops diagnostics prefixed with `[ede_diagnostic]` before showing `errors`.
    pub fn visible_errors(&self) -> impl Iterator<Item = &str> {
        self.errors.iter().filter_map(|error| {
            let trimmed = error.trim();
            (!trimmed.starts_with("[ede_diagnostic]")).then_some(trimmed)
        })
    }
    /// A success result with HTTP 529 is still overload.
    pub fn is_overload(&self) -> bool {
        self.api_error_status == Some(529)
    }
}

/// `sdk.d.ts` `TerminalReason`. Unknown strings stay readable and round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReason {
    BlockingLimit,
    RapidRefillBreaker,
    PromptTooLong,
    ImageError,
    ModelError,
    ApiError,
    MalformedToolUseExhausted,
    AbortedStreaming,
    AbortedTools,
    StopHookPrevented,
    HookStopped,
    ToolDeferred,
    MaxTurns,
    BackgroundRequested,
    Completed,
    BudgetExhausted,
    StructuredOutputRetryExhausted,
    ToolDeferredUnavailable,
    TurnSetupFailed,
    Other(String),
}
impl TerminalReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::BlockingLimit => "blocking_limit",
            Self::RapidRefillBreaker => "rapid_refill_breaker",
            Self::PromptTooLong => "prompt_too_long",
            Self::ImageError => "image_error",
            Self::ModelError => "model_error",
            Self::ApiError => "api_error",
            Self::MalformedToolUseExhausted => "malformed_tool_use_exhausted",
            Self::AbortedStreaming => "aborted_streaming",
            Self::AbortedTools => "aborted_tools",
            Self::StopHookPrevented => "stop_hook_prevented",
            Self::HookStopped => "hook_stopped",
            Self::ToolDeferred => "tool_deferred",
            Self::MaxTurns => "max_turns",
            Self::BackgroundRequested => "background_requested",
            Self::Completed => "completed",
            Self::BudgetExhausted => "budget_exhausted",
            Self::StructuredOutputRetryExhausted => "structured_output_retry_exhausted",
            Self::ToolDeferredUnavailable => "tool_deferred_unavailable",
            Self::TurnSetupFailed => "turn_setup_failed",
            Self::Other(value) => value,
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "blocking_limit" => Self::BlockingLimit,
            "rapid_refill_breaker" => Self::RapidRefillBreaker,
            "prompt_too_long" => Self::PromptTooLong,
            "image_error" => Self::ImageError,
            "model_error" => Self::ModelError,
            "api_error" => Self::ApiError,
            "malformed_tool_use_exhausted" => Self::MalformedToolUseExhausted,
            "aborted_streaming" => Self::AbortedStreaming,
            "aborted_tools" => Self::AbortedTools,
            "stop_hook_prevented" => Self::StopHookPrevented,
            "hook_stopped" => Self::HookStopped,
            "tool_deferred" => Self::ToolDeferred,
            "max_turns" => Self::MaxTurns,
            "background_requested" => Self::BackgroundRequested,
            "completed" => Self::Completed,
            "budget_exhausted" => Self::BudgetExhausted,
            "structured_output_retry_exhausted" => Self::StructuredOutputRetryExhausted,
            "tool_deferred_unavailable" => Self::ToolDeferredUnavailable,
            "turn_setup_failed" => Self::TurnSetupFailed,
            other => Self::Other(other.to_owned()),
        }
    }
}
impl Serialize for TerminalReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for TerminalReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CommandLifecycleFrame {
    pub command_uuid: String,
    pub state: String,
    pub uuid: String,
    pub session_id: String,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolProgressFrame {
    pub tool_use_id: String,
    pub tool_name: String,
    pub parent_tool_use_id: Option<String>,
    pub elapsed_time_seconds: f64,
    pub uuid: String,
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub heartbeat: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemSubtype {
    Status,
    CompactBoundary,
    ToolProgress,
    HookStarted,
    HookProgress,
    HookResponse,
    TaskStarted,
    TaskUpdated,
    TaskProgress,
    TaskNotification,
    BackgroundTasksChanged,
    ApiRetry,
    ModelRefusalFallback,
    ModelRefusalNoFallback,
    CommandsChanged,
    SessionStateChanged,
    CommandLifecycle,
    ControlRequestProgress,
    ThinkingTokens,
    Informational,
    Other(String),
}
impl SystemSubtype {
    pub fn parse(value: &str) -> Self {
        match value {
            "status" => Self::Status,
            "compact_boundary" => Self::CompactBoundary,
            "tool_progress" => Self::ToolProgress,
            "hook_started" => Self::HookStarted,
            "hook_progress" => Self::HookProgress,
            "hook_response" => Self::HookResponse,
            "task_started" => Self::TaskStarted,
            "task_updated" => Self::TaskUpdated,
            "task_progress" => Self::TaskProgress,
            "task_notification" => Self::TaskNotification,
            "background_tasks_changed" => Self::BackgroundTasksChanged,
            "api_retry" => Self::ApiRetry,
            "model_refusal_fallback" => Self::ModelRefusalFallback,
            "model_refusal_no_fallback" => Self::ModelRefusalNoFallback,
            "commands_changed" => Self::CommandsChanged,
            "session_state_changed" => Self::SessionStateChanged,
            "command_lifecycle" => Self::CommandLifecycle,
            "control_request_progress" => Self::ControlRequestProgress,
            "thinking_tokens" => Self::ThinkingTokens,
            "informational" => Self::Informational,
            other => Self::Other(other.into()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TaskStarted {
    pub task_id: String,
    pub description: String,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub subagent_type: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TaskProgress {
    pub task_id: String,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub summary: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TaskNotification {
    pub task_id: String,
    pub status: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub output_file: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HookEvent {
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i64>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum SystemPayload {
    Status {
        status: Option<String>,
    },
    CompactBoundary {
        compact_metadata: Value,
    },
    TaskStarted(TaskStarted),
    TaskUpdated {
        task_id: String,
        patch: Value,
    },
    TaskProgress(TaskProgress),
    TaskNotification(TaskNotification),
    Hook(HookEvent),
    ToolProgress(Value),
    ApiRetry(Value),
    ThinkingTokens {
        estimated_tokens: f64,
        estimated_tokens_delta: Option<f64>,
    },
    ModelRefusalFallback(Value),
    Other(Value),
}
#[derive(Debug, Clone, PartialEq)]
pub struct SystemFrame {
    pub subtype: SystemSubtype,
    pub uuid: Option<String>,
    pub session_id: Option<String>,
    pub payload: SystemPayload,
    pub raw: Value,
}
impl SystemFrame {
    fn decode(raw: Value, subtype: &str) -> serde_json::Result<Self> {
        let payload = match subtype {
            "status" => SystemPayload::Status {
                status: raw.get("status").and_then(Value::as_str).map(str::to_owned),
            },
            "compact_boundary" => SystemPayload::CompactBoundary {
                compact_metadata: required_value(&raw, "compact_metadata")?,
            },
            "task_started" => SystemPayload::TaskStarted(serde_json::from_value(raw.clone())?),
            "task_updated" => SystemPayload::TaskUpdated {
                task_id: required_string(&raw, "task_id")?,
                patch: required_value(&raw, "patch")?,
            },
            "task_progress" => SystemPayload::TaskProgress(serde_json::from_value(raw.clone())?),
            "task_notification" => {
                SystemPayload::TaskNotification(serde_json::from_value(raw.clone())?)
            }
            "hook_started" | "hook_progress" | "hook_response" => {
                SystemPayload::Hook(serde_json::from_value(raw.clone())?)
            }
            "tool_progress" => SystemPayload::ToolProgress(raw.clone()),
            "api_retry" => SystemPayload::ApiRetry(raw.clone()),
            "thinking_tokens" => SystemPayload::ThinkingTokens {
                estimated_tokens: raw
                    .get("estimated_tokens")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| {
                        <serde_json::Error as serde::de::Error>::custom("missing estimated_tokens")
                    })?,
                estimated_tokens_delta: raw.get("estimated_tokens_delta").and_then(Value::as_f64),
            },
            "model_refusal_fallback" | "model_refusal_no_fallback" => {
                SystemPayload::ModelRefusalFallback(raw.clone())
            }
            _ => SystemPayload::Other(raw.clone()),
        };
        Ok(Self {
            subtype: SystemSubtype::parse(subtype),
            uuid: raw.get("uuid").and_then(Value::as_str).map(str::to_owned),
            session_id: raw
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            payload,
            raw,
        })
    }
}
fn required_value(raw: &Value, name: &str) -> serde_json::Result<Value> {
    raw.get(name)
        .cloned()
        .ok_or_else(|| <serde_json::Error as serde::de::Error>::custom(format!("missing {name}")))
}
fn required_string(raw: &Value, name: &str) -> serde_json::Result<String> {
    raw.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| <serde_json::Error as serde::de::Error>::custom(format!("missing {name}")))
}
impl Frame {
    pub fn decode(raw: Value) -> serde_json::Result<Self> {
        let kind = raw.get("type").and_then(Value::as_str).unwrap_or("");
        let subtype = raw.get("subtype").and_then(Value::as_str);
        let parsed = match (kind, subtype) {
            ("system", Some("init")) => FrameKind::Init(serde_json::from_value(raw.clone())?),
            ("system", Some(s)) => FrameKind::System(SystemFrame::decode(raw.clone(), s)?),
            ("assistant", _) => FrameKind::Assistant(serde_json::from_value(raw.clone())?),
            ("user", _) => FrameKind::User(serde_json::from_value(raw.clone())?),
            ("stream_event", _) => FrameKind::StreamEvent(serde_json::from_value(raw.clone())?),
            ("result", _) => FrameKind::Result(serde_json::from_value(raw.clone())?),
            ("rate_limit_event", _) => {
                FrameKind::RateLimitEvent(serde_json::from_value(raw.clone())?)
            }
            ("command_lifecycle", _) => {
                FrameKind::CommandLifecycle(serde_json::from_value(raw.clone())?)
            }
            ("tool_progress", _) => FrameKind::ToolProgress(serde_json::from_value(raw.clone())?),
            ("auth_status", _) => FrameKind::AuthStatus(serde_json::from_value(raw.clone())?),
            ("conversation_reset", _) => FrameKind::ConversationReset(raw.clone()),
            _ => FrameKind::Unknown {
                kind: kind.into(),
                subtype: subtype.map(str::to_owned),
            },
        };
        Ok(Self { kind: parsed, raw })
    }
}

/// SDK submitMessage input; content blocks (including image source blocks) pass through unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<Value>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserPriority {
    Now,
    Next,
    Later,
}

/// `{type:"image",source:{type:"base64",media_type,data}}`. Place image blocks
/// before the command text block; a command block followed by an image does not expand.
pub fn image_block(media_type: &str, data_base64: &str) -> Value {
    serde_json::json!({
        "type": "image",
        "source": { "type": "base64", "media_type": media_type, "data": data_base64 }
    })
}
pub fn text_block(text: &str) -> Value {
    serde_json::json!({ "type": "text", "text": text })
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessageBody {
    pub role: String,
    pub content: UserContent,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: UserMessageBody,
    pub parent_tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<UserPriority>,
}
impl UserMessage {
    pub fn text(text: impl Into<String>, uuid: Option<String>, now: bool) -> Self {
        Self::text_with_priority(text, uuid, now.then_some(UserPriority::Now))
    }
    pub fn text_with_priority(
        text: impl Into<String>,
        uuid: Option<String>,
        priority: Option<UserPriority>,
    ) -> Self {
        Self {
            kind: "user".into(),
            message: UserMessageBody {
                role: "user".into(),
                content: UserContent::Text(text.into()),
            },
            parent_tool_use_id: None,
            uuid,
            priority,
        }
    }
    pub fn blocks(blocks: Vec<Value>, uuid: Option<String>, now: bool) -> Self {
        Self::blocks_with_priority(blocks, uuid, now.then_some(UserPriority::Now))
    }
    pub fn blocks_with_priority(
        blocks: Vec<Value>,
        uuid: Option<String>,
        priority: Option<UserPriority>,
    ) -> Self {
        Self {
            kind: "user".into(),
            message: UserMessageBody {
                role: "user".into(),
                content: UserContent::Blocks(blocks),
            },
            parent_tool_use_id: None,
            uuid,
            priority,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "lowercase")]
pub enum UserDialogResult {
    Completed { result: Value },
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionClassification {
    UserTemporary,
    UserPermanent,
    UserReject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "lowercase")]
pub enum PermissionResult {
    Allow {
        #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
        #[serde(rename = "updatedPermissions", skip_serializing_if = "Option::is_none")]
        updated_permissions: Option<Vec<Value>>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(
            rename = "decisionClassification",
            skip_serializing_if = "Option::is_none"
        )]
        decision_classification: Option<DecisionClassification>,
    },
    Deny {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(
            rename = "decisionClassification",
            skip_serializing_if = "Option::is_none"
        )]
        decision_classification: Option<DecisionClassification>,
    },
}
impl PermissionResult {
    pub fn with_tool_use_id(mut self, id: &str) -> Self {
        match &mut self {
            Self::Allow { tool_use_id, .. } | Self::Deny { tool_use_id, .. } => {
                *tool_use_id = Some(id.into())
            }
        }
        self
    }
    /// SDK cancel reply: deny, interrupt, `user_reject`.
    pub fn user_cancelled(tool_use_id: impl Into<String>) -> Self {
        Self::Deny {
            message: "User cancelled tool execution.".into(),
            interrupt: Some(true),
            tool_use_id: Some(tool_use_id.into()),
            decision_classification: Some(DecisionClassification::UserReject),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_upstream_claude_fixtures() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../upstream/apps/server/src/orchestration-v2/testkit/fixtures");
        let mut count = 0;
        let mut errors = Vec::new();
        for entry in walk(&root) {
            let name = entry.file_name().unwrap().to_string_lossy();
            if !name.contains("claude") {
                continue;
            }
            if entry.extension().and_then(|ext| ext.to_str()) != Some("ndjson") {
                continue;
            }
            let fixture = std::fs::read_to_string(&entry).unwrap();
            for line in fixture.lines() {
                let record: Value = serde_json::from_str(line)
                    .unwrap_or_else(|err| panic!("{}: {err}", entry.display()));
                if record.get("type").and_then(Value::as_str) != Some("emit_inbound") {
                    continue;
                }
                let raw = record.get("frame").cloned().unwrap_or(Value::Null);
                match Frame::decode(raw.clone()) {
                    Ok(frame) => {
                        assert_eq!(frame.raw, raw);
                        count += 1;
                    }
                    Err(err) => errors.push(format!("{} {raw}: {err}", entry.display())),
                }
            }
        }
        assert!(errors.is_empty(), "{}", errors.join("\n"));
        assert!(count > 1000, "decoded {count} frames");
    }
    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = vec![];
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }
    #[test]
    fn permission_result_wire_casing() {
        let result = PermissionResult::user_cancelled("toolu_1");
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["behavior"], "deny");
        assert_eq!(value["toolUseID"], "toolu_1");
        assert_eq!(value["interrupt"], true);
        assert_eq!(value["decisionClassification"], "user_reject");
        assert_eq!(value["message"], "User cancelled tool execution.");
        let dialog = serde_json::to_value(UserDialogResult::Completed {
            result: serde_json::json!("compact"),
        })
        .unwrap();
        assert_eq!(dialog["behavior"], "completed");
        assert_eq!(dialog["result"], "compact");
        assert_eq!(
            serde_json::to_value(UserDialogResult::Cancelled).unwrap()["behavior"],
            "cancelled"
        );
        let steered =
            serde_json::to_value(UserMessage::text("go", Some("prompt-1".into()), true)).unwrap();
        assert_eq!(steered["priority"], "now");
        assert_eq!(steered["parent_tool_use_id"], Value::Null);
        assert_eq!(steered["uuid"], "prompt-1");
    }
    #[test]
    fn unknown_is_preserved_and_invalid_known_rejected() {
        let raw = serde_json::json!({"type":"future_frame","subtype":"x","a":1});
        assert_eq!(Frame::decode(raw.clone()).unwrap().raw, raw);
        assert!(Frame::decode(serde_json::json!({"type":"result","subtype":"success"})).is_err());
    }
}
