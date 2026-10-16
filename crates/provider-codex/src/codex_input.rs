//! Port of `codexSkillMentionText` and `toCodexInput` in
//! upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts.
//!
//! A turn's user input is the message text (skill sigils rewritten to `$`, with
//! each attachment's saved path appended) followed by one data-URL item per image
//! attachment the provider can read.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use turin_contracts::chat_attachment::ChatAttachment;
use turin_provider_core::attachment_prompt::{
    is_provider_native_image_attachment, provider_message_text_with_attachment_paths,
};
use turin_shared::composer_inline_tokens::collect_sent_prompt_skill_tokens;

/// Maps an attachment to its file on disk, or `None` for an unknown or unsafe id
/// (`resolveAttachmentPath` over the server's attachments directory).
pub use turin_text_generation::codex_text_generation::AttachmentPathResolver;

/// Why a turn's input could not be built. The message is the upstream protocol error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CodexInputError(pub String);

/// The composers let a skill be typed with any currency sigil (`€review`), but
/// Codex only parses `$name` as a skill mention. Rewrite the sigil so the skill
/// runs; currency amounts like `€20` do not match and stay prose.
pub fn codex_skill_mention_text(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut cursor = 0;
    for token in collect_sent_prompt_skill_tokens(text) {
        out.extend_from_slice(&units[cursor..token.start]);
        out.extend(format!("${}", token.value).encode_utf16());
        cursor = token.end;
    }
    out.extend_from_slice(&units[cursor..]);
    String::from_utf16(&out).unwrap_or_else(|_| text.to_owned())
}

fn attachment_id(attachment: &ChatAttachment) -> &str {
    match attachment {
        ChatAttachment::Image(image) => image.id.as_str(),
        ChatAttachment::File(file) => file.id.as_str(),
        ChatAttachment::Unknown(unknown) => unknown.id.as_str(),
    }
}

/// `resolveCodexAttachment`: the image as a `data:` URL item.
async fn image_item(
    attachment: &ChatAttachment,
    resolve: Option<&AttachmentPathResolver>,
) -> Result<Value, CodexInputError> {
    let id = attachment_id(attachment);
    let Some(path) = resolve.and_then(|resolve| resolve(attachment)) else {
        return Err(CodexInputError(format!("Invalid attachment id '{id}'")));
    };
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| CodexInputError(format!("Failed to read attachment '{id}'.")))?;
    let ChatAttachment::Image(image) = attachment else {
        return Err(CodexInputError(format!("Invalid attachment id '{id}'")));
    };
    Ok(json!({
        "type": "image",
        "url": format!("data:{};base64,{}", image.mime_type.as_str(), STANDARD.encode(bytes)),
    }))
}

/// `toCodexInput`.
pub async fn to_codex_input(
    text: &str,
    attachments: &[ChatAttachment],
    resolve: Option<&AttachmentPathResolver>,
) -> Result<Vec<Value>, CodexInputError> {
    let mut items = Vec::new();
    let text = provider_message_text_with_attachment_paths(
        &codex_skill_mention_text(text),
        attachments,
        |attachment| {
            resolve
                .and_then(|resolve| resolve(attachment))
                .map(|path| path.to_string_lossy().into_owned())
        },
    );
    if !text.is_empty() {
        items.push(json!({"type": "text", "text": text}));
    }
    for attachment in attachments
        .iter()
        .filter(|attachment| is_provider_native_image_attachment(attachment))
    {
        items.push(image_item(attachment, resolve).await?);
    }
    if items.is_empty() {
        return Err(CodexInputError(
            "Turn requires non-empty text or attachments.".to_owned(),
        ));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    //! The skill cases are `CodexAdapterV2.test.ts` "sends currency-sigil skill
    //! mentions as the $ mention Codex parses"; the rest cover `toCodexInput`.

    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[test]
    fn sends_currency_sigil_skill_mentions_as_the_dollar_mention_codex_parses() {
        let cases = [
            ("€review do it", "$review do it"),
            ("£ship", "$ship"),
            ("please ¥review this diff", "please $review this diff"),
            ("first line\n₹ship it", "first line\n$ship it"),
            ("𑿝review then €2spec", "$review then $2spec"),
            ("$review", "$review"),
            ("costs €20", "costs €20"),
            ("€5k", "€5k"),
            ("budget €100M or €1e6", "budget €100M or €1e6"),
            ("5€review", "5€review"),
        ];
        for (text, expected) in cases {
            assert_eq!(codex_skill_mention_text(text), expected, "{text}");
        }
    }

    fn image(id: &str, mime: &str) -> ChatAttachment {
        serde_json::from_value(json!({
            "type": "image", "id": id, "name": format!("{id}.png"),
            "mimeType": mime, "sizeBytes": 3
        }))
        .unwrap()
    }

    fn file(id: &str) -> ChatAttachment {
        serde_json::from_value(json!({
            "type": "file", "id": id, "name": "notes.txt",
            "mimeType": "text/plain", "sizeBytes": 3
        }))
        .unwrap()
    }

    fn resolver(dir: &std::path::Path) -> AttachmentPathResolver {
        let dir = dir.to_path_buf();
        Arc::new(move |attachment| {
            let name = match attachment {
                ChatAttachment::Image(image) => format!("{}.png", image.id.as_str()),
                ChatAttachment::File(file) => format!("{}.txt", file.id.as_str()),
                ChatAttachment::Unknown(_) => return None,
            };
            Some(dir.join(name))
        })
    }

    #[tokio::test]
    async fn plain_text_is_one_text_item_with_skill_sigils_rewritten() {
        let input = to_codex_input("€review do it", &[], None).await.unwrap();
        assert_eq!(
            input,
            vec![json!({"type": "text", "text": "$review do it"})]
        );
    }

    #[tokio::test]
    async fn attachment_paths_follow_the_text_and_images_become_data_urls() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("img-1.png"), [1u8, 2, 3]).unwrap();
        std::fs::write(temp.path().join("doc-1.txt"), b"abc").unwrap();
        let resolve = resolver(temp.path());
        let input = to_codex_input(
            "look",
            &[image("img-1", "image/png"), file("doc-1")],
            Some(&resolve),
        )
        .await
        .unwrap();
        assert_eq!(input.len(), 2);
        let text = input[0]["text"].as_str().unwrap();
        assert!(text.starts_with("look\n\n[Attached image \"img-1.png\" is saved at: "));
        assert!(text.contains("[Attached file \"notes.txt\" is saved at: "));
        assert_eq!(
            input[1],
            json!({"type": "image", "url": "data:image/png;base64,AQID"})
        );
    }

    #[tokio::test]
    async fn an_image_only_turn_has_no_text_item() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("img-1.png"), [1u8]).unwrap();
        let resolve = resolver(temp.path());
        // The saved-path note is text, so the turn still has one.
        let input = to_codex_input("", &[image("img-1", "image/png")], Some(&resolve))
            .await
            .unwrap();
        assert_eq!(input[0]["type"], "text");
        assert_eq!(input[1]["type"], "image");
    }

    #[tokio::test]
    async fn an_empty_turn_is_an_error() {
        let error = to_codex_input("", &[], None).await.unwrap_err();
        assert_eq!(error.0, "Turn requires non-empty text or attachments.");
    }

    #[tokio::test]
    async fn an_unresolvable_or_unreadable_image_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let attachments = [image("img-1", "image/png")];
        let error = to_codex_input("x", &attachments, None).await.unwrap_err();
        assert_eq!(error.0, "Invalid attachment id 'img-1'");
        let resolve = resolver(temp.path());
        let error = to_codex_input("x", &attachments, Some(&resolve))
            .await
            .unwrap_err();
        assert_eq!(error.0, "Failed to read attachment 'img-1'.");
    }
}
