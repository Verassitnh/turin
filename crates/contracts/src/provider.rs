//! Port of upstream/packages/contracts/src/provider.ts

use crate::base_schemas::{
    ApprovalRequestId, EventId, IsoDateTime, ProviderItemId, ThreadId, TrimmedNonEmptyString,
    TurnId,
};
use crate::chat_attachment::{ChatAttachment, PROVIDER_SEND_TURN_MAX_INPUT_CHARS};
use crate::model_selection::ModelSelection;
use crate::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use crate::provider_policy::{
    ProviderApprovalDecision, ProviderApprovalPolicy, ProviderInteractionMode, ProviderRequestKind,
    ProviderSandboxMode, ProviderUserInputAnswers, RuntimeMode, UserInputAttachments,
};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderSessionStatus {
    Connecting,
    Ready,
    Running,
    Error,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub provider: ProviderDriverKind,
    // Optional during the driver/instance migration. Once every producer
    // populates it, routing flips to instance-id-only and the legacy
    // `provider` field is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<ProviderInstanceId>,
    pub status: ProviderSessionStatus,
    pub runtime_mode: RuntimeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TrimmedNonEmptyString>,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cursor: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<TurnId>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionStartInput {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderDriverKind>,
    // See ProviderSession for the migration story.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cursor: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ProviderApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<ProviderSandboxMode>,
    pub runtime_mode: RuntimeMode,
}

mod attachment_bounds {
    use super::*;
    use crate::chat_attachment::get_provider_attachment_limit_error;

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<ChatAttachment>>, D::Error> {
        let attachments = Option::<Vec<ChatAttachment>>::deserialize(d)?;
        if let Some(attachments) = &attachments {
            if let Some(error) = get_provider_attachment_limit_error(attachments) {
                return Err(serde::de::Error::custom(error));
            }
        }
        Ok(attachments)
    }

    pub fn input_text<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<TrimmedNonEmptyString>, D::Error> {
        let value = <Option<TrimmedNonEmptyString> as serde::Deserialize>::deserialize(d)?;
        if let Some(text) = &value {
            if text.as_str().len() > PROVIDER_SEND_TURN_MAX_INPUT_CHARS {
                return Err(serde::de::Error::custom(format!(
                    "turn input can be at most {PROVIDER_SEND_TURN_MAX_INPUT_CHARS} chars"
                )));
            }
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSendTurnInput {
    pub thread_id: ThreadId,
    /// Internal recovery signal. Allows an empty turn only for adapters that
    /// explicitly support promptless continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "attachment_bounds::input_text"
    )]
    pub input: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "attachment_bounds::deserialize"
    )]
    pub attachments: Option<Vec<ChatAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_mode: Option<ProviderInteractionMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurnStartResult {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cursor: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInterruptTurnInput {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStopSessionInput {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRespondToRequestInput {
    pub thread_id: ThreadId,
    pub request_id: ApprovalRequestId,
    pub decision: ProviderApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRespondToUserInputInput {
    pub thread_id: ThreadId,
    pub request_id: ApprovalRequestId,
    pub answers: ProviderUserInputAnswers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_by_question_id: Option<UserInputAttachments>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUploadFeedbackInput {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUploadFeedbackResult {
    pub feedback_id: TrimmedNonEmptyString,
}

/// `ProviderUploadFeedbackError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProviderUploadFeedbackError {
    #[serde(rename = "ProviderUploadFeedbackError")]
    #[error("Failed to upload feedback for thread {thread_id}.")]
    UploadFeedback {
        thread_id: ThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderEventKind {
    Session,
    Notification,
    Request,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEvent {
    pub id: EventId,
    pub kind: ProviderEventKind,
    pub provider: ProviderDriverKind,
    // See ProviderSession for the migration story.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<ProviderInstanceId>,
    pub thread_id: ThreadId,
    pub created_at: IsoDateTime,
    pub method: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ProviderItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<ApprovalRequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_kind: Option<ProviderRequestKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}
