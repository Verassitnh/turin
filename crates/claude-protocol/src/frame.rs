//! Port of @anthropic-ai/claude-agent-sdk@0.3.276 sdk.d.ts SDKMessage and input envelopes.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame { pub kind: FrameKind, pub raw: Value }
#[derive(Debug, Clone, PartialEq)]
pub enum FrameKind {
    Init(InitFrame), Assistant(AssistantFrame), User(UserFrame), StreamEvent(StreamEventFrame),
    Result(ResultFrame), RateLimitEvent(RateLimitFrame), System(SystemFrame),
    CommandLifecycle(CommandLifecycleFrame), ToolProgress(ToolProgressFrame), ConversationReset(Value), Unknown { kind: String, subtype: Option<String> },
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InitFrame { pub session_id: String, pub model: String, #[serde(default)] pub tools: Vec<String>, #[serde(default)] pub capabilities: Vec<String> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantFrame { pub message: Value, pub uuid: String, pub session_id: String, #[serde(default)] pub parent_tool_use_id: Option<String>, #[serde(default)] pub user_message_uuid: Option<String> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserFrame { pub message: Value, pub uuid: String, pub session_id: String, #[serde(default)] pub parent_tool_use_id: Option<String>, #[serde(default)] pub tool_use_result: Option<Value> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamEventFrame { pub event: Value, pub uuid: String, pub session_id: String, #[serde(default)] pub parent_tool_use_id: Option<String>, #[serde(default)] pub user_message_uuid: Option<String> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResultFrame {
    pub subtype: String, pub is_error: bool, pub uuid: String, pub session_id: String,
    #[serde(default)] pub result: Option<String>, #[serde(default)] pub errors: Vec<String>,
    #[serde(default)] pub terminal_reason: Option<String>, #[serde(default)] pub api_error_status: Option<u16>,
    #[serde(default)] pub num_turns: Option<u64>, #[serde(default)] pub usage: Option<Value>,
    #[serde(default, rename = "modelUsage")] pub model_usage: Option<Value>,
    #[serde(default)] pub total_cost_usd: Option<f64>, #[serde(default)] pub permission_denials: Vec<Value>,
    #[serde(default)] pub user_message_uuid: Option<String>, #[serde(default)] pub user_message_uuids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RateLimitFrame { pub rate_limit_info: Value, pub uuid: String, pub session_id: String }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CommandLifecycleFrame { pub command_uuid: String, pub state: String, pub uuid: String, pub session_id: String }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolProgressFrame { pub tool_use_id: String, pub tool_name: String, pub parent_tool_use_id: Option<String>,
    pub elapsed_time_seconds: f64, pub uuid: String, pub session_id: String,
    #[serde(default)] pub task_id: Option<String>, #[serde(default)] pub heartbeat: Option<bool> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemSubtype { Status, CompactBoundary, ToolProgress, HookStarted, HookProgress, HookResponse,
    TaskStarted, TaskUpdated, TaskProgress, TaskNotification, BackgroundTasksChanged,
    ApiRetry, ModelRefusalFallback, ModelRefusalNoFallback, CommandsChanged, SessionStateChanged,
    CommandLifecycle, ControlRequestProgress, ThinkingTokens, Informational, Other(String) }
impl SystemSubtype { pub fn parse(value: &str) -> Self { match value {
    "status" => Self::Status, "compact_boundary" => Self::CompactBoundary,
    "tool_progress" => Self::ToolProgress, "hook_started" => Self::HookStarted,
    "hook_progress" => Self::HookProgress, "hook_response" => Self::HookResponse,
    "task_started" => Self::TaskStarted, "task_updated" => Self::TaskUpdated,
    "task_progress" => Self::TaskProgress, "task_notification" => Self::TaskNotification,
    "background_tasks_changed" => Self::BackgroundTasksChanged, "api_retry" => Self::ApiRetry,
    "model_refusal_fallback" => Self::ModelRefusalFallback, "model_refusal_no_fallback" => Self::ModelRefusalNoFallback,
    "commands_changed" => Self::CommandsChanged, "session_state_changed" => Self::SessionStateChanged,
    "command_lifecycle" => Self::CommandLifecycle, "control_request_progress" => Self::ControlRequestProgress,
    "thinking_tokens" => Self::ThinkingTokens, "informational" => Self::Informational,
    other => Self::Other(other.into()) } } }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TaskStarted { pub task_id: String, pub description: String, #[serde(default)] pub tool_use_id: Option<String>, #[serde(default)] pub task_type: Option<String>, #[serde(default)] pub subagent_type: Option<String> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TaskProgress { pub task_id: String, #[serde(default)] pub tool_use_id: Option<String>, #[serde(default)] pub description: Option<String>, #[serde(default)] pub usage: Option<Value>, #[serde(default)] pub summary: Option<String> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TaskNotification { pub task_id: String, pub status: String, #[serde(default)] pub summary: Option<String>, #[serde(default)] pub tool_use_id: Option<String>, #[serde(default)] pub output_file: Option<String> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HookEvent { pub hook_id: String, pub hook_name: String, pub hook_event: String, #[serde(default)] pub output: Option<String>, #[serde(default)] pub stdout: Option<String>, #[serde(default)] pub stderr: Option<String>, #[serde(default)] pub exit_code: Option<i64> }
#[derive(Debug, Clone, PartialEq)]
pub enum SystemPayload { Status { status: String }, CompactBoundary { compact_metadata: Value },
    TaskStarted(TaskStarted), TaskUpdated { task_id: String, patch: Value }, TaskProgress(TaskProgress),
    TaskNotification(TaskNotification), Hook(HookEvent), ToolProgress(Value), ApiRetry(Value), Other(Value) }
#[derive(Debug, Clone, PartialEq)]
pub struct SystemFrame { pub subtype: SystemSubtype, pub uuid: Option<String>, pub session_id: Option<String>, pub payload: SystemPayload, pub raw: Value }
impl SystemFrame {
    fn decode(raw: Value, subtype: &str) -> serde_json::Result<Self> {
        let payload = match subtype {
            "status" => SystemPayload::Status { status: required_string(&raw, "status")? },
            "compact_boundary" => SystemPayload::CompactBoundary { compact_metadata: required_value(&raw, "compact_metadata")? },
            "task_started" => SystemPayload::TaskStarted(serde_json::from_value(raw.clone())?),
            "task_updated" => SystemPayload::TaskUpdated { task_id: required_string(&raw,"task_id")?, patch: required_value(&raw,"patch")? },
            "task_progress" => SystemPayload::TaskProgress(serde_json::from_value(raw.clone())?),
            "task_notification" => SystemPayload::TaskNotification(serde_json::from_value(raw.clone())?),
            "hook_started" | "hook_progress" | "hook_response" => SystemPayload::Hook(serde_json::from_value(raw.clone())?),
            "tool_progress" => SystemPayload::ToolProgress(raw.clone()),
            "api_retry" => SystemPayload::ApiRetry(raw.clone()),
            _ => SystemPayload::Other(raw.clone()),
        };
        Ok(Self { subtype: SystemSubtype::parse(subtype), uuid: raw.get("uuid").and_then(Value::as_str).map(str::to_owned), session_id: raw.get("session_id").and_then(Value::as_str).map(str::to_owned), payload, raw })
    }
}
fn required_value(raw: &Value, name: &str) -> serde_json::Result<Value> {
    raw.get(name).cloned().ok_or_else(|| <serde_json::Error as serde::de::Error>::custom(format!("missing {name}")))
}
fn required_string(raw: &Value, name: &str) -> serde_json::Result<String> {
    raw.get(name).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| <serde_json::Error as serde::de::Error>::custom(format!("missing {name}")))
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
            ("rate_limit_event", _) => FrameKind::RateLimitEvent(serde_json::from_value(raw.clone())?),
            ("command_lifecycle", _) => FrameKind::CommandLifecycle(serde_json::from_value(raw.clone())?),
            ("tool_progress", _) => FrameKind::ToolProgress(serde_json::from_value(raw.clone())?),
            ("conversation_reset", _) => FrameKind::ConversationReset(raw.clone()),
            _ => FrameKind::Unknown { kind: kind.into(), subtype: subtype.map(str::to_owned) },
        };
        Ok(Self { kind: parsed, raw })
    }
}

/// SDK submitMessage input; content blocks (including image source blocks) pass through unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent { Text(String), Blocks(Vec<Value>) }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessageBody { pub role: String, pub content: UserContent }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    #[serde(rename = "type")] pub kind: String,
    pub message: UserMessageBody,
    pub parent_tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub priority: Option<String>,
}
impl UserMessage {
    pub fn text(text: impl Into<String>, uuid: Option<String>, now: bool) -> Self {
        Self { kind: "user".into(), message: UserMessageBody { role: "user".into(), content: UserContent::Text(text.into()) }, parent_tool_use_id: None, uuid, priority: now.then(|| "now".into()) }
    }
    pub fn blocks(blocks: Vec<Value>, uuid: Option<String>, now: bool) -> Self {
        Self { kind: "user".into(), message: UserMessageBody { role: "user".into(), content: UserContent::Blocks(blocks) }, parent_tool_use_id: None, uuid, priority: now.then(|| "now".into()) }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "lowercase")]
pub enum UserDialogResult { Completed { result: Value }, Cancelled }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "lowercase")]
pub enum PermissionResult {
    Allow { #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")] updated_input: Option<Value>,
        #[serde(rename = "updatedPermissions", skip_serializing_if = "Option::is_none")] updated_permissions: Option<Vec<Value>>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")] tool_use_id: Option<String>,
        #[serde(rename = "decisionClassification", skip_serializing_if = "Option::is_none")] decision_classification: Option<String> },
    Deny { message: String, #[serde(skip_serializing_if = "Option::is_none")] interrupt: Option<bool>,
        #[serde(rename = "toolUseID", skip_serializing_if = "Option::is_none")] tool_use_id: Option<String>,
        #[serde(rename = "decisionClassification", skip_serializing_if = "Option::is_none")] decision_classification: Option<String> },
}
impl PermissionResult { pub fn with_tool_use_id(mut self, id: &str) -> Self { match &mut self { Self::Allow { tool_use_id, .. } | Self::Deny { tool_use_id, .. } => *tool_use_id = Some(id.into()) } self } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_upstream_claude_fixtures() {
        let fixtures = [
            include_str!("../../../upstream/apps/server/src/orchestration-v2/testkit/fixtures/simple/claude_transcript.ndjson"),
            include_str!("../../../upstream/apps/server/src/orchestration-v2/testkit/fixtures/message_steering/claude_transcript.ndjson"),
            include_str!("../../../upstream/apps/server/src/orchestration-v2/testkit/fixtures/claude_background_subagent_lifecycle/claude_transcript.ndjson"),
            include_str!("../../../upstream/apps/server/src/orchestration-v2/testkit/fixtures/claude_result_is_error/claude_transcript.ndjson"),
        ];
        let mut count = 0;
        for fixture in fixtures { for line in fixture.lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            if record.get("type").and_then(Value::as_str) != Some("emit_inbound") { continue; }
            let raw = record.get("frame").unwrap().clone();
            let frame = Frame::decode(raw.clone()).unwrap();
            assert_eq!(frame.raw, raw);
            count += 1;
        }}
        assert!(count > 100);
    }
    #[test]
    fn permission_result_wire_casing() {
        let result = PermissionResult::Deny { message: "No".into(), interrupt: Some(true), tool_use_id: None, decision_classification: Some("userDenied".into()) }.with_tool_use_id("toolu_1");
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["toolUseID"], "toolu_1");
        assert_eq!(value["interrupt"], true);
        assert_eq!(value["decisionClassification"], "userDenied");
    }
    #[test]
    fn unknown_is_preserved_and_invalid_known_rejected() {
        let raw = serde_json::json!({"type":"future_frame","subtype":"x","a":1});
        assert_eq!(Frame::decode(raw.clone()).unwrap().raw,raw);
        assert!(Frame::decode(serde_json::json!({"type":"result","subtype":"success"})).is_err());
    }
}
