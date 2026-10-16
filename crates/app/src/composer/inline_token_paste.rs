//! Port of upstream/apps/web/src/components/composerInlineTokenPaste.ts
//!
//! A paste that carries the composer's structured context payload brings its
//! records along, and the pasted links are rewritten to the ids they were
//! imported under.
//!
//! The browser clipboard's three flavors map onto GPUI's: `text/plain` is the
//! clipboard string, the `COMPOSER_CONTEXT_CLIPBOARD_MIME` payload rides in
//! the string's metadata, and `text/html` is read when another app put it
//! there (GPUI does not expose HTML, so it is usually absent).

use std::collections::{HashMap, HashSet};

use turin_contracts::composer_context::{ComposerContextRecord, KnownComposerContextRecord};
use turin_contracts::composer_context_clipboard::ComposerContextClipboardFragment;
use turin_shared::composer_context_clipboard::{
    decode_composer_context_clipboard_html, decode_composer_context_fragment,
};
use turin_shared::composer_context_references::{
    collect_composer_context_references, format_composer_context_reference,
    replace_composer_context_references,
};

/// The clipboard flavors a paste reads (`clipboardData.getData`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PastedClipboard<'a> {
    pub text: &'a str,
    /// The `COMPOSER_CONTEXT_CLIPBOARD_MIME` flavor.
    pub fragment: Option<&'a str>,
    pub html: Option<&'a str>,
}

/// `readPastedComposerContext`: clipboard records referenced by the copied
/// text, including dependent screenshots.
pub fn read_pasted_composer_context(
    clipboard: PastedClipboard<'_>,
) -> Option<ComposerContextClipboardFragment> {
    // Only records whose links are in the pasted text get imported; a fragment
    // may carry more (it was built for a larger copy) and must not start
    // transfers for those.
    let decoded = decode_composer_context_fragment(clipboard.fragment)
        .or_else(|| decode_composer_context_clipboard_html(clipboard.html))?;
    let mut pasted_ids: HashSet<String> = collect_composer_context_references(clipboard.text)
        .into_iter()
        .map(|occurrence| occurrence.context_id.as_str().to_owned())
        .collect();
    for record in &decoded.records {
        if let ComposerContextRecord::Known(KnownComposerContextRecord::PreviewAnnotation(
            annotation,
        )) = record
            && pasted_ids.contains(annotation.context_id.as_str())
            && let Some(screenshot) = &annotation.screenshot_context_id
        {
            pasted_ids.insert(screenshot.as_str().to_owned());
        }
    }
    Some(ComposerContextClipboardFragment {
        records: decoded
            .records
            .into_iter()
            .filter(|record| pasted_ids.contains(record.context_id().as_str()))
            .collect(),
        ..decoded
    })
}

/// Imports a structured paste's records; returns the ids that changed.
pub type ImportContextFragment<'a> =
    &'a dyn Fn(&ComposerContextClipboardFragment) -> HashMap<String, String>;

/// `importPastedComposerText`: the pasted text with its context links
/// pointing at the imported records.
pub fn import_pasted_composer_text(
    clipboard: PastedClipboard<'_>,
    import_context_fragment: Option<ImportContextFragment<'_>>,
) -> String {
    let pasted_text = clipboard.text;
    let fragment = import_context_fragment.and_then(|_| read_pasted_composer_context(clipboard));
    let rewritten_ids = match (fragment, import_context_fragment) {
        (Some(fragment), Some(import)) if !fragment.records.is_empty() => Some(import(&fragment)),
        _ => None,
    };
    match rewritten_ids {
        Some(ids) if !ids.is_empty() => {
            replace_composer_context_references(pasted_text, |occurrence| {
                match ids.get(occurrence.context_id.as_str()) {
                    Some(next_id) => {
                        let kind = if occurrence.kind.as_str() == "element" {
                            "preview-annotation"
                        } else {
                            occurrence.kind.as_str()
                        };
                        format_composer_context_reference(kind, next_id, &occurrence.label)
                    }
                    None => occurrence.source.clone(),
                }
            })
        }
        _ => pasted_text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_shared::composer_context_clipboard::encode_composer_context_fragment;

    fn fragment_json(ids: &[&str]) -> String {
        let records: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "version": 1,
                    "kind": "terminal",
                    "contextId": id,
                    "label": "Terminal 1 lines 1-2",
                    "terminalId": "term-1",
                    "terminalLabel": "Terminal 1",
                    "lineStart": 1,
                    "lineEnd": 2,
                    "text": "echo hi",
                })
            })
            .collect();
        let fragment: ComposerContextClipboardFragment =
            serde_json::from_value(serde_json::json!({
                "version": 1,
                "source": { "environmentId": "environment" },
                "records": records,
            }))
            .expect("fragment");
        encode_composer_context_fragment(&fragment).expect("encoded")
    }

    fn reference(id: &str) -> String {
        format_composer_context_reference("terminal", id, "Terminal 1 lines 1-2")
    }

    #[test]
    fn imports_only_the_records_the_pasted_text_references() {
        let text = format!("see {}", reference("terminal_a"));
        let fragment = fragment_json(&["terminal_a", "terminal_b"]);
        let read = read_pasted_composer_context(PastedClipboard {
            text: &text,
            fragment: Some(&fragment),
            html: None,
        })
        .expect("fragment");
        let ids: Vec<&str> = read
            .records
            .iter()
            .map(|r| r.context_id().as_str())
            .collect();
        assert_eq!(ids, vec!["terminal_a"]);
    }

    #[test]
    fn rewrites_links_to_the_imported_ids() {
        let text = format!(
            "see {} and {}",
            reference("terminal_a"),
            reference("terminal_z")
        );
        let fragment = fragment_json(&["terminal_a"]);
        let import = |_: &ComposerContextClipboardFragment| {
            HashMap::from([("terminal_a".to_owned(), "terminal_b".to_owned())])
        };
        let pasted = import_pasted_composer_text(
            PastedClipboard {
                text: &text,
                fragment: Some(&fragment),
                html: None,
            },
            Some(&import),
        );
        assert_eq!(
            pasted,
            format!(
                "see {} and {}",
                reference("terminal_b"),
                reference("terminal_z")
            )
        );
    }

    #[test]
    fn keeps_the_text_without_a_payload_or_an_importer() {
        let text = format!("see {}", reference("terminal_a"));
        let import = |_: &ComposerContextClipboardFragment| HashMap::new();
        assert_eq!(
            import_pasted_composer_text(
                PastedClipboard {
                    text: &text,
                    ..PastedClipboard::default()
                },
                Some(&import)
            ),
            text
        );
        let fragment = fragment_json(&["terminal_a"]);
        assert_eq!(
            import_pasted_composer_text(
                PastedClipboard {
                    text: &text,
                    fragment: Some(&fragment),
                    html: None
                },
                None
            ),
            text
        );
    }
}
