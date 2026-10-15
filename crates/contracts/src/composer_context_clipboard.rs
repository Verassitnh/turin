//! Port of upstream/packages/contracts/src/composerContextClipboard.ts

use crate::base_schemas::{EnvironmentId, MessageId, ThreadId};
use crate::composer_context::{ComposerContextRecord, records_field};
use serde::{Deserialize, Serialize};

/// Structured clipboard payload for context copied between drafts and
/// messages. Carries the records behind the copied links plus where they came
/// from, never bytes or URLs; the receiver resolves binaries through its own
/// environment.
pub const COMPOSER_CONTEXT_CLIPBOARD_MIME: &str = "web application/x-t3-context-fragment+json";

crate::int_literal!(
    /// Constant `version: 1` marker of a clipboard fragment.
    ComposerContextClipboardVersion,
    1
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerContextClipboardSource {
    pub environment_id: EnvironmentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerContextClipboardFragment {
    pub version: ComposerContextClipboardVersion,
    pub source: ComposerContextClipboardSource,
    // The bound applies to the raw input, as in `OrchestrationMessageContext`:
    // forward-compatible decoding drops unknown records first, so a checked
    // output length never sees the flood.
    #[serde(with = "records_field")]
    pub records: Vec<ComposerContextRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_decodes_and_drops_undecodable_records() {
        let fragment: ComposerContextClipboardFragment = serde_json::from_value(serde_json::json!({
            "version": 1,
            "source": { "environmentId": "e1", "threadId": "t1" },
            "records": [
                { "version": 1, "contextId": "a", "label": "l", "kind": "mention", "path": "x" },
                { "version": 1, "contextId": "b", "label": "l", "kind": "garbage-record", "payload": [1] },
                "not-a-record"
            ]
        }))
        .unwrap();
        assert_eq!(fragment.records.len(), 2);
        assert_eq!(fragment.source.environment_id.as_str(), "e1");
    }
}
