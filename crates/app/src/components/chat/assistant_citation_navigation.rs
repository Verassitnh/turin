//! Port of upstream/apps/web/src/lib/assistantCitationNavigation.ts
//!
//! Router navigation (`assistantCitationNavigation`) becomes a plain struct for
//! the native shell; hash parsing matches upstream for deep links.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::timeline::assistant_citations::{
    AssistantCitation, format_assistant_citation_href, parse_assistant_citation_href,
};

const CITATION_HASH_PREFIX: &str = "assistant-citation=";

/// Base64url keeps router hash normalization from decoding quote whitespace or source IDs.
pub fn assistant_citation_hash(citation: &AssistantCitation) -> String {
    format!(
        "{CITATION_HASH_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(format_assistant_citation_href(citation))
    )
}

pub fn assistant_citation_from_location(href: &str) -> Option<AssistantCitation> {
    let hash_index = href.find('#')?;
    let hash = &href[hash_index + 1..];
    if !hash.starts_with(CITATION_HASH_PREFIX) || hash.len() > 140_000 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(&hash.as_bytes()[CITATION_HASH_PREFIX.len()..])
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    parse_assistant_citation_href(&decoded)
}

/// A fresh activation lets the same link reveal its source again after dismissal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantCitationNavigation {
    pub environment_id: String,
    pub thread_id: String,
    pub hash: String,
    pub activation_id: String,
}

pub fn assistant_citation_navigation(
    citation: &AssistantCitation,
    activation_id: impl Into<String>,
) -> AssistantCitationNavigation {
    AssistantCitationNavigation {
        environment_id: citation.environment_id.clone(),
        thread_id: citation.thread_id.clone(),
        hash: assistant_citation_hash(citation),
        activation_id: activation_id.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation() -> AssistantCitation {
        AssistantCitation {
            version: 1,
            environment_id: "environment-one".into(),
            thread_id: "thread-one".into(),
            message_id: "assistant-one".into(),
            text: "first line\n\tsecond line (with a #hash & spaces) 🚀".into(),
            comment: None,
            start: 12,
            end: 59,
            prefix: "Before:\n".into(),
            suffix: "\nAfter.".into(),
        }
    }

    #[test]
    fn preserves_encoded_quote_whitespace_through_hash_roundtrip() {
        let citation = citation();
        let hash = assistant_citation_hash(&citation);
        let href = format!("/environment-one/thread-one#{hash}");
        assert_eq!(assistant_citation_from_location(&href), Some(citation));
    }

    #[test]
    fn navigation_carries_fresh_activation_ids() {
        let citation = citation();
        let first = assistant_citation_navigation(&citation, "activation-a");
        let second = assistant_citation_navigation(&citation, "activation-b");
        assert_eq!(first.hash, second.hash);
        assert_ne!(first.activation_id, second.activation_id);
    }

    #[test]
    fn ignores_non_citation_fragments() {
        assert!(assistant_citation_from_location("/environment-one/thread-one").is_none());
        assert!(
            assistant_citation_from_location("/environment-one/thread-one#ordinary-heading")
                .is_none()
        );
        assert!(assistant_citation_from_location("/a/b#assistant-citation=%invalid").is_none());
        assert!(
            assistant_citation_from_location(&format!(
                "/a/b#assistant-citation={}",
                "a".repeat(140_000)
            ))
            .is_none()
        );
    }
}
