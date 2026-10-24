//! Port of `makeClaudeUserMessageWithAttachments` in
//! upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts.
//!
//! Images go to Claude as base64 `image` blocks; every attachment's saved path
//! is also appended to the text so tools can read or copy the actual file.

use std::collections::HashSet;
use std::path::Path;

use base64::Engine as _;
use serde_json::{Value, json};
use turin_contracts::chat_attachment::ChatAttachment;
use turin_provider_core::attachment_paths::resolve_attachment_path;
use turin_provider_core::attachment_prompt::provider_message_text_with_attachment_paths;

use crate::claude_skill_dispatch::{dispatch_content_blocks, plan_claude_skill_dispatch};

const SUPPORTED_CLAUDE_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/gif", "image/jpeg", "image/png", "image/webp"];

/// Content blocks for a user message, or `None` when plain text will do.
pub fn claude_user_content_blocks(
    text: &str,
    attachments: &[ChatAttachment],
    attachments_dir: Option<&Path>,
    skill_names: &HashSet<String>,
) -> Result<Option<Vec<Value>>, String> {
    if attachments.is_empty() {
        return Ok(dispatch_content_blocks(text, skill_names));
    }
    let resolve =
        |attachment: &ChatAttachment| resolve_attachment_path(attachments_dir?, attachment);
    // The model's tools cannot dereference inlined pixels; the path lets a
    // turn copy the actual file (the query grants the attachments dir).
    let text = provider_message_text_with_attachment_paths(text, attachments, resolve);
    let dispatch = plan_claude_skill_dispatch(&text, skill_names);
    let mut blocks = Vec::new();
    if let Some(leading) = dispatch.as_ref().and_then(|d| d.leading_text.clone()) {
        blocks.push(json!({"type": "text", "text": leading}));
    }
    for attachment in attachments {
        let ChatAttachment::Image(image) = attachment else {
            continue;
        };
        let mime_type = image.mime_type.as_str();
        if !SUPPORTED_CLAUDE_IMAGE_MIME_TYPES.contains(&mime_type) {
            return Err(format!(
                "Unsupported Claude image attachment type '{mime_type}'"
            ));
        }
        let path = resolve(attachment)
            .ok_or_else(|| format!("Invalid attachment id '{}'", image.id.as_str()))?;
        let bytes = std::fs::read(&path)
            .map_err(|_| format!("Failed to read attachment '{}'", image.id.as_str()))?;
        blocks.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": mime_type,
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        }));
    }
    // Images go before the command block: a text block after them still
    // expands, a command block followed by an image does not.
    if let Some(dispatch) = dispatch {
        blocks.push(json!({"type": "text", "text": dispatch.command_text}));
    } else if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    Ok(Some(blocks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: &str, mime_type: &str) -> ChatAttachment {
        serde_json::from_value(json!({
            "type": "image",
            "id": id,
            "name": "pasted-image.png",
            "mimeType": mime_type,
            "sizeBytes": 3,
        }))
        .expect("image attachment")
    }

    #[test]
    fn text_only_messages_stay_plain() {
        let blocks = claude_user_content_blocks("hi", &[], None, &HashSet::new()).unwrap();
        assert_eq!(blocks, None);
    }

    #[test]
    fn images_become_base64_blocks_before_the_text() {
        let dir = tempfile::tempdir().unwrap();
        let attachment = image("thread-1-00000000-0000-4000-8000-000000000001", "image/png");
        let path = resolve_attachment_path(dir.path(), &attachment).expect("path");
        std::fs::write(&path, [1u8, 2, 3]).unwrap();

        let blocks =
            claude_user_content_blocks("Look", &[attachment], Some(dir.path()), &HashSet::new())
                .unwrap()
                .expect("blocks");

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], "AQID");
        let text = blocks[1]["text"].as_str().unwrap();
        assert!(text.starts_with("Look\n\n[Attached image \"pasted-image.png\" is saved at: "));
    }

    #[test]
    fn a_missing_image_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let attachment = image("thread-1-00000000-0000-4000-8000-000000000002", "image/png");
        let error =
            claude_user_content_blocks("Look", &[attachment], Some(dir.path()), &HashSet::new())
                .unwrap_err();
        assert!(error.starts_with("Failed to read attachment"));
    }
}
