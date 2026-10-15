//! Port of upstream/apps/server/src/provider/userInputAttachments.ts
//! (`appendUserInputAttachmentPaths`), the only part the write pipeline needs.

use super::attachment_claims::AttachmentClaimError;
use super::attachment_store::{attachment_fields, resolve_attachment_path};
use serde_json::Value;
use std::path::Path;
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::provider_policy::{
    ProviderUserInputAnswers, UserInputAttachment, UserInputAttachments,
};

/// A question attachment as the general attachment type (same wire shape).
pub fn to_chat_attachment(attachment: &UserInputAttachment) -> Option<ChatAttachment> {
    serde_json::from_value(serde_json::to_value(attachment).ok()?).ok()
}

pub fn to_user_input_attachment(attachment: &ChatAttachment) -> Option<UserInputAttachment> {
    serde_json::from_value(serde_json::to_value(attachment).ok()?).ok()
}

fn quote_reference(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

fn attachment_type(attachment: &ChatAttachment) -> &'static str {
    if attachment_fields(attachment).is_image {
        "image"
    } else {
        "file"
    }
}

/// Keeps provider answer protocols unchanged; the appended paths refer to
/// files on the provider's machine.
pub async fn append_user_input_attachment_paths(
    answers: &ProviderUserInputAnswers,
    attachments_by_question_id: Option<&UserInputAttachments>,
    attachments_dir: &Path,
) -> Result<ProviderUserInputAnswers, AttachmentClaimError> {
    let mut answers = answers.clone();
    for (question_id, attachments) in attachments_by_question_id.into_iter().flatten() {
        if attachments.is_empty() {
            continue;
        }
        let mut references = Vec::new();
        for attachment in attachments {
            let chat = to_chat_attachment(attachment)
                .ok_or_else(|| AttachmentClaimError::new("Unreadable attachment."))?;
            let fields = attachment_fields(&chat);
            let name = fields.name.to_owned();
            let unavailable = || {
                AttachmentClaimError::new(format!(
                    "Attachment '{name}' is no longer available. Attach it again."
                ))
            };
            let Some(path) = resolve_attachment_path(attachments_dir, &chat) else {
                return Err(unavailable());
            };
            if !tokio::fs::try_exists(&path)
                .await
                .map_err(|cause| AttachmentClaimError {
                    message: format!("Could not access attachment '{name}'."),
                    cause: Some(Box::new(cause)),
                })?
            {
                return Err(unavailable());
            }
            references.push(format!(
                "Attached {} {}: {}",
                attachment_type(&chat),
                quote_reference(&name),
                quote_reference(&path.to_string_lossy())
            ));
        }
        let text = references.join("\n");
        let merged = match answers.get(question_id) {
            Some(Value::Array(existing)) => {
                let mut next = existing.clone();
                next.push(Value::String(text));
                Value::Array(next)
            }
            Some(Value::String(existing)) if !existing.is_empty() => {
                Value::String(format!("{existing}\n\n{text}"))
            }
            _ => Value::String(text),
        };
        answers.insert(question_id.clone(), merged);
    }
    Ok(answers)
}
