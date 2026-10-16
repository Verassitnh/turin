//! Port of upstream/apps/web/src/components/composerInlineTokenPaste.ts
//!
//! Reads the structured context a copy carried (`DataTransfer`) and rewrites
//! the pasted text's context links onto the ids the import chose. The
//! `DataTransfer` is [`ClipboardRead`].

use std::collections::{HashMap, HashSet};

use turin_contracts::composer_context::{ComposerContextRecord, KnownComposerContextRecord};
use turin_contracts::composer_context_clipboard::ComposerContextClipboardFragment;
use turin_shared::composer_context_clipboard::{
    COMPOSER_CONTEXT_CLIPBOARD_MIME, decode_composer_context_clipboard_html,
    decode_composer_context_fragment,
};
use turin_shared::composer_context_references::{
    collect_composer_context_references, format_composer_context_reference,
    replace_composer_context_references,
};

/// Imports a fragment's records and returns the old-to-new context id map.
pub type ImportContextFragment<'a> =
    &'a dyn Fn(&ComposerContextClipboardFragment) -> HashMap<String, String>;

/// `Pick<DataTransfer, "getData">`: an empty string when the flavor is absent.
pub trait ClipboardRead {
    fn get_data(&self, format: &str) -> String;
}

fn record_context_id(record: &ComposerContextRecord) -> &str {
    record.context_id().as_str()
}

/// Clipboard records referenced by the copied text, including dependent
/// screenshots.
pub fn read_pasted_composer_context(
    clipboard: &dyn ClipboardRead,
) -> Option<ComposerContextClipboardFragment> {
    let pasted_text = clipboard.get_data("text/plain");
    // Only records whose links are in the pasted text get imported; a fragment
    // may carry more (it was built for a larger copy) and must not start
    // transfers for those.
    let fragment_text = clipboard.get_data(COMPOSER_CONTEXT_CLIPBOARD_MIME);
    let html = clipboard.get_data("text/html");
    let decoded = decode_composer_context_fragment(Some(&fragment_text))
        .or_else(|| decode_composer_context_clipboard_html(Some(&html)))?;
    let mut pasted_ids: HashSet<String> = collect_composer_context_references(&pasted_text)
        .into_iter()
        .map(|occurrence| occurrence.context_id.as_str().to_owned())
        .collect();
    for record in &decoded.records {
        // A known record has no `payload` key; only these carry a screenshot link.
        if let ComposerContextRecord::Known(KnownComposerContextRecord::PreviewAnnotation(
            annotation,
        )) = record
            && pasted_ids.contains(record_context_id(record))
            && let Some(screenshot) = &annotation.screenshot_context_id
        {
            pasted_ids.insert(screenshot.as_str().to_owned());
        }
    }
    let records = decoded
        .records
        .iter()
        .filter(|record| pasted_ids.contains(record_context_id(record)))
        .cloned()
        .collect();
    Some(ComposerContextClipboardFragment { records, ..decoded })
}

/// Imports the same structured clipboard payload for focused paste and
/// paste-to-focus. `import_context_fragment` returns the old-to-new context id
/// map.
pub fn import_pasted_composer_text(
    clipboard: &dyn ClipboardRead,
    import_context_fragment: Option<ImportContextFragment<'_>>,
) -> String {
    let pasted_text = clipboard.get_data("text/plain");
    let fragment = import_context_fragment.and_then(|_| read_pasted_composer_context(clipboard));
    let rewritten_ids = match (&fragment, import_context_fragment) {
        (Some(fragment), Some(import)) if !fragment.records.is_empty() => Some(import(fragment)),
        _ => None,
    };
    match rewritten_ids {
        Some(ids) if !ids.is_empty() => {
            replace_composer_context_references(&pasted_text, |occurrence| {
                match ids.get(occurrence.context_id.as_str()) {
                    Some(next_id) => {
                        let kind = occurrence.kind.as_str();
                        format_composer_context_reference(
                            if kind == "element" {
                                "preview-annotation"
                            } else {
                                kind
                            },
                            next_id,
                            &occurrence.label,
                        )
                    }
                    None => occurrence.source.clone(),
                }
            })
        }
        _ => pasted_text,
    }
}

#[cfg(test)]
#[path = "composer_inline_token_paste_tests.rs"]
mod tests;
