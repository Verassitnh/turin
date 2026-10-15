//! Port of upstream/packages/shared/src/composerContextLegacySend.ts
//!
//! Serializes a canonical message (inline reference links plus records) into the pre-inline-context
//! wire shape, for servers that do not advertise `inlineMessageContext`. Those servers drop the
//! records and forward the links as literal text, so the payload has to travel inside the message.
//!
//! The output is the format `upgrade_legacy_context_message` parses, so a newer client reading the
//! resulting message reconstructs the same records.

use std::collections::{HashMap, HashSet};

use turin_contracts::composer_context::{
    ComposerContextRecord, ElementContextDetails, KnownComposerContextRecord,
    PreviewAnnotationContextRecord, ReviewCommentContextRecord, TerminalContextRecord,
};

use crate::composer_context_references::{
    collect_composer_context_references, element_details, escape_attribute, record_label,
};
use crate::js_string::replace_js_whitespace_runs;
use turin_contracts::base_schemas::js_trim_end;

pub fn serialize_legacy_context_message(text: &str, records: &[ComposerContextRecord]) -> String {
    // A later record with the same id wins, as a JS `Map` built from entries does.
    let records_by_id: HashMap<&str, &ComposerContextRecord> = records
        .iter()
        .map(|record| (record.context_id().as_str(), record))
        .collect();
    let mut used: HashSet<&str> = HashSet::new();

    // Inline references become the plain label the old composer wrote; the payload follows in a
    // trailing block, which is where the legacy format carried it.
    let mut text = text.to_owned();
    let occurrences = collect_composer_context_references(&text);
    for occurrence in occurrences.iter().rev() {
        let Some(record) = records_by_id.get(occurrence.context_id.as_str()) else {
            continue;
        };
        used.insert(record.context_id().as_str());
        let replacement = match record {
            ComposerContextRecord::Known(KnownComposerContextRecord::ReviewComment(review)) => {
                render_review_comment(review)
            }
            other => inline_label(other),
        };
        text.replace_range(occurrence.start..occurrence.end, &replacement);
    }

    let terminals: Vec<String> = records
        .iter()
        .filter_map(|record| match record {
            ComposerContextRecord::Known(KnownComposerContextRecord::Terminal(terminal)) => {
                Some(render_terminal_entry(terminal))
            }
            _ => None,
        })
        .collect();
    let elements: Vec<String> = records
        .iter()
        .filter_map(|record| match record {
            ComposerContextRecord::Known(KnownComposerContextRecord::Element(element)) => {
                Some(render_element_details_entry(&element_details(element)))
            }
            _ => None,
        })
        .collect();
    let previews = records.iter().filter_map(|record| match record {
        ComposerContextRecord::Known(KnownComposerContextRecord::PreviewAnnotation(preview)) => {
            Some(format!(
                "<preview_annotation>\n{}\n</preview_annotation>",
                render_preview_body(preview),
            ))
        }
        _ => None,
    });
    // Review comments already inlined their payload above; anything else unreferenced still ships
    // its block so no context is silently dropped.
    let unreferenced_reviews = records.iter().filter_map(|record| match record {
        ComposerContextRecord::Known(KnownComposerContextRecord::ReviewComment(review))
            if !used.contains(record.context_id().as_str()) =>
        {
            Some(render_review_comment(review))
        }
        _ => None,
    });

    let blocks: Vec<String> = [
        render_block("terminal_context", &terminals),
        render_block("element_context", &elements),
    ]
    .into_iter()
    .chain(previews)
    .chain(unreferenced_reviews)
    .filter(|block| !block.is_empty())
    .collect();

    std::iter::once(js_trim_end(&text).to_owned())
        .chain(blocks)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The label the old composer wrote inline: a terminal excerpt is `@terminal-1:509-514`.
fn inline_label(record: &ComposerContextRecord) -> String {
    match record {
        ComposerContextRecord::Known(KnownComposerContextRecord::Terminal(terminal)) => {
            let slug = replace_js_whitespace_runs(
                &terminal.terminal_label.as_str().trim().to_lowercase(),
                "-",
            );
            let range = if terminal.line_start == terminal.line_end {
                terminal.line_start.to_string()
            } else {
                format!("{}-{}", terminal.line_start, terminal.line_end)
            };
            format!("@{slug}:{range}")
        }
        other => record_label(other).to_owned(),
    }
}

fn render_block(tag: &str, entries: &[String]) -> String {
    let body = entries
        .iter()
        .filter(|entry| !entry.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if body.is_empty() {
        String::new()
    } else {
        format!("<{tag}>\n{body}\n</{tag}>")
    }
}

fn indent(value: &str) -> String {
    value
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_terminal_entry(record: &TerminalContextRecord) -> String {
    let range = if record.line_start == record.line_end {
        format!("line {}", record.line_start)
    } else {
        format!("lines {}-{}", record.line_start, record.line_end)
    };
    // A trailing newline would otherwise number a line past the range the label declares.
    let count = (record.line_end as i64 - record.line_start as i64 + 1).max(0) as usize;
    let body = record
        .text
        .split('\n')
        .take(count)
        .enumerate()
        .map(|(index, line)| format!("{} | {line}", record.line_start + index as u64))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "- {} {range}:\n{}",
        record.terminal_label.as_str(),
        indent(&body),
    )
}

fn render_element_details_entry(record: &ElementContextDetails) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !record.page_url.is_empty() {
        lines.push(format!("url: {}", record.page_url));
    }
    if let Some(selector) = record.selector.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("selector: {selector}"));
    }
    if let Some(source) = record.source.as_ref().filter(|source| {
        source
            .file_name
            .as_deref()
            .is_some_and(|name| !name.is_empty())
    }) {
        let mut parts = vec![source.file_name.clone().unwrap_or_default()];
        parts.extend(
            [source.line_number, source.column_number]
                .into_iter()
                .flatten()
                .map(|part| part.to_string()),
        );
        lines.push(format!("source: {}", parts.join(":")));
    }
    if !record.html_preview.is_empty() {
        lines.push(format!("html:\n{}", indent(&record.html_preview)));
    }
    if !record.styles.is_empty() {
        lines.push(format!("styles:\n{}", indent(&record.styles)));
    }
    let name = record
        .component_name
        .as_deref()
        .unwrap_or(record.tag_name.as_str());
    format!("- <{name}>:\n{}", indent(&lines.join("\n")))
}

fn render_preview_body(record: &PreviewAnnotationContextRecord) -> String {
    let page = if !record.page_url.is_empty() {
        record.page_url.as_str()
    } else {
        record.page_title.as_deref().unwrap_or("")
    };
    let mut lines = vec![
        format!("Id: {}", record.annotation_id),
        format!("Page: {page}"),
    ];
    if !record.comment.is_empty() {
        lines.push(format!("Comment: {}", record.comment));
    }
    if !record.target_summary.is_empty() {
        lines.push(format!("Targets: {}", record.target_summary));
    }
    if !record.style_changes.is_empty() {
        lines.push("Requested visual changes:".to_owned());
        lines.extend(
            record
                .style_changes
                .iter()
                .map(|change| format!("- {change}")),
        );
    }
    if let Some(elements) = record
        .elements
        .as_ref()
        .filter(|elements| !elements.is_empty())
    {
        lines.push("<element_context>".to_owned());
        lines.push(
            elements
                .iter()
                .map(render_element_details_entry)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        lines.push("</element_context>".to_owned());
    }
    lines.join("\n")
}

/// Longest run of consecutive backticks in `text`.
fn longest_backtick_run(text: &str) -> usize {
    text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0)
}

fn render_review_comment(record: &ReviewCommentContextRecord) -> String {
    let attributes = [
        format!(
            "sectionId=\"{}\"",
            escape_attribute(record.section_id.as_str())
        ),
        format!(
            "sectionTitle=\"{}\"",
            escape_attribute(&record.section_title)
        ),
        format!(
            "filePath=\"{}\"",
            escape_attribute(record.file_path.as_str())
        ),
        format!("rangeLabel=\"{}\"", escape_attribute(&record.range_label)),
        format!("startIndex=\"{}\"", record.start_index),
        format!("endIndex=\"{}\"", record.end_index),
    ]
    .join(" ");
    // The fence must be longer than any run of backticks inside the diff so the body stays intact.
    let fence = "`".repeat(3.max(longest_backtick_run(&record.diff) + 1));
    let body = if record.diff.is_empty() {
        record.text.clone()
    } else {
        format!(
            "{}\n\n{fence}{}\n{}\n{fence}",
            record.text,
            record.fence_language.as_deref().unwrap_or("diff"),
            record.diff,
        )
    };
    format!("<review_comment {attributes}>\n{body}\n</review_comment>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer_context_legacy::upgrade_legacy_context_message;
    use crate::composer_context_references::format_composer_context_record_reference;
    use serde_json::{Value, json};

    fn record(value: Value) -> ComposerContextRecord {
        serde_json::from_value(value).expect("valid record")
    }

    fn terminal() -> ComposerContextRecord {
        record(json!({
            "version": 1,
            "contextId": "terminal_t1",
            "kind": "terminal",
            "label": "Terminal 1 lines 3-4",
            "terminalId": "terminal-1",
            "terminalLabel": "Terminal 1",
            "lineStart": 3,
            "lineEnd": 4,
            "text": "boom\nagain",
        }))
    }

    fn review() -> ComposerContextRecord {
        record(json!({
            "version": 1,
            "contextId": "review-comment_rc1",
            "kind": "review-comment",
            "label": "b.ts L4",
            "sectionId": "file:a/b.ts",
            "sectionTitle": "File comment",
            "filePath": "a/b.ts",
            "startIndex": 3,
            "endIndex": 3,
            "rangeLabel": "L4",
            "text": "Why this branch?",
            "diff": "const x = 1;",
            "fenceLanguage": "ts",
        }))
    }

    fn annotation() -> ComposerContextRecord {
        record(json!({
            "version": 1,
            "contextId": "preview-annotation_ann1",
            "kind": "preview-annotation",
            "label": "Fix the checkout button",
            "annotationId": "ann1",
            "pageUrl": "https://example.com/checkout",
            "pageTitle": "Checkout",
            "comment": "Make this button clearer",
            "targetSummary": "1 selected element",
            "styleChanges": ["color: red → blue"],
            "elements": [{
                "pageUrl": "https://example.com/checkout",
                "pageTitle": "Checkout",
                "tagName": "button",
                "selector": "#submit-order",
                "htmlPreview": "<button id=\"submit-order\">Buy now</button>",
                "componentName": "SubmitOrderButton",
                "source": {
                    "functionName": "SubmitOrderButton",
                    "fileName": "src/Checkout.tsx",
                    "lineNumber": 42,
                    "columnNumber": 7,
                },
                "styles": "color: red;",
            }],
        }))
    }

    fn reference(record: &ComposerContextRecord) -> String {
        format_composer_context_record_reference(record)
    }

    #[test]
    fn carries_terminal_payloads_an_older_server_would_otherwise_discard() {
        let terminal = terminal();
        let text = format!("Look at {} please", reference(&terminal));
        let legacy = serialize_legacy_context_message(&text, std::slice::from_ref(&terminal));

        // An older server forwards text verbatim, so the payload has to be in it.
        assert!(!legacy.contains("t3-context://"));
        assert!(legacy.contains("boom"));

        // A newer client reading that message reconstructs the same excerpt.
        let upgraded = upgrade_legacy_context_message(&legacy);
        assert_eq!(upgraded.records.len(), 1);
        let value = serde_json::to_value(&upgraded.records[0]).unwrap();
        assert_eq!(value["kind"], "terminal");
        assert_eq!(value["terminalLabel"], "Terminal 1");
        assert_eq!(value["lineStart"], 3);
        assert_eq!(value["lineEnd"], 4);
        assert_eq!(value["text"], "boom\nagain");
    }

    #[test]
    fn inlines_a_review_comment_with_its_diff_intact() {
        let review = review();
        let text = format!("See {} here", reference(&review));
        let legacy = serialize_legacy_context_message(&text, std::slice::from_ref(&review));
        assert!(!legacy.contains("t3-context://"));

        let upgraded = upgrade_legacy_context_message(&legacy);
        let value = serde_json::to_value(&upgraded.records[0]).unwrap();
        assert_eq!(value["kind"], "review-comment");
        assert_eq!(value["filePath"], "a/b.ts");
        assert_eq!(value["rangeLabel"], "L4");
        assert_eq!(value["text"], "Why this branch?");
        assert_eq!(value["diff"], "const x = 1;");
    }

    #[test]
    fn preserves_the_separator_between_a_skill_and_a_trailing_pr_through_an_older_server() {
        let mut pull_request = serde_json::to_value(review()).unwrap();
        for (key, value) in [
            ("label", json!("#11440")),
            ("sectionId", json!("pull-request:11440")),
            ("sectionTitle", json!("Pull request")),
            ("filePath", json!("PR #11440")),
            ("rangeLabel", json!("summary")),
            ("text", json!("Audit Mobile Photo Import")),
            ("diff", json!("")),
        ] {
            pull_request[key] = value;
        }
        let pull_request = record(pull_request);
        for separator in [" ", "\n", "\n\n"] {
            let text = format!("$pr-audit{separator}{}", reference(&pull_request));
            let legacy =
                serialize_legacy_context_message(&text, std::slice::from_ref(&pull_request));
            let upgraded = upgrade_legacy_context_message(&legacy);

            assert_eq!(upgraded.records.len(), 1, "{separator:?}");
            assert_eq!(
                upgraded.text,
                format!("$pr-audit{separator}{}", reference(&upgraded.records[0])),
                "{separator:?}"
            );
        }
    }

    #[test]
    fn retains_picked_element_details_for_preview_annotations_sent_through_an_older_server() {
        let annotation = annotation();
        let text = format!("Update {}", reference(&annotation));
        let upgraded = upgrade_legacy_context_message(&serialize_legacy_context_message(
            &text,
            std::slice::from_ref(&annotation),
        ));
        let value = serde_json::to_value(&upgraded.records[0]).unwrap();
        assert_eq!(value["kind"], "preview-annotation");
        assert_eq!(value["pageUrl"], "https://example.com/checkout");
        assert_eq!(value["elements"][0]["selector"], "#submit-order");
        assert_eq!(
            value["elements"][0]["htmlPreview"],
            "<button id=\"submit-order\">Buy now</button>"
        );
        assert_eq!(
            value["elements"][0]["source"]["fileName"],
            "src/Checkout.tsx"
        );
        assert_eq!(value["elements"][0]["source"]["lineNumber"], 42);
        assert_eq!(value["elements"][0]["source"]["columnNumber"], 7);
    }

    #[test]
    fn keeps_prose_without_context_untouched() {
        assert_eq!(
            serialize_legacy_context_message("just prose", &[]),
            "just prose"
        );
    }

    #[test]
    fn appends_review_comments_the_text_never_referenced() {
        let legacy = serialize_legacy_context_message("just prose", &[review()]);
        assert!(legacy.contains("Why this branch?"));
        let upgraded = upgrade_legacy_context_message(&legacy);
        assert_eq!(upgraded.records.len(), 1);
        let value = serde_json::to_value(&upgraded.records[0]).unwrap();
        assert_eq!(value["kind"], "review-comment");
        assert_eq!(value["filePath"], "a/b.ts");
        assert_eq!(value["text"], "Why this branch?");
    }

    #[test]
    fn numbers_terminal_output_within_the_declared_line_range() {
        let ranged = record(json!({
            "version": 1,
            "contextId": "terminal_t1",
            "kind": "terminal",
            "label": "Terminal 1 lines 3-4",
            "terminalId": "terminal-1",
            "terminalLabel": "Terminal 1",
            "lineStart": 3,
            "lineEnd": 4,
            "text": "boom\nagain\n",
        }));
        let legacy = serialize_legacy_context_message("look", std::slice::from_ref(&ranged));
        assert!(legacy.contains("3 | boom\n  4 | again"));
        assert!(!legacy.contains("5 |"));
    }

    #[test]
    fn lengthens_the_review_fence_past_backtick_runs_in_the_diff() {
        let review = record(json!({
            "version": 1,
            "contextId": "review-comment_rc2",
            "kind": "review-comment",
            "label": "c.md L1",
            "sectionId": "file:c.md",
            "sectionTitle": "File comment",
            "filePath": "c.md",
            "startIndex": 1,
            "endIndex": 1,
            "rangeLabel": "L1",
            "text": "Quoted fence",
            "diff": "````\nnested\n````",
            "fenceLanguage": "md",
        }));
        let legacy = serialize_legacy_context_message("", std::slice::from_ref(&review));
        assert!(legacy.contains("`````md\n````\nnested\n````\n`````"));
        let upgraded = upgrade_legacy_context_message(&legacy);
        let value = serde_json::to_value(&upgraded.records[0]).unwrap();
        assert_eq!(value["diff"], "````\nnested\n````");
        assert_eq!(value["fenceLanguage"], "md");
    }
}
