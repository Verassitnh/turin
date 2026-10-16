//! Port of upstream/apps/web/src/components/ChatView.logic.ts (blob preview URLs).

use crate::components::chat::timeline::types::{ChatMessage, TimelineEntry, TimelineEntryKind};

pub fn collect_user_message_blob_preview_urls(message: &ChatMessage) -> Vec<String> {
    if !matches!(
        message.role,
        crate::components::chat::timeline::types::MessageRole::User
    ) {
        return Vec::new();
    }
    message
        .attachments
        .iter()
        .filter(|attachment| attachment.is_image())
        .filter_map(|attachment| attachment.preview_path.as_deref())
        .filter(|url| url.starts_with("blob:"))
        .map(str::to_owned)
        .collect()
}

pub fn timeline_has_ephemeral_preview_urls(entries: &[TimelineEntry]) -> bool {
    entries.iter().any(|entry| {
        let TimelineEntryKind::Message { message, .. } = &entry.kind else {
            return false;
        };
        !collect_user_message_blob_preview_urls(message).is_empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::chat::timeline::types::{
        ChatAttachment, ChatMessage, MessageRole, TimelineEntry, TimelineEntryKind,
    };

    fn user_message(preview_path: Option<&str>) -> ChatMessage {
        ChatMessage {
            id: "preview-message".to_owned(),
            role: MessageRole::User,
            text: "Preview".to_owned(),
            attachments: vec![ChatAttachment {
                id: "preview".to_owned(),
                kind: "image".to_owned(),
                name: "preview.png".to_owned(),
                mime_type: "image/png".to_owned(),
                size_bytes: 1,
                preview_path: preview_path.map(str::to_owned),
            }],
            run_id: None,
            streaming: false,
            created_by: None,
            creation_source: None,
            scheduled_task_id: None,
            sender_thread_id: None,
            created_at: "2026-09-10T12:00:00.000Z".to_owned(),
            updated_at: "2026-09-10T12:00:00.000Z".to_owned(),
            input_intent: None,
        }
    }

    #[test]
    fn detects_blob_handoff_previews() {
        let entries = vec![TimelineEntry {
            id: "entry".to_owned(),
            created_at: "2026-09-10T12:00:00.000Z".to_owned(),
            kind: TimelineEntryKind::Message {
                message: user_message(Some("blob:handoff")),
                projected_item: None,
            },
            attempt: None,
        }];
        assert!(timeline_has_ephemeral_preview_urls(&entries));
    }

    #[test]
    fn ignores_https_previews() {
        let entries = vec![TimelineEntry {
            id: "entry".to_owned(),
            created_at: "2026-09-10T12:00:00.000Z".to_owned(),
            kind: TimelineEntryKind::Message {
                message: user_message(Some("https://cdn.example/a.png")),
                projected_item: None,
            },
            attempt: None,
        }];
        assert!(!timeline_has_ephemeral_preview_urls(&entries));
    }
}
