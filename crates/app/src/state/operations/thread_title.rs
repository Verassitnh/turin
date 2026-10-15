//! Port of upstream/packages/client-runtime/src/operations/threadTitle.ts

use std::sync::LazyLock;

use regex::Regex;

use crate::state::contracts::{js_len, js_trim};

pub struct ThreadTitleSeedInput<'a> {
    pub text: &'a str,
    /// Attachment names, in order.
    pub attachment_names: &'a [&'a str],
    pub fallback_labels: &'a [Option<&'a str>],
}

/// `assistantCitationsToPlainText` (packages/shared/src/assistantCitations.ts):
/// `[Assistant quote](t3-citation://v1/…)` links read as their quoted text (and
/// comment). Links that do not decode are left alone.
pub fn assistant_citations_to_plain_text(prompt: &str) -> String {
    static LINK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\[Assistant quote\]\((t3-citation://v1/[^\s)]+)\)").expect("static regex")
    });
    LINK.replace_all(prompt, |captures: &regex::Captures<'_>| {
        let source = captures[0].to_owned();
        let Ok(url) = url::Url::parse(&captures[1]) else {
            return source;
        };
        let param = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        let Some(text) = param("text") else {
            return source;
        };
        match param("comment") {
            None => text,
            Some(comment) => format!("{text}\nComment: {comment}"),
        }
    })
    .into_owned()
}

fn normalize_title_seed(value: &str) -> String {
    static WHITESPACE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s+").expect("static regex"));
    let plain = assistant_citations_to_plain_text(value);
    WHITESPACE.replace_all(js_trim(&plain), " ").into_owned()
}

/// `truncate` (packages/shared/src/String.ts) with its default of 50.
fn truncate(text: &str) -> String {
    let trimmed = js_trim(text);
    if js_len(trimmed) <= 50 {
        return trimmed.to_owned();
    }
    let kept: Vec<u16> = trimmed.encode_utf16().take(50).collect();
    // A cut inside a surrogate pair drops the whole pair (a `&str` cannot hold a lone surrogate).
    format!(
        "{}...",
        String::from_utf16_lossy(&kept).trim_end_matches('\u{fffd}')
    )
}

pub fn derive_thread_title_seed(input: &ThreadTitleSeedInput<'_>) -> String {
    let text = normalize_title_seed(input.text);
    if !text.is_empty() {
        return truncate(&text);
    }

    let attachment_name =
        normalize_title_seed(input.attachment_names.first().copied().unwrap_or(""));
    if !attachment_name.is_empty() {
        return truncate(&format!("Image: {attachment_name}"));
    }

    for label in input.fallback_labels {
        let normalized = normalize_title_seed(label.unwrap_or(""));
        if !normalized.is_empty() {
            return truncate(&normalized);
        }
    }

    "New thread".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_normalized_message_text() {
        assert_eq!(
            derive_thread_title_seed(&ThreadTitleSeedInput {
                text: "  Investigate\n  login   failures  ",
                attachment_names: &["login-error.png"],
                fallback_labels: &[Some("Terminal 1")],
            }),
            "Investigate login failures"
        );
    }

    #[test]
    fn uses_the_first_attachment_name_when_text_is_empty() {
        assert_eq!(
            derive_thread_title_seed(&ThreadTitleSeedInput {
                text: " \n ",
                attachment_names: &["login-error.png", "other.png"],
                fallback_labels: &[],
            }),
            "Image: login-error.png"
        );
    }

    #[test]
    fn uses_the_first_non_empty_fallback_label_after_text_and_attachments() {
        assert_eq!(
            derive_thread_title_seed(&ThreadTitleSeedInput {
                text: "",
                attachment_names: &[],
                fallback_labels: &[None, Some("  "), Some("Terminal 1")],
            }),
            "Terminal 1"
        );
    }

    #[test]
    fn falls_back_to_new_thread() {
        assert_eq!(
            derive_thread_title_seed(&ThreadTitleSeedInput {
                text: "",
                attachment_names: &[],
                fallback_labels: &[],
            }),
            "New thread"
        );
    }

    #[test]
    fn applies_the_shared_title_truncation() {
        let text = "x".repeat(60);
        assert_eq!(
            derive_thread_title_seed(&ThreadTitleSeedInput {
                text: &text,
                attachment_names: &[],
                fallback_labels: &[],
            }),
            format!("{}...", "x".repeat(50))
        );
    }

    #[test]
    fn reads_citation_links_as_their_quoted_text() {
        let prompt = "see [Assistant quote](t3-citation://v1/env/thread/message?text=hello+world&start=0&end=5&prefix=&suffix=&comment=why) now";
        assert_eq!(
            assistant_citations_to_plain_text(prompt),
            "see hello world\nComment: why now"
        );
    }
}
