//! Port of upstream/packages/contracts/src/providerPolicy.ts

use crate::base_schemas::TrimmedNonEmptyString;
use crate::chat_attachment::{
    ChatFileAttachment, ChatImageAttachment, PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
};
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

pub const DEFAULT_PROVIDER_INTERACTION_MODE: ProviderInteractionMode =
    ProviderInteractionMode::Default;

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
    #[serde(deserialize_with = "deserialize_user_input_attachments")]
    pub attachments_by_question_id: UserInputAttachments,
}

/// Decode-time `Schema.isMaxLength(PROVIDER_SEND_TURN_MAX_ATTACHMENTS)` check
/// on every per-question attachment array.
pub fn deserialize_user_input_attachments<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<UserInputAttachments, D::Error> {
    let map = UserInputAttachments::deserialize(d)?;
    for (question_id, attachments) in &map {
        if attachments.len() > PROVIDER_SEND_TURN_MAX_ATTACHMENTS {
            return Err(serde::de::Error::custom(format!(
                "expected a length of at most {PROVIDER_SEND_TURN_MAX_ATTACHMENTS} attachments for question {question_id:?}, got {}",
                attachments.len()
            )));
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn literals_use_exact_wire_names() {
        for (value, wire) in [
            (RuntimeMode::ApprovalRequired, "approval-required"),
            (RuntimeMode::AutoAcceptEdits, "auto-accept-edits"),
            (RuntimeMode::Auto, "auto"),
            (RuntimeMode::FullAccess, "full-access"),
        ] {
            assert_eq!(serde_json::to_value(value).unwrap(), json!(wire));
        }
        for (value, wire) in [
            (ProviderRequestKind::Command, "command"),
            (ProviderRequestKind::FileRead, "file-read"),
            (ProviderRequestKind::FileChange, "file-change"),
            (ProviderRequestKind::McpElicitation, "mcp-elicitation"),
            (ProviderRequestKind::Permission, "permission"),
        ] {
            assert_eq!(serde_json::to_value(value).unwrap(), json!(wire));
        }
        assert_eq!(
            serde_json::to_value(ProviderApprovalDecision::AcceptForSession).unwrap(),
            json!("acceptForSession")
        );
        assert_eq!(DEFAULT_RUNTIME_MODE, RuntimeMode::FullAccess);
        assert_eq!(
            DEFAULT_PROVIDER_INTERACTION_MODE,
            ProviderInteractionMode::Default
        );
        assert!(serde_json::from_value::<ProviderSandboxMode>(json!("nope")).is_err());
    }

    #[test]
    fn approval_option_warning_is_optional_and_trimmed() {
        let option: ProviderApprovalOption = serde_json::from_value(
            json!({"decision":"accept","label":" Yes ","warning":" careful "}),
        )
        .unwrap();
        assert_eq!(option.label.as_str(), "Yes");
        assert_eq!(option.warning.as_ref().unwrap().as_str(), "careful");
        let bare: ProviderApprovalOption =
            serde_json::from_value(json!({"decision":"cancel","label":"No"})).unwrap();
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"decision":"cancel","label":"No"})
        );
        assert!(
            serde_json::from_value::<ProviderApprovalOption>(
                json!({"decision":"cancel","label":"No","warning":"  "})
            )
            .is_err()
        );
    }

    #[test]
    fn answer_payload_round_trips_and_keeps_key_order() {
        let payload: UserInputAttachmentAnswerPayload = serde_json::from_str(
            r#"{"requestId":"r1","questionTextById":{"z":"Z?","a":"A?"},"answers":{"z":1,"a":null},"attachmentsByQuestionId":{}}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"requestId":"r1","questionTextById":{"z":"Z?","a":"A?"},"answers":{"z":1,"a":null},"attachmentsByQuestionId":{}}"#
        );
    }

    #[test]
    fn attachments_per_question_are_capped() {
        let file =
            r#"{"type":"file","id":"a1","name":"n.txt","mimeType":"text/plain","sizeBytes":1}"#;
        let decode = |count: usize| {
            let items = vec![file; count].join(",");
            serde_json::from_str::<UserInputAttachmentAnswerPayload>(&format!(
                r#"{{"requestId":"r","answers":{{}},"attachmentsByQuestionId":{{"q":[{items}]}}}}"#
            ))
        };
        assert!(decode(PROVIDER_SEND_TURN_MAX_ATTACHMENTS).is_ok());
        assert!(decode(PROVIDER_SEND_TURN_MAX_ATTACHMENTS + 1).is_err());
    }
}
