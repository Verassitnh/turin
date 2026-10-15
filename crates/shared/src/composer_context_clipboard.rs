//! Port of upstream/packages/shared/src/composerContextClipboard.ts
//!
//! Upstream decodes with the Effect schema and `JSON.parse`; here serde does both, and the contract's
//! `records_field` drops records that do not decode, as the schema does upstream.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::js_len;
use turin_contracts::composer_context_clipboard::ComposerContextClipboardFragment;

use crate::js_string::{decode_uri_component, encode_uri_component};

pub use turin_contracts::composer_context_clipboard::COMPOSER_CONTEXT_CLIPBOARD_MIME;

const MAX_FRAGMENT_CHARS: usize = 16_000_000;

/// Room for the wrapper element, its attribute name, and the copied HTML itself.
const HTML_WRAPPER_SLACK_CHARS: usize = 4096;

static FRAGMENT_ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"data-t3-context-fragment=["']([^"']+)["']"#).expect("static regex")
});

pub fn encode_composer_context_fragment(
    fragment: &ComposerContextClipboardFragment,
) -> Option<String> {
    let encoded = serde_json::to_string(fragment).ok()?;
    (js_len(&encoded) <= MAX_FRAGMENT_CHARS).then_some(encoded)
}

/// HTML is the portable flavor shared by browsers and native system clipboards.
pub fn encode_composer_context_clipboard_html(
    text: &str,
    fragment: &str,
    html: Option<&str>,
) -> String {
    let encoded = encode_uri_component(fragment);
    if let Some(html) = html {
        return format!(r#"<div data-t3-context-fragment="{encoded}">{html}</div>"#);
    }
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(r#"<pre data-t3-context-fragment="{encoded}">{escaped}</pre>"#)
}

pub fn decode_composer_context_clipboard_html(
    html: Option<&str>,
) -> Option<ComposerContextClipboardFragment> {
    let html = html.filter(|html| !html.is_empty())?;
    // `encodeURIComponent` expands one non-ASCII code unit to up to nine characters, so a fragment
    // just under the limit must still survive the round trip through the attribute.
    if js_len(html) > MAX_FRAGMENT_CHARS * 9 + HTML_WRAPPER_SLACK_CHARS {
        return None;
    }
    let encoded = FRAGMENT_ATTRIBUTE.captures(html)?.get(1)?.as_str();
    let raw = decode_uri_component(encoded).ok()?;
    decode_composer_context_fragment(Some(&raw))
}

/// Clipboard data is untrusted: anything that is not a valid version-1 fragment is ignored.
pub fn decode_composer_context_fragment(
    raw: Option<&str>,
) -> Option<ComposerContextClipboardFragment> {
    let raw = raw.filter(|raw| !raw.is_empty())?;
    if js_len(raw) > MAX_FRAGMENT_CHARS {
        return None;
    }
    serde_json::from_str(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn terminal_record(index: usize, text: String) -> Value {
        json!({
            "version": 1,
            "contextId": format!("terminal-{index}"),
            "kind": "terminal",
            "label": "Build",
            "terminalId": "build",
            "terminalLabel": "Build",
            "lineStart": 1,
            "lineEnd": 1,
            "text": text,
        })
    }

    fn fragment_of(json: &Value) -> ComposerContextClipboardFragment {
        serde_json::from_value(json.clone()).expect("valid fragment")
    }

    fn as_value(fragment: &ComposerContextClipboardFragment) -> Value {
        serde_json::to_value(fragment).unwrap()
    }

    #[test]
    fn preserves_rich_html_while_carrying_context_metadata() {
        let json = json!({ "version": 1, "source": { "environmentId": "env" }, "records": [] });
        let fragment = encode_composer_context_fragment(&fragment_of(&json)).unwrap();
        let rich = r#"<p><strong>Important</strong> <a href="https://example.com">link</a></p>"#;
        let html = encode_composer_context_clipboard_html("Important link", &fragment, Some(rich));
        assert!(html.contains(rich));
        assert_eq!(
            as_value(&decode_composer_context_clipboard_html(Some(&html)).unwrap()),
            serde_json::from_str::<Value>(&fragment).unwrap()
        );
    }

    #[test]
    fn round_trips_selections_larger_than_two_million_characters() {
        let records: Vec<Value> = (0..32)
            .map(|index| terminal_record(index, "x".repeat(64_000)))
            .collect();
        let json =
            json!({ "version": 1, "source": { "environmentId": "env" }, "records": records });
        let fragment = fragment_of(&json);
        let encoded = encode_composer_context_fragment(&fragment).unwrap();
        assert!(js_len(&encoded) > 2_000_000);
        assert_eq!(
            as_value(&decode_composer_context_fragment(Some(&encoded)).unwrap()),
            json
        );

        let oversized: Vec<Value> = (0..200)
            .map(|index| terminal_record(index, "\u{0}".repeat(64_000)))
            .collect();
        let oversized = fragment_of(&json!({
            "version": 1,
            "source": { "environmentId": "env" },
            "records": oversized,
        }));
        assert_eq!(encode_composer_context_fragment(&oversized), None);
    }

    #[test]
    fn round_trips_native_html_without_interpreting_captured_content_as_markup() {
        let raw = json!({
            "version": 1,
            "source": { "environmentId": "env-1" },
            "records": [{
                "version": 1,
                "kind": "terminal",
                "contextId": "terminal",
                "label": "Build \"failure\"",
                "terminalId": "main",
                "terminalLabel": "Main",
                "lineStart": 1,
                "lineEnd": 1,
                "text": "<script> & café",
            }],
        });
        let raw_text = raw.to_string();
        let html = encode_composer_context_clipboard_html("<script> & café", &raw_text, None);
        assert!(html.contains("&lt;script&gt; &amp; café"));
        assert!(!html.contains("<script>"));
        assert_eq!(
            as_value(&decode_composer_context_clipboard_html(Some(&html)).unwrap()),
            raw
        );
        assert_eq!(
            decode_composer_context_clipboard_html(Some(
                r#"<pre data-t3-context-fragment="%ZZ">bad</pre>"#
            )),
            None
        );
        assert_eq!(
            decode_composer_context_clipboard_html(Some("<p>Ordinary clipboard</p>")),
            None
        );
    }

    #[test]
    fn accepts_a_large_non_ascii_fragment_through_the_html_attribute() {
        // Each CJK code unit encodes to nine characters, so the attribute is far longer than the raw text.
        let raw = json!({
            "version": 1,
            "source": { "environmentId": "env-1" },
            "records": [{
                "version": 1,
                "kind": "terminal",
                "contextId": "terminal",
                "label": "ビルド",
                "terminalId": "main",
                "terminalLabel": "Main",
                "lineStart": 1,
                "lineEnd": 1,
                "text": "日本語".repeat(21_000),
            }],
        })
        .to_string();
        let html = encode_composer_context_clipboard_html("output", &raw, None);
        assert!(js_len(&html) > js_len(&raw) * 8);
        assert_eq!(
            serde_json::to_value(decode_composer_context_clipboard_html(Some(&html)).unwrap())
                .unwrap(),
            serde_json::from_str::<Value>(&raw).unwrap()
        );
    }

    #[test]
    fn round_trips_a_fragment_and_drops_records_it_cannot_decode() {
        let json = json!({
            "version": 1,
            "source": { "environmentId": "env-1", "threadId": "thread-1" },
            "records": [
                { "version": 1, "contextId": "ctx-1", "kind": "skill", "label": "$x", "name": "x" },
                { "version": 1, "contextId": "ctx-2", "kind": "image", "label": "bad" },
            ],
        });
        let encoded = encode_composer_context_fragment(&fragment_of(&json)).unwrap();
        let decoded = decode_composer_context_fragment(Some(&encoded)).unwrap();
        assert_eq!(
            decoded.source.thread_id.as_ref().map(|id| id.as_str()),
            Some("thread-1")
        );
        let context_ids: Vec<Value> = as_value(&decoded)["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["contextId"].clone())
            .collect();
        assert_eq!(context_ids, vec![json!("ctx-1")]);
    }

    #[test]
    fn rejects_garbage_other_versions_and_oversized_payloads() {
        assert_eq!(decode_composer_context_fragment(None), None);
        assert_eq!(decode_composer_context_fragment(Some("not json")), None);
        assert_eq!(
            decode_composer_context_fragment(Some(
                r#"{"version":2,"source":{"environmentId":"e"},"records":[]}"#
            )),
            None
        );
        assert_eq!(
            decode_composer_context_fragment(Some(&"x".repeat(2_000_001))),
            None
        );
        assert!(COMPOSER_CONTEXT_CLIPBOARD_MIME.starts_with("web application/"));
    }
}
