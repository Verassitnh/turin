//! Port of upstream/packages/contracts/src/provider.ts

use crate::base_schemas::{
    ApprovalRequestId, EventId, IsoDateTime, ProviderItemId, ThreadId, TrimmedNonEmptyString,
    TurnId,
};
use crate::chat_attachment::{ChatAttachment, PROVIDER_SEND_TURN_MAX_INPUT_CHARS};
use crate::model_selection::ModelSelection;
use crate::provider_instance::{ProviderDriverKind, ProviderInstanceId, optional_unknown};
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
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
            if
            // `Schema.isMaxLength` counts UTF-16 code units.
            text.as_str().encode_utf16().count() > PROVIDER_SEND_TURN_MAX_INPUT_CHARS {
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
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
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProviderUploadFeedbackError {
    #[serde(rename = "ProviderUploadFeedbackError")]
    #[error("Failed to upload feedback for thread {thread_id}.")]
    UploadFeedback {
        thread_id: ThreadId,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "optional_unknown"
        )]
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub payload: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn option_value<'a>(
        selection: &'a ModelSelection,
        id: &str,
    ) -> Option<&'a crate::model::ProviderOptionSelectionValue> {
        selection
            .options
            .as_ref()?
            .iter()
            .find(|option| option.id.as_str() == id)
            .map(|option| &option.value)
    }

    fn start(value: serde_json::Value) -> Result<ProviderSessionStartInput, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn send(value: serde_json::Value) -> Result<ProviderSendTurnInput, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn session_start_accepts_codex_payload_and_migrates_legacy_selection() {
        let parsed = start(json!({
            "threadId": "thread-1",
            "provider": "codex",
            "cwd": "/tmp/workspace",
            "modelSelection": {
                "provider": "codex",
                "model": "gpt-5.3-codex",
                "options": [
                    {"id": "reasoningEffort", "value": "high"},
                    {"id": "fastMode", "value": true},
                ],
            },
            "runtimeMode": "full-access",
        }))
        .unwrap();
        assert_eq!(parsed.runtime_mode, RuntimeMode::FullAccess);
        let selection = parsed.model_selection.unwrap();
        assert_eq!(selection.instance_id.as_str(), "codex");
        assert_eq!(selection.model.as_str(), "gpt-5.3-codex");
        assert_eq!(
            option_value(&selection, "reasoningEffort"),
            Some(&crate::model::ProviderOptionSelectionValue::Str(
                TrimmedNonEmptyString::new("high").unwrap()
            ))
        );
        assert_eq!(
            option_value(&selection, "fastMode"),
            Some(&crate::model::ProviderOptionSelectionValue::Bool(true))
        );
    }

    #[test]
    fn session_start_rejects_missing_runtime_mode() {
        assert!(start(json!({"threadId": "thread-1", "provider": "codex"})).is_err());
    }

    #[test]
    fn session_start_accepts_claude_cursor_and_fork_drivers() {
        for (provider, model) in [
            ("claudeAgent", "claude-sonnet-4-6"),
            ("cursor", "composer-2"),
        ] {
            let parsed = start(json!({
                "threadId": "thread-1",
                "provider": provider,
                "cwd": "/tmp/workspace",
                "modelSelection": {"provider": provider, "model": model},
                "runtimeMode": "full-access",
            }))
            .unwrap();
            assert_eq!(parsed.provider.unwrap().as_str(), provider);
            assert_eq!(
                parsed.model_selection.unwrap().instance_id.as_str(),
                provider
            );
        }
        let fork = start(json!({
            "threadId": "thread-1",
            "provider": "ollama",
            "providerInstanceId": "ollama_local",
            "runtimeMode": "full-access",
            "modelSelection": {"instanceId": "ollama_local", "model": "llama3.3"},
        }))
        .unwrap();
        assert_eq!(fork.provider.unwrap().as_str(), "ollama");
        assert_eq!(fork.provider_instance_id.unwrap().as_str(), "ollama_local");
    }

    #[test]
    fn provider_instance_id_is_optional_and_validated() {
        let legacy = start(json!({
            "threadId": "thread-1", "provider": "codex", "runtimeMode": "full-access",
        }))
        .unwrap();
        assert_eq!(legacy.provider_instance_id, None);
        assert!(
            !serde_json::to_string(&legacy)
                .unwrap()
                .contains("providerInstanceId")
        );
        let bad = start(json!({
            "threadId": "thread-1", "provider": "codex",
            "providerInstanceId": "1bad", "runtimeMode": "full-access",
        }));
        assert!(bad.is_err());
    }

    fn image(index: usize) -> serde_json::Value {
        json!({
            "type": "image", "id": format!("image-{index}"), "name": "image.png",
            "mimeType": "image/png", "sizeBytes": 1,
        })
    }

    #[test]
    fn send_turn_accepts_100_attachments_and_rejects_101() {
        let mut attachments: Vec<_> = (0..100).map(image).collect();
        let parsed = send(json!({"threadId": "thread-1", "attachments": attachments})).unwrap();
        assert_eq!(parsed.attachments.unwrap().len(), 100);
        attachments.push(image(0));
        assert!(send(json!({"threadId": "thread-1", "attachments": attachments})).is_err());
    }

    #[test]
    fn send_turn_caps_total_image_bytes_without_charging_videos() {
        for kind in ["image", "file"] {
            let make = |size: u64| {
                json!({
                    "type": kind, "id": "image", "name": "image.png",
                    "mimeType": "image/png", "sizeBytes": size,
                })
            };
            let video = json!({
                "type": "file", "id": "video", "name": "video.mp4",
                "mimeType": "video/mp4", "sizeBytes": 50 * 1024 * 1024,
            });
            let mut ok: Vec<_> = (0..8).map(|_| make(10 * 1024 * 1024)).collect();
            ok.push(video);
            assert_eq!(
                send(json!({"threadId": "thread-1", "attachments": ok}))
                    .unwrap()
                    .attachments
                    .unwrap()
                    .len(),
                9
            );
            let mut over: Vec<_> = (0..8).map(|_| make(10 * 1024 * 1024)).collect();
            over.push(make(1));
            let error = send(json!({"threadId": "thread-1", "attachments": over}))
                .unwrap_err()
                .to_string();
            assert!(error.contains("80 MiB"), "{kind}: {error}");
        }
    }

    #[test]
    fn send_turn_accepts_model_selections() {
        let codex = send(json!({
            "threadId": "thread-1",
            "modelSelection": {"provider": "codex", "model": "gpt-5.3-codex", "options": [
                {"id": "reasoningEffort", "value": "xhigh"}, {"id": "fastMode", "value": true}]},
        }))
        .unwrap();
        let selection = codex.model_selection.unwrap();
        assert_eq!(selection.instance_id.as_str(), "codex");
        assert_eq!(
            option_value(&selection, "reasoningEffort"),
            Some(&crate::model::ProviderOptionSelectionValue::Str(
                TrimmedNonEmptyString::new("xhigh").unwrap()
            ))
        );
        let claude = send(json!({
            "threadId": "thread-1",
            "modelSelection": {"provider": "claudeAgent", "model": "claude-sonnet-4-6", "options": [
                {"id": "effort", "value": "ultrathink"}]},
        }))
        .unwrap();
        assert_eq!(
            claude.model_selection.unwrap().instance_id.as_str(),
            "claudeAgent"
        );
    }

    #[test]
    fn send_turn_input_is_trimmed_non_empty_and_capped_in_utf16_units() {
        assert!(send(json!({"threadId": "t", "input": "   "})).is_err());
        assert_eq!(
            send(json!({"threadId": "t", "input": "  hi  "}))
                .unwrap()
                .input
                .unwrap()
                .as_str(),
            "hi"
        );
        let at_cap = "a".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS);
        assert!(send(json!({"threadId": "t", "input": at_cap})).is_ok());
        assert!(send(json!({"threadId": "t", "input": "a".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS + 1)})).is_err());
        // 60_001 astral chars are 120_002 UTF-16 units but only 60_001 chars.
        let astral = "\u{1F600}".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS / 2 + 1);
        assert!(send(json!({"threadId": "t", "input": astral})).is_err());
    }

    #[test]
    fn feedback_input_result_and_error() {
        let with_reason: ProviderUploadFeedbackInput = serde_json::from_value(
            json!({"threadId": "thread-1", "reason": "The agent stopped early."}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&with_reason).unwrap(),
            json!({"threadId": "thread-1", "reason": "The agent stopped early."})
        );
        let bare: ProviderUploadFeedbackInput =
            serde_json::from_value(json!({"threadId": "thread-1"})).unwrap();
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"threadId": "thread-1"})
        );
        let result: ProviderUploadFeedbackResult =
            serde_json::from_value(json!({"feedbackId": "provider-thread-1"})).unwrap();
        assert_eq!(result.feedback_id.as_str(), "provider-thread-1");

        let error = ProviderUploadFeedbackError::UploadFeedback {
            thread_id: ThreadId::new("thread-1").unwrap(),
            cause: Some(json!("provider request secret")),
        };
        assert_eq!(
            error.to_string(),
            "Failed to upload feedback for thread thread-1."
        );
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"_tag": "ProviderUploadFeedbackError", "threadId": "thread-1", "cause": "provider request secret"})
        );
    }

    #[test]
    fn session_and_event_carry_instance_routing() {
        let session: ProviderSession = serde_json::from_value(json!({
            "provider": "ollama", "providerInstanceId": "ollama_local", "status": "ready",
            "runtimeMode": "full-access", "threadId": "thread-1",
            "createdAt": "2024-01-01T00:00:00Z", "updatedAt": "2024-01-01T00:00:00Z",
        }))
        .unwrap();
        assert_eq!(session.provider.as_str(), "ollama");
        assert_eq!(
            session.provider_instance_id.unwrap().as_str(),
            "ollama_local"
        );
        // IsoDateTime is a plain string, passed through untouched.
        assert_eq!(session.created_at, "2024-01-01T00:00:00Z");

        let event: ProviderEvent = serde_json::from_value(json!({
            "id": "event-1", "kind": "notification", "provider": "codex",
            "providerInstanceId": "codex_personal", "threadId": "thread-1",
            "createdAt": "2024-01-01T00:00:00Z", "method": "session.created",
        }))
        .unwrap();
        assert_eq!(
            event.provider_instance_id.unwrap().as_str(),
            "codex_personal"
        );
    }

    #[test]
    fn unknown_payloads_keep_explicit_null() {
        let result: ProviderTurnStartResult =
            serde_json::from_value(json!({"threadId": "t", "turnId": "u", "resumeCursor": null}))
                .unwrap();
        assert_eq!(result.resume_cursor, Some(serde_json::Value::Null));
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"threadId": "t", "turnId": "u", "resumeCursor": null})
        );
        let absent: ProviderTurnStartResult =
            serde_json::from_value(json!({"threadId": "t", "turnId": "u"})).unwrap();
        assert_eq!(absent.resume_cursor, None);
        let event: ProviderEvent = serde_json::from_value(json!({
            "id": "e", "kind": "error", "provider": "codex", "threadId": "t",
            "createdAt": "x", "method": "m", "payload": null, "textDelta": "",
        }))
        .unwrap();
        assert_eq!(event.payload, Some(serde_json::Value::Null));
        assert_eq!(event.text_delta.as_deref(), Some(""));
    }

    #[test]
    fn session_status_and_event_kind_reject_unknown_literals() {
        assert!(serde_json::from_value::<ProviderSessionStatus>(json!("paused")).is_err());
        assert!(serde_json::from_value::<ProviderEventKind>(json!("other")).is_err());
    }
}
