//! Port of upstream/packages/contracts/src/orchestrationV2.ts
//!
//! The orchestration-v2 contracts: the command/event model of the
//! event-sourced orchestrator. Types keep upstream names (serde camelCase)
//! so persisted JSON — a T3 database import — round-trips byte for byte.
//!
//! Skipped, per PORTING.md: `ORCHESTRATION_V2_WS_METHODS` and
//! `OrchestrationV2RpcSchemas` (WS/RPC plumbing; Turin is one process). The
//! pure payload structs those groups carry are kept. The `*Json` variants
//! are wire-identical to the runtime schemas here (both spell timestamps as
//! ISO strings once serde_json is the only codec), so they are type aliases.
//!

use crate::base_schemas::{
    CheckpointId, CheckpointRef, CheckpointScopeId, CommandId, ContextHandoffId, ContextTransferId,
    EventId, IsoDateTime, MessageId, NodeId, NonNegativeInt, PlanId, PositiveInt, ProjectId,
    ProviderSessionId, ProviderThreadId, ProviderTurnId, RawEventId, RunAttemptId, RunId,
    RuntimeRequestId, ScheduledTaskId, TaggedUnion, ThreadId, TrimmedNonEmptyString, TurnItemId,
    UtcDateTime,
};
use crate::chat_attachment::ChatAttachment;
use crate::composer_context::OrchestrationMessageContext;
use crate::model_selection::ModelSelection;
use crate::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use crate::provider_policy::{
    ProviderApprovalDecision, ProviderApprovalOption, ProviderInteractionMode, ProviderRequestKind,
    ProviderUserInputAnswers, RuntimeMode, UserInputAttachmentAnswerPayload, UserInputAttachments,
};
use crate::provider_runtime::{
    ThreadTokenUsageSnapshot, ToolActivityIcon, ToolActivitySource, ToolActivitySurface,
    TurnTokenUsage,
};
use crate::thread_pull_request::{
    ThreadLinkedPullRequest, ThreadPullRequestLink, ThreadPullRequestLinkSource,
    ThreadPullRequestSnapshot, ThreadPullRequestStack, ThreadPullRequestWatch,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use crate::thread_pull_request::ThreadPullRequestKey;

// ---------------------------------------------------------------------------
// Open literal unions
// ---------------------------------------------------------------------------

/// Who created a thread, message, or command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2Actor {
    User,
    Agent,
    System,
}

/// What process created a thread or dispatched a message. Turin only ever
/// produces `web`-shaped (in-process) sources, but persisted rows carry the
/// rest, and `mcp` is the delegated-task thread source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2CreationSource {
    Web,
    Mobile,
    Mcp,
    Provider,
    Server,
}

/// Whether a thread's history was born in v2 or imported from v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ThreadHistoryOrigin {
    Native,
    V1Import,
}

/// How strongly the app can reference a provider-native object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2NativeRefStrength {
    Strong,
    Weak,
    None,
}

crate::string_literal!(
    /// Constant `"strong"` strength of an imported native thread ref.
    StrongNativeRefStrength,
    "strong"
);

crate::string_literal!(
    /// Constant `"user"` reason of a user-initiated unsettle/unsnooze.
    UserUnsettleReason,
    "user"
);

crate::string_literal!(
    /// Constant `"message"` response mode of a user-input request item.
    MessageResponseMode,
    "message"
);

crate::int_literal!(
    /// Constant `2` item identity version of provider-derived metadata.
    ItemIdentityVersionV2,
    2
);

/// `Schema.Literal(true)`: encodes as `true`, decodes only from `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrueLiteral;

impl Serialize for TrueLiteral {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for TrueLiteral {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if bool::deserialize(d)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected the literal true"))
        }
    }
}

/// `Schema.NonEmptyString`: any non-empty string, whitespace preserved
/// (workflow members' prompt and result excerpts keep their indentation).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NonEmptyString(String);

impl NonEmptyString {
    pub fn new(value: impl AsRef<str>) -> Result<Self, crate::base_schemas::ContractError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(crate::base_schemas::ContractError {
                expected: "a non-empty string",
                got: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for NonEmptyString {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Provider refs, lineage, context transfers
// ---------------------------------------------------------------------------

/// How the app refers to a provider-native object (thread, turn, item, task).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderRef {
    pub driver: ProviderDriverKind,
    /// Null where the provider never gave us a durable native id.
    pub native_id: Option<TrimmedNonEmptyString>,
    pub strength: OrchestrationV2NativeRefStrength,
    /// Equality-check token for the same native id under a rewritten session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<TrimmedNonEmptyString>,
    /// Disambiguates repeats of the same native id inside one thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<NonNegativeInt>,
}

/// Fork/subagent lineage of an app thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2AppThreadLineage {
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<OrchestrationV2LineageRelationship>,
    pub root_thread_id: ThreadId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2LineageRelationship {
    Fork,
    Subagent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ContextTransferType {
    Fork,
    ProviderHandoff,
    MergeBack,
    SubagentSpawn,
    SubagentResult,
}

/// Where a context transfer read its source conversation from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ContextSourcePoint {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_item_id: Option<TurnItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thread_ref: Option<OrchestrationV2ProviderRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_ref: Option<OrchestrationV2ProviderRef>,
}

/// The point a fork starts from: the thread's latest stable state, a run, or
/// a checkpoint (upstream `OrchestrationV2ThreadForkSourcePoint`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2ThreadForkSourcePoint {
    LatestStable,
    Run { run_id: RunId },
    Checkpoint { checkpoint_id: CheckpointId },
}

/// How a context transfer was resolved (upstream inline union on `strategy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "strategy",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2ContextTransferResolution {
    NativeFork {
        provider_thread_ref: OrchestrationV2ProviderRef,
    },
    PortableContext {
        context_handoff_id: ContextHandoffId,
    },
    DeltaContext {
        context_handoff_id: ContextHandoffId,
    },
    ForkDeltaContext {
        context_handoff_id: ContextHandoffId,
    },
    CheckpointContext {
        context_handoff_id: ContextHandoffId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ContextTransferStatus {
    Pending,
    ResolvedNative,
    ResolvedPortable,
    Failed,
    Consumed,
    Superseded,
}

/// A pending or resolved move of conversation context between threads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ContextTransfer {
    pub id: ContextTransferId,
    pub r#type: OrchestrationV2ContextTransferType,
    pub source_thread_id: ThreadId,
    pub target_thread_id: ThreadId,
    pub source_point: OrchestrationV2ContextSourcePoint,
    pub base_point: Option<OrchestrationV2ContextSourcePoint>,
    pub source_provider_instance_id: Option<ProviderInstanceId>,
    pub target_provider_instance_id: Option<ProviderInstanceId>,
    pub target_run_id: Option<RunId>,
    pub status: OrchestrationV2ContextTransferStatus,
    pub resolution: Option<OrchestrationV2ContextTransferResolution>,
    pub created_by: OrchestrationV2Actor,
    pub error: Option<String>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub consumed_at: Option<UtcDateTime>,
}

// ---------------------------------------------------------------------------
// Provider capabilities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SessionCapabilities {
    pub supports_multiple_provider_threads_per_session: bool,
    pub supports_model_switch_in_session: bool,
    pub supports_provider_switching_via_handoff: bool,
    pub supports_runtime_mode_switch_in_session: bool,
    pub pending_requests_survive_restart: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadCapabilities {
    pub can_create_empty_thread: bool,
    pub can_read_thread_snapshot: bool,
    pub can_rollback_thread: bool,
    pub can_fork_thread: bool,
    pub can_fork_from_turn: bool,
    pub can_fork_from_subagent_thread: bool,
    pub exposes_native_thread_id: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2TurnCapabilities {
    pub exposes_native_turn_id: bool,
    pub emits_turn_started: bool,
    pub emits_turn_completed: bool,
    pub supports_interrupt: bool,
    pub supports_active_steering: bool,
    /// Some native steering mechanisms cancel pending tools before consuming the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_interrupts_tools: Option<bool>,
    pub supports_steering_by_interrupt_restart: bool,
    pub supports_queued_messages: bool,
    pub terminal_status_quality: OrchestrationV2NativeRefStrength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2StreamingCapabilities {
    pub streams_assistant_text: bool,
    pub streams_reasoning: bool,
    pub streams_tool_output: bool,
    pub streams_plan_text: bool,
    pub emits_message_completed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ToolCapabilities {
    pub exposes_tool_item_ids: bool,
    pub emits_tool_started: bool,
    pub emits_tool_completed: bool,
    pub emits_tool_output: bool,
    pub supports_mcp_tools: bool,
    pub supports_dynamic_tool_callbacks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ApprovalCapabilities {
    pub supports_command_approval: bool,
    pub supports_file_read_approval: bool,
    pub supports_file_change_approval: bool,
    pub supports_apply_patch_approval: bool,
    pub approvals_have_native_request_ids: bool,
    pub approval_callbacks_are_live_only: bool,
    pub approvals_can_originate_from_subagents: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2PlanningCapabilities {
    pub emits_plan_updated: bool,
    pub emits_todo_list: bool,
    pub emits_proposed_plan: bool,
    pub supports_structured_questions: bool,
    pub plan_deltas_have_item_ids: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SubagentCapabilities {
    pub supports_subagents: bool,
    pub exposes_subagent_thread_ids: bool,
    pub emits_subagent_lifecycle: bool,
    pub can_wait_for_subagents: bool,
    pub can_close_subagents: bool,
    pub can_fork_subagent_thread: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ContextCapabilities {
    pub accepts_system_context: bool,
    pub accepts_developer_context: bool,
    pub accepts_synthetic_user_context: bool,
    pub can_generate_summaries: bool,
    pub can_consume_handoff_summaries: bool,
    pub supports_delta_handoff: bool,
    pub supports_full_thread_handoff: bool,
    pub max_recommended_handoff_chars: Option<PositiveInt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2CheckpointCapabilities {
    pub app_can_checkpoint_filesystem: bool,
    pub supports_nested_checkpoint_scopes: bool,
    pub provider_can_rollback_conversation: bool,
    pub provider_rollback_returns_snapshot: bool,
    pub provider_can_read_conversation_snapshot: bool,
}

/// Where T3 runtime modes are actually enforced. "native" providers receive
/// the approval and sandbox policy each turn and confine their own execution.
/// "client-boundary" providers only have policy applied where T3 mediates the
/// work (permission requests and client fs/terminal handlers); provider-owned
/// execution is not confined, so sandbox guarantees are reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrchestrationV2RuntimePolicyEnforcement {
    Native,
    ClientBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2RuntimePolicyCapabilities {
    pub enforcement: OrchestrationV2RuntimePolicyEnforcement,
}

fn default_runtime_policy() -> OrchestrationV2RuntimePolicyCapabilities {
    OrchestrationV2RuntimePolicyCapabilities {
        enforcement: OrchestrationV2RuntimePolicyEnforcement::ClientBoundary,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2IdentityCapabilities {
    pub native_thread_ids: OrchestrationV2NativeRefStrength,
    pub native_turn_ids: OrchestrationV2NativeRefStrength,
    pub native_item_ids: OrchestrationV2NativeRefStrength,
    pub native_request_ids: OrchestrationV2NativeRefStrength,
}

/// Everything a provider driver can and cannot do, recorded per session so
/// replay never overclaims enforcement: events persisted before
/// `runtimePolicy` existed decode to the weaker client-boundary guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderCapabilities {
    pub sessions: OrchestrationV2SessionCapabilities,
    pub threads: OrchestrationV2ThreadCapabilities,
    pub turns: OrchestrationV2TurnCapabilities,
    pub streaming: OrchestrationV2StreamingCapabilities,
    pub tools: OrchestrationV2ToolCapabilities,
    pub approvals: OrchestrationV2ApprovalCapabilities,
    pub planning: OrchestrationV2PlanningCapabilities,
    pub subagents: OrchestrationV2SubagentCapabilities,
    pub context: OrchestrationV2ContextCapabilities,
    pub checkpointing: OrchestrationV2CheckpointCapabilities,
    pub identity: OrchestrationV2IdentityCapabilities,
    #[serde(default = "default_runtime_policy")]
    pub runtime_policy: OrchestrationV2RuntimePolicyCapabilities,
}

// ---------------------------------------------------------------------------
// Limit recovery
// ---------------------------------------------------------------------------

/// A usage-limit recovery marker on a thread: the run that hit the limit,
/// when the provider resets, and the standing choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2LimitRecovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<CommandId>,
    pub run_id: RunId,
    pub reset_at: IsoDateTime,
    pub auto_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<bool>,
}

/// A choice update preserves omitted options for this same run and reset.
///
/// Upstream attaches a struct-level filter: at least one of `autoResume` /
/// `snooze` must be present, so an update can never silently disable both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2LimitRecoveryUpdate {
    pub run_id: RunId,
    pub reset_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<bool>,
}

impl<'de> Deserialize<'de> for OrchestrationV2LimitRecoveryUpdate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            run_id: RunId,
            reset_at: IsoDateTime,
            #[serde(default)]
            auto_resume: Option<bool>,
            #[serde(default)]
            snooze: Option<bool>,
        }
        let raw = Raw::deserialize(d)?;
        if raw.auto_resume.is_none() && raw.snooze.is_none() {
            return Err(serde::de::Error::custom(
                "A recovery update must include autoResume or snooze.",
            ));
        }
        Ok(Self {
            run_id: raw.run_id,
            reset_at: raw.reset_at,
            auto_resume: raw.auto_resume,
            snooze: raw.snooze,
        })
    }
}

// ---------------------------------------------------------------------------
// App thread
// ---------------------------------------------------------------------------

/// In-flight title regeneration marker; cleared when a new title lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2TitleRegeneration {
    pub request_id: CommandId,
    pub started_at: UtcDateTime,
}

/// Latest rollback that failed after every retry; cleared when the next
/// rollback starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2RollbackFailure {
    pub request_id: CommandId,
    pub message: TrimmedNonEmptyString,
}

/// Where an app thread was forked from (upstream inline union on `type`,
/// shared by the thread, shell, and fork turn item).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2ForkSource {
    Run {
        thread_id: ThreadId,
        run_id: RunId,
    },
    Node {
        node_id: NodeId,
    },
    ProviderThread {
        provider_thread_id: ProviderThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_turn_id: Option<ProviderTurnId>,
    },
}

/// Links a server-generated restart continuation to the interrupted run
/// (upstream inline struct, shared by runs and messages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SourcePlanRef {
    pub thread_id: ThreadId,
    pub plan_id: PlanId,
}

/// The app's own record of a thread: identity, provider selection, lineage,
/// and sidebar state. This is what every `thread.*` event carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2AppThread {
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: TrimmedNonEmptyString,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<TrimmedNonEmptyString>,
    pub worktree_path: Option<TrimmedNonEmptyString>,
    /// Pull request the user linked to this thread (#8160); optional so
    /// pre-linking servers still decode.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub linked_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<Vec<ThreadPullRequestLink>>,
    /// Pull request discovered from the thread's current branch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub branch_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<OrchestrationV2ThreadHistoryOrigin>,
    pub lineage: OrchestrationV2AppThreadLineage,
    pub forked_from: Option<OrchestrationV2ForkSource>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub archived_at: Option<UtcDateTime>,
    /// Explicit settle state that outranks activity-derived settlement.
    /// Events persisted before the default existed decode to null.
    #[serde(default)]
    pub settled_override: Option<OrchestrationV2SettleOverride>,
    #[serde(default)]
    pub settled_at: Option<UtcDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub unsettled_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub snoozed_until: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub snoozed_at: Option<Option<UtcDateTime>>,
    /// Manual wakes restart inactivity without changing the sidebar's sort position.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub last_snooze_wake_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub limit_recovery: Option<Option<OrchestrationV2LimitRecovery>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub pinned_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub auto_settle_disabled_at: Option<Option<UtcDateTime>>,
    /// Fractional-index slot in the user-arranged pinned order. Optional so
    /// payloads from pre-reorder servers still decode.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub pin_order_key: Option<Option<TrimmedNonEmptyString>>,
    /// Fractional-index slot in the user-arranged active order.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub active_order_key: Option<Option<TrimmedNonEmptyString>>,
    /// Server-side visited watermark; rows before it decode to null.
    #[serde(default)]
    pub last_visited_at: Option<UtcDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub title_regeneration: Option<Option<OrchestrationV2TitleRegeneration>>,
    /// Latest accepted rollback. Only its failure is recorded in `rollbackFailure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_request_id: Option<CommandId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub rollback_failure: Option<Option<OrchestrationV2RollbackFailure>>,
    pub deleted_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrchestrationV2SettleOverride {
    #[serde(rename = "settled")]
    Settled,
    #[serde(rename = "active")]
    Active,
}

/// A subagent the provider spawned on its own (Claude's Agent tool, Codex or
/// Cursor native subagents). The provider owns its conversation, so it cannot
/// take messages; T3 delegate_task children (`creationSource: "mcp"`) can.
pub fn is_provider_native_subagent_thread(thread: &OrchestrationV2AppThread) -> bool {
    thread.lineage.relationship_to_parent == Some(OrchestrationV2LineageRelationship::Subagent)
        && thread.creation_source == OrchestrationV2CreationSource::Provider
}

// ---------------------------------------------------------------------------
// Runs and delegated-task completion cohorts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2RunStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2DelegatedCompletionTaskDeliveryState {
    Pending,
    Claimed,
    Acknowledged,
    Delivered,
    Disposed,
}

/// Where a delegated task's terminal result stands on its way to the parent
/// thread: one parent run observes (claims) it, the parent's runtime
/// acknowledges it, the turn delivers it, and settlement disposes of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2DelegatedCompletionTaskDelivery {
    pub state: OrchestrationV2DelegatedCompletionTaskDeliveryState,
    pub observed_by_run_id: Option<RunId>,
}

/// One delivery of delegated-task results to the parent thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2DelegatedCompletionDelivery {
    pub generation: PositiveInt,
    pub message_id: MessageId,
    pub task_ids: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2DelegatedCompletionCohortDisposition {
    Open,
    Stopped,
    Disposed,
}

/// The delegated tasks whose results this run delivers to its parent: a
/// cohort with a generation counter (older generations are superseded) and
/// the delivery this run's message carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2DelegatedCompletionCohort {
    pub disposition: OrchestrationV2DelegatedCompletionCohortDisposition,
    pub next_generation: PositiveInt,
    pub delivery: Option<OrchestrationV2DelegatedCompletionDelivery>,
}

/// Background work that restart recovery cancelled; the next provider turn is told once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2RestartCancelledBackgroundWork {
    pub kind: OrchestrationV2RestartCancelledBackgroundWorkKind,
    pub label: TrimmedNonEmptyString,
    /// Stable identity (turn item or provider task id) so same-named work is not merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2RestartCancelledBackgroundWorkKind {
    Subagent,
    Shell,
    Monitor,
    Task,
}

/// Replaces a run's recorded restart-cancelled work without touching its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2RunBackgroundWorkCancelled {
    pub run_id: RunId,
    pub restart_cancelled_background_work: Vec<OrchestrationV2RestartCancelledBackgroundWork>,
}

/// How a launch prepares a run's workspace (upstream
/// `OrchestrationV2ThreadLaunchWorkspaceStrategy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2ThreadLaunchWorkspaceStrategy {
    Root {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<TrimmedNonEmptyString>,
    },
    ExistingWorktree {
        worktree_path: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<TrimmedNonEmptyString>,
    },
    Worktree {
        base_ref: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_from_origin: Option<bool>,
    },
}

/// Failure code on the error item a failed workspace preparation leaves.
pub const ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE: &str =
    "workspace_preparation_failed";

/// One user turn's run record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2Run {
    pub id: RunId,
    pub thread_id: ThreadId,
    pub ordinal: PositiveInt,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub user_message_id: MessageId,
    pub root_node_id: Option<NodeId>,
    pub active_attempt_id: Option<RunAttemptId>,
    pub status: OrchestrationV2RunStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub queue_position: Option<Option<PositiveInt>>,
    /// Restart recovery holds the queue until the user explicitly resumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_held: Option<bool>,
    pub requested_at: UtcDateTime,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
    pub checkpoint_id: Option<CheckpointId>,
    pub context_handoff_id: Option<ContextHandoffId>,
    /// Links server-generated restart continuations to the interrupted run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_continuation_of_run_id: Option<RunId>,
    /// The delegated task this run answers, including a follow-up in an existing child thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_task_id: Option<NodeId>,
    /// Set on wake runs (background notifications, delegated task results,
    /// restart continuations): when the work they continue started. Read it
    /// through [`orchestration_v2_run_work_started_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_started_at: Option<UtcDateTime>,
    /// Set by restart recovery on the thread's latest started run. Delivered to
    /// the provider with the first later run that reaches a provider turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_cancelled_background_work:
        Option<Vec<OrchestrationV2RestartCancelledBackgroundWork>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan_ref: Option<OrchestrationV2SourcePlanRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<OrchestrationV2DelegatedCompletionCohort>,
    /// How a launch prepares this run's workspace; prepared-run.retry repeats it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_preparation: Option<OrchestrationV2ThreadLaunchWorkspaceStrategy>,
}

/// When the work a run belongs to started. A wake does not start new work, so
/// working timers count from the prompt that did, not from the latest wake.
pub fn orchestration_v2_run_work_started_at(run: &OrchestrationV2Run) -> &UtcDateTime {
    run.work_started_at
        .as_ref()
        .or(run.started_at.as_ref())
        .unwrap_or(&run.requested_at)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2RunAttemptReason {
    Initial,
    SteeringRestart,
    Retry,
    ProviderRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2RunAttemptStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    Superseded,
}

/// One attempt at driving a provider thread for a run (steering restarts and
/// retries open new attempts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2RunAttempt {
    pub id: RunAttemptId,
    /// Provider-thread rows can be reused after recovery; retain the native input destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<String>,
    pub run_id: RunId,
    pub attempt_ordinal: PositiveInt,
    pub root_node_id: NodeId,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub reason: OrchestrationV2RunAttemptReason,
    pub status: OrchestrationV2RunAttemptStatus,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ExecutionNodeKind {
    RootTurn,
    AssistantMessage,
    Reasoning,
    Plan,
    TodoList,
    ToolCall,
    ApprovalRequest,
    UserInputRequest,
    Subagent,
    Hook,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ExecutionNodeStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

/// The execution tree a run grows: one node per unit of work, so the timeline
/// can render partial progress and counts can attribute work to runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ExecutionNode {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: Option<NodeId>,
    pub root_node_id: NodeId,
    pub kind: OrchestrationV2ExecutionNodeKind,
    pub status: OrchestrationV2ExecutionNodeStatus,
    /// Whether this node's work advances the thread's run count.
    pub counts_for_run: bool,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<OrchestrationV2ProviderRef>,
    pub runtime_request_id: Option<RuntimeRequestId>,
    pub checkpoint_scope_id: Option<CheckpointScopeId>,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
}

// ---------------------------------------------------------------------------
// Workflow runs (provider-native multi-agent scripts)
// ---------------------------------------------------------------------------

/// One entry of a dynamic workflow's phase plan. The plan grows while the run
/// executes: a phase a script only reaches at runtime joins the list when it
/// starts, so a client must treat the list as append-only rather than as a
/// fixed arc declared up front.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2WorkflowPhase {
    pub index: NonNegativeInt,
    pub title: TrimmedNonEmptyString,
}

/// One agent inside a workflow run. Timestamps are epoch milliseconds rather
/// than `DateTimeUtc` because only the top-level entity fields are remapped
/// for the JSON wire shape (see `OrchestrationV2SubagentJson`), so a nested
/// `DateTimeUtc` would never be encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2WorkflowAgent {
    /// Spawn ordinal within the run. Stable identity: an agent keeps it across updates.
    pub index: NonNegativeInt,
    pub label: TrimmedNonEmptyString,
    /// Provider-assigned id, absent until the agent actually starts. Names the
    /// member's transcript file inside the run's transcript directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<TrimmedNonEmptyString>,
    /// The member's own thread, so the UI can open it like any subagent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_thread_id: Option<ThreadId>,
    pub state: OrchestrationV2WorkflowAgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_index: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_title: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TrimmedNonEmptyString>,
    /// Retry counter; > 1 means the agent was restarted after a provider error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<NonNegativeInt>,
    /// Prompt and result excerpts. A workflow member's whole conversation is one
    /// prompt and one answer, so together these are its transcript as far as the
    /// provider exposes it — both are capped by the CLI at a few hundred
    /// characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<NonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<NonEmptyString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2WorkflowAgentState {
    Queued,
    Running,
    Completed,
    Failed,
}

/// Filesystem handles the provider reports when a run launches. The transcript
/// directory is what makes a member's full answer readable: the progress
/// snapshot only carries a capped excerpt, so the server reads the member's
/// transcript from here to fill its thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2WorkflowRunHandles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_dir: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_path: Option<TrimmedNonEmptyString>,
}

/// Workflow-run shape carried by the coordinator subagent. The snapshot is the
/// provider's own replace-in-full view of the run; members are additionally
/// projected as subagents in their own right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SubagentWorkflow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_handles: Option<OrchestrationV2WorkflowRunHandles>,
    pub phases: Vec<OrchestrationV2WorkflowPhase>,
    pub agents: Vec<OrchestrationV2WorkflowAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<NonNegativeInt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2CompletionWake {
    /// Offer a continuation on every terminal (async delegations).
    Always,
    /// Offer only when the parent has no live run (wait-mode delegations,
    /// whose result returns through the blocking tool call).
    SettledOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2SubagentOrigin {
    ProviderNative,
    AppOwned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2SubagentStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// A subagent task: provider-native or T3-owned, with its own child thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2Subagent {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: NodeId,
    pub origin: OrchestrationV2SubagentOrigin,
    pub created_by: OrchestrationV2Actor,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub child_thread_id: Option<ThreadId>,
    pub native_task_ref: Option<OrchestrationV2ProviderRef>,
    pub prompt: String,
    pub title: Option<String>,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    /// Parent-wake policy for app-owned tasks: "always" offers a continuation on
    /// every terminal (async delegations; queue_after_active sequences it behind
    /// a live parent run), "settled_only" offers only when the parent has no
    /// live run (wait-mode delegations, whose result returns through the
    /// blocking tool call). Absent on legacy records; treated as settled_only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_wake: Option<OrchestrationV2CompletionWake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_delivery: Option<OrchestrationV2DelegatedCompletionTaskDelivery>,
    pub status: OrchestrationV2SubagentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// Present only on a workflow coordinator task; absent on ordinary subagents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<OrchestrationV2SubagentWorkflow>,
    pub result: Option<String>,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
}

/// Idle work is resumable, but does not keep a turn or its subscription alive.
pub fn is_orchestration_v2_work_active(status: OrchestrationV2ExecutionNodeStatus) -> bool {
    matches!(
        status,
        OrchestrationV2ExecutionNodeStatus::Pending
            | OrchestrationV2ExecutionNodeStatus::Running
            | OrchestrationV2ExecutionNodeStatus::Waiting
    )
}

// ---------------------------------------------------------------------------
// Checkpoint scopes, provider sessions, provider threads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2CheckpointScopeKind {
    RootRun,
    Subagent,
    Tool,
    ProviderThread,
    Manual,
}

/// A nested checkpoint scope: the root run scope, and nested scopes per
/// subagent / tool / manual capture. Child scopes do not advance the app run
/// count, so one user turn still counts as one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2CheckpointScope {
    pub id: CheckpointScopeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_scope_id: Option<CheckpointScopeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub kind: OrchestrationV2CheckpointScopeKind,
    pub ordinal_within_parent: NonNegativeInt,
    pub advances_app_run_count: bool,
    pub cwd: TrimmedNonEmptyString,
    pub created_at: UtcDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2ProviderSessionStatus {
    Starting,
    Ready,
    Running,
    Waiting,
    Stopped,
    Error,
}

/// A live provider process: one session can serve several provider threads
/// (shared runtimes) and carries the capabilities this build measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderSession {
    pub id: ProviderSessionId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub status: OrchestrationV2ProviderSessionStatus,
    pub cwd: TrimmedNonEmptyString,
    pub model: Option<TrimmedNonEmptyString>,
    pub capabilities: OrchestrationV2ProviderCapabilities,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderSessionDetached {
    pub provider_session_id: ProviderSessionId,
    pub detached_at: UtcDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Kind unions with a decode-only fallback arm
// ---------------------------------------------------------------------------

/// Provider-owned background work that can outlive the root turn (for example
/// a Claude background Bash task). Associated with the provider thread so
/// shared runtimes cannot make an unrelated app thread look busy. Adapters
/// pick the kind; `background_task` is work they cannot name. Rosters
/// persisted before kinds existed carry no `kind` and load as
/// `background_task`, and a kind from a newer server decodes as
/// `background_task` too — the fallback is decode-only: values always
/// encode as the known member they decoded to.
///
/// (Upstream builds this shape with `kindUnionWithFallback`; here the
/// fallback lives in a custom Deserialize, and the known-kind strictness —
/// a known kind whose fields do not decode still fails — comes from
/// decoding each kind strictly.)
#[derive(Debug, Clone, PartialEq)]
pub enum OrchestrationV2PendingBackgroundTask {
    Subagent {
        task_id: TrimmedNonEmptyString,
        /// The work's name: a subagent's title, a command's description, a monitor's.
        description: Option<TrimmedNonEmptyString>,
        /// The subagent's own thread, when it has one.
        child_thread_id: Option<ThreadId>,
    },
    Command {
        task_id: TrimmedNonEmptyString,
        description: Option<TrimmedNonEmptyString>,
    },
    Monitor {
        task_id: TrimmedNonEmptyString,
        description: Option<TrimmedNonEmptyString>,
    },
    BackgroundTask {
        task_id: TrimmedNonEmptyString,
        description: Option<TrimmedNonEmptyString>,
    },
}

impl OrchestrationV2PendingBackgroundTask {
    pub fn task_id(&self) -> &TrimmedNonEmptyString {
        match self {
            Self::Subagent { task_id, .. }
            | Self::Command { task_id, .. }
            | Self::Monitor { task_id, .. }
            | Self::BackgroundTask { task_id, .. } => task_id,
        }
    }

    pub fn description(&self) -> Option<&TrimmedNonEmptyString> {
        match self {
            Self::Subagent { description, .. }
            | Self::Command { description, .. }
            | Self::Monitor { description, .. }
            | Self::BackgroundTask { description, .. } => description.as_ref(),
        }
    }

    fn from_object(mut object: serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        // Upstream's fallback arm takes `kind: optional(String)`: a kind that
        // is present but not a string fails the whole union.
        let kind = match object.get("kind") {
            None => None,
            Some(serde_json::Value::String(kind)) => Some(kind.clone()),
            Some(_) => return Err("a pending background task `kind` must be a string".to_owned()),
        };
        match kind.as_deref() {
            Some("subagent") => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Raw {
                    task_id: TrimmedNonEmptyString,
                    #[serde(default)]
                    description: Option<TrimmedNonEmptyString>,
                    #[serde(default)]
                    child_thread_id: Option<ThreadId>,
                }
                let raw: Raw = serde_json::from_value(serde_json::Value::Object(object))
                    .map_err(|e| e.to_string())?;
                Ok(Self::Subagent {
                    task_id: raw.task_id,
                    description: raw.description,
                    child_thread_id: raw.child_thread_id,
                })
            }
            Some("command") => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Raw {
                    task_id: TrimmedNonEmptyString,
                    #[serde(default)]
                    description: Option<TrimmedNonEmptyString>,
                }
                let raw: Raw = serde_json::from_value(serde_json::Value::Object(object))
                    .map_err(|e| e.to_string())?;
                Ok(Self::Command {
                    task_id: raw.task_id,
                    description: raw.description,
                })
            }
            Some("monitor") => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Raw {
                    task_id: TrimmedNonEmptyString,
                    #[serde(default)]
                    description: Option<TrimmedNonEmptyString>,
                }
                let raw: Raw = serde_json::from_value(serde_json::Value::Object(object))
                    .map_err(|e| e.to_string())?;
                Ok(Self::Monitor {
                    task_id: raw.task_id,
                    description: raw.description,
                })
            }
            // `background_task`, no kind at all (rosters stored before kinds
            // existed), or a kind from a newer server: all decode through the
            // generic member, keeping taskId and description and dropping the
            // rest.
            _ => {
                object.insert("kind".to_owned(), serde_json::json!("background_task"));
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Raw {
                    task_id: TrimmedNonEmptyString,
                    #[serde(default)]
                    description: Option<TrimmedNonEmptyString>,
                }
                let raw: Raw = serde_json::from_value(serde_json::Value::Object(object))
                    .map_err(|e| e.to_string())?;
                Ok(Self::BackgroundTask {
                    task_id: raw.task_id,
                    description: raw.description,
                })
            }
        }
    }
}

impl Serialize for OrchestrationV2PendingBackgroundTask {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut object = serde_json::Map::new();
        object.insert(
            "taskId".to_owned(),
            serde_json::to_value(self.task_id()).map_err(serde::ser::Error::custom)?,
        );
        object.insert(
            "kind".to_owned(),
            match self {
                Self::Subagent { .. } => serde_json::json!("subagent"),
                Self::Command { .. } => serde_json::json!("command"),
                Self::Monitor { .. } => serde_json::json!("monitor"),
                Self::BackgroundTask { .. } => serde_json::json!("background_task"),
            },
        );
        if let Some(description) = self.description() {
            object.insert(
                "description".to_owned(),
                serde_json::to_value(description).map_err(serde::ser::Error::custom)?,
            );
        }
        if let Self::Subagent {
            child_thread_id: Some(child_thread_id),
            ..
        } = self
        {
            object.insert(
                "childThreadId".to_owned(),
                serde_json::to_value(child_thread_id).map_err(serde::ser::Error::custom)?,
            );
        }
        serde_json::Value::Object(object).serialize(s)
    }
}

impl<'de> Deserialize<'de> for OrchestrationV2PendingBackgroundTask {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let object = match value {
            serde_json::Value::Object(object) => object,
            _ => {
                return Err(serde::de::Error::custom(
                    "a pending background task must be an object",
                ));
            }
        };
        Self::from_object(object).map_err(serde::de::Error::custom)
    }
}

/// Provider and adapter metadata that should not overwrite the app thread's title.
/// No `Eq`: `ModelSelection` is not `Eq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderThreadNativeMetadata {
    /// Provider-reported selection for display, separate from the app's saved preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub title: Option<Option<TrimmedNonEmptyString>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub updated_at: Option<Option<TrimmedNonEmptyString>>,
    /// Version 2 scopes provider-derived item ids by provider instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_identity_version: Option<ItemIdentityVersionV2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ProviderGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

/// A provider-native goal set with `/goal` (Codex and Claude). The provider
/// keeps working until it judges the objective met and owns this state; T3
/// mirrors the latest native report. Usage fields are provider-specific.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderGoal {
    pub objective: TrimmedNonEmptyString,
    pub status: OrchestrationV2ProviderGoalStatus,
    /// Codex accounting for the goal across its turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<NonNegativeInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub token_budget: Option<Option<NonNegativeInt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_used_seconds: Option<NonNegativeInt>,
    /// Claude: evaluator checks that found the goal unmet, and the latest reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
}

/// Where a provider thread was forked from inside the provider (upstream
/// inline struct on `OrchestrationV2ProviderThread`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderThreadForkedFrom {
    pub provider_thread_id: ProviderThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<ProviderTurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ProviderThreadStatus {
    NotLoaded,
    Idle,
    Active,
    Archived,
    Closed,
    Error,
}

/// The app's record of a provider conversation, with its pending-work roster
/// and the native metadata the provider reported for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderThread {
    pub id: ProviderThreadId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: Option<ProviderSessionId>,
    pub app_thread_id: Option<ThreadId>,
    pub owner_node_id: Option<NodeId>,
    pub native_thread_ref: Option<OrchestrationV2ProviderRef>,
    pub native_conversation_head_ref: Option<OrchestrationV2ProviderRef>,
    pub status: OrchestrationV2ProviderThreadStatus,
    pub first_run_ordinal: Option<PositiveInt>,
    pub last_run_ordinal: Option<PositiveInt>,
    pub handoff_ids: Vec<ContextHandoffId>,
    pub forked_from: Option<OrchestrationV2ProviderThreadForkedFrom>,
    /// Optional Type so adapters can omit empty rosters; historical JSON decodes to [].
    #[serde(default)]
    pub pending_background_tasks: Vec<OrchestrationV2PendingBackgroundTask>,
    /// `optional(NullOr(..)).pipe(withDecodingDefault(null))`: absent decodes to
    /// null and the key is always encoded.
    #[serde(default)]
    pub context_usage: Option<ThreadTokenUsageSnapshot>,
    #[serde(default)]
    pub native_metadata: Option<OrchestrationV2ProviderThreadNativeMetadata>,
    /// Native goal on this provider thread; rows written before goals decode to null.
    #[serde(default)]
    pub goal: Option<OrchestrationV2ProviderGoal>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

// ---------------------------------------------------------------------------
// Historical messages and context handoffs
// ---------------------------------------------------------------------------

/// One message of a source conversation as a handoff summarizes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2HistoricalMessage {
    pub role: OrchestrationV2HistoricalMessageRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub item_id: TurnItemId,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub status: String,
    pub kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2HistoricalMessageRole {
    User,
    Assistant,
}

/// The source conversation window a handoff covers (upstream inline struct).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ContextHandoffHistory {
    pub messages: Vec<OrchestrationV2HistoricalMessage>,
    pub coverage: String,
    pub omitted_items: NonNegativeInt,
    /// IDs omitted during preparation, before the target's delivery budget is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_item_ids: Option<Vec<TurnItemId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2ContextHandoffDeliveryStatus {
    Pending,
    Injected,
    Inline,
}

/// What a handoff physically delivered into the target conversation
/// (upstream inline struct).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ContextHandoffDelivery {
    pub native_thread_id: String,
    pub status: OrchestrationV2ContextHandoffDeliveryStatus,
    pub item_ids: Vec<TurnItemId>,
    /// Covered by recovery instructions, but not present in native model history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_item_ids: Option<Vec<TurnItemId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ContextHandoffStrategy {
    DeltaSinceTargetLastSeen,
    ForkDeltaSummary,
    FullThreadSummary,
    CheckpointSummary,
    ManualContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2ContextHandoffStatus {
    Pending,
    Ready,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2CoveredRunOrdinals {
    pub from: PositiveInt,
    pub to: PositiveInt,
}

/// A context handoff: the summary (and optional history) delivered into a
/// target provider thread so it can continue another conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ContextHandoff {
    pub id: ContextHandoffId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub transfer_id: Option<Option<ContextTransferId>>,
    pub thread_id: ThreadId,
    pub target_run_id: RunId,
    pub from_provider_thread_ids: Vec<ProviderThreadId>,
    pub to_provider_thread_id: ProviderThreadId,
    pub covered_run_ordinals: OrchestrationV2CoveredRunOrdinals,
    pub strategy: OrchestrationV2ContextHandoffStrategy,
    pub status: OrchestrationV2ContextHandoffStatus,
    pub summary_message_id: Option<MessageId>,
    pub summary_text: String,
    // Optional fields keep existing preview events and projections readable without a migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<OrchestrationV2ContextHandoffHistory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<OrchestrationV2ContextHandoffDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_in_turn_item: Option<TrueLiteral>,
    pub created_by_provider_instance_id: Option<ProviderInstanceId>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

// ---------------------------------------------------------------------------
// Provider turns and runtime requests
// ---------------------------------------------------------------------------

/// Live context usage reported by the provider mid-turn (#8144).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderTurnTokenUsage {
    pub used_tokens: NonNegativeInt,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub max_tokens: Option<Option<NonNegativeInt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<NonNegativeInt>,
    /// Prompt cache lifetime the provider wrote with this report. Clients treat
    /// the cache as cold once `updatedAt` is this old.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_ttl_ms: Option<PositiveInt>,
    /// ISO timestamp of the provider's report; string so wire encoding is stable.
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ProviderTurnStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
}

/// A provider-native turn, mapped onto the app's node for one attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderTurn {
    pub id: ProviderTurnId,
    pub provider_thread_id: ProviderThreadId,
    pub node_id: NodeId,
    pub run_attempt_id: Option<RunAttemptId>,
    pub native_turn_ref: Option<OrchestrationV2ProviderRef>,
    pub ordinal: PositiveInt,
    pub status: OrchestrationV2ProviderTurnStatus,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<OrchestrationV2ProviderTurnTokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_token_usage: Option<TurnTokenUsage>,
}

/// Read side of [`OrchestrationV2ProviderTurn`] that
/// [`latest_provider_turn_for_attempt`] needs (upstream picks the same three
/// fields off a generic `Turn`).
pub trait OrchestrationV2ProviderTurnLike {
    fn run_attempt_id(&self) -> Option<&RunAttemptId>;
    fn ordinal(&self) -> PositiveInt;
}

impl OrchestrationV2ProviderTurnLike for OrchestrationV2ProviderTurn {
    fn run_attempt_id(&self) -> Option<&RunAttemptId> {
        self.run_attempt_id.as_ref()
    }
    fn ordinal(&self) -> PositiveInt {
        self.ordinal
    }
}

/// The provider turn a run attempt is on or ended with. A Codex goal keeps one
/// run open across several native turns, so an attempt's first turn is not
/// always its last.
pub fn latest_provider_turn_for_attempt<'a, T: OrchestrationV2ProviderTurnLike>(
    provider_turns: impl IntoIterator<Item = &'a T>,
    attempt_id: Option<&RunAttemptId>,
) -> Option<&'a T> {
    let mut latest: Option<&'a T> = None;
    for turn in provider_turns {
        // No attempt id filters every turn out (upstream `attemptId == null`).
        if attempt_id.is_none() || turn.run_attempt_id() != attempt_id {
            continue;
        }
        let is_latest = match latest {
            None => true,
            Some(latest) => turn.ordinal() > latest.ordinal(),
        };
        if is_latest {
            latest = Some(turn);
        }
    }
    latest
}

/// A runtime request's kind: a provider approval kind, or one of the three
/// orchestration-native kinds (upstream union of `ProviderRequestKind` with
/// literals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrchestrationV2RuntimeRequestKind {
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "file-read")]
    FileRead,
    #[serde(rename = "file-change")]
    FileChange,
    #[serde(rename = "mcp-elicitation")]
    McpElicitation,
    #[serde(rename = "permission")]
    Permission,
    #[serde(rename = "dynamic_tool_call")]
    DynamicToolCall,
    #[serde(rename = "user_input")]
    UserInput,
    #[serde(rename = "auth_refresh")]
    AuthRefresh,
}

/// How (and whether) a runtime request can still be answered (upstream inline
/// union on `type`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2ResponseCapability {
    /// Answerable while this provider session is live.
    Live {
        provider_session_id: ProviderSessionId,
    },
    /// Answerable only by dispatching a message.
    Message,
    /// No longer answerable; `reason` explains what to do instead.
    NotResumable { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2RuntimeRequestStatus {
    Pending,
    Resolved,
    Expired,
    Cancelled,
}

/// A pending approval / user-input request surfaced by a provider turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2RuntimeRequest {
    pub id: RuntimeRequestId,
    pub node_id: NodeId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_request_ref: Option<OrchestrationV2ProviderRef>,
    pub kind: OrchestrationV2RuntimeRequestKind,
    pub status: OrchestrationV2RuntimeRequestStatus,
    pub response_capability: OrchestrationV2ResponseCapability,
    pub created_at: UtcDateTime,
    pub resolved_at: Option<UtcDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ProviderApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<ProviderUserInputAnswers>,
}

// ---------------------------------------------------------------------------
// Notification sources
// ---------------------------------------------------------------------------

/// What a notification reports on. Several pieces of work of one kind share
/// that kind; mixed or unnamed work is `background_task`.
///
/// Sources are stored and sent in the shape clients before `subagent` and
/// `command` existed decode, since they reject a kind they do not know: a
/// command is `background_command`, and a subagent is `background_task` with
/// `work: "subagent"`, a field those clients ignore. Encoding picks the first
/// member that fits, so those come first. The plain `subagent` and `command`
/// members after them decode values that were already decoded once. An
/// unknown kind (or none at all) decodes to plain `background_task`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestrationV2NotificationSource {
    DelegatedTask {
        task_ids: Vec<NodeId>,
        /// The task's own thread, when the notification reports one task.
        child_thread_id: Option<ThreadId>,
    },
    Subagent {
        /// The subagent's own thread, when the notification reports one subagent.
        child_thread_id: Option<ThreadId>,
    },
    Command,
    Monitor,
    BackgroundTask,
    /// T3 Code itself, such as a restart continuing an interrupted turn.
    System,
}

impl OrchestrationV2NotificationSource {
    fn from_object(object: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        // Upstream's fallback arm takes `kind: optional(String)`: a kind that
        // is present but not a string fails the whole union.
        let kind = match object.get("kind") {
            None => None,
            Some(serde_json::Value::String(kind)) => Some(kind.as_str()),
            Some(_) => return Err("a notification source `kind` must be a string".to_owned()),
        };
        match kind {
            Some("delegated_task") => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Raw {
                    task_ids: Vec<NodeId>,
                    #[serde(default)]
                    child_thread_id: Option<ThreadId>,
                }
                let raw: Raw = serde_json::from_value(serde_json::Value::Object(object.clone()))
                    .map_err(|e| e.to_string())?;
                Ok(Self::DelegatedTask {
                    task_ids: raw.task_ids,
                    child_thread_id: raw.child_thread_id,
                })
            }
            Some("subagent") => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Raw {
                    #[serde(default)]
                    child_thread_id: Option<ThreadId>,
                }
                let raw: Raw = serde_json::from_value(serde_json::Value::Object(object.clone()))
                    .map_err(|e| e.to_string())?;
                Ok(Self::Subagent {
                    child_thread_id: raw.child_thread_id,
                })
            }
            Some("background_task")
                if object.get("work").and_then(|w| w.as_str()) == Some("subagent") =>
            {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Raw {
                    #[serde(default)]
                    child_thread_id: Option<ThreadId>,
                }
                // A broken `childThreadId` fails the `work: "subagent"` member
                // only; the plain `background_task` member after it still
                // matches, as in upstream's member-order decoding.
                Ok(
                    match serde_json::from_value::<Raw>(serde_json::Value::Object(object.clone())) {
                        Ok(raw) => Self::Subagent {
                            child_thread_id: raw.child_thread_id,
                        },
                        Err(_) => Self::BackgroundTask,
                    },
                )
            }
            Some("command") | Some("background_command") => Ok(Self::Command),
            Some("monitor") => Ok(Self::Monitor),
            Some("system") => Ok(Self::System),
            Some("background_task") => Ok(Self::BackgroundTask),
            // A kind from a newer server, or no kind at all: generic
            // background work.
            _ => Ok(Self::BackgroundTask),
        }
    }
}

impl Serialize for OrchestrationV2NotificationSource {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let value = match self {
            Self::DelegatedTask {
                task_ids,
                child_thread_id,
            } => {
                let mut object = serde_json::Map::new();
                object.insert("kind".to_owned(), serde_json::json!("delegated_task"));
                object.insert(
                    "taskIds".to_owned(),
                    serde_json::to_value(task_ids).map_err(S::Error::custom)?,
                );
                if let Some(child_thread_id) = child_thread_id {
                    object.insert(
                        "childThreadId".to_owned(),
                        serde_json::to_value(child_thread_id).map_err(S::Error::custom)?,
                    );
                }
                serde_json::Value::Object(object)
            }
            Self::Subagent { child_thread_id } => {
                let mut object = serde_json::Map::new();
                object.insert("kind".to_owned(), serde_json::json!("background_task"));
                object.insert("work".to_owned(), serde_json::json!("subagent"));
                if let Some(child_thread_id) = child_thread_id {
                    object.insert(
                        "childThreadId".to_owned(),
                        serde_json::to_value(child_thread_id).map_err(S::Error::custom)?,
                    );
                }
                serde_json::Value::Object(object)
            }
            Self::Command => serde_json::json!({ "kind": "background_command" }),
            Self::Monitor => serde_json::json!({ "kind": "monitor" }),
            Self::BackgroundTask => serde_json::json!({ "kind": "background_task" }),
            Self::System => serde_json::json!({ "kind": "system" }),
        };
        value.serialize(s)
    }
}

impl<'de> Deserialize<'de> for OrchestrationV2NotificationSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::Object(object) => {
                Self::from_object(&object).map_err(serde::de::Error::custom)
            }
            _ => Err(serde::de::Error::custom(
                "a notification source must be an object",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2NotificationOutcome {
    Completed,
    Failed,
    Cancelled,
    Updated,
    Unknown,
}

// A notification records an observed event, not whether its payload has reached the agent.
// Provider delivery, wake policy, and agent-facing instructions belong to the backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2Notification {
    pub source: OrchestrationV2NotificationSource,
    // Item status describes this timeline record; outcome describes the reported work.
    pub outcome: OrchestrationV2NotificationOutcome,
    pub summary: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Delegated completion recorded on a conversation message (upstream inline
/// struct, shared with the `message.dispatch` command).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2MessageDelegatedCompletion {
    pub parent_run_id: RunId,
    pub generation: PositiveInt,
    pub task_ids: Vec<NodeId>,
}

/// One message in a thread's timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ConversationMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<OrchestrationV2Notification>,
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    /// The sending agent's thread in this environment, separate from the receiving thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    pub id: MessageId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub role: OrchestrationV2ConversationMessageRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<OrchestrationMessageContext>,
    pub attachments: Vec<ChatAttachment>,
    pub streaming: bool,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<OrchestrationV2MessageDelegatedCompletion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2ConversationMessageRole {
    User,
    Assistant,
    System,
}

// ---------------------------------------------------------------------------
// Plans, user-input questions, checkpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2PlanStepStatus {
    Pending,
    Running,
    Completed,
}

/// One step of a todo-list plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2PlanStep {
    pub id: TrimmedNonEmptyString,
    pub text: TrimmedNonEmptyString,
    pub status: OrchestrationV2PlanStepStatus,
    /// Durable server-owned boundary used to calculate elapsed time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_anchor_at: Option<IsoDateTime>,
    /// Elapsed time for a completed step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<NonNegativeInt>,
}

/// One selectable answer of a structured question (upstream inline struct).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2UserInputQuestionOption {
    pub label: TrimmedNonEmptyString,
    pub description: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// A structured question a provider asked the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2UserInputQuestion {
    pub id: TrimmedNonEmptyString,
    pub header: TrimmedNonEmptyString,
    pub question: TrimmedNonEmptyString,
    pub options: Vec<OrchestrationV2UserInputQuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_custom_answer: Option<bool>,
    /// Editable initial text. Answers preserve whitespace and allow an empty string when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2PlanArtifactStatus {
    Draft,
    Active,
    Completed,
    Superseded,
}

/// A plan artifact: the proposed plan or the todo list a run maintains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2PlanArtifact {
    ProposedPlan {
        id: PlanId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: NodeId,
        status: OrchestrationV2PlanArtifactStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail_in_turn_item: Option<TrueLiteral>,
        markdown: String,
    },
    TodoList {
        id: PlanId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: NodeId,
        status: OrchestrationV2PlanArtifactStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail_in_turn_item: Option<TrueLiteral>,
        steps: Vec<OrchestrationV2PlanStep>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2CheckpointFileSummary {
    pub path: TrimmedNonEmptyString,
    pub kind: TrimmedNonEmptyString,
    pub additions: NonNegativeInt,
    pub deletions: NonNegativeInt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2CheckpointStatus {
    Ready,
    Missing,
    Error,
    Stale,
}

/// A captured checkpoint: a hidden git ref plus the file summary that lets the
/// timeline render a diff chip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2Checkpoint {
    pub id: CheckpointId,
    pub thread_id: ThreadId,
    pub scope_id: CheckpointScopeId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_checkpoint_id: Option<CheckpointId>,
    pub ordinal_within_scope: NonNegativeInt,
    pub app_run_ordinal: Option<PositiveInt>,
    #[serde(rename = "ref")]
    pub ref_: CheckpointRef,
    pub status: OrchestrationV2CheckpointStatus,
    pub files: Vec<OrchestrationV2CheckpointFileSummary>,
    pub captured_at: UtcDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2CheckpointRollbackRequest {
    pub scope_id: CheckpointScopeId,
    pub checkpoint_id: CheckpointId,
    pub requested_at: UtcDateTime,
}

/// `OrchestrationV2CheckpointUnavailableError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2CheckpointUnavailableError {
    #[serde(rename = "OrchestrationV2CheckpointUnavailableError")]
    #[error("Rollback target {target} is unavailable for thread {thread_id}.")]
    OrchestrationV2CheckpointUnavailableError { thread_id: ThreadId, target: String },
}

// ---------------------------------------------------------------------------
// Turn items
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2TurnItemStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// One structured file operation reported by a provider inside a file_change item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2FileChangeDetail {
    pub operation: TrimmedNonEmptyString,
    pub path: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ProviderFailureClass {
    UsageLimit,
    ProviderError,
    TransportError,
    PermissionError,
    ValidationError,
    Unknown,
}

/// A provider failure's `message`: trimmed non-empty, at most 4,096 chars
/// (upstream `TrimmedNonEmptyString.check(Schema.isMaxLength(4_096))`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OrchestrationV2ProviderFailureMessage(TrimmedNonEmptyString);

impl OrchestrationV2ProviderFailureMessage {
    pub fn new(value: impl AsRef<str>) -> Result<Self, crate::base_schemas::ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        // `Schema.isMaxLength` counts UTF-16 code units, like JS `length`.
        if value.as_str().encode_utf16().count() > 4_096 {
            return Err(crate::base_schemas::ContractError {
                expected: "a failure message of at most 4,096 chars",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for OrchestrationV2ProviderFailureMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for OrchestrationV2ProviderFailureMessage {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for OrchestrationV2ProviderFailureMessage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// A provider failure's `code`: trimmed non-empty, at most 128 chars
/// (upstream `TrimmedNonEmptyString.check(Schema.isMaxLength(128))`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OrchestrationV2ProviderFailureCode(TrimmedNonEmptyString);

impl OrchestrationV2ProviderFailureCode {
    pub fn new(value: impl AsRef<str>) -> Result<Self, crate::base_schemas::ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        // `Schema.isMaxLength` counts UTF-16 code units, like JS `length`.
        if value.as_str().encode_utf16().count() > 128 {
            return Err(crate::base_schemas::ContractError {
                expected: "a failure code of at most 128 chars",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for OrchestrationV2ProviderFailureCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for OrchestrationV2ProviderFailureCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for OrchestrationV2ProviderFailureCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Transport-safe failure information suitable for persistence and display.
/// Producers must redact credentials before constructing this value. The
/// schema bounds every provider-controlled string as a second line of defense.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderFailure {
    pub class: OrchestrationV2ProviderFailureClass,
    pub message: OrchestrationV2ProviderFailureMessage,
    pub code: Option<OrchestrationV2ProviderFailureCode>,
    pub retryable: Option<bool>,
    /// Reported reset time; absent when the provider cannot name one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub reset_at: Option<Option<IsoDateTime>>,
}

/// Provider-reported retry progress. Some providers expose all fields (Claude),
/// while others only expose `willRetry` and encode counters in display text
/// (Codex), so the protocol-specific values remain nullable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProviderRetry {
    pub attempt: PositiveInt,
    pub max_attempts: Option<PositiveInt>,
    pub retry_delay_ms: Option<NonNegativeInt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2ProviderThreadDisposition {
    Reusable,
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2UserMessageInputIntent {
    TurnStart,
    QueuedTurn,
    Steer,
    PromotedQueuedToSteer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2FileSearchResult {
    pub file_name: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2WebSearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2SecretRequestStatus {
    Pending,
    Saved,
    Declined,
    Cancelled,
}

/// The handoff strategy a handoff turn item labels the timeline with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2HandoffStrategy {
    DeltaSinceTargetLastSeen,
    ForkDeltaSummary,
    FullThreadSummary,
    CheckpointSummary,
    ManualContext,
}

/// The unit of a turn's timeline. One item per thing the agent did or that
/// happened, tagged by `type`.
///
/// Member order matches upstream's union (order matters for the
/// forward-compatible array codec's documentation, not for dispatch — serde
/// picks by `type`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2TurnItem {
    Notification {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        source: OrchestrationV2NotificationSource,
        outcome: OrchestrationV2NotificationOutcome,
        summary: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    UserMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        created_by: OrchestrationV2Actor,
        creation_source: OrchestrationV2CreationSource,
        message_id: MessageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_task_id: Option<ScheduledTaskId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_thread_id: Option<ThreadId>,
        input_intent: OrchestrationV2UserMessageInputIntent,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<OrchestrationMessageContext>,
        attachments: Vec<ChatAttachment>,
    },
    AssistantMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        message_id: MessageId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachments: Option<Vec<ChatAttachment>>,
        streaming: bool,
    },
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        text: String,
        streaming: bool,
    },
    ProposedPlan {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        plan_id: PlanId,
        markdown: String,
        streaming: bool,
    },
    TodoList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        plan_id: PlanId,
        steps: Vec<OrchestrationV2PlanStep>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
    },
    UserInputRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        request_id: RuntimeRequestId,
        questions: Vec<OrchestrationV2UserInputQuestion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question_answer: Option<UserInputAttachmentAnswerPayload>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_mode: Option<MessageResponseMode>,
    },
    FileChange {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        file_name: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additions: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deletions: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_str: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_str: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_str: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changes: Option<Vec<OrchestrationV2FileChangeDetail>>,
    },
    CommandExecution {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Set on the wire when output was withheld; fetch it with getTurnItem.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_omitted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_indicates_failure: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
    },
    FileSearch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        results: Option<Vec<OrchestrationV2FileSearchResult>>,
    },
    WebSearch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patterns: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        results: Option<Vec<OrchestrationV2WebSearchResult>>,
    },
    ApprovalRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        request_id: RuntimeRequestId,
        request_kind: ProviderRequestKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        /// App requesting access, for mcp-elicitation approvals (#8058).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_name: Option<String>,
        /// Approval choices advertised by the provider (#8058).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<Vec<ProviderApprovalOption>>,
    },
    Checkpoint {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        checkpoint_id: CheckpointId,
        scope_id: CheckpointScopeId,
        files: Vec<OrchestrationV2CheckpointFileSummary>,
    },
    RunInterruptRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        message: String,
    },
    RunInterruptResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        message: String,
    },
    SystemNotice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        message: String,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        failure: OrchestrationV2ProviderFailure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<OrchestrationV2ProviderRetry>,
    },
    Compaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_token_count: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_token_count: Option<NonNegativeInt>,
    },
    Handoff {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        context_handoff_id: ContextHandoffId,
        from_provider_thread_ids: Vec<ProviderThreadId>,
        to_provider_thread_id: ProviderThreadId,
        from_provider_instance_ids: Vec<ProviderInstanceId>,
        to_provider_instance_id: ProviderInstanceId,
        // Model selections active on the covered source runs and the target
        // model, so timelines can label the handoff by model rather than by
        // provider instance id. Absent on items persisted before these fields.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_model_selections: Option<Vec<ModelSelection>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_model: Option<String>,
        strategy: OrchestrationV2HandoffStrategy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Fork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        // Fork overrides the base `providerThreadId` (NullOr) with
        // Schema.optional, so the key is omitted rather than nulled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        source: OrchestrationV2ForkSource,
        target_thread_id: ThreadId,
    },
    ThreadCreated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        target_thread_id: ThreadId,
        target_run_id: Option<RunId>,
        target_provider_instance_id: ProviderInstanceId,
        target_model: TrimmedNonEmptyString,
    },
    /// A secret an agent asked the user for. The value never passes through
    /// orchestration: the item carries only what was asked and how it was
    /// answered.
    SecretRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        label: TrimmedNonEmptyString,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        secret_status: OrchestrationV2SecretRequestStatus,
    },
    Subagent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        subagent_id: NodeId,
        origin: OrchestrationV2SubagentOrigin,
        driver: ProviderDriverKind,
        provider_instance_id: ProviderInstanceId,
        child_thread_id: Option<ThreadId>,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<String>,
        result: Option<String>,
    },
    DynamicTool {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_non_execution_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_surface: Option<ToolActivitySurface>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_icon: Option<ToolActivityIcon>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_source: Option<ToolActivitySource>,
        id: TurnItemId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        provider_thread_id: Option<ProviderThreadId>,
        provider_turn_id: Option<ProviderTurnId>,
        native_item_ref: Option<OrchestrationV2ProviderRef>,
        parent_item_id: Option<TurnItemId>,
        ordinal: NonNegativeInt,
        status: OrchestrationV2TurnItemStatus,
        title: Option<String>,
        started_at: Option<UtcDateTime>,
        completed_at: Option<UtcDateTime>,
        updated_at: UtcDateTime,
        tool_name: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewed_image_path: Option<TrimmedNonEmptyString>,
        /// `Schema.Unknown`: a JSON-stringified `undefined` drops the key, so
        /// an absent `input` decodes (to null).
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
        /// Set on the wire when output was withheld; fetch it with getTurnItem.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_omitted: Option<bool>,
    },
}

impl TaggedUnion for OrchestrationV2TurnItem {
    const TAG: &'static str = "type";
    fn known_tags() -> &'static [&'static str] {
        &[
            "notification",
            "user_message",
            "assistant_message",
            "reasoning",
            "proposed_plan",
            "todo_list",
            "user_input_request",
            "file_change",
            "command_execution",
            "file_search",
            "web_search",
            "approval_request",
            "checkpoint",
            "run_interrupt_request",
            "run_interrupt_result",
            "system_notice",
            "error",
            "compaction",
            "handoff",
            "fork",
            "thread_created",
            "secret_request",
            "subagent",
            "dynamic_tool",
        ]
    }
}

/// `hasUnknownUnionTag(TurnItem.members, "type")`: whether a raw turn item
/// payload carries a `type` this build does not know.
pub fn is_unknown_turn_item(payload: &serde_json::Value) -> bool {
    crate::base_schemas::has_unknown_union_tag(
        payload,
        OrchestrationV2TurnItem::TAG,
        OrchestrationV2TurnItem::known_tags(),
    )
}

/// A timeline row: a turn item at a position, from this thread or inherited
/// from one it was forked from (or synthesized by the server).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ProjectedTurnItem {
    pub position: NonNegativeInt,
    pub visibility: OrchestrationV2ProjectedTurnItemVisibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    pub item: OrchestrationV2TurnItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2ProjectedTurnItemVisibility {
    Local,
    Inherited,
    Synthetic,
}

/// Serde codec for arrays of projected rows whose nested item may be of a
/// type this build does not know: unknown rows are dropped instead of
/// failing the thread, and a known type that does not decode still fails
/// (upstream `projectedTurnItemArray`).
pub mod projected_turn_item_array {
    use super::*;

    pub fn serialize<S: Serializer>(
        rows: &[OrchestrationV2ProjectedTurnItem],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        rows.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<OrchestrationV2ProjectedTurnItem>, D::Error> {
        let raw = Vec::<serde_json::Value>::deserialize(d)?;
        let mut rows = Vec::with_capacity(raw.len());
        for value in raw {
            if let Some(item) = value.get("item")
                && is_unknown_turn_item(item)
            {
                continue;
            }
            rows.push(serde_json::from_value(value).map_err(serde::de::Error::custom)?);
        }
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Raw provider events and domain events
// ---------------------------------------------------------------------------

/// One raw protocol frame, as observed on the wire to or from a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2RawProviderEvent {
    pub id: RawEventId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: ProviderSessionId,
    pub sequence: PositiveInt,
    pub direction: OrchestrationV2RawProviderEventDirection,
    pub message_kind: OrchestrationV2RawProviderEventMessageKind,
    pub method: Option<TrimmedNonEmptyString>,
    pub json_rpc_id: Option<OrchestrationV2JsonRpcId>,
    /// `Schema.Unknown`: an absent key decodes (to null).
    #[serde(default)]
    pub payload: serde_json::Value,
    pub observed_at: UtcDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2RawProviderEventDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2RawProviderEventMessageKind {
    Request,
    Response,
    Notification,
    Error,
}

/// `Schema.NullOr(Schema.Union([Schema.String, Schema.Number]))`: a JSON-RPC
/// id is whatever the provider used — string or number (upstream inline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OrchestrationV2JsonRpcId {
    Str(String),
    /// `serde_json::Number`, not `f64`: an integer id must re-encode as `5`,
    /// not `5.0` (see PORTING.md Deviations).
    Number(serde_json::Number),
}

/// One committed event of the orchestrator's event log, tagged by `type`.
/// Every event carries its producing entity as `payload`.
///
/// The `thread.*` group shares one payload shape (upstream declares the 20
/// literals as one union member); here they are one variant each so the
/// literal round-trips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum OrchestrationV2DomainEvent {
    #[serde(rename = "thread.created")]
    ThreadCreated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.archived")]
    ThreadArchived {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.unarchived")]
    ThreadUnarchived {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.deleted")]
    ThreadDeleted {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.settled")]
    ThreadSettled {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.unsettled")]
    ThreadUnsettled {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.snoozed")]
    ThreadSnoozed {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.unsnoozed")]
    ThreadUnsnoozed {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.pinned")]
    ThreadPinned {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.auto-settle-set")]
    ThreadAutoSettleSet {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.unpinned")]
    ThreadUnpinned {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.pin-reordered")]
    ThreadPinReordered {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.active-reordered")]
    ThreadActiveReordered {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.visited")]
    ThreadVisited {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.marked-unread")]
    ThreadMarkedUnread {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.metadata-updated")]
    ThreadMetadataUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.pull-request-synced")]
    ThreadPullRequestSynced {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.runtime-mode-updated")]
    ThreadRuntimeModeUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.interaction-mode-updated")]
    ThreadInteractionModeUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.model-selection-updated")]
    ThreadModelSelectionUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.provider-switched")]
    ThreadProviderSwitched {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2AppThread,
    },
    #[serde(rename = "thread.pull-request-link-updated")]
    ThreadPullRequestLinkUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: ThreadPullRequestLink,
    },
    #[serde(rename = "run.created")]
    RunCreated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2Run,
    },
    #[serde(rename = "run.updated")]
    RunUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2Run,
    },
    #[serde(rename = "run.background-work-cancelled")]
    RunBackgroundWorkCancelled {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2RunBackgroundWorkCancelled,
    },
    #[serde(rename = "run-attempt.created")]
    RunAttemptCreated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2RunAttempt,
    },
    #[serde(rename = "run-attempt.updated")]
    RunAttemptUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2RunAttempt,
    },
    #[serde(rename = "node.updated")]
    NodeUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ExecutionNode,
    },
    #[serde(rename = "subagent.updated")]
    SubagentUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2Subagent,
    },
    #[serde(rename = "provider-session.attached")]
    ProviderSessionAttached {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ProviderSession,
    },
    #[serde(rename = "provider-session.updated")]
    ProviderSessionUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ProviderSession,
    },
    #[serde(rename = "provider-session.detached")]
    ProviderSessionDetached {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ProviderSessionDetached,
    },
    #[serde(rename = "provider-thread.updated")]
    ProviderThreadUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ProviderThread,
    },
    #[serde(rename = "provider-turn.updated")]
    ProviderTurnUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ProviderTurn,
    },
    #[serde(rename = "runtime-request.updated")]
    RuntimeRequestUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2RuntimeRequest,
    },
    #[serde(rename = "message.updated")]
    MessageUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ConversationMessage,
    },
    #[serde(rename = "turn-item.updated")]
    TurnItemUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2TurnItem,
    },
    #[serde(rename = "plan.updated")]
    PlanUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2PlanArtifact,
    },
    #[serde(rename = "checkpoint-scope.created")]
    CheckpointScopeCreated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2CheckpointScope,
    },
    #[serde(rename = "checkpoint.captured")]
    CheckpointCaptured {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2Checkpoint,
    },
    #[serde(rename = "checkpoint.rollback-requested")]
    CheckpointRollbackRequested {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2CheckpointRollbackRequest,
    },
    #[serde(rename = "context-handoff.updated")]
    ContextHandoffUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ContextHandoff,
    },
    #[serde(rename = "context-transfer.created")]
    ContextTransferCreated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ContextTransfer,
    },
    #[serde(rename = "context-transfer.updated")]
    ContextTransferUpdated {
        id: EventId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<ProviderInstanceId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_event_id: Option<RawEventId>,
        occurred_at: UtcDateTime,
        payload: OrchestrationV2ContextTransfer,
    },
}

/// Every `type` this build knows on [`OrchestrationV2DomainEvent`]
/// (upstream `knownDomainEventTypes`).
pub const KNOWN_ORCHESTRATION_V2_EVENT_TYPES: &[&str] = &[
    "thread.created",
    "thread.archived",
    "thread.unarchived",
    "thread.deleted",
    "thread.settled",
    "thread.unsettled",
    "thread.snoozed",
    "thread.unsnoozed",
    "thread.pinned",
    "thread.auto-settle-set",
    "thread.unpinned",
    "thread.pin-reordered",
    "thread.active-reordered",
    "thread.visited",
    "thread.marked-unread",
    "thread.metadata-updated",
    "thread.pull-request-synced",
    "thread.runtime-mode-updated",
    "thread.interaction-mode-updated",
    "thread.model-selection-updated",
    "thread.provider-switched",
    "thread.pull-request-link-updated",
    "run.created",
    "run.updated",
    "run.background-work-cancelled",
    "run-attempt.created",
    "run-attempt.updated",
    "node.updated",
    "subagent.updated",
    "provider-session.attached",
    "provider-session.updated",
    "provider-session.detached",
    "provider-thread.updated",
    "provider-turn.updated",
    "runtime-request.updated",
    "message.updated",
    "turn-item.updated",
    "plan.updated",
    "checkpoint-scope.created",
    "checkpoint.captured",
    "checkpoint.rollback-requested",
    "context-handoff.updated",
    "context-transfer.created",
    "context-transfer.updated",
];

/// Whether `type` is an [`OrchestrationV2DomainEvent`] discriminant in this build.
pub fn is_known_orchestration_v2_event_type(type_: &str) -> bool {
    KNOWN_ORCHESTRATION_V2_EVENT_TYPES.contains(&type_)
}

// ---------------------------------------------------------------------------
// Projections and shells
// ---------------------------------------------------------------------------

/// The full read model of one thread: every entity the orchestrator recorded.
/// `turnItems` and `visibleTurnItems` decode forward-compatibly — items of a
/// type this build does not know are dropped (upstream `turnItemArray` /
/// `projectedTurnItemArray`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadProjection {
    pub thread: OrchestrationV2AppThread,
    pub runs: Vec<OrchestrationV2Run>,
    pub attempts: Vec<OrchestrationV2RunAttempt>,
    pub nodes: Vec<OrchestrationV2ExecutionNode>,
    pub subagents: Vec<OrchestrationV2Subagent>,
    pub provider_sessions: Vec<OrchestrationV2ProviderSession>,
    pub provider_threads: Vec<OrchestrationV2ProviderThread>,
    pub provider_turns: Vec<OrchestrationV2ProviderTurn>,
    pub runtime_requests: Vec<OrchestrationV2RuntimeRequest>,
    pub messages: Vec<OrchestrationV2ConversationMessage>,
    pub plans: Vec<OrchestrationV2PlanArtifact>,
    #[serde(with = "crate::base_schemas::forward_compatible_union_array")]
    pub turn_items: Vec<OrchestrationV2TurnItem>,
    pub checkpoint_scopes: Vec<OrchestrationV2CheckpointScope>,
    pub checkpoints: Vec<OrchestrationV2Checkpoint>,
    pub context_handoffs: Vec<OrchestrationV2ContextHandoff>,
    pub context_transfers: Vec<OrchestrationV2ContextTransfer>,
    #[serde(with = "projected_turn_item_array")]
    pub visible_turn_items: Vec<OrchestrationV2ProjectedTurnItem>,
    pub updated_at: UtcDateTime,
}

/// The status a sidebar row shows: idle, or the thread's latest run's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ShellThreadStatus {
    Idle,
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

/// The pending runtime request a sidebar row surfaces (its cheapest fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2PendingRuntimeRequestSummary {
    pub id: RuntimeRequestId,
    pub kind: OrchestrationV2RuntimeRequestKind,
    pub created_at: UtcDateTime,
}

/// The latest visible message a sidebar row surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2LatestVisibleMessageSummary {
    pub id: MessageId,
    pub role: OrchestrationV2ConversationMessageRole,
    pub text: String,
    pub updated_at: UtcDateTime,
}

/// What the latest run is actively doing, for a sidebar status pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationV2ActivityRunStatus {
    Preparing,
    Starting,
    Running,
    Waiting,
}

/// A sidebar thread row: everything a list needs without the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadShell {
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<TrimmedNonEmptyString>,
    pub worktree_path: Option<TrimmedNonEmptyString>,
    /// Pull request the user linked to this thread (#8160).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub linked_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<Vec<ThreadPullRequestLink>>,
    /// Pull request discovered from the thread's current branch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub branch_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    pub lineage: OrchestrationV2AppThreadLineage,
    pub forked_from: Option<OrchestrationV2ForkSource>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<OrchestrationV2ThreadHistoryOrigin>,
    pub latest_run_id: Option<RunId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub latest_run_requested_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub latest_run_started_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub latest_run_completed_at: Option<Option<UtcDateTime>>,
    pub active_run_id: Option<RunId>,
    /// [`orchestration_v2_run_work_started_at`] of the activity-owning run: a
    /// wake keeps the start of the work it continues; request time while preparing.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub activity_run_started_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub activity_run_status: Option<Option<OrchestrationV2ActivityRunStatus>>,
    pub status: OrchestrationV2ShellThreadStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub last_error: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub last_error_class: Option<Option<OrchestrationV2ProviderFailureClass>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub usage_limit_reset_at: Option<Option<IsoDateTime>>,
    pub pending_runtime_request: Option<OrchestrationV2PendingRuntimeRequestSummary>,
    pub latest_visible_message: Option<OrchestrationV2LatestVisibleMessageSummary>,
    pub latest_user_message_at: Option<UtcDateTime>,
    /// The last message the user wrote. Wakes and agent messages also use the
    /// user role, so they move latestUserMessageAt but not this.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub latest_user_authored_message_at: Option<Option<UtcDateTime>>,
    pub has_actionable_proposed_plan: bool,
    // Normalized post-settlement background work for sidebar Waiting pills.
    // Empty when the latest root run is still active or no pending work remains.
    #[serde(default)]
    pub pending_background_tasks: Vec<OrchestrationV2PendingBackgroundTask>,
    // Distinct provider instances that have owned a root provider thread here,
    // in first-use order, so lists can show where a handed-off thread has been.
    // Omitted by servers that predate it; decodes to [].
    #[serde(default)]
    pub provider_instance_history: Vec<ProviderInstanceId>,
    /// Native goal on the active provider thread; omitted by servers without goals.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub goal: Option<Option<OrchestrationV2ProviderGoal>>,
    pub item_count: NonNegativeInt,
    pub visible_item_count: NonNegativeInt,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub archived_at: Option<UtcDateTime>,
    pub settled_override: Option<OrchestrationV2SettleOverride>,
    pub settled_at: Option<UtcDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub unsettled_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub snoozed_until: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub snoozed_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub limit_recovery: Option<Option<OrchestrationV2LimitRecovery>>,
    /// Omitted by servers that predate thread pinning.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub pinned_at: Option<Option<UtcDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub auto_settle_disabled_at: Option<Option<UtcDateTime>>,
    /// Slot in the user-arranged pinned order; omitted by pre-reorder servers.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub pin_order_key: Option<Option<TrimmedNonEmptyString>>,
    /// Slot in the user-arranged active order; omitted by pre-reorder servers.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub active_order_key: Option<Option<TrimmedNonEmptyString>>,
    /// Omitted by servers that predate server-side visited tracking; clients
    /// fall back to their local visited state when the field is absent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub last_visited_at: Option<Option<UtcDateTime>>,
    /// In-flight title regeneration marker; null/absent when no request is pending.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub title_regeneration: Option<Option<OrchestrationV2TitleRegeneration>>,
    pub deleted_at: Option<UtcDateTime>,
}

/// The shell of the active thread list plus the archived one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadShellSnapshot {
    pub schema_version: PositiveInt,
    pub snapshot_sequence: NonNegativeInt,
    pub threads: Vec<OrchestrationV2ThreadShell>,
    pub archived_threads: Vec<OrchestrationV2ThreadShell>,
}

/// The full shell: threads plus projects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ShellSnapshot {
    pub schema_version: PositiveInt,
    pub snapshot_sequence: NonNegativeInt,
    pub threads: Vec<OrchestrationV2ThreadShell>,
    pub archived_threads: Vec<OrchestrationV2ThreadShell>,
    pub projects: Vec<crate::orchestration_project::OrchestrationProjectShell>,
}

/// Which list a shell stream row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2ShellLocation {
    Active,
    Archive,
}

/// One shell subscription frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
#[allow(clippy::large_enum_variant)] // wire enum mirrors upstream; boxing would leak into every consumer
pub enum OrchestrationV2ShellStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot")]
    Snapshot {
        snapshot: OrchestrationV2ShellSnapshot,
        /// Workspace roots whose repository-identity resolution completed for
        /// this enrichment refresh. Omitted on authoritative HTTP/initial
        /// WebSocket snapshots. Older clients ignore the field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated {
        sequence: NonNegativeInt,
        project: crate::orchestration_project::OrchestrationProjectShell,
    },
    #[serde(rename = "project.removed")]
    ProjectRemoved {
        sequence: NonNegativeInt,
        project_id: ProjectId,
    },
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        sequence: NonNegativeInt,
        location: OrchestrationV2ShellLocation,
        thread: OrchestrationV2ThreadShell,
    },
    #[serde(rename = "thread.removed")]
    ThreadRemoved {
        sequence: NonNegativeInt,
        location: OrchestrationV2ShellLocation,
        thread_id: ThreadId,
    },
}

/// One committed event, as stored in the event log and replayed to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2StoredEvent {
    pub sequence: NonNegativeInt,
    pub command_id: Option<CommandId>,
    pub event: OrchestrationV2DomainEvent,
}

// ---------------------------------------------------------------------------
// JSON variants
// ---------------------------------------------------------------------------
// Upstream re-declares each entity with `Schema.DateTimeUtcFromString` for
// the JSON codec. With serde_json as the only codec those variants are
// wire-identical to the runtime schemas, so they are aliases.

pub type OrchestrationV2AppThreadJson = OrchestrationV2AppThread;
pub type OrchestrationV2RunJson = OrchestrationV2Run;
pub type OrchestrationV2RunAttemptJson = OrchestrationV2RunAttempt;
pub type OrchestrationV2ExecutionNodeJson = OrchestrationV2ExecutionNode;
pub type OrchestrationV2SubagentJson = OrchestrationV2Subagent;
pub type OrchestrationV2CheckpointScopeJson = OrchestrationV2CheckpointScope;
pub type OrchestrationV2ProviderSessionJson = OrchestrationV2ProviderSession;
pub type OrchestrationV2ProviderSessionDetachedJson = OrchestrationV2ProviderSessionDetached;
pub type OrchestrationV2ProviderThreadJson = OrchestrationV2ProviderThread;
pub type OrchestrationV2ContextHandoffJson = OrchestrationV2ContextHandoff;
pub type OrchestrationV2ContextTransferJson = OrchestrationV2ContextTransfer;
pub type OrchestrationV2ProviderTurnJson = OrchestrationV2ProviderTurn;
pub type OrchestrationV2RuntimeRequestJson = OrchestrationV2RuntimeRequest;
pub type OrchestrationV2ConversationMessageJson = OrchestrationV2ConversationMessage;
pub type OrchestrationV2CheckpointJson = OrchestrationV2Checkpoint;
pub type OrchestrationV2CheckpointRollbackRequestJson = OrchestrationV2CheckpointRollbackRequest;
pub type OrchestrationV2TurnItemJson = OrchestrationV2TurnItem;
pub type OrchestrationV2ProjectedTurnItemJson = OrchestrationV2ProjectedTurnItem;
pub type OrchestrationV2ThreadProjectionJson = OrchestrationV2ThreadProjection;
pub type OrchestrationV2PendingRuntimeRequestSummaryJson =
    OrchestrationV2PendingRuntimeRequestSummary;
pub type OrchestrationV2LatestVisibleMessageSummaryJson =
    OrchestrationV2LatestVisibleMessageSummary;
pub type OrchestrationV2ThreadShellJson = OrchestrationV2ThreadShell;
pub type OrchestrationV2ShellSnapshotJson = OrchestrationV2ShellSnapshot;
pub type OrchestrationV2RawProviderEventJson = OrchestrationV2RawProviderEvent;
pub type OrchestrationV2DomainEventJson = OrchestrationV2DomainEvent;
pub type OrchestrationV2StoredEventJson = OrchestrationV2StoredEvent;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// A native thread ref the `thread.create` command imports wholesale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ImportedNativeThreadRef {
    pub driver: ProviderDriverKind,
    pub native_id: TrimmedNonEmptyString,
    pub strength: StrongNativeRefStrength,
}

/// An imported native thread: its ref plus any metadata the provider handed over.
/// No `Eq`: the metadata carries a `ModelSelection`, which is not `Eq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ImportedNativeThread {
    #[serde(rename = "ref")]
    pub ref_: OrchestrationV2ImportedNativeThreadRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<OrchestrationV2ProviderThreadNativeMetadata>,
}

/// How a message.dispatch is delivered to the thread (upstream inline union
/// on `type`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2DispatchMode {
    /// Create the run (and its workspace) but hold it until prepared-run.release.
    DeferStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_strategy: Option<OrchestrationV2ThreadLaunchWorkspaceStrategy>,
    },
    /// Steer the active run instead of starting a new one.
    SteerActive {
        target_run_id: RunId,
    },
    /// Interrupt the active run and start over with this message.
    RestartActive {
        target_run_id: RunId,
    },
    /// Wait for the active run to finish, then start.
    QueueAfterActive,
    StartImmediately,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2DeliveryIntent {
    Auto,
    Steer,
    Restart,
}

/// What the sync expects the thread to look like before it applies the host's
/// answer (upstream inline struct on `thread.pull-request.sync`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2PullRequestSyncExpected {
    pub workspace_root: TrimmedNonEmptyString,
    pub branch: Option<TrimmedNonEmptyString>,
    pub worktree_path: Option<TrimmedNonEmptyString>,
    pub linked_pull_request: Option<ThreadLinkedPullRequest>,
    pub branch_pull_request: Option<ThreadLinkedPullRequest>,
}

/// Link-first payload of `thread.pull-request.watch` (upstream inline struct).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2PullRequestWatchLink {
    pub url: TrimmedNonEmptyString,
    pub source: ThreadPullRequestLinkSource,
}

/// Wake payload of `thread.pull-request-watch.sync` (upstream inline struct).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2WakeMessage {
    pub message_id: MessageId,
    pub text: String,
    pub notification: OrchestrationV2Notification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2PreparedRunPhase {
    Worktree,
    Setup,
}

/// Confirmed provider outcome of a stopped subagent's background work
/// (upstream inline struct on `thread.background-work.settle`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2BackgroundWorkSettleOutcome {
    pub status: OrchestrationV2BackgroundWorkSettleOutcomeStatus,
    pub completed_at: UtcDateTime,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrchestrationV2BackgroundWorkSettleOutcomeStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2BackgroundWorkSettleSubagent {
    pub id: NodeId,
    pub stopped_at: UtcDateTime,
    pub outcome: OrchestrationV2BackgroundWorkSettleOutcome,
}

/// Everything the orchestrator accepts from a client, tagged by `type`.
/// Field order inside each variant follows upstream's struct declarations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum OrchestrationV2Command {
    #[serde(rename = "thread.create")]
    ThreadCreate {
        created_by: OrchestrationV2Actor,
        creation_source: OrchestrationV2CreationSource,
        command_id: CommandId,
        thread_id: ThreadId,
        project_id: ProjectId,
        title: TrimmedNonEmptyString,
        model_selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: ProviderInteractionMode,
        branch: Option<TrimmedNonEmptyString>,
        worktree_path: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        imported_native_thread: Option<OrchestrationV2ImportedNativeThread>,
    },
    #[serde(rename = "thread.archive")]
    ThreadArchive {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "thread.unarchive")]
    ThreadUnarchive {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "thread.delete")]
    ThreadDelete {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "thread.settle")]
    ThreadSettle {
        command_id: CommandId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settled_at: Option<UtcDateTime>,
    },
    /// Server-internal settlement (#8600): dispatched by the settlement sweep,
    /// never by clients. Rejected when the thread changed after `snapshotAt` or
    /// carries any explicit settled override, so automatic settlement can never
    /// race a user action or clobber an explicit un-settle.
    #[serde(rename = "thread.auto-settle")]
    ThreadAutoSettle {
        command_id: CommandId,
        thread_id: ThreadId,
        snapshot_at: UtcDateTime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settled_at: Option<UtcDateTime>,
    },
    #[serde(rename = "thread.unsettle")]
    ThreadUnsettle {
        command_id: CommandId,
        thread_id: ThreadId,
        reason: UserUnsettleReason,
    },
    #[serde(rename = "thread.snooze")]
    ThreadSnooze {
        command_id: CommandId,
        thread_id: ThreadId,
        snoozed_until: IsoDateTime,
    },
    #[serde(rename = "thread.unsnooze")]
    ThreadUnsnooze {
        command_id: CommandId,
        thread_id: ThreadId,
        reason: UserUnsettleReason,
    },
    #[serde(rename = "thread.auto-settle.set")]
    ThreadAutoSettleSet {
        command_id: CommandId,
        thread_id: ThreadId,
        enabled: bool,
    },
    #[serde(rename = "thread.pin")]
    ThreadPin {
        command_id: CommandId,
        thread_id: ThreadId,
        // Initial slot in the user-arranged pinned order (see thread.pin.reorder).
        // Optional: clients on pre-reorder servers omit it, and the pinned block
        // falls back to creation order for keyless threads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order_key: Option<TrimmedNonEmptyString>,
    },
    #[serde(rename = "thread.unpin")]
    ThreadUnpin {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "thread.pin.reorder")]
    ThreadPinReorder {
        command_id: CommandId,
        thread_id: ThreadId,
        // Fractional-index key: pinned threads sort lexicographically by these
        // keys, so a drag writes one key to one thread — neighbors are never
        // touched. Clients compute a key that sorts between the dropped
        // position's neighbors.
        order_key: TrimmedNonEmptyString,
    },
    #[serde(rename = "thread.active.reorder")]
    ThreadActiveReorder {
        command_id: CommandId,
        thread_id: ThreadId,
        order_key: TrimmedNonEmptyString,
    },
    #[serde(rename = "thread.visit")]
    ThreadVisit {
        command_id: CommandId,
        thread_id: ThreadId,
        /// Watermark of the thread state the viewer has seen (typically the
        /// shell's updatedAt). The server keeps the maximum of the stored and
        /// supplied values, so replays and out-of-order deliveries cannot move
        /// it backwards.
        visited_at: IsoDateTime,
    },
    #[serde(rename = "thread.mark-unread")]
    ThreadMarkUnread {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "thread.metadata.update")]
    ThreadMetadataUpdate {
        command_id: CommandId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<TrimmedNonEmptyString>,
        /// Kick off (true) or abandon (false) an async title regeneration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regenerate_title: Option<bool>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        branch: Option<Option<TrimmedNonEmptyString>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        worktree_path: Option<Option<TrimmedNonEmptyString>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        expected_worktree_path: Option<Option<TrimmedNonEmptyString>>,
        /// Reject unless no message or run has landed on this thread.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_empty: Option<bool>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        limit_recovery: Option<Option<OrchestrationV2LimitRecoveryUpdate>>,
        /// Link (object) or unlink (null) a pull request (#8160); absent leaves it unchanged.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        linked_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    },
    #[serde(rename = "thread.pull-request.link")]
    ThreadPullRequestLink {
        command_id: CommandId,
        thread_id: ThreadId,
        host: TrimmedNonEmptyString,
        repository: TrimmedNonEmptyString,
        number: PositiveInt,
        url: TrimmedNonEmptyString,
        source: ThreadPullRequestLinkSource,
    },
    #[serde(rename = "thread.pull-request.unlink")]
    ThreadPullRequestUnlink {
        command_id: CommandId,
        thread_id: ThreadId,
        host: TrimmedNonEmptyString,
        repository: TrimmedNonEmptyString,
        number: PositiveInt,
    },
    #[serde(rename = "thread.pull-request-link.sync")]
    ThreadPullRequestLinkSync {
        command_id: CommandId,
        thread_id: ThreadId,
        host: TrimmedNonEmptyString,
        repository: TrimmedNonEmptyString,
        number: PositiveInt,
        snapshot: ThreadPullRequestSnapshot,
        stack: Option<ThreadPullRequestStack>,
    },
    /// Start or stop the server watching a linked pull request for this thread's agent.
    #[serde(rename = "thread.pull-request.watch")]
    ThreadPullRequestWatch {
        command_id: CommandId,
        thread_id: ThreadId,
        host: TrimmedNonEmptyString,
        repository: TrimmedNonEmptyString,
        number: PositiveInt,
        watching: bool,
        /// Links the pull request first when starting a watch on one the thread has not linked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<OrchestrationV2PullRequestWatchLink>,
    },
    #[serde(rename = "thread.pull-request.sync")]
    ThreadPullRequestSync {
        command_id: CommandId,
        thread_id: ThreadId,
        project_id: ProjectId,
        snapshot_sequence: NonNegativeInt,
        expected: OrchestrationV2PullRequestSyncExpected,
        branch_pull_request: Option<ThreadLinkedPullRequest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        linked_pull_request: Option<ThreadLinkedPullRequest>,
    },
    #[serde(rename = "thread.title.regeneration.complete")]
    ThreadTitleRegenerationComplete {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: CommandId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<TrimmedNonEmptyString>,
    },
    #[serde(rename = "thread.runtime-mode.set")]
    ThreadRuntimeModeSet {
        command_id: CommandId,
        thread_id: ThreadId,
        runtime_mode: RuntimeMode,
    },
    #[serde(rename = "thread.interaction-mode.set")]
    ThreadInteractionModeSet {
        command_id: CommandId,
        thread_id: ThreadId,
        interaction_mode: ProviderInteractionMode,
    },
    #[serde(rename = "thread.model-selection.set")]
    ThreadModelSelectionSet {
        command_id: CommandId,
        thread_id: ThreadId,
        model_selection: ModelSelection,
    },
    #[serde(rename = "provider-session.detach")]
    ProviderSessionDetach {
        command_id: CommandId,
        thread_id: ThreadId,
        provider_session_id: ProviderSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "message.dispatch")]
    MessageDispatch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notification: Option<OrchestrationV2Notification>,
        created_by: OrchestrationV2Actor,
        creation_source: OrchestrationV2CreationSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_task_id: Option<ScheduledTaskId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_thread_id: Option<ThreadId>,
        command_id: CommandId,
        thread_id: ThreadId,
        message_id: MessageId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<OrchestrationMessageContext>,
        attachments: Vec<ChatAttachment>,
        /// Seed the temporary title and generate a durable replacement for the first message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title_seed: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_selection: Option<ModelSelection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_plan_ref: Option<OrchestrationV2SourcePlanRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restart_continuation_of_run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage_limit_continuation_of_run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manual_continuation_of_run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage_limit_recovery_request_id: Option<CommandId>,
        /// Resolve untargeted delivery against the server's serialized thread state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_intent: Option<OrchestrationV2DeliveryIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delegated_completion: Option<OrchestrationV2MessageDelegatedCompletion>,
        dispatch_mode: OrchestrationV2DispatchMode,
    },
    #[serde(rename = "prepared-run.release")]
    PreparedRunRelease {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    },
    /// Provider acceptance of a mailbox delivery; distinct from the agent reading its result.
    #[serde(rename = "notification.delivery.accept")]
    NotificationDeliveryAccept {
        command_id: CommandId,
        thread_id: ThreadId,
        message_id: MessageId,
    },
    #[serde(rename = "prepared-run.progress")]
    PreparedRunProgress {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        phase: OrchestrationV2PreparedRunPhase,
    },
    #[serde(rename = "prepared-run.fail")]
    PreparedRunFail {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        failure: OrchestrationV2ProviderFailure,
    },
    /// Puts a run whose workspace preparation failed back into preparation.
    #[serde(rename = "prepared-run.retry")]
    PreparedRunRetry {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    },
    /// Stops one provider-native child while its owning run keeps working.
    #[serde(rename = "subagent.stop")]
    SubagentStop {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        subagent_id: NodeId,
    },
    #[serde(rename = "run.interrupt")]
    RunInterrupt {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Set by the Stop button. Stop also holds the queue, ends the thread's
        /// pull request watches, and stops every delegated task under the thread.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold_queue: Option<bool>,
    },
    #[serde(rename = "queued-message.promote-to-steer")]
    QueuedMessagePromoteToSteer {
        command_id: CommandId,
        thread_id: ThreadId,
        queued_run_id: RunId,
        target_run_id: RunId,
    },
    #[serde(rename = "queue.resume")]
    QueueResume {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[serde(rename = "queued-run.reorder")]
    QueuedRunReorder {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        before_run_id: Option<RunId>,
    },
    #[serde(rename = "queued-run.cancel")]
    QueuedRunCancel {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    },
    #[serde(rename = "queued-run.edit")]
    QueuedRunEdit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<OrchestrationMessageContext>,
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        text: String,
        // Full replacement list. Absent = leave the message's attachments as-is,
        // so pre-attachment clients editing text keep the original attachments.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachments: Option<Vec<ChatAttachment>>,
    },
    #[serde(rename = "runtime-request.respond")]
    RuntimeRequestRespond {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<ProviderApprovalDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<ProviderUserInputAnswers>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachments_by_question_id: Option<UserInputAttachments>,
    },
    #[serde(rename = "thread.user-input.dismiss")]
    ThreadUserInputDismiss {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
    },
    #[serde(rename = "checkpoint.rollback")]
    CheckpointRollback {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restore_files: Option<bool>,
        command_id: CommandId,
        thread_id: ThreadId,
        scope_id: CheckpointScopeId,
        checkpoint_id: CheckpointId,
    },
    #[serde(rename = "thread.fork")]
    ThreadFork {
        created_by: OrchestrationV2Actor,
        creation_source: OrchestrationV2CreationSource,
        command_id: CommandId,
        source_thread_id: ThreadId,
        target_thread_id: ThreadId,
        source_point: OrchestrationV2ThreadForkSourcePoint,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_at: Option<UtcDateTime>,
    },
    #[serde(rename = "thread.merge_back")]
    ThreadMergeBack {
        created_by: OrchestrationV2Actor,
        creation_source: OrchestrationV2CreationSource,
        command_id: CommandId,
        source_thread_id: ThreadId,
        target_thread_id: ThreadId,
        source_point: OrchestrationV2ThreadForkSourcePoint,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_at: Option<UtcDateTime>,
    },
    #[serde(rename = "delegated_task.request")]
    DelegatedTaskRequest {
        created_by: OrchestrationV2Actor,
        creation_source: OrchestrationV2CreationSource,
        command_id: CommandId,
        parent_thread_id: ThreadId,
        parent_run_id: RunId,
        parent_node_id: NodeId,
        task: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<TrimmedNonEmptyString>,
        model_selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: ProviderInteractionMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_strategy: Option<OrchestrationV2ThreadLaunchWorkspaceStrategy>,
        // Omitted behaves as "settled_only" (no wake while the parent has a live
        // run); producers that want fire-and-forget wakes must set "always".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completion_wake: Option<OrchestrationV2CompletionWake>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_at: Option<UtcDateTime>,
    },
    #[serde(rename = "delegated_task.wake-policy")]
    DelegatedTaskWakePolicy {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
        completion_wake: OrchestrationV2CompletionWake,
    },
    #[serde(rename = "delegated_task.completion-delivery.acknowledge")]
    DelegatedTaskCompletionDeliveryAcknowledge {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
        observed_by_run_id: Option<RunId>,
    },
    #[serde(rename = "delegated_task.completion-delivery.dispose")]
    DelegatedTaskCompletionDeliveryDispose {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
    },
    #[serde(rename = "thread.created.record")]
    ThreadCreatedRecord {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        parent_run_id: RunId,
        parent_node_id: NodeId,
        target_thread_id: ThreadId,
        target_run_id: Option<RunId>,
    },
    #[serde(rename = "provider.switch")]
    ProviderSwitch {
        command_id: CommandId,
        thread_id: ThreadId,
        model_selection: ModelSelection,
    },
}

/// Commands only the server dispatches. They stay out of
/// [`OrchestrationV2Command`], the `dispatchCommand` payload, so no client can
/// send them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[allow(clippy::large_enum_variant)] // wire enum mirrors upstream; boxing would leak into every consumer
pub enum OrchestrationV2InternalCommand {
    /// Records what a pull request watch saw, and wakes the agent in the same
    /// transaction when `wake` is set. Rejected once the watch started at
    /// `startedAt` has ended, and a wake is rejected on a settled or archived
    /// thread, so a read that raced either changes nothing.
    #[serde(rename = "thread.pull-request-watch.sync")]
    ThreadPullRequestWatchSync {
        command_id: CommandId,
        thread_id: ThreadId,
        host: TrimmedNonEmptyString,
        repository: TrimmedNonEmptyString,
        number: PositiveInt,
        started_at: IsoDateTime,
        /// The watch to record, or null to end it.
        watch: Option<ThreadPullRequestWatch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake: Option<OrchestrationV2WakeMessage>,
    },
    /// Records that the provider rollback `requestId` failed for good.
    #[serde(rename = "checkpoint.rollback.fail")]
    CheckpointRollbackFail {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: CommandId,
        message: TrimmedNonEmptyString,
    },
    /// Follows a Stop once its provider returned: background work the settled
    /// thread still shows on that provider thread is no longer reported by any
    /// provider process, so it is marked interrupted. Only the stopped turn's
    /// run and older runs are settled; a later run's work is its own. A
    /// subagent target limits repair to that task and its runless child work,
    /// using a confirmed provider outcome.
    #[serde(rename = "thread.background-work.settle")]
    ThreadBackgroundWorkSettle {
        command_id: CommandId,
        thread_id: ThreadId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<OrchestrationV2BackgroundWorkSettleSubagent>,
    },
    /// Stop for one thread, whatever it is doing: interrupts its running turn,
    /// holds its queue, ends its pull request watches, and drops pending
    /// delegated-task wakes. Nothing to stop is an accepted no-op. Delegated
    /// tasks under the thread get their own `thread.stop`.
    #[serde(rename = "thread.stop")]
    ThreadStop {
        command_id: CommandId,
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Records or updates a secret an agent asked the user for. Internal so no
    /// client can mark a request saved without the value being stored.
    #[serde(rename = "secret_request.record")]
    SecretRequestRecord {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        node_id: NodeId,
        turn_item_id: TurnItemId,
        label: TrimmedNonEmptyString,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        secret_status: OrchestrationV2SecretRequestStatus,
    },
}

/// Everything the server's orchestrator accepts: client commands plus internal ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OrchestrationV2ServerCommand {
    Client(OrchestrationV2Command),
    Internal(OrchestrationV2InternalCommand),
}

// ---------------------------------------------------------------------------
// Snapshot / subscription payloads
// ---------------------------------------------------------------------------

/// The archived-threads shell snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ArchivedShellSnapshot {
    pub schema_version: PositiveInt,
    pub snapshot_sequence: NonNegativeInt,
    pub projects: Vec<crate::orchestration_project::OrchestrationProjectShell>,
    pub threads: Vec<OrchestrationV2ThreadShell>,
}

/// One archived-shell subscription frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
#[allow(clippy::large_enum_variant)] // wire enum mirrors upstream; boxing would leak into every consumer
pub enum OrchestrationV2ArchivedShellStreamItem {
    #[serde(rename = "snapshot")]
    Snapshot {
        snapshot: OrchestrationV2ArchivedShellSnapshot,
    },
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        sequence: NonNegativeInt,
        thread: OrchestrationV2ThreadShell,
    },
    #[serde(rename = "thread.removed")]
    ThreadRemoved {
        sequence: NonNegativeInt,
        thread_id: ThreadId,
    },
}

/// The initial message a launch sends before its first run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadLaunchInitialMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<OrchestrationMessageContext>,
    pub attachments: Vec<ChatAttachment>,
}

/// Input of a thread launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadLaunchInput {
    pub command_id: CommandId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_source: Option<OrchestrationV2CreationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_existing_thread: Option<bool>,
    pub project_id: ProjectId,
    pub title: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate_title: Option<bool>,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub workspace_strategy: OrchestrationV2ThreadLaunchWorkspaceStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<OrchestrationV2ThreadLaunchInitialMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadLaunchResult {
    pub thread_id: ThreadId,
    pub projection: OrchestrationV2ThreadProjection,
    pub resumed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2DispatchCommandResult {
    pub sequence: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2GetThreadProjectionInput {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SubscribeShellInput {
    /// When provided, the server skips the initial full shell snapshot and
    /// instead replays shell events after this sequence before streaming live
    /// events. Clients that already hold a cached (or HTTP-loaded) shell
    /// snapshot pass its sequence here so the subscription resumes without
    /// re-sending the entire projects/threads list (overlapping events are
    /// deduped by sequence on the client).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<NonNegativeInt>,
    /// Requests a marker between initial catch-up and live delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_completion_marker: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SubscribeThreadInput {
    pub thread_id: ThreadId,
    /// When provided, the server skips the initial snapshot frame and instead
    /// replays events after this sequence before streaming live events.
    /// Clients that load the snapshot over HTTP pass the snapshot's sequence
    /// here so the live subscription resumes without a gap (overlapping
    /// events are deduped by sequence on the client).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<NonNegativeInt>,
    /// Requests a marker between initial catch-up and live delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_completion_marker: Option<bool>,
    /// Allows snapshot fallbacks to contain a bounded, pageable history window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_bounded_snapshot: Option<bool>,
    /// Allows bounded snapshot fallbacks to omit `projection.turnItems`
    /// entries that repeat local visible rows. See `turnItemsOmitLocalVisible`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_compact_turn_items: Option<bool>,
}

/// A cached full projection with progressive-history metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadDetailSnapshot {
    pub snapshot_sequence: NonNegativeInt,
    pub projection: OrchestrationV2ThreadProjection,
    /// Progressive history metadata. Omitted on full-projection caches and
    /// older clients. When present with hasMoreHistory/cursor, the projection
    /// is a bounded window and must not be treated as a complete timeline.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub history_cursor: Option<Option<TrimmedNonEmptyString>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_more_history: Option<bool>,
    /// Max local turn ordinal from the authoritative full projection at
    /// snapshot time. Optional for backward-compatible warm cache entries.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub latest_local_turn_ordinal: Option<Option<NonNegativeInt>>,
}

/// Progressive cold open: full control-plane projection arrays with only a
/// recent window of timeline rows. `historyCursor` is opaque; clients must
/// not parse it. Resume live events with `afterSequence = snapshotSequence`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadBoundedSnapshot {
    pub snapshot_sequence: NonNegativeInt,
    pub projection: OrchestrationV2ThreadProjection,
    pub history_cursor: Option<TrimmedNonEmptyString>,
    pub has_more_history: bool,
    /// Max local turn ordinal over the authoritative full projection
    /// turnItems. Clients use this as a partial-timeline watermark when the
    /// bounded window has no local rows (inherited-only).
    pub latest_local_turn_ordinal: Option<NonNegativeInt>,
    /// True when complete turns or required live control state exceed the usual byte budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_budget_exceeded: Option<bool>,
    /// Set only for clients that opted in: `projection.turnItems` omits the
    /// items of local visible rows, which lead the full list. Clients must
    /// restore them with `boundedSnapshotProjection` before using the
    /// projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_items_omit_local_visible: Option<TrueLiteral>,
}

/// Older timeline page for progressive history. Rows are chronological.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadHistoryPage {
    pub snapshot_sequence: NonNegativeInt,
    #[serde(with = "projected_turn_item_array")]
    pub items: Vec<OrchestrationV2ProjectedTurnItem>,
    pub next_cursor: Option<TrimmedNonEmptyString>,
    pub has_more_history: bool,
}

// ---------------------------------------------------------------------------
// Thread stream items (with the skippable unknown-event arm)
// ---------------------------------------------------------------------------

/// One thread-subscription frame.
///
/// The fourth arm folds upstream's `OrchestrationV2UnknownThreadStreamEvent`:
/// a thread event whose type this build does not know, or a
/// `turn-item.updated` carrying a turn item type it does not know. Newer
/// servers add both; older clients decode them to this case and skip them,
/// still advancing their resume cursor, instead of failing the whole
/// subscription. A known type whose payload does not decode still fails.
/// Decode-only: servers never send it (serializing it is an error).
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // wire enum mirrors upstream; boxing would leak into every consumer
pub enum OrchestrationV2ThreadStreamItem {
    /// `kind: "synchronized"`.
    Synchronized,
    /// `kind: "snapshot"`: the frame, plus progressive-history metadata that
    /// is present on bounded socket fallbacks and absent on legacy-compatible
    /// full snapshots.
    Snapshot {
        snapshot_sequence: NonNegativeInt,
        projection: OrchestrationV2ThreadProjection,
        history_cursor: Option<Option<TrimmedNonEmptyString>>,
        has_more_history: Option<bool>,
        latest_local_turn_ordinal: Option<Option<NonNegativeInt>>,
        payload_budget_exceeded: Option<bool>,
        /// Same meaning as on [`OrchestrationV2ThreadBoundedSnapshot`].
        turn_items_omit_local_visible: Option<TrueLiteral>,
    },
    /// `kind: "event"`.
    Event {
        sequence: NonNegativeInt,
        event: OrchestrationV2DomainEvent,
    },
    /// Decoded from `kind: "event"` whose event this build does not know.
    UnknownEvent {
        sequence: NonNegativeInt,
        event_type: String,
    },
}

/// Derived codec for the three encodable arms of the thread stream.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
#[allow(clippy::large_enum_variant)] // wire enum mirrors upstream; boxing would leak into every consumer
enum ThreadStreamItemWire {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot")]
    Snapshot {
        snapshot_sequence: NonNegativeInt,
        projection: OrchestrationV2ThreadProjection,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        history_cursor: Option<Option<TrimmedNonEmptyString>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_more_history: Option<bool>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        latest_local_turn_ordinal: Option<Option<NonNegativeInt>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload_budget_exceeded: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_items_omit_local_visible: Option<TrueLiteral>,
    },
    #[serde(rename = "event")]
    Event {
        sequence: NonNegativeInt,
        event: OrchestrationV2DomainEvent,
    },
}

impl Serialize for OrchestrationV2ThreadStreamItem {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::UnknownEvent { .. } => Err(serde::ser::Error::custom(
                "Servers never send unknown thread events.",
            )),
            Self::Synchronized => ThreadStreamItemWire::Synchronized.serialize(s),
            Self::Snapshot {
                snapshot_sequence,
                projection,
                history_cursor,
                has_more_history,
                latest_local_turn_ordinal,
                payload_budget_exceeded,
                turn_items_omit_local_visible,
            } => ThreadStreamItemWire::Snapshot {
                snapshot_sequence: *snapshot_sequence,
                projection: projection.clone(),
                history_cursor: history_cursor.clone(),
                has_more_history: *has_more_history,
                latest_local_turn_ordinal: *latest_local_turn_ordinal,
                payload_budget_exceeded: *payload_budget_exceeded,
                turn_items_omit_local_visible: *turn_items_omit_local_visible,
            }
            .serialize(s),
            Self::Event { sequence, event } => ThreadStreamItemWire::Event {
                sequence: *sequence,
                event: event.clone(),
            }
            .serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for OrchestrationV2ThreadStreamItem {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match value.get("kind").and_then(|kind| kind.as_str()) {
            Some("event") => {
                // The unknown arm matches only when the event type is not
                // known to this build, or it is a turn-item.updated whose
                // item type is not known; a known event with a broken
                // payload is a real defect and falls through to the strict
                // decode, which fails.
                let event_type = value
                    .get("event")
                    .and_then(|event| event.get("type"))
                    .and_then(|type_| type_.as_str());
                let unknown = event_type.is_some_and(|event_type| {
                    !is_known_orchestration_v2_event_type(event_type)
                        || (event_type == "turn-item.updated"
                            && value
                                .get("event")
                                .and_then(|event| event.get("payload"))
                                .is_some_and(is_unknown_turn_item))
                });
                if unknown {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Raw {
                        sequence: NonNegativeInt,
                        event: EventFrame,
                    }
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct EventFrame {
                        r#type: String,
                    }
                    let raw: Raw =
                        serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                    return Ok(Self::UnknownEvent {
                        sequence: raw.sequence,
                        event_type: raw.event.r#type,
                    });
                }
                match serde_json::from_value::<ThreadStreamItemWire>(value) {
                    Ok(ThreadStreamItemWire::Event { sequence, event }) => {
                        Ok(Self::Event { sequence, event })
                    }
                    Ok(_) => unreachable!("kind tag selects the event arm"),
                    Err(error) => Err(serde::de::Error::custom(error)),
                }
            }
            _ => match serde_json::from_value::<ThreadStreamItemWire>(value) {
                Ok(ThreadStreamItemWire::Synchronized) => Ok(Self::Synchronized),
                Ok(ThreadStreamItemWire::Snapshot {
                    snapshot_sequence,
                    projection,
                    history_cursor,
                    has_more_history,
                    latest_local_turn_ordinal,
                    payload_budget_exceeded,
                    turn_items_omit_local_visible,
                }) => Ok(Self::Snapshot {
                    snapshot_sequence,
                    projection,
                    history_cursor,
                    has_more_history,
                    latest_local_turn_ordinal,
                    payload_budget_exceeded,
                    turn_items_omit_local_visible,
                }),
                Ok(ThreadStreamItemWire::Event { .. }) => unreachable!("kind tag is checked above"),
                Err(error) => Err(serde::de::Error::custom(error)),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `OrchestrationV2DispatchCommandError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2DispatchCommandError {
    #[serde(rename = "OrchestrationV2DispatchCommandError")]
    #[error("{message}")]
    OrchestrationV2DispatchCommandError {
        command_id: CommandId,
        command_type: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

/// `OrchestrationV2GetThreadProjectionError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2GetThreadProjectionError {
    #[serde(rename = "OrchestrationV2GetThreadProjectionError")]
    #[error("{message}")]
    OrchestrationV2GetThreadProjectionError {
        thread_id: ThreadId,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

/// `OrchestrationV2GetShellSnapshotError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2GetShellSnapshotError {
    #[serde(rename = "OrchestrationV2GetShellSnapshotError")]
    #[error("{message}")]
    OrchestrationV2GetShellSnapshotError {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

/// `OrchestrationV2ThreadLaunchError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2ThreadLaunchError {
    #[serde(rename = "OrchestrationV2ThreadLaunchError")]
    #[error("{message}")]
    OrchestrationV2ThreadLaunchError {
        command_id: CommandId,
        project_id: ProjectId,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2StopWorkflowInput {
    pub thread_id: ThreadId,
    pub subagent_id: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrchestrationV2StopWorkflowReason {
    NotRunning,
    Unsupported,
    Unavailable,
    StopFailed,
}

/// `OrchestrationV2StopWorkflowError` (Schema.TaggedError). The message is
/// derived from `reason`, so Display is written by hand (rule: manual
/// Display when the message is derived from fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2StopWorkflowError {
    #[serde(rename = "OrchestrationV2StopWorkflowError")]
    OrchestrationV2StopWorkflowError {
        thread_id: ThreadId,
        subagent_id: NodeId,
        reason: OrchestrationV2StopWorkflowReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

impl std::fmt::Display for OrchestrationV2StopWorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OrchestrationV2StopWorkflowError { reason, .. } => f.write_str(match reason {
                OrchestrationV2StopWorkflowReason::NotRunning => {
                    "This workflow is no longer running."
                }
                OrchestrationV2StopWorkflowReason::Unsupported => {
                    "This provider cannot stop a workflow."
                }
                OrchestrationV2StopWorkflowReason::Unavailable => {
                    "This workflow has no live provider task."
                }
                OrchestrationV2StopWorkflowReason::StopFailed => "Failed to stop the workflow.",
            }),
        }
    }
}

impl std::error::Error for OrchestrationV2StopWorkflowError {}

/// The union of the orchestration RPC errors (upstream `OrchestrationV2RpcError`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(untagged)]
pub enum OrchestrationV2RpcError {
    #[error(transparent)]
    DispatchCommand(OrchestrationV2DispatchCommandError),
    #[error(transparent)]
    GetThreadProjection(OrchestrationV2GetThreadProjectionError),
    #[error(transparent)]
    GetShellSnapshot(OrchestrationV2GetShellSnapshotError),
    #[error(transparent)]
    ThreadLaunch(OrchestrationV2ThreadLaunchError),
    #[error(transparent)]
    StopWorkflow(OrchestrationV2StopWorkflowError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2GetWorkflowScriptInput {
    pub thread_id: ThreadId,
    /// Absolute path from the workflow's runHandles.scriptPath. The server
    /// re-derives containment; the client value is a hint, never trusted.
    pub script_path: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2GetWorkflowScriptResult {
    pub script_path: TrimmedNonEmptyString,
    pub contents: String,
    pub truncated: bool,
}

/// Shared by every read of a file the Claude harness persisted for a workflow
/// run — the script and each member's transcript. One vocabulary because both
/// reads apply identical containment rules.
const WORKFLOW_FILE_ERROR_MESSAGES: [(OrchestrationV2WorkflowFileErrorReason, &str); 8] = [
    (
        OrchestrationV2WorkflowFileErrorReason::InvalidPath,
        "Workflow files must be absolute paths with the expected extension.",
    ),
    (
        OrchestrationV2WorkflowFileErrorReason::RootUnavailable,
        "Workflow file root unavailable.",
    ),
    (
        OrchestrationV2WorkflowFileErrorReason::NotFound,
        "Workflow file not found.",
    ),
    (
        OrchestrationV2WorkflowFileErrorReason::OutsideRoot,
        "Path is outside the workflow files root.",
    ),
    (
        OrchestrationV2WorkflowFileErrorReason::WrongExtension,
        "Resolved file has the wrong extension.",
    ),
    (
        OrchestrationV2WorkflowFileErrorReason::NotRegularFile,
        "Workflow file is not a regular file.",
    ),
    (
        OrchestrationV2WorkflowFileErrorReason::ChangedDuringRead,
        "Workflow file changed between resolution and open.",
    ),
    (
        OrchestrationV2WorkflowFileErrorReason::ReadFailed,
        "Workflow file read failed.",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrchestrationV2WorkflowFileErrorReason {
    InvalidPath,
    RootUnavailable,
    NotFound,
    OutsideRoot,
    WrongExtension,
    NotRegularFile,
    ChangedDuringRead,
    ReadFailed,
}

/// `OrchestrationWorkflowFileError` (Schema.TaggedError); its message is the
/// `WORKFLOW_FILE_ERROR_MESSAGES` entry for `reason`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationWorkflowFileError {
    #[serde(rename = "OrchestrationWorkflowFileError")]
    OrchestrationWorkflowFileError {
        reason: OrchestrationV2WorkflowFileErrorReason,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

impl std::fmt::Display for OrchestrationWorkflowFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OrchestrationWorkflowFileError { reason, .. } => f.write_str(
                WORKFLOW_FILE_ERROR_MESSAGES
                    .iter()
                    .find(|(candidate, _)| candidate == reason)
                    .map(|(_, message)| *message)
                    .unwrap_or("Workflow file read failed."),
            ),
        }
    }
}

impl std::error::Error for OrchestrationWorkflowFileError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2GetTurnItemInput {
    pub thread_id: ThreadId,
    pub item_id: TurnItemId,
    /// The item updatedAt the client last saw. Only keys the client cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// One persisted turn item with its full, size-bounded input and output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2GetTurnItemResult {
    pub item: Option<OrchestrationV2TurnItem>,
}

const WORKFLOW_SCRIPT_ERROR_MESSAGES: [(OrchestrationV2WorkflowScriptErrorReason, &str); 8] = [
    (
        OrchestrationV2WorkflowScriptErrorReason::InvalidPath,
        "Workflow scripts must be absolute .js paths.",
    ),
    (
        OrchestrationV2WorkflowScriptErrorReason::RootUnavailable,
        "Script root unavailable.",
    ),
    (
        OrchestrationV2WorkflowScriptErrorReason::NotFound,
        "Script not found.",
    ),
    (
        OrchestrationV2WorkflowScriptErrorReason::OutsideRoot,
        "Script path is outside the workflow scripts root.",
    ),
    (
        OrchestrationV2WorkflowScriptErrorReason::NotJs,
        "Resolved script is not a .js file.",
    ),
    (
        OrchestrationV2WorkflowScriptErrorReason::NotRegularFile,
        "Script is not a regular file.",
    ),
    (
        OrchestrationV2WorkflowScriptErrorReason::ChangedDuringRead,
        "Script changed between resolution and open.",
    ),
    (
        OrchestrationV2WorkflowScriptErrorReason::ReadFailed,
        "Script read failed.",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrchestrationV2WorkflowScriptErrorReason {
    InvalidPath,
    RootUnavailable,
    NotFound,
    OutsideRoot,
    NotJs,
    NotRegularFile,
    ChangedDuringRead,
    ReadFailed,
}

/// `OrchestrationGetWorkflowScriptError` (Schema.TaggedError); its message is
/// the `WORKFLOW_SCRIPT_ERROR_MESSAGES` entry for `reason`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationGetWorkflowScriptError {
    #[serde(rename = "OrchestrationGetWorkflowScriptError")]
    OrchestrationGetWorkflowScriptError {
        reason: OrchestrationV2WorkflowScriptErrorReason,
        script_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

impl std::fmt::Display for OrchestrationGetWorkflowScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OrchestrationGetWorkflowScriptError { reason, .. } => f.write_str(
                WORKFLOW_SCRIPT_ERROR_MESSAGES
                    .iter()
                    .find(|(candidate, _)| candidate == reason)
                    .map(|(_, message)| *message)
                    .unwrap_or("Script read failed."),
            ),
        }
    }
}

impl std::error::Error for OrchestrationGetWorkflowScriptError {}

// ---------------------------------------------------------------------------
// Thread search
// ---------------------------------------------------------------------------

/// `TrimmedNonEmptyString.check(Schema.isMaxLength(200))`: the search query.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OrchestrationV2SearchQuery(TrimmedNonEmptyString);

impl OrchestrationV2SearchQuery {
    pub fn new(value: impl AsRef<str>) -> Result<Self, crate::base_schemas::ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        // `Schema.isMaxLength` counts UTF-16 code units (JS string length).
        if value.as_str().encode_utf16().count() > 200 {
            return Err(crate::base_schemas::ContractError {
                expected: "a search query of at most 200 chars",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for OrchestrationV2SearchQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for OrchestrationV2SearchQuery {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for OrchestrationV2SearchQuery {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Fails unless `count` (a UTF-16 length, like JS `.length`) is within `max`.
fn check_max_length<E: serde::de::Error>(what: &str, count: usize, max: usize) -> Result<(), E> {
    if count > max {
        return Err(E::custom(format!(
            "{what} must have at most {max} elements/characters, got {count}"
        )));
    }
    Ok(())
}

/// `Schema.String.check(Schema.isMaxLength(200))`.
fn deserialize_string_max_200<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    check_max_length("a string", value.encode_utf16().count(), 200)?;
    Ok(value)
}

/// `Schema.optional(Schema.String.check(Schema.isMaxLength(200)))`.
fn deserialize_optional_string_max_200<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    deserialize_string_max_200(d).map(Some)
}

/// `Schema.optionalKey(Schema.Array(skill).check(Schema.isMaxLength(1_000)))`.
fn deserialize_skills_max_1000<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<OrchestrationV2SearchThreadSkill>>, D::Error> {
    let skills = Vec::<OrchestrationV2SearchThreadSkill>::deserialize(d)?;
    check_max_length("skills", skills.len(), 1_000)?;
    Ok(Some(skills))
}

/// `Schema.optionalKey(Schema.Array(navigation).check(Schema.isMaxLength(17)))`.
fn deserialize_navigation_max_17<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<OrchestrationV2ThreadFindNavigation>>, D::Error> {
    let navigation = Vec::<OrchestrationV2ThreadFindNavigation>::deserialize(d)?;
    check_max_length("navigation", navigation.len(), 17)?;
    Ok(Some(navigation))
}

/// One skill a search is narrowed to, by the label this client displays
/// (including custom display names). `name` and `displayName` are at most 200
/// UTF-16 code units, checked on decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SearchThreadSkill {
    #[serde(deserialize_with = "deserialize_string_max_200")]
    pub name: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_string_max_200"
    )]
    pub display_name: Option<String>,
}

/// Where navigation continues from: a match's identity plus its occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SearchThreadStart {
    pub entry_id: TrimmedNonEmptyString,
    pub occurrence: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SearchThreadInput {
    pub thread_id: ThreadId,
    /// Match the skill labels displayed by this client, including custom display names.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_skills_max_1000"
    )]
    pub skills: Option<Vec<OrchestrationV2SearchThreadSkill>>,
    pub query: OrchestrationV2SearchQuery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<NonNegativeInt>,
    /// Select relative to an entry identity so updates before it do not shift navigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<OrchestrationV2SearchThreadStart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadFindMatch {
    pub entry_id: TrimmedNonEmptyString,
    pub run_id: Option<RunId>,
    pub occurrence: NonNegativeInt,
}

/// Counts and identities around the selection, so clients can step without
/// another round trip (upstream inline struct; the array is capped at 17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadFindNavigation {
    pub entry_id: TrimmedNonEmptyString,
    pub run_id: Option<RunId>,
    pub start_index: NonNegativeInt,
    pub count: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2SearchThreadResult {
    /// Omitted by older servers. An early match has no final ordinal or total yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
    pub snapshot_sequence: NonNegativeInt,
    pub total_matches: NonNegativeInt,
    pub active_index: NonNegativeInt,
    pub r#match: Option<OrchestrationV2ThreadFindMatch>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_navigation_max_17"
    )]
    pub navigation: Option<Vec<OrchestrationV2ThreadFindNavigation>>,
}

/// `OrchestrationV2SearchThreadError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationV2SearchThreadError {
    #[serde(rename = "OrchestrationV2SearchThreadError")]
    OrchestrationV2SearchThreadError { cause: serde_json::Value },
}

impl std::fmt::Display for OrchestrationV2SearchThreadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Could not search this thread. Please retry.")
    }
}

impl std::error::Error for OrchestrationV2SearchThreadError {}

// ---------------------------------------------------------------------------
// Provider replay transcripts
// ---------------------------------------------------------------------------

/// `Schema.optional(Schema.Unknown)`: absent is `None`, but an explicit `null`
/// is a value (`Some(Null)`), not absence.
fn deserialize_optional_unknown<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(d).map(Some)
}

/// One recorded provider-protocol frame expectation or emission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProviderReplayEntry {
    ExpectOutbound {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<TrimmedNonEmptyString>,
        frame: serde_json::Value,
    },
    EmitInbound {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<TrimmedNonEmptyString>,
        frame: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_ms: Option<NonNegativeInt>,
    },
    RuntimeExit {
        status: ProviderReplayRuntimeExitStatus,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_optional_unknown"
        )]
        error: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderReplayRuntimeExitStatus {
    Success,
    Error,
    Cancelled,
}

/// A recorded provider conversation, replayable by a fake provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderReplayTranscript {
    pub provider: TrimmedNonEmptyString,
    pub protocol: TrimmedNonEmptyString,
    pub version: TrimmedNonEmptyString,
    pub scenario: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<indexmap::IndexMap<String, serde_json::Value>>,
    pub entries: Vec<ProviderReplayEntry>,
}

/// NDJSON header of a replay stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderReplayTranscriptHeader {
    #[serde(rename = "type")]
    pub type_: ProviderReplayTranscriptHeaderType,
    pub provider: TrimmedNonEmptyString,
    pub protocol: TrimmedNonEmptyString,
    pub version: TrimmedNonEmptyString,
    pub scenario: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<indexmap::IndexMap<String, serde_json::Value>>,
}

crate::string_literal!(
    /// Constant `"transcript_start"` type of a replay NDJSON header.
    ProviderReplayTranscriptHeaderType,
    "transcript_start"
);

/// One NDJSON line of a replay stream: the header, or an entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderReplayNdjsonRecord {
    Header(ProviderReplayTranscriptHeader),
    Entry(ProviderReplayEntry),
}

// ---------------------------------------------------------------------------
// Tests (ported from upstream orchestrationV2.test.ts: the tests that pin
// encoding/decoding behaviour. Effect-runtime and RPC-transport tests are
// skipped, and assertions on Turin-dropped fields (providerRuntime-typed
// usage surfaces) are noted inline.)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: &str = "2026-04-20T00:00:00.000Z";

    /// Merges `extra` over `base` like the upstream tests' object spread.
    fn merge(mut base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            base.extend(extra.clone());
        }
        base
    }

    fn turn_item_json(kind: &str, extra: serde_json::Value) -> serde_json::Value {
        let base = json!({
            "id": format!("turn-item-{kind}"),
            "type": kind,
            "threadId": "thread-1",
            "runId": null,
            "nodeId": null,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": 1,
            "status": "completed",
            "title": null,
            "startedAt": null,
            "completedAt": null,
            "updatedAt": NOW,
        });
        merge(base, extra)
    }

    fn app_thread_json() -> serde_json::Value {
        json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": "thread-1",
            "projectId": "project-1",
            "title": "Thread",
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "activeProviderThreadId": null,
            "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1" },
            "forkedFrom": null,
            "createdAt": NOW,
            "updatedAt": NOW,
            "archivedAt": null,
            "deletedAt": null,
        })
    }

    fn empty_projection_json() -> serde_json::Value {
        json!({
            "thread": app_thread_json(),
            "runs": [],
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": [],
            "providerThreads": [],
            "providerTurns": [],
            "runtimeRequests": [],
            "messages": [],
            "plans": [],
            "turnItems": [],
            "checkpointScopes": [],
            "checkpoints": [],
            "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": [],
            "updatedAt": NOW,
        })
    }

    #[test]
    fn command_execution_metadata_survives_without_output_text() {
        for metadata in [
            json!({}),
            json!({ "outputIndicatesFailure": true }),
            json!({ "outputIndicatesFailure": false }),
        ] {
            let item = turn_item_json(
                "command_execution",
                json!({ "id": "command-item", "input": "example", "title": "Command" }),
            );
            let item = match &metadata {
                serde_json::Value::Object(extra) => {
                    let mut item = item;
                    item.as_object_mut().unwrap().extend(extra.clone());
                    item
                }
                _ => unreachable!(),
            };
            let runtime: OrchestrationV2TurnItem = serde_json::from_value(item.clone()).unwrap();
            // The Json alias is wire-identical, so it decodes the same payload.
            let stored: OrchestrationV2TurnItemJson = serde_json::from_value(item).unwrap();
            assert_eq!(
                serde_json::to_value(&runtime).unwrap(),
                serde_json::to_value(&stored).unwrap()
            );
            let encoded = serde_json::to_value(&runtime).unwrap();
            assert_eq!(encoded["type"], "command_execution");
            assert!(encoded.get("output").is_none());
            assert_eq!(
                encoded.get("outputIndicatesFailure"),
                metadata.get("outputIndicatesFailure")
            );
        }
    }

    #[test]
    fn thread_stream_decodes_newer_event_types_as_skippable() {
        let detached = |id: &str, sequence: u64| {
            json!({
                "kind": "event",
                "sequence": sequence,
                "event": {
                    "id": id,
                    "type": "provider-session.detached",
                    "threadId": "thread-1",
                    "occurredAt": NOW,
                    "payload": { "providerSessionId": "provider-session-1", "detachedAt": NOW },
                },
            })
        };
        let items: Vec<OrchestrationV2ThreadStreamItem> = serde_json::from_value(json!([
            detached("event-1", 1),
            {
                "kind": "event",
                "sequence": 2,
                "event": {
                    "id": "event-2",
                    // A type no build of this client knows, standing in for a newer server's event.
                    "type": "run.from-a-future-server",
                    "threadId": "thread-1",
                    "occurredAt": NOW,
                    "payload": { "runId": "run-1" },
                },
            },
            detached("event-3", 3),
        ]))
        .unwrap();

        assert!(matches!(
            items[0],
            OrchestrationV2ThreadStreamItem::Event { sequence: 1, .. }
        ));
        assert_eq!(
            items[1],
            OrchestrationV2ThreadStreamItem::UnknownEvent {
                sequence: 2,
                event_type: "run.from-a-future-server".to_owned(),
            }
        );
        assert!(matches!(
            items[2],
            OrchestrationV2ThreadStreamItem::Event { sequence: 3, .. }
        ));

        // A known type with a broken payload is a real defect, not a newer event.
        let broken = json!([{
            "kind": "event",
            "sequence": 4,
            "event": {
                "id": "event-4",
                "type": "provider-session.detached",
                "threadId": "thread-1",
                "occurredAt": NOW,
                "payload": {},
            },
        }]);
        assert!(serde_json::from_value::<Vec<OrchestrationV2ThreadStreamItem>>(broken).is_err());
        // And it can never be sent back.
        assert!(serde_json::to_value(&items[1]).is_err());
    }

    #[test]
    fn snapshot_and_events_skip_unknown_turn_item_types() {
        let known = turn_item_json(
            "system_notice",
            json!({ "id": "item-known", "message": "Hello" }),
        );
        // A type no build of this client knows, standing in for a newer server's item.
        let future = turn_item_json("hologram", json!({ "id": "item-future", "beam": "ref-1" }));
        let projected = |position: u64, item: &serde_json::Value| {
            json!({
                "position": position,
                "visibility": "local",
                "sourceThreadId": "thread-1",
                "sourceItemId": item["id"],
                "item": item,
            })
        };
        let mut projection = empty_projection_json();
        projection["turnItems"] = json!([future, known]);
        projection["visibleTurnItems"] = json!([projected(0, &future), projected(1, &known)]);
        let turn_item_event = |sequence: u64, payload: &serde_json::Value| {
            json!({
                "kind": "event",
                "sequence": sequence,
                "event": {
                    "id": format!("event-{sequence}"),
                    "type": "turn-item.updated",
                    "threadId": "thread-1",
                    "occurredAt": NOW,
                    "payload": payload,
                },
            })
        };

        let items: Vec<OrchestrationV2ThreadStreamItem> = serde_json::from_value(json!([
            { "kind": "snapshot", "snapshotSequence": 1, "projection": projection },
            turn_item_event(2, &future),
            turn_item_event(3, &known),
        ]))
        .unwrap();

        let OrchestrationV2ThreadStreamItem::Snapshot { projection, .. } = &items[0] else {
            panic!("expected a snapshot");
        };
        let ids: Vec<&str> = projection.turn_items.iter().map(item_id).collect();
        assert_eq!(ids, vec!["item-known"]);
        let visible_ids: Vec<&str> = projection
            .visible_turn_items
            .iter()
            .map(|row| item_id(&row.item))
            .collect();
        assert_eq!(visible_ids, vec!["item-known"]);
        assert_eq!(
            items[1],
            OrchestrationV2ThreadStreamItem::UnknownEvent {
                sequence: 2,
                event_type: "turn-item.updated".to_owned(),
            }
        );
        let OrchestrationV2ThreadStreamItem::Event { event, .. } = &items[2] else {
            panic!("expected an event");
        };
        let OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } = event else {
            panic!("expected turn-item.updated");
        };
        assert_eq!(item_id(payload), "item-known");
        assert!(matches!(
            payload,
            OrchestrationV2TurnItem::SystemNotice { .. }
        ));

        // History pages drop unknown rows too.
        let page: OrchestrationV2ThreadHistoryPage = serde_json::from_value(json!({
            "snapshotSequence": 1,
            "items": [projected(0, &future), projected(1, &known)],
            "nextCursor": null,
            "hasMoreHistory": false,
        }))
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(item_id(&page.items[0].item), "item-known");

        // A known turn item type with a broken payload is a real defect, not a newer item.
        let broken = turn_item_json("system_notice", json!({ "id": "item-broken" }));
        assert!(
            serde_json::from_value::<Vec<OrchestrationV2ThreadStreamItem>>(json!([
                turn_item_event(4, &broken),
            ]))
            .is_err()
        );
        let mut broken_projection = empty_projection_json();
        broken_projection["turnItems"] = json!([broken]);
        assert!(
            serde_json::from_value::<Vec<OrchestrationV2ThreadStreamItem>>(json!([{
                "kind": "snapshot",
                "snapshotSequence": 1,
                "projection": broken_projection,
            }]))
            .is_err()
        );
    }

    fn item_id(item: &OrchestrationV2TurnItem) -> &str {
        match item {
            OrchestrationV2TurnItem::Notification { id, .. }
            | OrchestrationV2TurnItem::UserMessage { id, .. }
            | OrchestrationV2TurnItem::AssistantMessage { id, .. }
            | OrchestrationV2TurnItem::Reasoning { id, .. }
            | OrchestrationV2TurnItem::ProposedPlan { id, .. }
            | OrchestrationV2TurnItem::TodoList { id, .. }
            | OrchestrationV2TurnItem::UserInputRequest { id, .. }
            | OrchestrationV2TurnItem::FileChange { id, .. }
            | OrchestrationV2TurnItem::CommandExecution { id, .. }
            | OrchestrationV2TurnItem::FileSearch { id, .. }
            | OrchestrationV2TurnItem::WebSearch { id, .. }
            | OrchestrationV2TurnItem::ApprovalRequest { id, .. }
            | OrchestrationV2TurnItem::Checkpoint { id, .. }
            | OrchestrationV2TurnItem::RunInterruptRequest { id, .. }
            | OrchestrationV2TurnItem::RunInterruptResult { id, .. }
            | OrchestrationV2TurnItem::SystemNotice { id, .. }
            | OrchestrationV2TurnItem::Error { id, .. }
            | OrchestrationV2TurnItem::Compaction { id, .. }
            | OrchestrationV2TurnItem::Handoff { id, .. }
            | OrchestrationV2TurnItem::Fork { id, .. }
            | OrchestrationV2TurnItem::ThreadCreated { id, .. }
            | OrchestrationV2TurnItem::SecretRequest { id, .. }
            | OrchestrationV2TurnItem::Subagent { id, .. }
            | OrchestrationV2TurnItem::DynamicTool { id, .. } => id.as_str(),
        }
    }

    #[test]
    fn subscribe_thread_input_negotiates_capabilities() {
        let bounded: OrchestrationV2SubscribeThreadInput = serde_json::from_value(
            json!({ "threadId": "thread-1", "acceptBoundedSnapshot": true }),
        )
        .unwrap();
        assert_eq!(bounded.accept_bounded_snapshot, Some(true));
        let plain: OrchestrationV2SubscribeThreadInput =
            serde_json::from_value(json!({ "threadId": "thread-1" })).unwrap();
        assert_eq!(plain.accept_bounded_snapshot, None);

        let compact: OrchestrationV2SubscribeThreadInput = serde_json::from_value(json!({
            "threadId": "thread-1",
            "acceptCompactTurnItems": true,
        }))
        .unwrap();
        assert_eq!(compact.accept_compact_turn_items, Some(true));
    }

    #[test]
    fn snapshot_turn_items_omit_local_visible_marker_is_true_only() {
        let decode = |marker: serde_json::Value| {
            serde_json::from_value::<OrchestrationV2ThreadStreamItem>(json!({
                "kind": "snapshot",
                "snapshotSequence": 1,
                "projection": empty_projection_json(),
                "turnItemsOmitLocalVisible": marker,
            }))
        };
        // Older servers never send the marker; newer clients treat absence as full turnItems.
        let absent = serde_json::from_value::<OrchestrationV2ThreadStreamItem>(json!({
            "kind": "snapshot",
            "snapshotSequence": 1,
            "projection": empty_projection_json(),
        }))
        .unwrap();
        let OrchestrationV2ThreadStreamItem::Snapshot {
            turn_items_omit_local_visible,
            ..
        } = absent
        else {
            panic!("expected a snapshot");
        };
        assert_eq!(turn_items_omit_local_visible, None);

        let marked = decode(json!(true)).unwrap();
        let OrchestrationV2ThreadStreamItem::Snapshot {
            turn_items_omit_local_visible,
            ..
        } = marked
        else {
            panic!("expected a snapshot");
        };
        assert_eq!(turn_items_omit_local_visible, Some(TrueLiteral));
        // The marker only means "omitted"; any other value is a protocol error.
        assert!(decode(json!(false)).is_err());
    }

    #[test]
    fn capabilities_predate_runtime_policy_decode_weaker() {
        // Events written before the field existed must replay; absent decodes to
        // the weaker client-boundary guarantee so history never overclaims.
        let mut legacy = json!({
            "sessions": {
                "supportsMultipleProviderThreadsPerSession": false,
                "supportsModelSwitchInSession": false,
                "supportsProviderSwitchingViaHandoff": true,
                "supportsRuntimeModeSwitchInSession": false,
                "pendingRequestsSurviveRestart": false,
            },
            "threads": {
                "canCreateEmptyThread": true,
                "canReadThreadSnapshot": false,
                "canRollbackThread": true,
                "canForkThread": false,
                "canForkFromTurn": false,
                "canForkFromSubagentThread": false,
                "exposesNativeThreadId": true,
            },
            "turns": {
                "exposesNativeTurnId": false,
                "emitsTurnStarted": true,
                "emitsTurnCompleted": true,
                "supportsInterrupt": true,
                "supportsActiveSteering": false,
                "supportsSteeringByInterruptRestart": true,
                "supportsQueuedMessages": true,
                "terminalStatusQuality": "strong",
            },
            "streaming": {
                "streamsAssistantText": true,
                "streamsReasoning": true,
                "streamsToolOutput": true,
                "streamsPlanText": false,
                "emitsMessageCompleted": true,
            },
            "tools": {
                "exposesToolItemIds": true,
                "emitsToolStarted": true,
                "emitsToolCompleted": true,
                "emitsToolOutput": true,
                "supportsMcpTools": false,
                "supportsDynamicToolCallbacks": false,
            },
            "approvals": {
                "supportsCommandApproval": true,
                "supportsFileReadApproval": true,
                "supportsFileChangeApproval": true,
                "supportsApplyPatchApproval": false,
                "approvalsHaveNativeRequestIds": false,
                "approvalCallbacksAreLiveOnly": true,
                "approvalsCanOriginateFromSubagents": false,
            },
            "planning": {
                "emitsPlanUpdated": true,
                "emitsTodoList": true,
                "emitsProposedPlan": false,
                "supportsStructuredQuestions": true,
                "planDeltasHaveItemIds": false,
            },
            "subagents": {
                "supportsSubagents": false,
                "exposesSubagentThreadIds": false,
                "emitsSubagentLifecycle": false,
                "canWaitForSubagents": false,
                "canCloseSubagents": false,
                "canForkSubagentThread": false,
            },
            "context": {
                "acceptsSystemContext": false,
                "acceptsDeveloperContext": false,
                "acceptsSyntheticUserContext": true,
                "canGenerateSummaries": true,
                "canConsumeHandoffSummaries": true,
                "supportsDeltaHandoff": true,
                "supportsFullThreadHandoff": true,
                "maxRecommendedHandoffChars": null,
            },
            "checkpointing": {
                "appCanCheckpointFilesystem": true,
                "supportsNestedCheckpointScopes": true,
                "providerCanRollbackConversation": true,
                "providerRollbackReturnsSnapshot": true,
                "providerCanReadConversationSnapshot": false,
            },
            "identity": {
                "nativeThreadIds": "strong",
                "nativeTurnIds": "weak",
                "nativeItemIds": "weak",
                "nativeRequestIds": "weak",
            },
        });
        let decoded: OrchestrationV2ProviderCapabilities =
            serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(
            decoded.runtime_policy.enforcement,
            OrchestrationV2RuntimePolicyEnforcement::ClientBoundary
        );

        legacy["runtimePolicy"] = json!({ "enforcement": "native" });
        let explicit: OrchestrationV2ProviderCapabilities = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            explicit.runtime_policy.enforcement,
            OrchestrationV2RuntimePolicyEnforcement::Native
        );
    }

    #[test]
    fn nested_checkpoint_scopes_do_not_advance_app_run_count() {
        let root_scope: OrchestrationV2CheckpointScope = serde_json::from_value(json!({
            "id": "scope-root-1",
            "threadId": "thread-1",
            "runId": "run-1",
            "nodeId": "node-root-1",
            "parentScopeId": null,
            "providerThreadId": "provider-thread-1",
            "kind": "root_run",
            "ordinalWithinParent": 1,
            "advancesAppRunCount": true,
            "cwd": "/tmp/project",
            "createdAt": NOW,
        }))
        .unwrap();
        let child_scope: OrchestrationV2CheckpointScope = serde_json::from_value(json!({
            "id": "scope-child-1",
            "threadId": "thread-1",
            "runId": "run-1",
            "nodeId": "node-child-1",
            "parentScopeId": "scope-root-1",
            "providerThreadId": "provider-thread-child-1",
            "kind": "subagent",
            "ordinalWithinParent": 1,
            "advancesAppRunCount": false,
            "cwd": "/tmp/project",
            "createdAt": NOW,
        }))
        .unwrap();

        assert!(root_scope.advances_app_run_count);
        assert_eq!(
            child_scope.parent_scope_id.as_ref().map(|id| id.as_str()),
            Some(root_scope.id.as_str())
        );
        assert!(!child_scope.advances_app_run_count);
    }

    #[test]
    fn checkpoint_captures_attach_to_scopes_nodes_and_run_ordinals() {
        let checkpoint: OrchestrationV2Checkpoint = serde_json::from_value(json!({
            "id": "checkpoint-1",
            "threadId": "thread-1",
            "scopeId": "scope-child-1",
            "runId": "run-1",
            "nodeId": "node-child-1",
            "parentCheckpointId": "checkpoint-root-1",
            "ordinalWithinScope": 1,
            "appRunOrdinal": null,
            "ref": "git-ref-1",
            "status": "ready",
            "files": [{ "path": "package.json", "kind": "modified", "additions": 2, "deletions": 1 }],
            "capturedAt": NOW,
        }))
        .unwrap();

        assert!(checkpoint.app_run_ordinal.is_none());
        assert_eq!(checkpoint.scope_id.as_str(), "scope-child-1");
        assert_eq!(
            checkpoint
                .parent_checkpoint_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("checkpoint-root-1")
        );
        assert_eq!(checkpoint.ref_.as_str(), "git-ref-1");
    }

    #[test]
    fn command_and_domain_event_shapes_decode() {
        let command: OrchestrationV2Command = serde_json::from_value(json!({
            "type": "message.dispatch",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": "command-1",
            "threadId": "thread-1",
            "messageId": "message-1",
            "text": "hello",
            "attachments": [],
            "dispatchMode": { "type": "start_immediately" },
        }))
        .unwrap();
        let OrchestrationV2Command::MessageDispatch { command_id, .. } = &command else {
            panic!("expected message.dispatch");
        };
        assert_eq!(command_id.as_str(), "command-1");

        let event: OrchestrationV2DomainEvent = serde_json::from_value(json!({
            "id": "event-1",
            "type": "run.created",
            "threadId": "thread-1",
            "runId": "run-1",
            "occurredAt": NOW,
            "payload": {
                "id": "run-1",
                "threadId": "thread-1",
                "ordinal": 1,
                "providerInstanceId": "codex",
                "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
                "providerThreadId": "provider-thread-1",
                "userMessageId": "message-1",
                "rootNodeId": null,
                "activeAttemptId": null,
                "status": "queued",
                "requestedAt": NOW,
                "startedAt": null,
                "completedAt": null,
                "checkpointId": null,
                "contextHandoffId": null,
            },
        }))
        .unwrap();
        let OrchestrationV2DomainEvent::RunCreated { id, payload, .. } = &event else {
            panic!("expected run.created");
        };
        assert_eq!(id.as_str(), "event-1");
        assert_eq!(payload.id.as_str(), "run-1");
    }

    #[test]
    fn delegated_task_request_decodes() {
        let command: OrchestrationV2Command = serde_json::from_value(json!({
            "type": "delegated_task.request",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": "command-delegate-1",
            "parentThreadId": "thread-parent-1",
            "parentRunId": "run-parent-1",
            "parentNodeId": "node-parent-1",
            "task": "Inspect the API boundary.",
            "title": "API inspection",
            "modelSelection": { "instanceId": "claudeAgent", "model": "claude-sonnet-4-6" },
            "runtimeMode": "approval-required",
            "interactionMode": "plan",
        }))
        .unwrap();
        let OrchestrationV2Command::DelegatedTaskRequest {
            parent_thread_id,
            parent_run_id,
            parent_node_id,
            ..
        } = &command
        else {
            panic!("expected delegated_task.request");
        };
        assert_eq!(parent_thread_id.as_str(), "thread-parent-1");
        assert_eq!(parent_run_id.as_str(), "run-parent-1");
        assert_eq!(parent_node_id.as_str(), "node-parent-1");
    }

    #[test]
    fn delegated_task_wake_policy_is_required_and_closed() {
        let command: OrchestrationV2Command = serde_json::from_value(json!({
            "type": "delegated_task.wake-policy",
            "commandId": "command-delegate-wake-policy-1",
            "parentThreadId": "thread-parent-1",
            "taskId": "node-subagent-1",
            "completionWake": "always",
        }))
        .unwrap();
        let OrchestrationV2Command::DelegatedTaskWakePolicy {
            task_id,
            completion_wake,
            ..
        } = &command
        else {
            panic!("expected delegated_task.wake-policy");
        };
        assert_eq!(task_id.as_str(), "node-subagent-1");
        assert_eq!(*completion_wake, OrchestrationV2CompletionWake::Always);

        // The policy carries the whole decision, so it is required and closed.
        assert!(
            serde_json::from_value::<OrchestrationV2Command>(json!({
                "type": "delegated_task.wake-policy",
                "commandId": "command-delegate-wake-policy-2",
                "parentThreadId": "thread-parent-1",
                "taskId": "node-subagent-1",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<OrchestrationV2Command>(json!({
                "type": "delegated_task.wake-policy",
                "commandId": "command-delegate-wake-policy-3",
                "parentThreadId": "thread-parent-1",
                "taskId": "node-subagent-1",
                "completionWake": "sometimes",
            }))
            .is_err()
        );
    }

    #[test]
    fn thread_created_records_decode() {
        let command: OrchestrationV2Command = serde_json::from_value(json!({
            "type": "thread.created.record",
            "commandId": "command-thread-record-1",
            "parentThreadId": "thread-parent-1",
            "parentRunId": "run-parent-1",
            "parentNodeId": "node-parent-1",
            "targetThreadId": "thread-child-1",
            "targetRunId": "run-child-1",
        }))
        .unwrap();
        let OrchestrationV2Command::ThreadCreatedRecord {
            target_thread_id,
            target_run_id,
            ..
        } = &command
        else {
            panic!("expected thread.created.record");
        };
        assert_eq!(target_thread_id.as_str(), "thread-child-1");
        assert_eq!(
            target_run_id.as_ref().map(|id| id.as_str()).unwrap(),
            "run-child-1"
        );

        let item: OrchestrationV2TurnItem = serde_json::from_value(turn_item_json(
            "thread_created",
            json!({
                "id": "turn-item-thread-created-1",
                "threadId": "thread-parent-1",
                "runId": "run-parent-1",
                "nodeId": "node-parent-1",
                "providerThreadId": "provider-thread-parent-1",
                "ordinal": 4,
                "title": "Child thread",
                "startedAt": NOW,
                "completedAt": NOW,
                "targetThreadId": "thread-child-1",
                "targetRunId": "run-child-1",
                "targetProviderInstanceId": "claude-default",
                "targetModel": "claude-sonnet-4-6",
            }),
        ))
        .unwrap();
        let OrchestrationV2TurnItem::ThreadCreated { target_run_id, .. } = &item else {
            panic!("expected thread_created");
        };
        assert_eq!(
            target_run_id.as_ref().map(|id| id.as_str()).unwrap(),
            "run-child-1"
        );
    }

    #[test]
    fn provider_replay_transcript_decodes() {
        let transcript: ProviderReplayTranscript = serde_json::from_value(json!({
            "provider": "codex",
            "protocol": "codex.app-server",
            "version": "0.120.0",
            "scenario": "simple",
            "metadata": { "source": "real-probe" },
            "entries": [
                { "type": "expect_outbound", "label": "initialize", "frame": { "id": 1, "method": "initialize" } },
                { "type": "emit_inbound", "label": "initialize-result", "frame": { "id": 1, "result": { "ok": true } } },
                { "type": "runtime_exit", "status": "success" },
            ],
        }))
        .unwrap();

        assert_eq!(transcript.entries.len(), 3);
        assert_eq!(transcript.protocol.as_str(), "codex.app-server");
    }

    #[test]
    fn typed_and_dynamic_tool_turn_items_decode() {
        let file_change: OrchestrationV2TurnItem = serde_json::from_value(turn_item_json(
            "file_change",
            json!({
                "id": "turn-item-file-change-1",
                "runId": "run-1",
                "nodeId": "node-file-change-1",
                "providerThreadId": "provider-thread-1",
                "providerTurnId": "provider-turn-1",
                "nativeItemRef": { "driver": "codex", "nativeId": "item-file-change-1", "strength": "strong" },
                "ordinal": 3,
                "title": "Edited package.json",
                "startedAt": NOW,
                "completedAt": NOW,
                "fileName": "package.json",
                "additions": 4,
                "deletions": 2,
                "diffStr": "@@ fixture diff",
            }),
        ))
        .unwrap();
        let OrchestrationV2TurnItem::FileChange {
            file_name,
            additions,
            ..
        } = &file_change
        else {
            panic!("expected file_change");
        };
        assert_eq!(file_name.as_str(), "package.json");
        assert_eq!(*additions, Some(4));

        let dynamic_tool: OrchestrationV2TurnItem = serde_json::from_value(turn_item_json(
            "dynamic_tool",
            json!({
                "id": "turn-item-dynamic-1",
                "runId": "run-1",
                "nodeId": "node-dynamic-1",
                "providerThreadId": "provider-thread-1",
                "providerTurnId": "provider-turn-1",
                "nativeItemRef": { "driver": "codex", "nativeId": "item-dynamic-1", "strength": "strong" },
                "ordinal": 4,
                "title": "Custom tool",
                "toolName": "custom.lookup",
                "input": { "query": "fixture" },
                "output": { "ok": true },
                "startedAt": NOW,
                "completedAt": NOW,
            }),
        ))
        .unwrap();
        assert_eq!(item_id(&dynamic_tool), "turn-item-dynamic-1");
    }

    #[test]
    fn error_turn_items_bound_failure_text() {
        let error_base = || {
            turn_item_json(
                "error",
                json!({
                    "id": "turn-item-error-1",
                    "runId": "run-1",
                    "nodeId": "node-root-1",
                    "providerThreadId": "provider-thread-1",
                    "providerTurnId": "provider-turn-1",
                    "ordinal": 199,
                    "status": "failed",
                    "title": "Provider error",
                    "startedAt": NOW,
                    "completedAt": NOW,
                }),
            )
        };
        let error_item: OrchestrationV2TurnItem = serde_json::from_value(merge(
            error_base(),
            json!({
                "failure": {
                    "class": "validation_error",
                    "message": "Invalid reasoning effort.",
                    "code": "invalid_request",
                    "retryable": false,
                },
            }),
        ))
        .unwrap();
        let OrchestrationV2TurnItem::Error { failure, .. } = &error_item else {
            panic!("expected error item");
        };
        assert_eq!(failure.message.as_str(), "Invalid reasoning effort.");

        let usage_limited: OrchestrationV2TurnItem = serde_json::from_value(merge(
            error_base(),
            json!({
                "failure": {
                    "class": "usage_limit",
                    "message": "Invalid reasoning effort.",
                    "code": "invalid_request",
                    "retryable": false,
                },
            }),
        ))
        .unwrap();
        let OrchestrationV2TurnItem::Error { failure, .. } = usage_limited else {
            panic!("expected error item");
        };
        assert_eq!(
            failure.class,
            OrchestrationV2ProviderFailureClass::UsageLimit
        );

        assert!(serde_json::from_value::<OrchestrationV2TurnItem>(merge(
            error_base(),
            json!({ "failure": { "class": "validation_error", "message": "x".repeat(4_097), "code": "invalid_request", "retryable": false } }),
        ))
        .is_err());
    }

    #[test]
    fn subagent_records_and_turn_items_decode() {
        let subagent: OrchestrationV2Subagent = serde_json::from_value(json!({
            "id": "node-subagent-1",
            "threadId": "thread-1",
            "runId": "run-1",
            "parentNodeId": "node-root-1",
            "origin": "provider_native",
            "createdBy": "agent",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerThreadId": "provider-thread-subagent-1",
            "childThreadId": null,
            "nativeTaskRef": { "driver": "codex", "nativeId": "native-task-1", "strength": "strong" },
            "prompt": "Inspect package.json",
            "title": "Package audit",
            "model": "gpt-5.4",
            "status": "completed",
            "progress": "Inspecting package metadata",
            "result": "Package is private.",
            "startedAt": NOW,
            "completedAt": NOW,
            "updatedAt": NOW,
        }))
        .unwrap();
        let turn_item: OrchestrationV2TurnItem = serde_json::from_value(turn_item_json(
            "subagent",
            json!({
                "id": "turn-item-subagent-1",
                "runId": "run-1",
                "nodeId": "node-subagent-1",
                "providerThreadId": "provider-thread-subagent-1",
                "providerTurnId": "provider-turn-1",
                "nativeItemRef": { "driver": "codex", "nativeId": "native-task-1", "strength": "strong" },
                "ordinal": 2,
                "title": "Package audit",
                "startedAt": NOW,
                "completedAt": NOW,
                "subagentId": "node-subagent-1",
                "origin": "provider_native",
                "driver": "codex",
                "providerInstanceId": "codex",
                "childThreadId": null,
                "prompt": "Inspect package.json",
                "progress": "Inspecting package metadata",
                "result": "Package is private.",
            }),
        ))
        .unwrap();

        assert_eq!(
            subagent.origin,
            OrchestrationV2SubagentOrigin::ProviderNative
        );
        assert_eq!(
            subagent.progress.as_deref(),
            Some("Inspecting package metadata")
        );
        assert!(subagent.child_thread_id.is_none());
        let OrchestrationV2TurnItem::Subagent { progress, .. } = &turn_item else {
            panic!("expected subagent item");
        };
        assert_eq!(progress.as_deref(), Some("Inspecting package metadata"));
    }

    #[test]
    fn workflow_prompt_and_result_preserve_whitespace_in_stored_and_wire_json() {
        for preview in ["  indented prompt\n", "\n \t\n"] {
            let coordinator = json!({
                "id": "workflow-coordinator",
                "threadId": "thread-1",
                "runId": "run-1",
                "parentNodeId": "node-root-1",
                "origin": "provider_native",
                "createdBy": "agent",
                "driver": "claudeAgent",
                "providerInstanceId": "claudeAgent",
                "providerThreadId": null,
                "childThreadId": null,
                "nativeTaskRef": null,
                "prompt": "Run workflow",
                "title": null,
                "model": null,
                "status": "running",
                "result": null,
                "startedAt": NOW,
                "completedAt": null,
                "updatedAt": NOW,
            });
            let subagent: OrchestrationV2SubagentJson = serde_json::from_value(merge(
                coordinator,
                json!({
                    "workflow": {
                        "phases": [],
                        "agents": [
                            { "index": 1, "label": "Member", "state": "completed", "prompt": preview, "result": preview },
                        ],
                    },
                }),
            ))
            .unwrap();
            let workflow = subagent.workflow.as_ref().unwrap();
            assert_eq!(
                workflow.agents[0].prompt.as_ref().unwrap().as_str(),
                preview
            );
            assert_eq!(
                workflow.agents[0].result.as_ref().unwrap().as_str(),
                preview
            );

            // What is stored must re-decode with the excerpts intact, through
            // a JSON string round trip like upstream's JSON.parse(JSON.stringify()).
            let encoded = serde_json::to_string(&subagent).unwrap();
            let redecoded: OrchestrationV2SubagentJson = serde_json::from_str(&encoded).unwrap();
            let OrchestrationV2SubagentWorkflow { agents, .. } =
                redecoded.workflow.as_ref().unwrap();
            assert_eq!(agents[0].prompt.as_ref().unwrap().as_str(), preview);
            assert_eq!(agents[0].result.as_ref().unwrap().as_str(), preview);
        }
    }

    #[test]
    fn subagent_completion_wake_policies_decode() {
        let app_owned_subagent = json!({
            "id": "node-subagent-2",
            "threadId": "thread-1",
            "runId": "run-1",
            "parentNodeId": "node-root-1",
            "origin": "app_owned",
            "createdBy": "agent",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerThreadId": null,
            "childThreadId": "thread-child-1",
            "nativeTaskRef": null,
            "prompt": "Inspect the API boundary.",
            "title": "API inspection",
            "model": "gpt-5.4",
            "status": "running",
            "result": null,
            "startedAt": NOW,
            "completedAt": null,
            "updatedAt": NOW,
        });

        // Legacy records predate the field and behave as settled_only.
        let decoded: OrchestrationV2Subagent =
            serde_json::from_value(app_owned_subagent.clone()).unwrap();
        assert_eq!(decoded.completion_wake, None);
        let always: OrchestrationV2Subagent = serde_json::from_value(merge(
            app_owned_subagent.clone(),
            json!({ "completionWake": "always" }),
        ))
        .unwrap();
        assert_eq!(
            always.completion_wake,
            Some(OrchestrationV2CompletionWake::Always)
        );
        let settled_only: OrchestrationV2Subagent = serde_json::from_value(merge(
            app_owned_subagent.clone(),
            json!({ "completionWake": "settled_only" }),
        ))
        .unwrap();
        assert_eq!(
            settled_only.completion_wake,
            Some(OrchestrationV2CompletionWake::SettledOnly)
        );
        assert!(
            serde_json::from_value::<OrchestrationV2Subagent>(merge(
                app_owned_subagent.clone(),
                json!({ "completionWake": "sometimes" }),
            ))
            .is_err()
        );
    }

    #[test]
    fn thread_projection_decodes_with_ordered_turn_items() {
        let command_execution = turn_item_json(
            "command_execution",
            json!({
                "id": "turn-item-command-1",
                "runId": "run-1",
                "nodeId": "node-command-1",
                "providerThreadId": "provider-thread-1",
                "providerTurnId": "provider-turn-1",
                "nativeItemRef": { "driver": "codex", "nativeId": "item-command-1", "strength": "strong" },
                "ordinal": 1,
                "title": "Ran command",
                "input": "bun typecheck",
                "output": "Tasks: 10 successful",
                "exitCode": 0,
                "startedAt": NOW,
                "completedAt": NOW,
            }),
        );
        let mut projection = empty_projection_json();
        projection["turnItems"] = json!([command_execution.clone()]);
        projection["visibleTurnItems"] = json!([{
            "position": 0,
            "visibility": "local",
            "sourceThreadId": "thread-1",
            "sourceItemId": "turn-item-command-1",
            "item": command_execution,
        }]);

        let decoded: OrchestrationV2ThreadProjection = serde_json::from_value(projection).unwrap();
        assert_eq!(decoded.turn_items.len(), 1);
        assert!(matches!(
            decoded.turn_items[0],
            OrchestrationV2TurnItem::CommandExecution { .. }
        ));
        assert_eq!(
            item_id(&decoded.visible_turn_items[0].item),
            "turn-item-command-1"
        );

        let bounded: OrchestrationV2ThreadStreamItem = serde_json::from_value(json!({
            "kind": "snapshot",
            "snapshotSequence": 12,
            "projection": {
                "thread": app_thread_json(),
                "runs": [], "attempts": [], "nodes": [], "subagents": [],
                "providerSessions": [], "providerThreads": [], "providerTurns": [],
                "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
                "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                "visibleTurnItems": [],
                "updatedAt": NOW,
            },
            "historyCursor": "older-page",
            "hasMoreHistory": true,
            "latestLocalTurnOrdinal": 1,
            "payloadBudgetExceeded": false,
        }))
        .unwrap();
        let OrchestrationV2ThreadStreamItem::Snapshot {
            history_cursor,
            has_more_history,
            latest_local_turn_ordinal,
            payload_budget_exceeded,
            ..
        } = bounded
        else {
            panic!("expected a snapshot");
        };
        assert_eq!(
            history_cursor,
            Some(Some(TrimmedNonEmptyString::new("older-page").unwrap()))
        );
        assert_eq!(has_more_history, Some(true));
        assert_eq!(latest_local_turn_ordinal, Some(Some(1)));
        assert_eq!(payload_budget_exceeded, Some(false));
    }

    #[test]
    fn lifecycle_turn_items_decode() {
        let compaction: OrchestrationV2TurnItem = serde_json::from_value(turn_item_json(
            "compaction",
            json!({
                "id": "turn-item-compaction-1",
                "runId": "run-1",
                "nodeId": "node-compaction-1",
                "providerThreadId": "provider-thread-1",
                "providerTurnId": "provider-turn-1",
                "ordinal": 5,
                "status": "running",
                "title": "Compacting context...",
                "driver": "codex",
                "beforeTokenCount": 180000,
                "startedAt": NOW,
                "completedAt": null,
            }),
        ))
        .unwrap();
        assert!(matches!(
            compaction,
            OrchestrationV2TurnItem::Compaction { .. }
        ));

        let handoff: OrchestrationV2TurnItem = serde_json::from_value(turn_item_json(
            "handoff",
            json!({
                "id": "turn-item-handoff-1",
                "runId": "run-2",
                "nodeId": null,
                "providerThreadId": "provider-thread-claude-1",
                "ordinal": 6,
                "title": "Handed off to Claude",
                "contextHandoffId": "handoff-1",
                "fromProviderThreadIds": ["provider-thread-codex-1"],
                "toProviderThreadId": "provider-thread-claude-1",
                "fromProviderInstanceIds": ["codex"],
                "toProviderInstanceId": "claudeAgent",
                "strategy": "delta_since_target_last_seen",
                "summary": "Codex completed the setup work.",
                "startedAt": NOW,
                "completedAt": NOW,
            }),
        ))
        .unwrap();
        let OrchestrationV2TurnItem::Handoff {
            to_provider_instance_id,
            ..
        } = &handoff
        else {
            panic!("expected handoff");
        };
        assert_eq!(to_provider_instance_id.as_str(), "claudeAgent");

        let fork: OrchestrationV2TurnItem = serde_json::from_value(turn_item_json(
            "fork",
            json!({
                "id": "turn-item-fork-1",
                "runId": "run-2",
                "nodeId": "node-subagent-1",
                "providerThreadId": "provider-thread-child-1",
                "ordinal": 7,
                "title": "Forked subagent thread",
                "source": { "type": "node", "nodeId": "node-subagent-1" },
                "targetThreadId": "thread-fork-1",
                "startedAt": NOW,
                "completedAt": NOW,
            }),
        ))
        .unwrap();
        let OrchestrationV2TurnItem::Fork { source, .. } = &fork else {
            panic!("expected fork");
        };
        assert!(matches!(source, OrchestrationV2ForkSource::Node { .. }));
    }

    #[test]
    fn tool_non_execution_kind_round_trips() {
        for kind in [Some("cancelled"), Some("denied"), None] {
            let mut extra = json!({
                "id": "tool-result",
                "threadId": "thread",
                "toolName": "task_status",
                "input": { "taskId": "child" },
                "status": if kind == Some("cancelled") { "cancelled" } else { "failed" },
                "startedAt": NOW,
                "completedAt": NOW,
                "output": "Tool did not execute",
            });
            if let Some(kind) = kind {
                extra["toolNonExecutionKind"] = json!(kind);
            }
            let item: OrchestrationV2TurnItem =
                serde_json::from_value(turn_item_json("dynamic_tool", extra)).unwrap();
            let wire = serde_json::to_value(&item).unwrap();
            assert_eq!(
                wire.get("toolNonExecutionKind").and_then(|v| v.as_str()),
                kind
            );
            let redecoded: OrchestrationV2TurnItem = serde_json::from_value(wire).unwrap();
            assert_eq!(
                serde_json::to_value(&redecoded).unwrap(),
                serde_json::to_value(&item).unwrap()
            );
        }
    }

    fn notification_item_json(source: serde_json::Value) -> serde_json::Value {
        turn_item_json(
            "notification",
            json!({
                "id": "notification",
                "threadId": "parent",
                "outcome": "failed",
                "summary": "Build failed",
                "detail": "Exit code 1",
                "source": source,
            }),
        )
    }

    #[test]
    fn notification_items_round_trip_and_validate() {
        let sources = [
            json!({ "kind": "delegated_task", "taskIds": ["task-1", "task-2"] }),
            json!({ "kind": "delegated_task", "taskIds": ["task-1"], "childThreadId": "child" }),
            json!({ "kind": "subagent", "childThreadId": "child" }),
            json!({ "kind": "subagent" }),
            json!({ "kind": "command" }),
            json!({ "kind": "monitor" }),
            json!({ "kind": "background_task" }),
        ];
        for source in &sources {
            let runtime: OrchestrationV2TurnItem =
                serde_json::from_value(notification_item_json(source.clone())).unwrap();
            // Item status describes the record; outcome describes the work.
            let OrchestrationV2TurnItem::Notification {
                status, outcome, ..
            } = &runtime
            else {
                panic!("expected a notification item");
            };
            assert_eq!(*status, OrchestrationV2TurnItemStatus::Completed);
            assert_eq!(*outcome, OrchestrationV2NotificationOutcome::Failed);
            let wire = serde_json::to_value(&runtime).unwrap();
            let redecoded: OrchestrationV2TurnItem = serde_json::from_value(wire).unwrap();
            assert_eq!(
                serde_json::to_value(&redecoded).unwrap(),
                serde_json::to_value(&runtime).unwrap()
            );
        }
        // A blank summary fails its own schema.
        assert!(
            serde_json::from_value::<OrchestrationV2TurnItem>(merge(
                notification_item_json(json!({ "kind": "command" })),
                json!({ "summary": "" }),
            ))
            .is_err()
        );
        // A known kind with fields that do not decode is a real defect, not a newer kind.
        assert!(
            serde_json::from_value::<OrchestrationV2TurnItem>(notification_item_json(
                json!({ "kind": "delegated_task" })
            ),)
            .is_err()
        );
        assert!(
            serde_json::from_value::<OrchestrationV2TurnItem>(merge(
                notification_item_json(json!({ "kind": "command" })),
                json!({ "source": { "kind": "subagent", "childThreadId": 7 } }),
            ))
            .is_err()
        );
    }

    #[test]
    fn notification_sources_stored_before_specific_kinds_decode() {
        let source_of = |source: serde_json::Value| {
            let item: OrchestrationV2TurnItem =
                serde_json::from_value(notification_item_json(source)).unwrap();
            let OrchestrationV2TurnItem::Notification { source, .. } = item else {
                panic!("expected a notification item");
            };
            source
        };
        // Stored shapes carry extra fields the pre-kinds clients wrote; those
        // fields are ignored, and the kind maps onto this build's vocabulary.
        assert_eq!(
            source_of(
                json!({ "kind": "background_task", "nativeRef": { "driver": "claude", "nativeId": "task-1" } })
            ),
            OrchestrationV2NotificationSource::BackgroundTask
        );
        assert_eq!(
            source_of(json!({ "kind": "background_command" })),
            OrchestrationV2NotificationSource::Command
        );
        assert_eq!(
            source_of(json!({ "kind": "monitor" })),
            OrchestrationV2NotificationSource::Monitor
        );
    }

    #[test]
    fn notification_sources_encode_for_pre_specific_kinds_clients() {
        // The wire shape clients shipped with before specific kinds reject a
        // kind outside their union, so subagent and command are sent the old way.
        let cases = [
            (
                json!({ "kind": "subagent", "childThreadId": "child" }),
                json!({ "kind": "background_task", "work": "subagent", "childThreadId": "child" }),
            ),
            (
                json!({ "kind": "subagent" }),
                json!({ "kind": "background_task", "work": "subagent" }),
            ),
            (
                json!({ "kind": "command" }),
                json!({ "kind": "background_command" }),
            ),
            (json!({ "kind": "monitor" }), json!({ "kind": "monitor" })),
            (
                json!({ "kind": "background_task" }),
                json!({ "kind": "background_task" }),
            ),
            (
                json!({ "kind": "delegated_task", "taskIds": ["task-1"], "childThreadId": "child" }),
                json!({ "kind": "delegated_task", "taskIds": ["task-1"], "childThreadId": "child" }),
            ),
        ];
        for (source, expected_wire) in &cases {
            let item: OrchestrationV2TurnItem =
                serde_json::from_value(notification_item_json(source.clone())).unwrap();
            let wire = serde_json::to_value(&item).unwrap();
            assert_eq!(wire["source"], *expected_wire);
            // Current clients read the specific kind back.
            let redecoded: OrchestrationV2TurnItem = serde_json::from_value(wire).unwrap();
            assert_eq!(
                serde_json::to_value(&redecoded).unwrap(),
                serde_json::to_value(&item).unwrap()
            );
        }
    }

    #[test]
    fn notification_source_from_newer_server_decodes_generic() {
        let item: OrchestrationV2TurnItem = serde_json::from_value(notification_item_json(json!({
            "kind": "workflow",
            "workflowId": "wf-1",
        })))
        .unwrap();
        let OrchestrationV2TurnItem::Notification { source, .. } = &item else {
            panic!("expected a notification item");
        };
        assert_eq!(source, &OrchestrationV2NotificationSource::BackgroundTask);
    }

    #[test]
    fn background_task_rosters_decode_kinds_and_fallbacks() {
        let provider_thread: OrchestrationV2ProviderThreadJson = serde_json::from_value(json!({
            "id": "provider-thread-1",
            "driver": "claude",
            "providerInstanceId": "claudeAgent",
            "providerSessionId": null,
            "appThreadId": "thread-1",
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": 1,
            "lastRunOrdinal": 1,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": NOW,
            "updatedAt": NOW,
            "pendingBackgroundTasks": [
                { "taskId": "bg-1", "description": "Background sleep", "taskType": "local_bash" },
                { "taskId": "bg-2" },
                { "taskId": "bg-3", "description": "Nightly", "kind": "workflow", "schedule": "0 3 * * *" },
                { "taskId": "bg-4", "description": "npm test", "kind": "command" },
                { "taskId": "bg-5", "kind": "subagent", "childThreadId": "thread-child" },
            ],
        }))
        .unwrap();
        let expected = vec![
            OrchestrationV2PendingBackgroundTask::BackgroundTask {
                task_id: TrimmedNonEmptyString::new("bg-1").unwrap(),
                description: Some(TrimmedNonEmptyString::new("Background sleep").unwrap()),
            },
            OrchestrationV2PendingBackgroundTask::BackgroundTask {
                task_id: TrimmedNonEmptyString::new("bg-2").unwrap(),
                description: None,
            },
            OrchestrationV2PendingBackgroundTask::BackgroundTask {
                task_id: TrimmedNonEmptyString::new("bg-3").unwrap(),
                description: Some(TrimmedNonEmptyString::new("Nightly").unwrap()),
            },
            OrchestrationV2PendingBackgroundTask::Command {
                task_id: TrimmedNonEmptyString::new("bg-4").unwrap(),
                description: Some(TrimmedNonEmptyString::new("npm test").unwrap()),
            },
            OrchestrationV2PendingBackgroundTask::Subagent {
                task_id: TrimmedNonEmptyString::new("bg-5").unwrap(),
                description: None,
                child_thread_id: Some(ThreadId::new("thread-child").unwrap()),
            },
        ];
        assert_eq!(provider_thread.pending_background_tasks, expected);

        // The fallback is decode-only: what was decoded encodes as its known member.
        let encoded = serde_json::to_value(&provider_thread).unwrap();
        let redecoded: OrchestrationV2ProviderThreadJson =
            serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&redecoded).unwrap()["pendingBackgroundTasks"],
            encoded["pendingBackgroundTasks"]
        );
        // A malformed known kind still fails.
        assert!(
            serde_json::from_value::<OrchestrationV2ProviderThreadJson>(merge(
                encoded,
                json!({ "pendingBackgroundTasks": [{ "taskId": "", "kind": "command" }] }),
            ))
            .is_err()
        );
    }

    #[test]
    fn provider_thread_defaults_empty_roster_and_null_metadata() {
        let provider_thread: OrchestrationV2ProviderThreadJson = serde_json::from_value(json!({
            "id": "provider-thread-1",
            "driver": "claude",
            "providerInstanceId": "claudeAgent",
            "providerSessionId": "provider-session-1",
            "appThreadId": "thread-1",
            "ownerNodeId": null,
            "nativeThreadRef": { "driver": "claude", "nativeId": "native-session-1", "strength": "strong" },
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": 1,
            "lastRunOrdinal": 1,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": NOW,
            "updatedAt": NOW,
        }))
        .unwrap();
        assert!(provider_thread.pending_background_tasks.is_empty());
        assert_eq!(provider_thread.native_metadata, None);
        assert_eq!(provider_thread.goal, None);
        // (Upstream also asserts contextUsage is null here; that field is
        // dropped until providerRuntime lands — see the module note.)

        let runtime_thread: OrchestrationV2ProviderThread = serde_json::from_value(json!({
            "id": "provider-thread-2",
            "driver": "claude",
            "providerInstanceId": "claudeAgent",
            "providerSessionId": null,
            "appThreadId": "thread-2",
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": null,
            "lastRunOrdinal": null,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": NOW,
            "updatedAt": NOW,
        }))
        .unwrap();
        assert!(runtime_thread.pending_background_tasks.is_empty());
        assert_eq!(runtime_thread.native_metadata, None);
        assert_eq!(runtime_thread.goal, None);
    }

    #[test]
    fn thread_shell_defaults_empty_roster() {
        let shell: OrchestrationV2ThreadShell = serde_json::from_value(json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": "thread-1",
            "projectId": "project-1",
            "title": "Thread",
            "providerInstanceId": "claudeAgent",
            "modelSelection": { "instanceId": "claudeAgent", "model": "claude-sonnet" },
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1" },
            "forkedFrom": null,
            "activeProviderThreadId": "provider-thread-1",
            "latestRunId": "run-1",
            "activeRunId": null,
            "status": "completed",
            "pendingRuntimeRequest": null,
            "latestVisibleMessage": null,
            "latestUserMessageAt": null,
            "hasActionableProposedPlan": false,
            "itemCount": 0,
            "visibleItemCount": 0,
            "createdAt": NOW,
            "updatedAt": NOW,
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "deletedAt": null,
        }))
        .unwrap();
        assert!(shell.pending_background_tasks.is_empty());
        assert!(shell.provider_instance_history.is_empty());
    }

    #[test]
    fn limit_recovery_updates_require_a_choice() {
        let identity = json!({ "runId": "run:limited", "resetAt": "2026-09-20T21:00:00.000Z" });
        let err = serde_json::from_value::<OrchestrationV2LimitRecoveryUpdate>(identity.clone())
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("must include autoResume or snooze")
        );
        for choice in [
            json!({ "autoResume": true }),
            json!({ "autoResume": false }),
            json!({ "snooze": true }),
            json!({ "snooze": false }),
            json!({ "autoResume": true, "snooze": false }),
        ] {
            let mut value = identity.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(choice.as_object().unwrap().clone());
            let decoded: OrchestrationV2LimitRecoveryUpdate =
                serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(&decoded).unwrap(),
                value,
                "choice {choice}"
            );
        }
    }

    #[test]
    fn latest_provider_turn_for_attempt_returns_highest_ordinal() {
        struct Turn {
            id: &'static str,
            run_attempt_id: Option<RunAttemptId>,
            ordinal: PositiveInt,
        }
        impl OrchestrationV2ProviderTurnLike for Turn {
            fn run_attempt_id(&self) -> Option<&RunAttemptId> {
                self.run_attempt_id.as_ref()
            }
            fn ordinal(&self) -> PositiveInt {
                self.ordinal
            }
        }
        let turns = [
            Turn {
                id: "first",
                run_attempt_id: Some(RunAttemptId::new("goal-attempt").unwrap()),
                ordinal: PositiveInt::new(3).unwrap(),
            },
            Turn {
                id: "other",
                run_attempt_id: Some(RunAttemptId::new("other-attempt").unwrap()),
                ordinal: PositiveInt::new(9).unwrap(),
            },
            Turn {
                id: "last",
                run_attempt_id: Some(RunAttemptId::new("goal-attempt").unwrap()),
                ordinal: PositiveInt::new(5).unwrap(),
            },
            Turn {
                id: "subagent",
                run_attempt_id: None,
                ordinal: PositiveInt::new(7).unwrap(),
            },
        ];
        let latest = latest_provider_turn_for_attempt(
            &turns,
            Some(&RunAttemptId::new("goal-attempt").unwrap()),
        );
        assert_eq!(latest.unwrap().id, "last");
        assert!(latest_provider_turn_for_attempt(&turns, None).is_none());
    }

    #[test]
    fn turn_item_tags_are_known() {
        // One sample per known member, decoded from wire JSON: the serialized
        // tags must equal `known_tags` as a set, or the forward-compatible
        // array codec would silently drop a member.
        let samples = [
            (
                "notification",
                json!({
                    "source": { "kind": "command" },
                    "outcome": "completed",
                    "summary": "Done",
                }),
            ),
            (
                "user_message",
                json!({
                    "createdBy": "user",
                    "creationSource": "web",
                    "messageId": "message-1",
                    "inputIntent": "turn_start",
                    "text": "Hello",
                    "attachments": [],
                }),
            ),
            (
                "assistant_message",
                json!({ "messageId": "message-1", "text": "Hi", "streaming": false }),
            ),
            (
                "reasoning",
                json!({ "text": "Thinking", "streaming": false }),
            ),
            (
                "proposed_plan",
                json!({ "planId": "plan-1", "markdown": "# Plan", "streaming": false }),
            ),
            (
                "todo_list",
                json!({
                    "planId": "plan-1",
                    "steps": [{ "id": "step-1", "text": "Step", "status": "pending" }],
                }),
            ),
            (
                "user_input_request",
                json!({
                    "requestId": "request-1",
                    "questions": [{
                        "id": "question-1",
                        "header": "Header",
                        "question": "Proceed?",
                        "options": [{ "label": "Yes", "description": "Go ahead" }],
                    }],
                }),
            ),
            ("file_change", json!({ "fileName": "package.json" })),
            ("command_execution", json!({ "input": "bun test" })),
            ("file_search", json!({})),
            ("web_search", json!({})),
            (
                "approval_request",
                json!({ "requestId": "request-1", "requestKind": "command" }),
            ),
            (
                "checkpoint",
                json!({
                    "checkpointId": "checkpoint-1",
                    "scopeId": "scope-1",
                    "files": [{ "path": "a.ts", "kind": "edited", "additions": 1, "deletions": 0 }],
                }),
            ),
            ("run_interrupt_request", json!({ "message": "Stop" })),
            ("run_interrupt_result", json!({ "message": "Stopped" })),
            ("system_notice", json!({ "message": "Noted" })),
            (
                "error",
                json!({
                    "failure": {
                        "class": "validation_error",
                        "message": "Invalid",
                        "code": "invalid_request",
                        "retryable": false,
                    },
                }),
            ),
            ("compaction", json!({})),
            (
                "handoff",
                json!({
                    "contextHandoffId": "handoff-1",
                    "fromProviderThreadIds": ["provider-thread-1"],
                    "toProviderThreadId": "provider-thread-2",
                    "fromProviderInstanceIds": ["codex"],
                    "toProviderInstanceId": "claudeAgent",
                    "strategy": "delta_since_target_last_seen",
                }),
            ),
            (
                "fork",
                json!({
                    "source": { "type": "node", "nodeId": "node-1" },
                    "targetThreadId": "thread-2",
                }),
            ),
            (
                "thread_created",
                json!({
                    "targetThreadId": "thread-2",
                    "targetProviderInstanceId": "claudeAgent",
                    "targetModel": "claude-sonnet-4-6",
                }),
            ),
            (
                "secret_request",
                json!({ "label": "Token", "reason": "deploy", "secretStatus": "pending" }),
            ),
            (
                "subagent",
                json!({
                    "subagentId": "node-1",
                    "origin": "provider_native",
                    "driver": "codex",
                    "providerInstanceId": "codex",
                    "prompt": "Look",
                }),
            ),
            ("dynamic_tool", json!({ "input": { "query": "fixture" } })),
        ];
        let items: Vec<OrchestrationV2TurnItem> = samples
            .iter()
            .map(|(kind, extra)| {
                serde_json::from_value(turn_item_json(kind, extra.clone()))
                    .unwrap_or_else(|error| panic!("{kind} sample must decode: {error}"))
            })
            .collect();
        crate::base_schemas::assert_known_tags(&items);
    }

    #[test]
    fn is_provider_native_subagent_thread_requires_provider_source() {
        let mut thread: OrchestrationV2AppThread =
            serde_json::from_value(app_thread_json()).unwrap();
        assert!(!is_provider_native_subagent_thread(&thread));
        thread.creation_source = OrchestrationV2CreationSource::Provider;
        assert!(!is_provider_native_subagent_thread(&thread));
        thread.lineage.relationship_to_parent = Some(OrchestrationV2LineageRelationship::Subagent);
        assert!(is_provider_native_subagent_thread(&thread));
    }

    #[test]
    fn work_started_at_falls_back_to_requested_at() {
        let run: OrchestrationV2Run = serde_json::from_value(json!({
            "id": "run-1",
            "threadId": "thread-1",
            "ordinal": 1,
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
            "providerThreadId": null,
            "userMessageId": "message-1",
            "rootNodeId": null,
            "activeAttemptId": null,
            "status": "queued",
            "requestedAt": NOW,
            "startedAt": null,
            "completedAt": null,
            "checkpointId": null,
            "contextHandoffId": null,
        }))
        .unwrap();
        assert!(orchestration_v2_run_work_started_at(&run) == &run.requested_at);
        assert!(is_orchestration_v2_work_active(
            OrchestrationV2ExecutionNodeStatus::Waiting
        ));
        assert!(!is_orchestration_v2_work_active(
            OrchestrationV2ExecutionNodeStatus::Idle
        ));
    }
}
