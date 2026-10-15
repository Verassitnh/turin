//! Port of upstream/packages/contracts/src/assistantCitations.ts
//!
//! Positions are UTF-16 offsets, not Markdown offsets. The whole-struct check
//! (`end > start`, non-blank text) is enforced on deserialize.

use crate::base_schemas::{EnvironmentId, MessageId, NonNegativeInt, ThreadId, js_trim};
use serde::{Deserialize, Deserializer, Serialize};

pub const ASSISTANT_CITATION_MAX_TEXT_LENGTH: usize = 8_000;
pub const ASSISTANT_CITATION_MAX_COMMENT_LENGTH: usize = 8_000;
pub const ASSISTANT_CITATION_CONTEXT_LENGTH: usize = 32;

/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// JS `string.length`: UTF-16 code units, which is what `isMaxLength` counts.
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// A quote of rendered assistant text with an optional user comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCitation {
    pub version: CitationVersion,
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub start: NonNegativeInt,
    pub end: NonNegativeInt,
    pub prefix: String,
    pub suffix: String,
}

crate::int_literal!(
    /// Constant `version: 1` marker of an assistant citation.
    CitationVersion,
    1
);

impl<'de> Deserialize<'de> for AssistantCitation {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            version: CitationVersion,
            environment_id: EnvironmentId,
            thread_id: ThreadId,
            message_id: MessageId,
            #[serde(deserialize_with = "non_empty_text")]
            text: String,
            #[serde(default)]
            comment: Option<String>,
            start: NonNegativeInt,
            end: NonNegativeInt,
            prefix: String,
            suffix: String,
        }

        fn non_empty_text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
            let value = String::deserialize(d)?;
            if value.is_empty() {
                Err(serde::de::Error::custom("citation text must be non-empty"))
            } else {
                Ok(value)
            }
        }

        let raw = Raw::deserialize(d)?;
        let err = serde::de::Error::custom;
        if [
            raw.environment_id.as_str(),
            raw.thread_id.as_str(),
            raw.message_id.as_str(),
        ]
        .iter()
        .any(|id| utf16_len(id) > 512)
        {
            return Err(err("citation ids must be at most 512 characters"));
        }
        if utf16_len(&raw.text) > ASSISTANT_CITATION_MAX_TEXT_LENGTH {
            return Err(err("citation text is too long"));
        }
        if raw
            .comment
            .as_deref()
            .is_some_and(|comment| utf16_len(comment) > ASSISTANT_CITATION_MAX_COMMENT_LENGTH)
        {
            return Err(err("citation comment is too long"));
        }
        if raw.start > MAX_SAFE_INTEGER || raw.end > MAX_SAFE_INTEGER {
            return Err(err("citation offsets must be safe integers"));
        }
        if utf16_len(&raw.prefix) > ASSISTANT_CITATION_CONTEXT_LENGTH
            || utf16_len(&raw.suffix) > ASSISTANT_CITATION_CONTEXT_LENGTH
        {
            return Err(err("citation context is too long"));
        }
        if raw.end <= raw.start {
            return Err(err("citation end must be greater than start"));
        }
        if js_trim(&raw.text).is_empty() {
            return Err(err("citation text must not be blank"));
        }
        Ok(Self {
            version: raw.version,
            environment_id: raw.environment_id,
            thread_id: raw.thread_id,
            message_id: raw.message_id,
            text: raw.text,
            comment: raw.comment,
            start: raw.start,
            end: raw.end,
            prefix: raw.prefix,
            suffix: raw.suffix,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_range_and_blank_text() {
        let base = serde_json::json!({
            "version": 1,
            "environmentId": "e1",
            "threadId": "t1",
            "messageId": "m1",
            "text": "quoted",
            "start": 0,
            "end": 6,
            "prefix": "",
            "suffix": "",
        });
        let ok: AssistantCitation = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(ok.end, 6);

        let mut inverted = base.clone();
        inverted["end"] = serde_json::json!(0);
        assert!(serde_json::from_value::<AssistantCitation>(inverted).is_err());

        let mut blank = base.clone();
        blank["text"] = serde_json::json!("   ");
        assert!(serde_json::from_value::<AssistantCitation>(blank).is_err());

        let mut feff = base.clone();
        feff["text"] = serde_json::json!("\u{feff}");
        assert!(serde_json::from_value::<AssistantCitation>(feff).is_err());

        let mut long_text = base.clone();
        long_text["text"] = serde_json::json!("a".repeat(8_001));
        assert!(serde_json::from_value::<AssistantCitation>(long_text).is_err());

        let mut long_comment = base.clone();
        long_comment["comment"] = serde_json::json!("a".repeat(8_001));
        assert!(serde_json::from_value::<AssistantCitation>(long_comment).is_err());

        let mut astral_context = base.clone();
        // 17 astral chars = 34 UTF-16 units > 32, though only 17 `char`s.
        astral_context["prefix"] = serde_json::json!("😀".repeat(17));
        assert!(serde_json::from_value::<AssistantCitation>(astral_context).is_err());

        let mut unsafe_end = base.clone();
        unsafe_end["end"] = serde_json::json!(1u64 << 53);
        assert!(serde_json::from_value::<AssistantCitation>(unsafe_end).is_err());

        let mut commented = base;
        commented["comment"] = serde_json::json!("note");
        let round: AssistantCitation = serde_json::from_value(commented.clone()).unwrap();
        assert_eq!(serde_json::to_value(&round).unwrap(), commented);
    }
}
