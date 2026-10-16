//! Stage question attachment drafts and include them in `runtime-request.respond`.

use std::collections::HashMap;

use gpui_kit::App;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use turin_contracts::ThreadId;

use crate::app_runtime::write::commands::{CommandError, Commands};
use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};
use crate::state::question_attachments::question_attachment_draft_id;

use super::question_attachment_drafts::QuestionAttachmentDrafts;
use super::timeline::types::{PendingUserInput, ResponseCapability};

pub struct QuestionDraftAttachments {
    images: Vec<ComposerImageAttachment>,
    files: Vec<ComposerFileAttachment>,
}

pub fn pending_request_attachments_enabled(
    capability: crate::state::thread_requests::UserInputResponseCapability,
) -> bool {
    capability != crate::state::thread_requests::UserInputResponseCapability::NotResumable
}

pub fn pending_user_input_attachments_enabled(prompt: &PendingUserInput) -> bool {
    prompt.response_capability != ResponseCapability::NotResumable
}

pub fn pending_user_input_attachments_disabled_reason(prompt: &PendingUserInput) -> Option<&'static str> {
    if prompt.response_capability == ResponseCapability::NotResumable {
        Some("This question cannot accept attachments.")
    } else {
        None
    }
}

pub fn load_question_drafts(
    cx: &App,
    environment_id: &str,
    thread_id: &str,
    request_id: &str,
    question_ids: &[String],
) -> HashMap<String, QuestionDraftAttachments> {
    let drafts = cx.default_global::<QuestionAttachmentDrafts>();
    let mut loaded = HashMap::new();
    for question_id in question_ids {
        let key = question_attachment_draft_id(environment_id, thread_id, request_id, question_id);
        let Some(draft) = drafts.store.get_composer_draft(&key) else {
            continue;
        };
        if draft.images.is_empty() && draft.files.is_empty() {
            continue;
        }
        loaded.insert(
            question_id.clone(),
            QuestionDraftAttachments {
                images: draft.images.clone(),
                files: draft.files.clone(),
            },
        );
    }
    loaded
}

async fn stage_drafts(
    commands: &Commands,
    by_question: &HashMap<String, QuestionDraftAttachments>,
) -> Result<Map<String, Value>, CommandError> {
    let mut attachments_by_question_id = Map::new();
    for (question_id, draft) in by_question {
        let mut staged = Vec::new();
        for image in &draft.images {
            let attachment = commands
                .stage_attachment(true, &image.name, &image.mime_type, (*image.data).clone())
                .await?;
            staged.push(attachment);
        }
        for file in &draft.files {
            let Some(data) = &file.data else {
                return Err(CommandError::new(format!(
                    "{} has no content to upload. Attach it again.",
                    file.name
                )));
            };
            let attachment = commands
                .stage_attachment(false, &file.name, &file.mime_type, (**data).clone())
                .await?;
            staged.push(attachment);
        }
        if !staged.is_empty() {
            attachments_by_question_id.insert(question_id.clone(), Value::Array(staged));
        }
    }
    Ok(attachments_by_question_id)
}

pub async fn respond_to_pending_user_input_with_attachments(
    commands: &Commands,
    thread_id: &ThreadId,
    request_id: &str,
    answers: IndexMap<String, String>,
    draft_attachments: &HashMap<String, QuestionDraftAttachments>,
) -> Result<(), CommandError> {
    let answers_value = Value::Object(
        answers
            .into_iter()
            .map(|(question, answer)| (question, Value::String(answer)))
            .collect(),
    );
    let attachments_by_question_id = stage_drafts(commands, draft_attachments).await?;
    let attachments = (!attachments_by_question_id.is_empty())
        .then(|| Value::Object(attachments_by_question_id));
    commands
        .respond_to_user_input_request(thread_id, request_id, answers_value, attachments)
        .await
}

pub fn clear_question_attachment_drafts_for_request(
    cx: &mut App,
    environment_id: &str,
    thread_id: &str,
    request_id: &str,
    question_ids: &[String],
) {
    let drafts = cx.default_global::<QuestionAttachmentDrafts>();
    for question_id in question_ids {
        let key = question_attachment_draft_id(environment_id, thread_id, request_id, question_id);
        drafts.store.clear_composer_content(&key);
        drafts.preparation.counts.remove(&key);
    }
    cx.refresh_windows();
}
