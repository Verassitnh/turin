//! Port of upstream/packages/contracts/src/assistantCitations.ts
//!
//! Positions are UTF-16 offsets, not Markdown offsets. The whole-struct check
//! (`end > start`, non-blank text) is enforced on deserialize.

use crate::base_schemas::{EnvironmentId, MessageId, NonNegativeInt, ThreadId};
use serde::{Deserialize, Deserializer, Serialize};

pub const ASSISTANT_CITATION_MAX_TEXT_LENGTH: usize = 8_000;
pub const ASSISTANT_CITATION_MAX_COMMENT_LENGTH: usize = 8_000;
pub const ASSISTANT_CITATION_CONTEXT_LENGTH: usize = 32;

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
        if raw.end <= raw.start {
            return Err(serde::de::Error::custom("citation end must be greater than start"));
        }
        if raw.text.trim().is_empty() {
            return Err(serde::de::Error::custom("citation text must not be blank"));
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

        let mut blank = base;
        blank["text"] = serde_json::json!("   ");
        assert!(serde_json::from_value::<AssistantCitation>(blank).is_err());
    }
}
