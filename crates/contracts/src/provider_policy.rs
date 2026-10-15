//! Port of upstream/packages/contracts/src/providerPolicy.ts

use crate::base_schemas::TrimmedNonEmptyString;
use crate::chat_attachment::{ChatFileAttachment, ChatImageAttachment};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderApprovalPolicy {
    #[serde(rename = "untrusted")]
    Untrusted,
    #[serde(rename = "on-failure")]
    OnFailure,
    #[serde(rename = "on-request")]
    OnRequest,
    #[serde(rename = "never")]
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderSandboxMode {
    #[serde(rename = "read-only")]
    ReadOnly,
    #[serde(rename = "workspace-write")]
    WorkspaceWrite,
    #[serde(rename = "danger-full-access")]
    DangerFullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuntimeMode {
    #[serde(rename = "approval-required")]
    ApprovalRequired,
    #[serde(rename = "auto-accept-edits")]
    AutoAcceptEdits,
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "full-access")]
    FullAccess,
}

pub const DEFAULT_RUNTIME_MODE: RuntimeMode = RuntimeMode::FullAccess;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderInteractionMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "plan")]
    Plan,
}

pub const DEFAULT_PROVIDER_INTERACTION_MODE: ProviderInteractionMode = ProviderInteractionMode::Default;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderRequestKind {
    Command,
    FileRead,
    FileChange,
    McpElicitation,
    Permission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssistantDeliveryMode {
    Buffered,
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderApprovalOption {
    pub decision: ProviderApprovalDecision,
    pub label: TrimmedNonEmptyString,
    /// Provider-supplied caution shown next to the option, such as a prompt injection warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<TrimmedNonEmptyString>,
}

/// `Schema.Record(Schema.String, Schema.Unknown)`: insertion order preserved.
pub type ProviderUserInputAnswers = indexmap::IndexMap<String, serde_json::Value>;

/// One entry of `attachmentsByQuestionId`: an image or file attachment.
/// (The unknown-attachment member is deliberately absent, as upstream.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserInputAttachment {
    Image(ChatImageAttachment),
    File(ChatFileAttachment),
}

/// `Schema.Record(...)` keyed by question id: insertion order preserved.
pub type UserInputAttachments = indexmap::IndexMap<String, Vec<UserInputAttachment>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAttachmentAnswerPayload {
    pub request_id: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_text_by_id: Option<indexmap::IndexMap<String, String>>,
    pub answers: ProviderUserInputAnswers,
    pub attachments_by_question_id: UserInputAttachments,
}
