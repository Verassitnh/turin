//! Upstream ships no test file for composerInlineTokenPaste.ts; these cover
//! `readPastedComposerContext` and `importPastedComposerText`.

use std::cell::RefCell;

use serde_json::{Value, json};
use turin_shared::composer_context_clipboard::{
    encode_composer_context_clipboard_html, encode_composer_context_fragment,
};

use super::*;

struct FakeClipboard {
    text: String,
    fragment: String,
    html: String,
}

impl ClipboardRead for FakeClipboard {
    fn get_data(&self, format: &str) -> String {
        match format {
            "text/plain" => self.text.clone(),
            "text/html" => self.html.clone(),
            COMPOSER_CONTEXT_CLIPBOARD_MIME => self.fragment.clone(),
            _ => String::new(),
        }
    }
}

fn skill(id: &str) -> Value {
    json!({ "version": 1, "contextId": id, "kind": "skill", "label": "$x", "name": "x" })
}

fn annotation(id: &str, screenshot: &str) -> Value {
    json!({
        "version": 1,
        "contextId": id,
        "kind": "preview-annotation",
        "label": "Fix this",
        "annotationId": "a1",
        "pageUrl": "https://example.com",
        "pageTitle": null,
        "comment": "Fix this",
        "targetSummary": "button",
        "styleChanges": [],
        "screenshotContextId": screenshot,
    })
}

fn screenshot(id: &str) -> Value {
    json!({
        "version": 1,
        "contextId": id,
        "kind": "image",
        "label": "shot.png",
        "attachmentId": "att-1",
        "name": "shot.png",
        "mimeType": "image/png",
        "sizeBytes": 3,
    })
}

fn fragment(records: Vec<Value>) -> String {
    let value = json!({ "version": 1, "source": { "environmentId": "env" }, "records": records });
    encode_composer_context_fragment(&serde_json::from_value(value).unwrap()).unwrap()
}

fn clipboard(text: &str, records: Vec<Value>) -> FakeClipboard {
    FakeClipboard {
        text: text.into(),
        fragment: fragment(records),
        html: String::new(),
    }
}

fn ids(fragment: &ComposerContextClipboardFragment) -> Vec<String> {
    fragment
        .records
        .iter()
        .map(|record| record.context_id().as_str().to_owned())
        .collect()
}

#[test]
fn imports_only_records_whose_links_are_in_the_pasted_text() {
    let paste = clipboard(
        "see [$x](t3-context://v1/skill/ctx-1)",
        vec![skill("ctx-1"), skill("ctx-2")],
    );
    let read = read_pasted_composer_context(&paste).unwrap();
    assert_eq!(ids(&read), ["ctx-1"]);
}

#[test]
fn a_pasted_annotation_brings_its_screenshot_along() {
    let paste = clipboard(
        "[Fix this](t3-context://v1/preview-annotation/ann-1)",
        vec![
            annotation("ann-1", "img-1"),
            screenshot("img-1"),
            screenshot("img-2"),
        ],
    );
    assert_eq!(
        ids(&read_pasted_composer_context(&paste).unwrap()),
        ["ann-1", "img-1"]
    );
}

#[test]
fn reads_nothing_without_a_decodable_fragment() {
    let paste = FakeClipboard {
        text: "[$x](t3-context://v1/skill/ctx-1)".into(),
        fragment: "not json".into(),
        html: String::new(),
    };
    assert_eq!(read_pasted_composer_context(&paste), None);
}

#[test]
fn falls_back_to_the_html_flavor() {
    let encoded = fragment(vec![skill("ctx-1")]);
    let paste = FakeClipboard {
        text: "[$x](t3-context://v1/skill/ctx-1)".into(),
        fragment: String::new(),
        html: encode_composer_context_clipboard_html("x", &encoded, None),
    };
    assert_eq!(
        ids(&read_pasted_composer_context(&paste).unwrap()),
        ["ctx-1"]
    );
}

#[test]
fn rewrites_links_onto_the_imported_ids_and_leaves_others_alone() {
    let paste = clipboard(
        "a [$x](t3-context://v1/skill/ctx-1) b [$y](t3-context://v1/skill/ctx-9)",
        vec![skill("ctx-1")],
    );
    let imported = RefCell::new(Vec::new());
    let import = |fragment: &ComposerContextClipboardFragment| {
        imported.borrow_mut().push(ids(fragment));
        HashMap::from([("ctx-1".to_owned(), "ctx-new".to_owned())])
    };
    let text = import_pasted_composer_text(&paste, Some(&import));
    assert_eq!(
        text,
        "a [$x](t3-context://v1/skill/ctx-new) b [$y](t3-context://v1/skill/ctx-9)"
    );
    assert_eq!(*imported.borrow(), [vec!["ctx-1".to_owned()]]);
}

#[test]
fn plain_text_passes_through_without_an_importer_or_a_rewrite() {
    let paste = clipboard("[$x](t3-context://v1/skill/ctx-1)", vec![skill("ctx-1")]);
    assert_eq!(
        import_pasted_composer_text(&paste, None),
        "[$x](t3-context://v1/skill/ctx-1)"
    );
    let empty = |_: &ComposerContextClipboardFragment| HashMap::new();
    assert_eq!(
        import_pasted_composer_text(&paste, Some(&empty)),
        "[$x](t3-context://v1/skill/ctx-1)"
    );
    let no_records = clipboard("[$x](t3-context://v1/skill/ctx-1)", vec![skill("other")]);
    let unused = |_: &ComposerContextClipboardFragment| -> HashMap<String, String> {
        panic!("no record is referenced")
    };
    assert_eq!(
        import_pasted_composer_text(&no_records, Some(&unused)),
        "[$x](t3-context://v1/skill/ctx-1)"
    );
}

#[test]
fn an_image_link_keeps_its_bang_form() {
    let paste = clipboard(
        "![shot.png](t3-context://v1/image/img-1)",
        vec![screenshot("img-1")],
    );
    let import = |_: &ComposerContextClipboardFragment| {
        HashMap::from([("img-1".to_owned(), "img-2".to_owned())])
    };
    assert_eq!(
        import_pasted_composer_text(&paste, Some(&import)),
        "![shot.png](t3-context://v1/image/img-2)"
    );
}
