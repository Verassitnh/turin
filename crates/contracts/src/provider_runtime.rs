//! Port of upstream/packages/contracts/src/providerRuntime.ts
//!
//! Upstream declares one `ProviderRuntime<Event>` struct per literal `type`
//! and unions them into `ProviderRuntimeEventV2`. Rust models the union
//! directly as a `type`-tagged enum: one variant per member (same member
//! order — these ride on persisted events), each carrying the shared base
//! fields (flattened) plus that member's payload.

use crate::base_schemas::{
    ContractError, EventId, IsoDateTime, NonNegativeInt, PositiveInt, ProviderItemId,
    RuntimeItemId, RuntimeRequestId, RuntimeTaskId, ThreadId, TrimmedNonEmptyString, TurnId,
};
use crate::provider_instance::{ProviderDriverKind, ProviderInstanceId, optional_unknown};
use crate::provider_policy::ProviderApprovalOption;
use crate::provider_usage_limits::ProviderUsageLimitsUpdate;
use serde::{Deserialize, Deserializer, Serialize};

/// `Schema.Record(Schema.String, Schema.Unknown)`: insertion order preserved.
type UnknownRecord = indexmap::IndexMap<String, serde_json::Value>;

const KNOWN_RUNTIME_EVENT_RAW_SOURCES: [&str; 8] = [
    "codex.app-server.notification",
    "codex.app-server.request",
    "codex.eventmsg",
    "claude.sdk.message",
    "claude.sdk.permission",
    "codex.sdk.thread-event",
    "opencode.sdk.event",
    "acp.jsonrpc",
];

/// `RuntimeEventRawSource`: one of the known raw-source slugs, or any
/// `"acp.<name>.extension"` — upstream's `Schema.TemplateLiteral` keeps
/// fork-supplied ACP extensions decodable without enumerating them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct RuntimeEventRawSource(String);

impl RuntimeEventRawSource {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let raw = value.as_ref();
        // `Schema.String` in a template literal also matches the empty string
        // (checked against effect 4: "acp..extension" decodes), but the
        // prefix and suffix cannot overlap, so "acp.extension" does not.
        let valid = KNOWN_RUNTIME_EVENT_RAW_SOURCES.contains(&raw)
            || (raw.starts_with("acp.")
                && raw.ends_with(".extension")
                && raw.len() >= "acp..extension".len());
        if !valid {
            return Err(ContractError {
                expected: "a known raw source or an \"acp.<name>.extension\" slug",
                got: raw.to_owned(),
            });
        }
        Ok(Self(raw.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Display for RuntimeEventRawSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RuntimeEventRawSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::new(raw.as_ref()).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEventRaw {
    pub source: RuntimeEventRawSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<TrimmedNonEmptyString>,
    pub payload: serde_json::Value,
}

/// Local alias of `TrimmedNonEmptyString` for provider-assigned request ids.
pub type ProviderRequestId = TrimmedNonEmptyString;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item_id: Option<ProviderItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<ProviderRequestId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeSessionState {
    Starting,
    Ready,
    Running,
    Waiting,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeThreadState {
    Active,
    Idle,
    Archived,
    Closed,
    Compacted,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeTurnState {
    Completed,
    Failed,
    Interrupted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimePlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeItemStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeContentStreamKind {
    AssistantText,
    ReasoningText,
    ReasoningSummaryText,
    PlanText,
    CommandOutput,
    FileChangeOutput,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeSessionExitKind {
    Graceful,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeErrorClass {
    ProviderError,
    TransportError,
    PermissionError,
    ValidationError,
    Unknown,
}

const TOOL_LIFECYCLE_ITEM_TYPES: [&str; 7] = [
    "command_execution",
    "file_change",
    "mcp_tool_call",
    "dynamic_tool_call",
    "collab_agent_tool_call",
    "web_search",
    "image_view",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLifecycleItemType {
    CommandExecution,
    FileChange,
    McpToolCall,
    DynamicToolCall,
    CollabAgentToolCall,
    WebSearch,
    ImageView,
}

/// Upstream `isToolLifecycleItemType`.
pub fn is_tool_lifecycle_item_type(value: &str) -> bool {
    TOOL_LIFECYCLE_ITEM_TYPES.contains(&value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalItemType {
    UserMessage,
    AssistantMessage,
    Reasoning,
    Plan,
    CommandExecution,
    FileChange,
    McpToolCall,
    DynamicToolCall,
    CollabAgentToolCall,
    WebSearch,
    ImageView,
    ReviewEntered,
    ReviewExited,
    ContextCompaction,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalRequestType {
    CommandExecutionApproval,
    FileReadApproval,
    FileChangeApproval,
    ApplyPatchApproval,
    ExecCommandApproval,
    McpElicitationApproval,
    PermissionApproval,
    ToolUserInput,
    DynamicToolCall,
    AuthTokensRefresh,
    Unknown,
}

/// Fields shared by every runtime event (upstream
/// `ProviderRuntimeEventBase`, spread into each member struct).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRuntimeEventBase {
    pub event_id: EventId,
    pub provider: ProviderDriverKind,
    // Optional during the driver/instance migration. See providerInstance.ts
    // for the routing-key-vs-driver-id distinction. Once every emitter
    // populates it (post-slice-4), routing flips to instance-id-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<ProviderInstanceId>,
    pub thread_id: ThreadId,
    pub created_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<RuntimeItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RuntimeRequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_refs: Option<ProviderRefs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RuntimeEventRaw>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub resume: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfiguredPayload {
    pub config: UnknownRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStateChangedPayload {
    pub state: RuntimeSessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExitedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recoverable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_kind: Option<RuntimeSessionExitKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thread_id: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStateChangedPayload {
    pub state: RuntimeThreadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_tokens: Option<NonNegativeInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadMetadataUpdatedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<UnknownRecord>,
}

/// Upstream's inline cost struct (`ThreadTokenUsageSnapshot.cost`). `amount`
/// carries an `isFinite` check; JSON cannot encode NaN/Infinity and serde_json
/// rejects non-finite floats, so finiteness is inherent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsageCost {
    #[serde(with = "crate::base_schemas::js_number")]
    pub amount: f64,
    #[serde(deserialize_with = "max_32::required")]
    pub currency: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsageSnapshot {
    pub used_tokens: NonNegativeInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_processed_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cached_input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacts_automatically: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ThreadTokenUsageCost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsageUpdatedPayload {
    pub usage: ThreadTokenUsageSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRealtimeStartedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime_session_id: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRealtimeItemAddedPayload {
    pub item: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRealtimeAudioDeltaPayload {
    pub audio: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRealtimeErrorPayload {
    pub message: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRealtimeClosedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<TrimmedNonEmptyString>,
}

string_literal!(
    /// Constant `"main_agent"` usage scope of a turn's token usage.
    MainAgentUsageScope,
    "main_agent"
);

string_literal!(
    /// Constant `"complete"` usage status (totals required).
    TurnTokenUsageCompleteStatus,
    "complete"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnTokenUsagePartialStatus {
    Partial,
    Unavailable,
}

/// Normalized main-agent usage for one turn. Input includes cache reads and
/// writes. Output includes reasoning, and reasoningTokens is an optional subset.
/// Complete means the provider supplied full input and output totals. Partial
/// means every included count is valid, but the full turn total is not known.
///
/// Upstream is a plain two-member struct union; the `complete` member carries
/// its literal as a validated constant so "partial"-with-totals still lands on
/// the second member, matching member-order decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum TurnTokenUsage {
    Complete {
        usage_scope: MainAgentUsageScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cached_input_tokens: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_creation_tokens: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<NonNegativeInt>,
        has_subagents: bool,
        usage_status: TurnTokenUsageCompleteStatus,
        input_tokens: NonNegativeInt,
        output_tokens: NonNegativeInt,
    },
    Partial {
        usage_scope: MainAgentUsageScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cached_input_tokens: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_creation_tokens: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<NonNegativeInt>,
        has_subagents: bool,
        usage_status: TurnTokenUsagePartialStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<NonNegativeInt>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCompletedPayload {
    pub state: RuntimeTurnState,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub stop_reason: Option<Option<TrimmedNonEmptyString>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_usage: Option<UnknownRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TurnTokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAbortedPayload {
    pub reason: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TurnTokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePlanStep {
    pub step: TrimmedNonEmptyString,
    pub status: RuntimePlanStepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnPlanUpdatedPayload {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub explanation: Option<Option<TrimmedNonEmptyString>>,
    pub plan: Vec<RuntimePlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnProposedDeltaPayload {
    pub delta: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnProposedCompletedPayload {
    pub plan_markdown: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiffUpdatedPayload {
    pub unified_diff: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolActivitySurface {
    Browser,
    Computer,
}

// Field-level bounds for the tool-activity and token-usage shapes (upstream
// `TrimmedNonEmptyString.check(Schema.isMaxLength(n))`). Lengths are UTF-16
// code units, counted after trimming.
crate::max_len!(max_32, 32);
crate::max_len!(max_160, 160);
crate::max_len!(max_512, 512);
crate::max_len!(max_4096, 4096);

mod tool_activity_app_id {
    use super::*;

    /// `appId`: at most 512 chars and `^[A-Za-z0-9._-]+$`.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TrimmedNonEmptyString, D::Error> {
        let value = TrimmedNonEmptyString::deserialize(d)?;
        let raw = value.as_str();
        if crate::base_schemas::js_len(raw) > 512
            || !raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(serde::de::Error::custom(
                "an app id matching ^[A-Za-z0-9._-]+$ (<= 512 chars)",
            ));
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all_fields = "camelCase")]
pub enum ToolActivityNativeAppReference {
    #[serde(rename = "app-id")]
    AppId {
        #[serde(deserialize_with = "tool_activity_app_id::deserialize")]
        app_id: TrimmedNonEmptyString,
    },
    #[serde(rename = "display-name")]
    DisplayName {
        #[serde(deserialize_with = "max_160::required")]
        display_name: TrimmedNonEmptyString,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all_fields = "camelCase")]
pub enum ToolActivityIcon {
    #[serde(rename = "website")]
    Website {
        #[serde(deserialize_with = "max_4096::required")]
        page_url: TrimmedNonEmptyString,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "max_4096::optional"
        )]
        favicon_url: Option<TrimmedNonEmptyString>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "max_4096::optional"
        )]
        favicon_url_dark: Option<TrimmedNonEmptyString>,
    },
    #[serde(rename = "native-app")]
    NativeApp { app: ToolActivityNativeAppReference },
    #[serde(rename = "themed-logo")]
    ThemedLogo {
        #[serde(deserialize_with = "max_4096::required")]
        logo_url: TrimmedNonEmptyString,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "max_4096::optional"
        )]
        logo_url_dark: Option<TrimmedNonEmptyString>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolActivitySourceKind {
    Browser,
    Computer,
    Integration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolActivitySource {
    #[serde(deserialize_with = "max_512::required")]
    pub key: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_160::required")]
    pub name: TrimmedNonEmptyString,
    pub kind: ToolActivitySourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ToolActivityIcon>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemLifecyclePayload {
    pub item_type: CanonicalItemType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RuntimeItemStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_surface: Option<ToolActivitySurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_icon: Option<ToolActivityIcon>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_source: Option<ToolActivitySource>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub data: Option<serde_json::Value>,
    /// Owning agent when this item ran inside a subagent (resolved from the
    /// SDK's parent_tool_use_id). Clients re-home attributed items out of the
    /// main timeline and into the owning agent's Agents-surface row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDeltaPayload {
    pub stream_kind: RuntimeContentStreamKind,
    pub delta: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_index: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOpenedPayload {
    pub request_type: CanonicalRequestType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ProviderApprovalOption>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub args: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestResolvedPayload {
    pub request_type: CanonicalRequestType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub resolution: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestionOption {
    pub label: TrimmedNonEmptyString,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

string_literal!(
    /// `responseMode`: constant `"message"`.
    UserInputResponseMode,
    "message"
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: TrimmedNonEmptyString,
    pub header: TrimmedNonEmptyString,
    pub question: TrimmedNonEmptyString,
    pub options: Vec<UserInputQuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_custom_answer: Option<bool>,
    /// Editable initial text. Answers preserve whitespace and allow an empty string when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_answer: Option<String>,
    /// `withConstructorDefault(false)` upstream: the constructor defaults to
    /// `false`, but decode/encode stay a plain optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputRequestedPayload {
    pub questions: Vec<UserInputQuestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_mode: Option<UserInputResponseMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputResolvedPayload {
    pub answers: UnknownRecord,
}

/// Typed per-task usage rollup. Field names match the orchestration-v2 subagent
/// usage vocabulary (#4779) so the eventual migration is a rename, not a remap.
/// Claude reports per-activation deltas; Codex reports cumulative totals — the
/// merge strategy is provider-specific and lives in client-runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskUsage {
    pub total_tokens: NonNegativeInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<NonNegativeInt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkflowPhase {
    pub index: NonNegativeInt,
    pub title: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunHandles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_path: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_dir: Option<TrimmedNonEmptyString>,
    /// Only http/https URLs may be stored here — sanitized at the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_url: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskAgentKind {
    Agent,
    Background,
}

/// Optional agent-identity linkage carried on every task lifecycle payload.
/// Repeated on progress and terminal rows (not just start) so client folds can
/// reconstruct an agent even when its start row aged out of activity retention.
/// All fields optional: old emitters and old rows decode unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskAgentLinkage {
    /// SDK task_type (subagent/shell/monitor/local_workflow/…), repeated on
    /// every row so folds can classify without the start row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<TrimmedNonEmptyString>,
    /// Server-stamped classification, set at ingestion.
    /// Clients trust this stamp outright; rows without it (legacy, pre-stamp)
    /// fall back to client-side heuristics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<TaskAgentKind>,
    /// Owning agent when the task itself was launched from inside a subagent
    /// (e.g. a subagent's background shell). Clients treat such tasks as
    /// agent-internal and keep them out of the parent work log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TrimmedNonEmptyString>,
    /// Reasoning effort when known (e.g. "high"). Open string: provider vocabularies differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_index: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_index: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_title: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<TaskWorkflowPhase>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_handles: Option<TaskRunHandles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<TrimmedNonEmptyString>,
    /// Codex agent hierarchy path, e.g. "/root/marlow".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<TrimmedNonEmptyString>,
    /// Set on provider-synthesized child-agent events (Codex) whose activity
    /// belongs in the Agents surface, never the parent timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_bypass: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStartedPayload {
    pub task_id: RuntimeTaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<TrimmedNonEmptyString>,
    #[serde(flatten)]
    pub linkage: TaskAgentLinkage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeTaskStatus {
    Pending,
    Running,
    Waiting,
    Idle,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgressPayload {
    pub task_id: RuntimeTaskId,
    pub description: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed_usage: Option<RuntimeTaskUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<TrimmedNonEmptyString>,
    /// Present on synthesized member/child progress rows that carry state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RuntimeTaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TrimmedNonEmptyString>,
    #[serde(flatten)]
    pub linkage: TaskAgentLinkage,
}

/// Non-terminal status patch (from the Claude SDK's task_updated, which main
/// previously dropped). killed→cancelled and paused→idle are mapped at the
/// adapter so the wire only carries the shared vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskUpdatedPayload {
    pub task_id: RuntimeTaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RuntimeTaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_backgrounded: Option<bool>,
    #[serde(flatten)]
    pub linkage: TaskAgentLinkage,
}

/// `TaskCompletedPayload.status`: the terminal subset of [`RuntimeTaskStatus`]
/// (upstream's inline literal set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskCompletedStatus {
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCompletedPayload {
    pub task_id: RuntimeTaskId,
    pub status: TaskCompletedStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed_usage: Option<RuntimeTaskUsage>,
    #[serde(flatten)]
    pub linkage: TaskAgentLinkage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStartedPayload {
    pub hook_id: TrimmedNonEmptyString,
    pub hook_name: TrimmedNonEmptyString,
    pub hook_event: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookProgressPayload {
    pub hook_id: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

/// `HookCompletedPayload.outcome` (upstream's inline literal set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookOutcome {
    Success,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookCompletedPayload {
    pub hook_id: TrimmedNonEmptyString,
    pub outcome: HookOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProgressPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub elapsed_seconds: Option<f64>,
    /// Owning task/agent when the tool ran inside a subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<RuntimeTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSummaryPayload {
    pub summary: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preceding_tool_use_ids: Option<Vec<TrimmedNonEmptyString>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatusPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_authenticating: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpdatedPayload {
    pub account: serde_json::Value,
}

/// Adapters normalise their native rate-limit payload at the boundary so the
/// consumer that folds it into the provider snapshot never sees driver shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRateLimitsUpdatedPayload {
    pub limits: ProviderUsageLimitsUpdate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatusUpdatedPayload {
    pub status: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOauthCompletedPayload {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReroutedPayload {
    pub from_model: TrimmedNonEmptyString,
    pub to_model: TrimmedNonEmptyString,
    pub reason: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWarningPayload {
    pub summary: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub range: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeprecationNoticePayload {
    pub summary: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesPersistedFile {
    pub filename: TrimmedNonEmptyString,
    pub file_id: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesPersistedFailedFile {
    pub filename: TrimmedNonEmptyString,
    pub error: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesPersistedPayload {
    pub files: Vec<FilesPersistedFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<Vec<FilesPersistedFailedFile>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDeniedPayload {
    pub tool_name: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeWarningPayload {
    pub message: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeErrorPayload {
    pub message: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<RuntimeErrorClass>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub detail: Option<serde_json::Value>,
}

/// The runtime event union (upstream `ProviderRuntimeEventV2`): one variant
/// per member struct, in upstream union order, each flattening
/// [`ProviderRuntimeEventBase`] and carrying its payload. The `type` literals
/// upstream declares as `Schema.Literal` consts become the variant renames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ProviderRuntimeEventV2 {
    #[serde(rename = "session.started")]
    SessionStarted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: SessionStartedPayload,
    },
    #[serde(rename = "session.configured")]
    SessionConfigured {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: SessionConfiguredPayload,
    },
    #[serde(rename = "session.state.changed")]
    SessionStateChanged {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: SessionStateChangedPayload,
    },
    #[serde(rename = "session.exited")]
    SessionExited {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: SessionExitedPayload,
    },
    #[serde(rename = "thread.started")]
    ThreadStarted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadStartedPayload,
    },
    #[serde(rename = "thread.state.changed")]
    ThreadStateChanged {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadStateChangedPayload,
    },
    #[serde(rename = "thread.metadata.updated")]
    ThreadMetadataUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadMetadataUpdatedPayload,
    },
    #[serde(rename = "thread.token-usage.updated")]
    ThreadTokenUsageUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadTokenUsageUpdatedPayload,
    },
    #[serde(rename = "thread.realtime.started")]
    ThreadRealtimeStarted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadRealtimeStartedPayload,
    },
    #[serde(rename = "thread.realtime.item-added")]
    ThreadRealtimeItemAdded {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadRealtimeItemAddedPayload,
    },
    #[serde(rename = "thread.realtime.audio.delta")]
    ThreadRealtimeAudioDelta {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadRealtimeAudioDeltaPayload,
    },
    #[serde(rename = "thread.realtime.error")]
    ThreadRealtimeError {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadRealtimeErrorPayload,
    },
    #[serde(rename = "thread.realtime.closed")]
    ThreadRealtimeClosed {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ThreadRealtimeClosedPayload,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TurnStartedPayload,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TurnCompletedPayload,
    },
    #[serde(rename = "turn.aborted")]
    TurnAborted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TurnAbortedPayload,
    },
    #[serde(rename = "turn.plan.updated")]
    TurnPlanUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TurnPlanUpdatedPayload,
    },
    #[serde(rename = "turn.proposed.delta")]
    TurnProposedDelta {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TurnProposedDeltaPayload,
    },
    #[serde(rename = "turn.proposed.completed")]
    TurnProposedCompleted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TurnProposedCompletedPayload,
    },
    #[serde(rename = "turn.diff.updated")]
    TurnDiffUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TurnDiffUpdatedPayload,
    },
    #[serde(rename = "item.started")]
    ItemStarted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ItemLifecyclePayload,
    },
    #[serde(rename = "item.updated")]
    ItemUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ItemLifecyclePayload,
    },
    #[serde(rename = "item.completed")]
    ItemCompleted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ItemLifecyclePayload,
    },
    #[serde(rename = "content.delta")]
    ContentDelta {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ContentDeltaPayload,
    },
    #[serde(rename = "request.opened")]
    RequestOpened {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: RequestOpenedPayload,
    },
    #[serde(rename = "request.resolved")]
    RequestResolved {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: RequestResolvedPayload,
    },
    #[serde(rename = "user-input.requested")]
    UserInputRequested {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: UserInputRequestedPayload,
    },
    #[serde(rename = "user-input.resolved")]
    UserInputResolved {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: UserInputResolvedPayload,
    },
    #[serde(rename = "task.started")]
    TaskStarted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TaskStartedPayload,
    },
    #[serde(rename = "task.progress")]
    TaskProgress {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TaskProgressPayload,
    },
    #[serde(rename = "task.updated")]
    TaskUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TaskUpdatedPayload,
    },
    #[serde(rename = "task.completed")]
    TaskCompleted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: TaskCompletedPayload,
    },
    #[serde(rename = "hook.started")]
    HookStarted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: HookStartedPayload,
    },
    #[serde(rename = "hook.progress")]
    HookProgress {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: HookProgressPayload,
    },
    #[serde(rename = "hook.completed")]
    HookCompleted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: HookCompletedPayload,
    },
    #[serde(rename = "tool.progress")]
    ToolProgress {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ToolProgressPayload,
    },
    #[serde(rename = "tool.summary")]
    ToolSummary {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ToolSummaryPayload,
    },
    #[serde(rename = "auth.status")]
    AuthStatus {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: AuthStatusPayload,
    },
    #[serde(rename = "account.updated")]
    AccountUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: AccountUpdatedPayload,
    },
    #[serde(rename = "account.rate-limits.updated")]
    AccountRateLimitsUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: AccountRateLimitsUpdatedPayload,
    },
    #[serde(rename = "mcp.status.updated")]
    McpStatusUpdated {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: McpStatusUpdatedPayload,
    },
    #[serde(rename = "mcp.oauth.completed")]
    McpOauthCompleted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: McpOauthCompletedPayload,
    },
    #[serde(rename = "model.rerouted")]
    ModelRerouted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ModelReroutedPayload,
    },
    #[serde(rename = "config.warning")]
    ConfigWarning {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ConfigWarningPayload,
    },
    #[serde(rename = "deprecation.notice")]
    DeprecationNotice {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: DeprecationNoticePayload,
    },
    #[serde(rename = "files.persisted")]
    FilesPersisted {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: FilesPersistedPayload,
    },
    #[serde(rename = "tool.denied")]
    ToolDenied {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: ToolDeniedPayload,
    },
    #[serde(rename = "runtime.warning")]
    RuntimeWarning {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: RuntimeWarningPayload,
    },
    #[serde(rename = "runtime.error")]
    RuntimeError {
        #[serde(flatten)]
        base: ProviderRuntimeEventBase,
        payload: RuntimeErrorPayload,
    },
}

/// Upstream `ProviderRuntimeEvent = ProviderRuntimeEventV2`.
pub type ProviderRuntimeEvent = ProviderRuntimeEventV2;

/// Upstream `ProviderRuntimeTurnStatus = RuntimeTurnState`.
pub type ProviderRuntimeTurnStatus = RuntimeTurnState;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(value: serde_json::Value) -> Result<ProviderRuntimeEvent, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn requires_input_and_output_totals_for_complete_turn_usage() {
        let complete_event = json!({
            "type": "turn.completed",
            "eventId": "event-complete-usage",
            "provider": "codex",
            "createdAt": "2026-02-28T00:00:00.000Z",
            "threadId": "thread-1",
            "turnId": "turn-1",
            "payload": {
                "state": "completed",
                "tokenUsage": {
                    "usageStatus": "complete",
                    "usageScope": "main_agent",
                    "hasSubagents": false,
                },
            },
        });

        assert!(decode(complete_event.clone()).is_err());

        let mut with_totals = complete_event.clone();
        with_totals["payload"]["tokenUsage"]["inputTokens"] = json!(10);
        with_totals["payload"]["tokenUsage"]["outputTokens"] = json!(2);
        let parsed = decode(with_totals).unwrap();
        assert!(matches!(parsed, ProviderRuntimeEvent::TurnCompleted { .. }));
        let parsed_value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            parsed_value["payload"]["tokenUsage"]["inputTokens"],
            json!(10)
        );

        // A partial usage status needs no totals.
        let mut partial = complete_event;
        partial["payload"]["tokenUsage"]["usageStatus"] = json!("partial");
        let parsed = decode(partial).unwrap();
        assert!(matches!(parsed, ProviderRuntimeEvent::TurnCompleted { .. }));
    }

    #[test]
    fn accepts_fork_provided_driver_kinds_as_branded_slugs() {
        let parsed = decode(json!({
            "type": "session.started",
            "eventId": "event-ollama-session",
            "provider": "ollama",
            "providerInstanceId": "ollama_local",
            "createdAt": "2026-02-28T00:00:00.000Z",
            "threadId": "thread-1",
            "payload": {
                "message": "started",
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::SessionStarted { base, payload } = &parsed else {
            panic!("expected session.started");
        };
        assert_eq!(base.provider.as_str(), "ollama");
        assert_eq!(
            base.provider_instance_id.as_ref().unwrap().as_str(),
            "ollama_local"
        );
        assert_eq!(payload.message.as_ref().unwrap().as_str(), "started");
    }

    #[test]
    fn decodes_turn_plan_updated_for_plan_rendering() {
        let parsed = decode(json!({
            "type": "turn.plan.updated",
            "eventId": "event-1",
            "provider": "claudeAgent",
            "sessionId": "runtime-session-1",
            "createdAt": "2026-02-28T00:00:00.000Z",
            "threadId": "thread-1",
            "turnId": "turn-1",
            "payload": {
                "explanation": "Implement schema updates",
                "plan": [
                    { "step": "Define event union", "status": "completed" },
                    { "step": "Wire adapter mapping", "status": "inProgress" },
                ],
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::TurnPlanUpdated { payload, .. } = &parsed else {
            panic!("expected turn.plan.updated");
        };
        assert_eq!(payload.plan.len(), 2);
        assert_eq!(payload.plan[1].status, RuntimePlanStepStatus::InProgress);
        // Explanation is an optional NullOr: a present value survives encode.
        let parsed_value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            parsed_value["payload"]["explanation"],
            json!("Implement schema updates")
        );
    }

    #[test]
    fn decodes_proposed_plan_completion_events() {
        let parsed = decode(json!({
            "type": "turn.proposed.completed",
            "eventId": "event-proposed-plan-1",
            "provider": "codex",
            "createdAt": "2026-02-28T00:00:00.000Z",
            "threadId": "thread-1",
            "turnId": "turn-1",
            "payload": {
                "planMarkdown": "# Ship it",
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::TurnProposedCompleted { payload, .. } = &parsed else {
            panic!("expected turn.proposed.completed");
        };
        assert_eq!(payload.plan_markdown.as_str(), "# Ship it");
    }

    #[test]
    fn decodes_user_input_requested_with_structured_questions() {
        let parsed = decode(json!({
            "type": "user-input.requested",
            "eventId": "event-2",
            "provider": "claudeAgent",
            "sessionId": "runtime-session-2",
            "createdAt": "2026-02-28T00:00:01.000Z",
            "threadId": "thread-2",
            "requestId": "request-1",
            "payload": {
                "questions": [
                    {
                        "id": "sandbox_mode",
                        "header": "Sandbox",
                        "question": "Which mode should be used?",
                        "options": [
                            {
                                "label": "workspace-write",
                                "description": "Allow edits in workspace only",
                            },
                            {
                                "label": "danger-full-access",
                                "description": "Allow unrestricted access",
                            },
                        ],
                    },
                ],
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::UserInputRequested { payload, .. } = &parsed else {
            panic!("expected user-input.requested");
        };
        assert_eq!(payload.questions[0].id.as_str(), "sandbox_mode");
        assert_eq!(payload.questions[0].options.len(), 2);
    }

    #[test]
    fn decodes_user_input_resolved_with_answer_map() {
        let parsed = decode(json!({
            "type": "user-input.resolved",
            "eventId": "event-3",
            "provider": "claudeAgent",
            "sessionId": "runtime-session-2",
            "createdAt": "2026-02-28T00:00:02.000Z",
            "threadId": "thread-2",
            "requestId": "request-1",
            "payload": {
                "answers": {
                    "sandbox_mode": "workspace-write",
                },
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::UserInputResolved { payload, .. } = &parsed else {
            panic!("expected user-input.resolved");
        };
        assert_eq!(
            payload.answers.get("sandbox_mode"),
            Some(&json!("workspace-write"))
        );
    }

    #[test]
    fn rejects_legacy_message_delta_type() {
        assert!(
            decode(json!({
                "type": "message.delta",
                "eventId": "event-4",
                "provider": "codex",
                "sessionId": "runtime-session-3",
                "createdAt": "2026-02-28T00:00:03.000Z",
                "payload": { "delta": "legacy" },
            }))
            .is_err()
        );
    }

    #[test]
    fn rejects_empty_branded_canonical_ids() {
        assert!(
            decode(json!({
                "type": "runtime.error",
                "eventId": "event-5",
                "provider": "codex",
                "sessionId": "runtime-session-3",
                "createdAt": "2026-02-28T00:00:03.000Z",
                "threadId": "   ",
                "payload": { "message": "boom" },
            }))
            .is_err()
        );
    }

    #[test]
    fn decodes_normalized_thread_token_usage_snapshots() {
        let parsed = decode(json!({
            "type": "thread.token-usage.updated",
            "eventId": "event-token-usage-1",
            "provider": "claudeAgent",
            "createdAt": "2026-02-28T00:00:04.000Z",
            "threadId": "thread-1",
            "payload": {
                "usage": {
                    "usedTokens": 31251,
                    "maxTokens": 200000,
                    "toolUses": 25,
                    "durationMs": 43567,
                },
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::ThreadTokenUsageUpdated { payload, .. } = &parsed else {
            panic!("expected thread.token-usage.updated");
        };
        // Compare through JSON so a future PositiveInt newtype still matches.
        assert_eq!(
            serde_json::to_value(payload.usage.max_tokens).unwrap(),
            json!(200000)
        );
        assert_eq!(
            serde_json::to_value(payload.usage.used_tokens).unwrap(),
            json!(31251)
        );
    }

    #[test]
    fn raw_source_accepts_known_slugs_and_acp_extension_template() {
        for known in KNOWN_RUNTIME_EVENT_RAW_SOURCES {
            assert!(
                RuntimeEventRawSource::new(known).is_ok(),
                "{known} should decode"
            );
        }
        assert!(RuntimeEventRawSource::new("acp.thing.extension").is_ok());
        // Passthrough: the raw slug encodes unchanged.
        let source = RuntimeEventRawSource::new("codex.eventmsg").unwrap();
        assert_eq!(
            serde_json::to_string(&source).unwrap(),
            r#""codex.eventmsg""#
        );

        assert!(RuntimeEventRawSource::new("claude.sdk.unknown").is_err());
        assert!(RuntimeEventRawSource::new("acp..extension").is_ok());
        assert!(RuntimeEventRawSource::new("acp.extension").is_err());
        assert!(RuntimeEventRawSource::new("acp.onlyprefix.").is_err());
        assert!(RuntimeEventRawSource::new("x.acp.mid.extension").is_err());
    }

    #[test]
    fn raw_event_round_trips_inside_an_event() {
        let parsed = decode(json!({
            "type": "content.delta",
            "eventId": "event-raw-1",
            "provider": "claudeAgent",
            "threadId": "thread-1",
            "createdAt": "2026-02-28T00:00:00.000Z",
            "raw": {
                "source": "acp.sync.extension",
                "method": "document/didChange",
                "payload": { "any": ["thing"] },
            },
            "payload": {
                "streamKind": "assistant_text",
                "delta": "hi",
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::ContentDelta { base, payload } = &parsed else {
            panic!("expected content.delta");
        };
        assert_eq!(payload.stream_kind, RuntimeContentStreamKind::AssistantText);
        let raw = base.raw.as_ref().unwrap();
        assert_eq!(raw.source.as_str(), "acp.sync.extension");
        assert_eq!(raw.method.as_ref().unwrap().as_str(), "document/didChange");
        // Round-trips byte-for-byte on the keys upstream declares.
        let parsed_value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(parsed_value["raw"]["source"], json!("acp.sync.extension"));
        assert_eq!(parsed_value["raw"]["payload"], json!({ "any": ["thing"] }));
    }

    #[test]
    fn task_payloads_carry_flattened_agent_linkage() {
        let parsed = decode(json!({
            "type": "task.started",
            "eventId": "event-task-1",
            "provider": "claudeAgent",
            "threadId": "thread-1",
            "createdAt": "2026-02-28T00:00:00.000Z",
            "payload": {
                "taskId": "task-1",
                "description": "Research adapters",
                "taskType": "subagent",
                "agentKind": "agent",
                "agentId": "agent-1",
                "phases": [{ "index": 0, "title": "Read code" }],
            },
        }))
        .unwrap();

        let ProviderRuntimeEvent::TaskStarted { payload, .. } = &parsed else {
            panic!("expected task.started");
        };
        assert_eq!(
            payload.linkage.task_type.as_ref().unwrap().as_str(),
            "subagent"
        );
        assert_eq!(payload.linkage.agent_kind, Some(TaskAgentKind::Agent));
        assert_eq!(
            payload.linkage.agent_id.as_ref().unwrap().as_str(),
            "agent-1"
        );
        // Linkage fields ride at the payload's top level, like upstream's
        // `...taskAgentLinkageFields` spread.
        let parsed_value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(parsed_value["payload"]["taskId"], json!("task-1"));
        assert_eq!(parsed_value["payload"]["taskType"], json!("subagent"));
        assert_eq!(parsed_value["payload"]["agentKind"], json!("agent"));
    }

    #[test]
    fn tool_activity_shapes_validate_bounds() {
        let reference = json!({ "_tag": "app-id", "appId": "com.example.Editor" });
        let parsed: ToolActivityNativeAppReference =
            serde_json::from_value(reference.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), reference);

        assert!(
            serde_json::from_value::<ToolActivityNativeAppReference>(json!({
                "_tag": "app-id", "appId": "not a slug",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ToolActivityNativeAppReference>(json!({
                "_tag": "app-id", "appId": "x".repeat(513),
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ToolActivityNativeAppReference>(json!({
                "_tag": "display-name", "displayName": "x".repeat(161),
            }))
            .is_err()
        );

        let icon: ToolActivityIcon = serde_json::from_value(json!({
            "_tag": "website",
            "pageUrl": "https://example.com",
            "faviconUrlDark": "/dark.png",
        }))
        .unwrap();
        let ToolActivityIcon::Website {
            page_url,
            favicon_url,
            favicon_url_dark,
        } = &icon
        else {
            panic!("expected website");
        };
        assert_eq!(page_url.as_str(), "https://example.com");
        assert_eq!(favicon_url, &None);
        assert_eq!(favicon_url_dark.as_ref().unwrap().as_str(), "/dark.png");
        // Optional favicon keys stay omitted when unset.
        let icon_value = serde_json::to_value(&icon).unwrap();
        assert!(icon_value.get("faviconUrl").is_none());
    }

    fn event(kind: &str, payload: serde_json::Value) -> serde_json::Value {
        json!({
            "type": kind,
            "eventId": "event-1",
            "provider": "codex",
            "threadId": "thread-1",
            "createdAt": "2026-02-28T00:00:00.000Z",
            "payload": payload,
        })
    }

    /// One minimal valid payload per union member, in upstream union order.
    fn minimal_events() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("session.started", json!({})),
            ("session.configured", json!({"config": {}})),
            ("session.state.changed", json!({"state": "ready"})),
            ("session.exited", json!({})),
            ("thread.started", json!({})),
            ("thread.state.changed", json!({"state": "active"})),
            ("thread.metadata.updated", json!({})),
            (
                "thread.token-usage.updated",
                json!({"usage": {"usedTokens": 1}}),
            ),
            ("thread.realtime.started", json!({})),
            ("thread.realtime.item-added", json!({"item": null})),
            ("thread.realtime.audio.delta", json!({"audio": {}})),
            ("thread.realtime.error", json!({"message": "m"})),
            ("thread.realtime.closed", json!({})),
            ("turn.started", json!({})),
            ("turn.completed", json!({"state": "completed"})),
            ("turn.aborted", json!({"reason": "r"})),
            ("turn.plan.updated", json!({"plan": []})),
            ("turn.proposed.delta", json!({"delta": ""})),
            ("turn.proposed.completed", json!({"planMarkdown": "p"})),
            ("turn.diff.updated", json!({"unifiedDiff": ""})),
            ("item.started", json!({"itemType": "unknown"})),
            ("item.updated", json!({"itemType": "command_execution"})),
            (
                "item.completed",
                json!({"itemType": "mcp_tool_call", "status": "inProgress"}),
            ),
            (
                "content.delta",
                json!({"streamKind": "reasoning_summary_text", "delta": ""}),
            ),
            (
                "request.opened",
                json!({"requestType": "apply_patch_approval"}),
            ),
            (
                "request.resolved",
                json!({"requestType": "auth_tokens_refresh"}),
            ),
            ("user-input.requested", json!({"questions": []})),
            ("user-input.resolved", json!({"answers": {}})),
            ("task.started", json!({"taskId": "t"})),
            ("task.progress", json!({"taskId": "t", "description": "d"})),
            ("task.updated", json!({"taskId": "t"})),
            (
                "task.completed",
                json!({"taskId": "t", "status": "stopped"}),
            ),
            (
                "hook.started",
                json!({"hookId": "h", "hookName": "n", "hookEvent": "e"}),
            ),
            ("hook.progress", json!({"hookId": "h"})),
            (
                "hook.completed",
                json!({"hookId": "h", "outcome": "success"}),
            ),
            ("tool.progress", json!({})),
            ("tool.summary", json!({"summary": "s"})),
            ("auth.status", json!({})),
            ("account.updated", json!({"account": 1})),
            (
                "account.rate-limits.updated",
                json!({"limits": {"windows": []}}),
            ),
            ("mcp.status.updated", json!({"status": null})),
            ("mcp.oauth.completed", json!({"success": true})),
            (
                "model.rerouted",
                json!({"fromModel": "a", "toModel": "b", "reason": "r"}),
            ),
            ("config.warning", json!({"summary": "s"})),
            ("deprecation.notice", json!({"summary": "s"})),
            ("files.persisted", json!({"files": []})),
            ("tool.denied", json!({"toolName": "t"})),
            ("runtime.warning", json!({"message": "m"})),
            ("runtime.error", json!({"message": "m"})),
        ]
    }

    #[test]
    fn every_union_member_round_trips_byte_for_byte() {
        let events = minimal_events();
        assert_eq!(events.len(), 49, "ProviderRuntimeEventV2 has 49 members");
        for (kind, payload) in events {
            let input = event(kind, payload);
            let decoded = decode(input.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(serde_json::to_value(&decoded).unwrap(), input, "{kind}");
            // Unknown event types are NOT forward compatible here (plain union).
            let mut unknown = input;
            unknown["type"] = json!(format!("{kind}.future"));
            assert!(decode(unknown).is_err(), "{kind}.future");
        }
    }

    #[test]
    fn unknown_payload_fields_keep_explicit_null_and_absent_distinct() {
        let with_null = event("session.started", json!({"resume": null}));
        let decoded = decode(with_null.clone()).unwrap();
        let ProviderRuntimeEvent::SessionStarted { payload, .. } = &decoded else {
            panic!("expected session.started");
        };
        assert_eq!(payload.resume, Some(serde_json::Value::Null));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), with_null);
        let absent = decode(event("session.started", json!({}))).unwrap();
        let ProviderRuntimeEvent::SessionStarted { payload, .. } = &absent else {
            panic!("expected session.started");
        };
        assert_eq!(payload.resume, None);
        // Required Unknown stays `null` too.
        let item = decode(event("thread.realtime.item-added", json!({"item": null}))).unwrap();
        assert!(matches!(
            item,
            ProviderRuntimeEvent::ThreadRealtimeItemAdded { .. }
        ));
        assert!(decode(event("thread.realtime.item-added", json!({}))).is_err());
    }

    #[test]
    fn nullable_optionals_are_tri_state() {
        let decode_completed =
            |payload: serde_json::Value| decode(event("turn.completed", payload)).unwrap();
        for (payload, expected) in [
            (json!({"state": "failed"}), None),
            (json!({"state": "failed", "stopReason": null}), Some(None)),
            (
                json!({"state": "failed", "stopReason": " end_turn "}),
                Some(Some("end_turn")),
            ),
        ] {
            let ProviderRuntimeEvent::TurnCompleted {
                payload: parsed, ..
            } = decode_completed(payload.clone())
            else {
                panic!("expected turn.completed");
            };
            let got = parsed
                .stop_reason
                .as_ref()
                .map(|inner| inner.as_ref().map(|v| v.as_str()));
            assert_eq!(got, expected);
            let encoded = serde_json::to_value(decode_completed(payload.clone())).unwrap();
            match expected {
                None => assert!(encoded["payload"].get("stopReason").is_none()),
                Some(None) => assert_eq!(encoded["payload"]["stopReason"], json!(null)),
                Some(Some(_)) => assert_eq!(encoded["payload"]["stopReason"], json!("end_turn")),
            }
        }
        let plan = |payload: serde_json::Value| {
            let ProviderRuntimeEvent::TurnPlanUpdated { payload, .. } =
                decode(event("turn.plan.updated", payload)).unwrap()
            else {
                panic!("expected turn.plan.updated");
            };
            payload.explanation
        };
        assert_eq!(plan(json!({"plan": []})), None);
        assert_eq!(plan(json!({"plan": [], "explanation": null})), Some(None));
        assert!(matches!(
            plan(json!({"plan": [], "explanation": "x"})),
            Some(Some(_))
        ));
        assert!(
            decode(event(
                "turn.completed",
                json!({"state": "completed", "stopReason": "  "})
            ))
            .is_err()
        );
    }

    #[test]
    fn turn_token_usage_union_semantics() {
        let usage = |value: serde_json::Value| serde_json::from_value::<TurnTokenUsage>(value);
        let base = json!({"usageScope": "main_agent", "hasSubagents": true});
        let with = |extra: serde_json::Value| {
            let mut value = base.clone();
            for (k, v) in extra.as_object().unwrap() {
                value[k] = v.clone();
            }
            value
        };
        assert!(matches!(
            usage(with(
                json!({"usageStatus": "complete", "inputTokens": 1, "outputTokens": 2})
            )),
            Ok(TurnTokenUsage::Complete { .. })
        ));
        // `complete` needs both totals, and `complete` is not a partial status.
        assert!(usage(with(json!({"usageStatus": "complete", "inputTokens": 1}))).is_err());
        assert!(matches!(
            usage(with(json!({"usageStatus": "unavailable"}))),
            Ok(TurnTokenUsage::Partial { .. })
        ));
        assert!(matches!(
            usage(with(
                json!({"usageStatus": "partial", "outputTokens": 3, "cacheCreationTokens": 4})
            )),
            Ok(TurnTokenUsage::Partial { .. })
        ));
        assert!(usage(with(json!({"usageStatus": "bogus"}))).is_err());
        assert!(
            usage(
                json!({"usageStatus": "partial", "usageScope": "subagent", "hasSubagents": false})
            )
            .is_err()
        );
        assert!(usage(json!({"usageStatus": "partial", "usageScope": "main_agent"})).is_err());
        // Negative counts are not NonNegativeInt.
        assert!(usage(with(json!({"usageStatus": "partial", "inputTokens": -1}))).is_err());
        // Encoding keeps camelCase keys and the literal status.
        let encoded = serde_json::to_value(
            usage(with(json!({"usageStatus": "complete", "inputTokens": 1, "outputTokens": 2, "cachedInputTokens": 0}))).unwrap(),
        )
        .unwrap();
        assert_eq!(encoded["usageStatus"], json!("complete"));
        assert_eq!(encoded["cachedInputTokens"], json!(0));
        assert_eq!(encoded["usageScope"], json!("main_agent"));
    }

    #[test]
    fn thread_token_usage_snapshot_checks() {
        let snapshot =
            |value: serde_json::Value| serde_json::from_value::<ThreadTokenUsageSnapshot>(value);
        assert!(snapshot(json!({"usedTokens": 0})).is_ok());
        assert!(snapshot(json!({"usedTokens": -1})).is_err());
        // PositiveInt: zero is rejected for maxTokens / autoCompactThreshold.
        assert!(snapshot(json!({"usedTokens": 1, "maxTokens": 0})).is_err());
        assert!(snapshot(json!({"usedTokens": 1, "autoCompactThreshold": 0})).is_err());
        assert!(
            snapshot(json!({"usedTokens": 1, "maxTokens": 1, "autoCompactThreshold": 1})).is_ok()
        );
        // Fractional cost amounts survive (Schema.Number -> f64).
        let ok =
            snapshot(json!({"usedTokens": 1, "cost": {"amount": 0.0125, "currency": " USD "}}))
                .unwrap();
        let encoded = serde_json::to_value(&ok).unwrap();
        assert_eq!(
            encoded["cost"],
            json!({"amount": 0.0125, "currency": "USD"})
        );
        assert!(
            snapshot(json!({"usedTokens": 1, "cost": {"amount": 1, "currency": "x".repeat(33)}}))
                .is_err()
        );
        assert!(
            snapshot(json!({"usedTokens": 1, "cost": {"amount": 1, "currency": "x".repeat(32)}}))
                .is_ok()
        );
        // 17 astral chars are 34 UTF-16 units.
        assert!(
            snapshot(
                json!({"usedTokens": 1, "cost": {"amount": 1, "currency": "\u{1F600}".repeat(17)}})
            )
            .is_err()
        );
    }

    #[test]
    fn raw_source_template_matches_effect_semantics() {
        assert!(RuntimeEventRawSource::new("acp..extension").is_ok());
        assert!(RuntimeEventRawSource::new("acp.a.b.extension").is_ok());
        assert!(RuntimeEventRawSource::new("acp.extension").is_err());
        assert!(RuntimeEventRawSource::new("acp.x.extension\n").is_err());
        assert!(
            serde_json::from_value::<RuntimeEventRaw>(json!({"source": "codex.eventmsg"})).is_err(),
            "payload is a required Unknown key"
        );
        let raw: RuntimeEventRaw =
            serde_json::from_value(json!({"source": "codex.eventmsg", "payload": null})).unwrap();
        assert_eq!(
            serde_json::to_value(&raw).unwrap(),
            json!({"source": "codex.eventmsg", "payload": null})
        );
    }

    #[test]
    fn literal_wire_names() {
        let wire = |value: &dyn erased::Ser| value.to_json();
        assert_eq!(
            wire(&RuntimeContentStreamKind::ReasoningSummaryText),
            json!("reasoning_summary_text")
        );
        assert_eq!(
            wire(&RuntimeErrorClass::PermissionError),
            json!("permission_error")
        );
        assert_eq!(wire(&RuntimeItemStatus::InProgress), json!("inProgress"));
        assert_eq!(
            wire(&RuntimePlanStepStatus::InProgress),
            json!("inProgress")
        );
        assert_eq!(
            wire(&CanonicalItemType::CollabAgentToolCall),
            json!("collab_agent_tool_call")
        );
        assert_eq!(
            wire(&CanonicalItemType::ContextCompaction),
            json!("context_compaction")
        );
        assert_eq!(
            wire(&CanonicalRequestType::McpElicitationApproval),
            json!("mcp_elicitation_approval")
        );
        assert_eq!(wire(&ToolLifecycleItemType::ImageView), json!("image_view"));
        assert_eq!(wire(&RuntimeTaskStatus::Interrupted), json!("interrupted"));
        assert!(is_tool_lifecycle_item_type("web_search"));
        assert!(!is_tool_lifecycle_item_type("user_message"));
        // Every tool lifecycle type is also a canonical item type.
        for kind in TOOL_LIFECYCLE_ITEM_TYPES {
            assert!(
                serde_json::from_value::<ToolLifecycleItemType>(json!(kind)).is_ok(),
                "{kind}"
            );
            assert!(
                serde_json::from_value::<CanonicalItemType>(json!(kind)).is_ok(),
                "{kind}"
            );
        }
        assert!(serde_json::from_value::<RuntimeSessionState>(json!("paused")).is_err());
        assert!(serde_json::from_value::<RuntimeTurnState>(json!("running")).is_err());
    }

    mod erased {
        pub trait Ser {
            fn to_json(&self) -> serde_json::Value;
        }
        impl<T: serde::Serialize> Ser for T {
            fn to_json(&self) -> serde_json::Value {
                serde_json::to_value(self).unwrap()
            }
        }
    }

    #[test]
    fn user_input_initial_answer_keeps_absent_and_exact_text() {
        let question = |initial: Option<&str>| {
            let mut value = json!({
                "id": "editor", "header": "Message", "question": "Edit message", "options": [],
            });
            if let Some(initial) = initial {
                value["initialAnswer"] = json!(initial);
            }
            value
        };
        let absent = serde_json::from_value::<UserInputQuestion>(question(None)).unwrap();
        assert_eq!(absent.initial_answer, None);
        let long = "detail\n".repeat(500);
        for initial in ["", "  fix(server): keep whitespace\n\n", long.as_str()] {
            let parsed =
                serde_json::from_value::<UserInputQuestion>(question(Some(initial))).unwrap();
            assert_eq!(parsed.initial_answer.as_deref(), Some(initial));
        }
    }

    #[test]
    fn user_input_question_and_request_payloads() {
        let parsed = decode(event(
            "user-input.requested",
            json!({
                "questions": [{
                    "id": "q", "header": "H", "question": "Q?",
                    "options": [{"label": "A", "description": "", "value": "a"}],
                    "allowCustomAnswer": true, "initialAnswer": "", "multiSelect": false,
                }],
                "responseMode": "message",
            }),
        ))
        .unwrap();
        let ProviderRuntimeEvent::UserInputRequested { payload, .. } = &parsed else {
            panic!("expected user-input.requested");
        };
        // multiSelect only defaults at construction: decode keeps absent/explicit as-is.
        assert_eq!(payload.questions[0].multi_select, Some(false));
        assert_eq!(payload.questions[0].initial_answer.as_deref(), Some(""));
        assert!(
            decode(event(
                "user-input.requested",
                json!({"questions": [], "responseMode": "other"})
            ))
            .is_err()
        );
        let opened = decode(event(
            "request.opened",
            json!({
                "requestType": "command_execution_approval",
                "options": [{"decision": "acceptForSession", "label": "Always", "warning": "careful"}],
                "args": {"cmd": "ls"},
            }),
        ))
        .unwrap();
        assert!(matches!(
            &opened,
            ProviderRuntimeEvent::RequestOpened { payload, .. }
                if payload.options.as_ref().unwrap().len() == 1
        ));
        assert_eq!(
            serde_json::to_value(&opened).unwrap()["payload"]["args"],
            json!({"cmd": "ls"})
        );
    }

    #[test]
    fn task_linkage_flattens_for_every_task_payload() {
        let linkage = json!({
            "taskType": "local_workflow", "agentKind": "background", "agentId": "a",
            "title": "T", "role": "r", "model": "m", "effort": "high", "toolUseId": "tu",
            "parentAgentId": "pa", "workflowName": "wf", "agentIndex": 1, "phaseIndex": 2,
            "phaseTitle": "pt", "phases": [{"index": 0, "title": "p0"}], "attempt": 3,
            "runHandles": {"runId": "r1", "scriptPath": "s", "transcriptDir": "d", "sessionUrl": "https://x"},
            "outputFile": "o", "agentPath": "/root/marlow", "timelineBypass": true,
        });
        for (kind, own) in [
            ("task.started", json!({"taskId": "t", "description": "d"})),
            (
                "task.progress",
                json!({"taskId": "t", "description": "d", "status": "waiting", "typedUsage": {"totalTokens": 5}}),
            ),
            (
                "task.updated",
                json!({"taskId": "t", "status": "idle", "endedAt": "2026-01-01T00:00:00Z", "isBackgrounded": true}),
            ),
            (
                "task.completed",
                json!({"taskId": "t", "status": "failed", "typedUsage": {"totalTokens": 5, "durationMs": 1}}),
            ),
        ] {
            let mut payload = own.clone();
            for (k, v) in linkage.as_object().unwrap() {
                payload[k] = v.clone();
            }
            let input = event(kind, payload);
            let decoded = decode(input.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(serde_json::to_value(&decoded).unwrap(), input, "{kind}");
        }
        assert!(
            decode(event(
                "task.started",
                json!({"taskId": "t", "agentKind": "other"})
            ))
            .is_err()
        );
        assert!(decode(event("task.started", json!({"taskId": "t", "attempt": -1}))).is_err());
        assert!(
            decode(event(
                "task.completed",
                json!({"taskId": "t", "status": "running"})
            ))
            .is_err()
        );
    }

    #[test]
    fn misc_payload_checks() {
        // Int fields accept negatives; NonNegativeInt does not.
        assert!(
            decode(event(
                "content.delta",
                json!({"streamKind": "unknown", "delta": "", "contentIndex": -1})
            ))
            .is_ok()
        );
        assert!(
            decode(event(
                "hook.completed",
                json!({"hookId": "h", "outcome": "error", "exitCode": -1})
            ))
            .is_ok()
        );
        assert!(
            decode(event(
                "thread.state.changed",
                json!({"state": "idle", "beforeTokens": -1})
            ))
            .is_err()
        );
        // Number fields keep fractions.
        let tool = decode(event(
            "tool.progress",
            json!({"elapsedSeconds": 1.5, "taskId": "t"}),
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&tool).unwrap()["payload"]["elapsedSeconds"],
            json!(1.5)
        );
        let completed = decode(event(
            "turn.completed",
            json!({"state": "completed", "totalCostUsd": 0.25, "modelUsage": {"b": 1, "a": 2}}),
        ))
        .unwrap();
        let encoded = serde_json::to_string(&completed).unwrap();
        assert!(encoded.contains(r#""totalCostUsd":0.25"#));
        // Record key order is preserved.
        assert!(encoded.find(r#""b":1"#).unwrap() < encoded.find(r#""a":2"#).unwrap());
        // Blank trimmed strings are rejected; surrounding whitespace is trimmed.
        assert!(decode(event("runtime.error", json!({"message": "   "}))).is_err());
        let trimmed = decode(event(
            "runtime.error",
            json!({"message": " boom ", "class": "transport_error", "code": "E1"}),
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&trimmed).unwrap()["payload"]["message"],
            json!("boom")
        );
        // Base routing keys.
        let mut with_refs = event("turn.started", json!({"model": "m", "effort": "high"}));
        with_refs["turnId"] = json!("turn-1");
        with_refs["itemId"] = json!("item-1");
        with_refs["requestId"] = json!("req-1");
        with_refs["providerRefs"] =
            json!({"providerTurnId": "pt", "providerItemId": "pi", "providerRequestId": "pr"});
        with_refs["providerInstanceId"] = json!("codex_work");
        let decoded = decode(with_refs.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), with_refs);
        // Missing required base fields fail.
        for key in ["eventId", "provider", "threadId", "createdAt"] {
            let mut missing = event("turn.started", json!({}));
            missing.as_object_mut().unwrap().remove(key);
            assert!(decode(missing).is_err(), "{key}");
        }
    }

    #[test]
    fn files_persisted_and_rate_limits_nest_correctly() {
        let files = event(
            "files.persisted",
            json!({
                "files": [{"filename": "a.txt", "fileId": "f1"}],
                "failed": [{"filename": "b.txt", "error": "nope"}],
            }),
        );
        assert_eq!(
            serde_json::to_value(decode(files.clone()).unwrap()).unwrap(),
            files
        );
        let limits = event(
            "account.rate-limits.updated",
            json!({"limits": {"windows": [{"id": "five_hour", "kind": "session", "label": "5h", "usedPercent": 12.5}]}}),
        );
        assert_eq!(
            serde_json::to_value(decode(limits.clone()).unwrap()).unwrap(),
            limits
        );
        // ProviderUsageLimitsUpdate.windows is a plain array: a bad window fails the event.
        assert!(decode(event(
            "account.rate-limits.updated",
            json!({"limits": {"windows": [{"id": "x", "kind": "session", "label": "l", "usedPercent": 101}]}}),
        ))
        .is_err());
    }

    #[test]
    fn tool_activity_union_and_source_bounds() {
        for bad in [
            json!({"_tag": "unknown"}),
            json!({"_tag": "native-app", "app": {"_tag": "app-id", "appId": ""}}),
            json!({"_tag": "website", "pageUrl": "x".repeat(4097)}),
            json!({"_tag": "themed-logo", "logoUrl": "u", "logoUrlDark": "x".repeat(4097)}),
        ] {
            assert!(
                serde_json::from_value::<ToolActivityIcon>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let icon =
            json!({"_tag": "native-app", "app": {"_tag": "display-name", "displayName": "Editor"}});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<ToolActivityIcon>(icon.clone()).unwrap())
                .unwrap(),
            icon
        );
        let source = |key: String, name: String| {
            serde_json::from_value::<ToolActivitySource>(
                json!({"key": key, "name": name, "kind": "integration"}),
            )
        };
        assert!(source("k".repeat(512), "n".repeat(160)).is_ok());
        assert!(source("k".repeat(513), "n".into()).is_err());
        assert!(source("k".into(), "n".repeat(161)).is_err());
        // 81 astral chars are 162 UTF-16 units.
        assert!(source("k".into(), "\u{1F600}".repeat(81)).is_err());
        let lifecycle = decode(event(
            "item.completed",
            json!({
                "itemType": "web_search", "toolSurface": "browser",
                "toolIcon": {"_tag": "themed-logo", "logoUrl": "https://l"},
                "toolSource": {"key": "k", "name": "n", "kind": "browser"},
                "agentId": "a", "parentToolUseId": "p", "data": {"x": 1},
            }),
        ))
        .unwrap();
        assert!(matches!(
            lifecycle,
            ProviderRuntimeEvent::ItemCompleted { .. }
        ));
    }

    #[test]
    fn whole_numbers_encode_without_a_fraction() {
        // `Schema.Number` fields go through JSON.stringify upstream: `2`, not `2.0`.
        let completed = decode(event(
            "turn.completed",
            json!({"state": "completed", "totalCostUsd": 2}),
        ))
        .unwrap();
        let encoded = serde_json::to_string(&completed).unwrap();
        assert!(encoded.contains(r#""totalCostUsd":2}"#), "{encoded}");

        let tool = decode(event("tool.progress", json!({"elapsedSeconds": 7}))).unwrap();
        assert!(
            serde_json::to_string(&tool)
                .unwrap()
                .contains(r#""elapsedSeconds":7}"#)
        );

        let usage = decode(event(
            "thread.token-usage.updated",
            json!({"usage": {"usedTokens": 1, "cost": {"amount": 3, "currency": "USD"}}}),
        ))
        .unwrap();
        assert!(
            serde_json::to_string(&usage)
                .unwrap()
                .contains(r#""amount":3,"#)
        );
    }

    #[test]
    fn tool_lifecycle_item_types_stay_in_sync_with_the_enums() {
        let all = [
            ToolLifecycleItemType::CommandExecution,
            ToolLifecycleItemType::FileChange,
            ToolLifecycleItemType::McpToolCall,
            ToolLifecycleItemType::DynamicToolCall,
            ToolLifecycleItemType::CollabAgentToolCall,
            ToolLifecycleItemType::WebSearch,
            ToolLifecycleItemType::ImageView,
        ];
        let wire: Vec<String> = all
            .iter()
            .map(|kind| {
                serde_json::to_value(kind)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(wire, TOOL_LIFECYCLE_ITEM_TYPES);
        // Canonical item types: user/assistant/reasoning/plan come first, the
        // tool lifecycle types follow in order (upstream spreads the array).
        let canonical = [
            "user_message",
            "assistant_message",
            "reasoning",
            "plan",
            "command_execution",
            "file_change",
            "mcp_tool_call",
            "dynamic_tool_call",
            "collab_agent_tool_call",
            "web_search",
            "image_view",
            "review_entered",
            "review_exited",
            "context_compaction",
            "error",
            "unknown",
        ];
        for kind in canonical {
            let parsed: CanonicalItemType = serde_json::from_value(json!(kind)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json!(kind));
        }
    }

    #[test]
    fn raw_event_keeps_every_declared_key() {
        let raw = json!({
            "source": "claude.sdk.permission",
            "method": "can_use_tool",
            "messageType": "control_request",
            "payload": {"b": 1, "a": [null]},
        });
        let parsed: RuntimeEventRaw = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), raw);
        assert!(
            serde_json::to_string(&parsed)
                .unwrap()
                .contains(r#""b":1,"a""#)
        );
        assert!(
            serde_json::from_value::<RuntimeEventRaw>(json!({
                "source": "codex.eventmsg", "method": "  ", "payload": 1,
            }))
            .is_err()
        );
    }

    #[test]
    fn app_id_bounds_count_utf16_and_pattern() {
        let app_id = |value: String| {
            serde_json::from_value::<ToolActivityNativeAppReference>(
                json!({"_tag": "app-id", "appId": value}),
            )
        };
        assert!(app_id("a".repeat(512)).is_ok());
        assert!(app_id("a".repeat(513)).is_err());
        assert!(app_id("a.b_c-d".into()).is_ok());
        assert!(app_id("a/b".into()).is_err());
        assert!(app_id("caf\u{e9}".into()).is_err());
        // Surrounding whitespace is trimmed before the pattern check.
        assert!(app_id("  com.example  ".into()).is_ok());
    }

    #[test]
    fn tool_source_icon_round_trips_all_variants() {
        for icon in [
            json!({"_tag": "website", "pageUrl": "https://a", "faviconUrl": "f", "faviconUrlDark": "d"}),
            json!({"_tag": "native-app", "app": {"_tag": "app-id", "appId": "com.a.B"}}),
            json!({"_tag": "themed-logo", "logoUrl": "l", "logoUrlDark": "d"}),
        ] {
            let source = json!({"key": "k", "name": "n", "kind": "computer", "icon": icon});
            let parsed: ToolActivitySource = serde_json::from_value(source.clone()).unwrap();
            assert_eq!(serde_json::to_value(&parsed).unwrap(), source);
        }
        assert!(
            serde_json::from_value::<ToolActivitySource>(
                json!({"key": "k", "name": "n", "kind": "other"})
            )
            .is_err()
        );
    }
}
