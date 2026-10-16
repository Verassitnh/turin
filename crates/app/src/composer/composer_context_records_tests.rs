//! Port of upstream/apps/web/src/lib/composerContextRecords.test.ts

use turin_contracts::base_schemas::{EnvironmentId, MessageId, PositiveInt, ThreadId};
use turin_contracts::composer_context::{
    ComposerContextRecord, KnownComposerContextRecord, OrchestrationMessageContext,
    PullRequestContextMetadata,
};
use turin_contracts::source_control::ChangeRequestState;
use turin_shared::composer_context_references::format_composer_context_reference;

use super::*;

// The builders return `Result`; the upstream cases build valid drafts, so these
// wrappers unwrap them. The error paths are tested at the end by their full names.
fn terminal_context_record(context: &TerminalContextDraft) -> TerminalContextRecord {
    super::terminal_context_record(context).unwrap()
}

fn review_comment_context_record(comment: &ReviewCommentContext) -> ReviewCommentContextRecord {
    super::review_comment_context_record(comment).unwrap()
}

fn attachment_context_record(bound: &BoundComposerAttachment) -> ComposerContextRecord {
    super::attachment_context_record(bound).unwrap()
}

fn build_message_context(input: MessageContextInput<'_>) -> Option<OrchestrationMessageContext> {
    super::build_message_context(input).unwrap()
}
use crate::composer::attachment_upload_state::{AttachmentUploadState, ReadyAttachmentUpload};

fn thread(id: &str) -> ThreadId {
    ThreadId::new(id).unwrap()
}

fn annotation() -> PreviewAnnotationPayload {
    PreviewAnnotationPayload {
        id: "ann_1".into(),
        page_url: "http://localhost:3000/checkout".into(),
        page_title: Some("Checkout".into()),
        comment: "  Make this   bigger ".into(),
        elements: vec![PreviewElementTarget {
            id: "el_1".into(),
            rect: PreviewRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            element: PickedElementPayload {
                page_url: "http://localhost:3000/checkout".into(),
                page_title: Some("Checkout".into()),
                tag_name: "BUTTON".into(),
                selector: Some("#pay".into()),
                html_preview: "<button id=\"pay\">Pay</button>".into(),
                component_name: Some("Button".into()),
                source: None,
                stack: Vec::new(),
                styles: "color: red;".into(),
                picked_at: "2026-01-01T00:00:00.000Z".into(),
            },
        }],
        regions: Vec::new(),
        strokes: Vec::new(),
        style_changes: vec![PreviewStyleChange {
            target_id: "el_1".into(),
            selector: Some("#pay".into()),
            property: "font-size".into(),
            previous_value: String::new(),
            value: "20px".into(),
        }],
        screenshot: None,
        created_at: "2026-01-01T00:00:00.000Z".into(),
    }
}

fn image(id: &str, name: &str) -> ComposerContextAttachment {
    ComposerContextAttachment::Image(ComposerImageDraft {
        id: id.into(),
        name: name.into(),
        mime_type: "image/png".into(),
        size_bytes: 1,
    })
}

fn file_draft(id: &str) -> ComposerFileDraft {
    ComposerFileDraft {
        id: id.into(),
        name: "notes.txt".into(),
        mime_type: "text/plain".into(),
        size_bytes: 1,
        uploaded_attachment_id: None,
        upload_environment_id: None,
    }
}

fn review(id: &str, range_label: &str) -> ReviewCommentContext {
    ReviewCommentContext {
        id: id.into(),
        section_id: "s".into(),
        section_title: "Review".into(),
        file_path: "a.ts".into(),
        start_index: 0,
        end_index: 0,
        range_label: range_label.into(),
        text: "Review".into(),
        diff: String::new(),
        fence_language: None,
        pull_request: None,
    }
}

fn terminal(id: &str) -> TerminalContextDraft {
    TerminalContextDraft {
        id: id.into(),
        thread_id: thread("t1"),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal".into(),
        line_start: 1,
        line_end: 1,
        text: "output".into(),
    }
}

fn decode_message(context: &OrchestrationMessageContext) -> OrchestrationMessageContext {
    let value = serde_json::to_value(context).unwrap();
    serde_json::from_value(value).unwrap()
}

fn ready(attachment_id: &str) -> AttachmentUploadState {
    AttachmentUploadState::Ready(ReadyAttachmentUpload {
        environment_id: "env".into(),
        attachment_id: attachment_id.into(),
    })
}

#[test]
fn copies_only_ready_or_persisted_server_side_attachment_ids() {
    let image = image("local-image", "shot.png");
    assert!(uploaded_attachment_context_record(&image, None).is_none());
    assert!(
        uploaded_attachment_context_record(
            &image,
            Some(&AttachmentUploadState::Uploading {
                environment_id: "env".into(),
                progress: 0.5,
                previous: None,
            }),
        )
        .is_none()
    );
    assert!(
        uploaded_attachment_context_record(
            &image,
            Some(&AttachmentUploadState::Failed {
                environment_id: "env".into(),
                reason: "offline".into(),
                attachment_id: Some("unfinished".into()),
                previous: None,
            }),
        )
        .is_none()
    );
    let uploaded =
        uploaded_attachment_context_record(&image, Some(&ready("uploaded-image"))).unwrap();
    assert_eq!(uploaded.context_id().as_str(), "image_local-image");
    match &uploaded {
        ComposerContextRecord::Known(KnownComposerContextRecord::Image(record)) => {
            assert_eq!(record.attachment_id.as_str(), "uploaded-image");
        }
        other => panic!("expected image, got {other:?}"),
    }
    let file = ComposerContextAttachment::File(file_draft("local-file"));
    assert!(uploaded_attachment_context_record(&file, None).is_none());
    let mut persisted = file_draft("local-file");
    persisted.uploaded_attachment_id = Some("persisted-file".into());
    persisted.upload_environment_id = Some("env".into());
    let uploaded =
        uploaded_attachment_context_record(&ComposerContextAttachment::File(persisted), None)
            .unwrap();
    assert_eq!(uploaded.context_id().as_str(), "file_local-file");
    match &uploaded {
        ComposerContextRecord::Known(KnownComposerContextRecord::File(record)) => {
            assert_eq!(record.attachment_id.as_str(), "persisted-file");
        }
        other => panic!("expected file, got {other:?}"),
    }
}

#[test]
fn does_not_bind_an_annotation_screenshot_to_a_same_id_file() {
    let note = annotation();
    let context = build_message_context(MessageContextInput {
        terminal_contexts: &[],
        review_comments: &[],
        preview_annotations: std::slice::from_ref(&note),
        thread_contexts: &[],
        attachments: &[BoundComposerAttachment {
            attachment: ComposerContextAttachment::File(ComposerFileDraft {
                id: note.id.clone(),
                name: "notes.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 1,
                uploaded_attachment_id: None,
                upload_environment_id: None,
            }),
            attachment_id: "uploaded-file".into(),
        }],
    })
    .unwrap();
    assert_eq!(
        context
            .records
            .iter()
            .map(|record| record.kind())
            .collect::<Vec<_>>(),
        ["preview-annotation", "file"]
    );
    let value = serde_json::to_value(&context.records[0]).unwrap();
    assert!(value.get("screenshotContextId").is_none());
}

#[test]
fn preserves_canonical_terminal_ids_across_repeated_imports() {
    for id in ["x", "terminal_x"] {
        let record = terminal_context_record(&terminal(id));
        let restored =
            terminal_context_record(&terminal_context_draft_from_record(&record, thread("t1")));
        assert_eq!(restored, record);
        assert_eq!(
            terminal_context_record(&terminal_context_draft_from_record(&restored, thread("t1"))),
            record
        );
    }
}

#[test]
fn preserves_canonical_review_ids_across_imports() {
    for id in ["x", "review-comment_x"] {
        let record = review_comment_context_record(&review(id, "L1"));
        assert_eq!(
            review_comment_context_record(&review_comment_from_record(&record)),
            record
        );
    }
}

#[test]
fn preserves_canonical_annotation_ids_across_imports() {
    for id in ["x", "preview-annotation_x"] {
        let mut payload = annotation();
        payload.id = id.into();
        let record = preview_annotation_context_record(&payload, None);
        assert_eq!(
            preview_annotation_context_record(&preview_annotation_from_record(&record), None)
                .context_id,
            record.context_id
        );
    }
}

#[test]
fn builds_distinct_records_for_producer_ids_that_differ_by_a_kind_prefix() {
    let attachments: Vec<_> = ["x", "image_x"]
        .into_iter()
        .map(|id| BoundComposerAttachment {
            attachment: ComposerContextAttachment::Image(ComposerImageDraft {
                id: id.into(),
                name: format!("{id}.png"),
                mime_type: "image/png".into(),
                size_bytes: 1,
            }),
            attachment_id: format!("uploaded-{id}"),
        })
        .collect();
    let context = build_message_context(MessageContextInput {
        terminal_contexts: &[],
        review_comments: &[],
        preview_annotations: &[],
        thread_contexts: &[],
        attachments: &attachments,
    })
    .unwrap();
    let decoded = decode_message(&context);
    assert_eq!(
        decoded
            .records
            .iter()
            .map(|record| record.context_id().as_str())
            .collect::<Vec<_>>(),
        ["image_x", "image_image_x"]
    );
}

#[test]
fn scopes_colliding_producer_ids_and_links_the_annotation_to_its_screenshot() {
    let id = "same.id:1";
    let mut note = annotation();
    note.id = id.into();
    let context = build_message_context(MessageContextInput {
        terminal_contexts: &[TerminalContextDraft {
            id: id.into(),
            thread_id: thread("t1"),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            terminal_id: "default".into(),
            terminal_label: "Terminal".into(),
            line_start: 1,
            line_end: 1,
            text: "output".into(),
        }],
        review_comments: &[ReviewCommentContext {
            id: id.into(),
            section_id: "s".into(),
            section_title: "Review".into(),
            file_path: "file.ts".into(),
            start_index: 0,
            end_index: 0,
            range_label: "L1".into(),
            text: "Review".into(),
            diff: String::new(),
            fence_language: None,
            pull_request: None,
        }],
        preview_annotations: &[note],
        thread_contexts: &[],
        attachments: &[
            BoundComposerAttachment {
                attachment: image(id, "shot.png"),
                attachment_id: "uploaded-image".into(),
            },
            BoundComposerAttachment {
                attachment: ComposerContextAttachment::File(ComposerFileDraft {
                    id: id.into(),
                    name: "file.txt".into(),
                    mime_type: "text/plain".into(),
                    size_bytes: 1,
                    uploaded_attachment_id: None,
                    upload_environment_id: None,
                }),
                attachment_id: "uploaded-file".into(),
            },
        ],
    })
    .unwrap();
    assert_eq!(decode_message(&context).records.len(), 5);
    let preview = context
        .records
        .iter()
        .find(|record| record.kind() == "preview-annotation")
        .unwrap();
    let image = context
        .records
        .iter()
        .find(|record| record.kind() == "image")
        .unwrap();
    match preview {
        ComposerContextRecord::Known(KnownComposerContextRecord::PreviewAnnotation(record)) => {
            assert_eq!(
                record.screenshot_context_id.as_ref().map(|id| id.as_str()),
                Some(image.context_id().as_str())
            );
        }
        other => panic!("expected preview, got {other:?}"),
    }
    match image {
        ComposerContextRecord::Known(KnownComposerContextRecord::Image(record)) => {
            assert_eq!(record.attachment_id.as_str(), "uploaded-image");
        }
        other => panic!("expected image, got {other:?}"),
    }
}

#[test]
fn restores_multiple_edits_on_one_target_without_parsing_display_strings() {
    let mut payload = annotation();
    payload.style_changes.push(PreviewStyleChange {
        target_id: "el_1".into(),
        selector: Some("#pay".into()),
        property: "content".into(),
        previous_value: "a → b".into(),
        value: "first\nsecond → third".into(),
    });
    let restored =
        preview_annotation_from_record(&preview_annotation_context_record(&payload, None));
    assert_eq!(restored.style_changes, payload.style_changes);
    assert_eq!(restored.elements[0].id, "el_1");
}

#[test]
fn presents_review_ranges_consistently() {
    let cases = [
        ("+181", "a.ts L181"),
        ("+181 to +183", "a.ts L181 to L183"),
        ("-63", "a.ts L63 (before)"),
        ("L4", "a.ts L4"),
    ];
    for (range_label, expected) in cases {
        assert_eq!(
            review_comment_context_label(&ReviewCommentContext {
                id: "review-1".into(),
                section_id: "file:src/a.ts".into(),
                section_title: "File comment".into(),
                file_path: "src/a.ts".into(),
                start_index: 0,
                end_index: 0,
                range_label: range_label.into(),
                text: String::new(),
                diff: String::new(),
                fence_language: None,
                pull_request: None,
            }),
            expected
        );
    }
}

fn pull_request() -> PullRequestContextMetadata {
    PullRequestContextMetadata {
        number: PositiveInt::new(42).unwrap(),
        title: "Improve context chips".into(),
        url: "https://github.com/pingdotgg/t3code/pull/42".into(),
        head_branch: "feat/context-chips".into(),
        base_branch: "main".into(),
        state: ChangeRequestState::Open,
        is_draft: false,
    }
}

#[test]
fn distinguishes_a_pr_summary_from_a_comment_on_its_diff() {
    let summary = ReviewCommentContext {
        id: "review-1".into(),
        section_id: "pull-request:42".into(),
        section_title: "PR #42".into(),
        file_path: "PR #42".into(),
        start_index: 0,
        end_index: 0,
        range_label: "Improve context chips".into(),
        text: "Pull request details".into(),
        diff: String::new(),
        fence_language: None,
        pull_request: Some(pull_request()),
    };
    assert!(is_pull_request_summary_context(&summary));
    assert_eq!(review_comment_context_label(&summary), "#42");
    assert_eq!(
        pull_request_context_display_state(&summary)
            .unwrap()
            .as_str(),
        "open"
    );
    assert_eq!(
        pull_request_context_kind_label(&summary),
        "Open pull request"
    );
    let mut draft = summary.clone();
    draft.pull_request.as_mut().unwrap().is_draft = true;
    assert_eq!(
        pull_request_context_display_state(&draft).unwrap().as_str(),
        "draft"
    );
    let mut comment = summary;
    comment.pull_request = None;
    comment.file_path = "src/a.ts".into();
    comment.range_label = "+12".into();
    comment.diff = "+const answer = 42;".into();
    assert!(!is_pull_request_summary_context(&comment));
}

#[test]
fn builds_a_preview_annotation_record_with_element_details_and_readable_style_changes() {
    let payload = annotation();
    assert_eq!(
        preview_annotation_context_label(&payload),
        "Make this bigger"
    );
    let record = preview_annotation_context_record(&payload, Some("ann_1"));
    assert_eq!(
        serde_json::to_value(&record).unwrap(),
        serde_json::json!({
            "version": 1,
            "contextId": "preview-annotation_ann_1",
            "kind": "preview-annotation",
            "label": "Make this bigger",
            "annotationId": "ann_1",
            "pageUrl": "http://localhost:3000/checkout",
            "pageTitle": "Checkout",
            "comment": "Make this   bigger",
            "targetSummary": "1 selected element",
            "styleChanges": ["font-size: (unset) → 20px"],
            "styleChangeDetails": [{
                "targetId": "el_1",
                "selector": "#pay",
                "property": "font-size",
                "previousValue": "",
                "value": "20px"
            }],
            "elementIds": ["el_1"],
            "screenshotContextId": "image_ann_1",
            "elements": [{
                "pageUrl": "http://localhost:3000/checkout",
                "pageTitle": "Checkout",
                "tagName": "button",
                "selector": "#pay",
                "htmlPreview": "<button id=\"pay\">Pay</button>",
                "componentName": "Button",
                "source": null,
                "styles": "color: red;"
            }]
        })
    );
}

fn remove_inline_context_reference(prompt: &str, context_id: &str) -> String {
    let occurrences: Vec<_> = collect_composer_context_references(prompt)
        .into_iter()
        .filter(|candidate| candidate.context_id.as_str() == context_id)
        .collect();
    if occurrences.is_empty() {
        return prompt.to_owned();
    }
    let mut result = prompt.to_owned();
    let mut cursor = prompt.len();
    for occurrence in occurrences.into_iter().rev() {
        let mut start = occurrence.start;
        let mut end = occurrence.end;
        let bytes = result.as_bytes();
        if end < bytes.len() && bytes[end] == b' ' {
            end += 1;
        } else if start > 0 && bytes[start - 1] == b' ' {
            start -= 1;
        }
        result = format!("{}{}", &result[..start], &result[end..]);
        cursor = start;
    }
    if cursor >= result.len() {
        result = result.trim_end().to_owned();
    }
    result
}

#[test]
fn removes_an_expired_terminal_chip_by_its_kind_scoped_id() {
    let context = TerminalContextDraft {
        id: "term-1".into(),
        thread_id: thread("t"),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 3,
        line_end: 4,
        text: "boom".into(),
    };
    let reference = terminal_context_reference(&context);
    assert_eq!(reference.context_id.as_str(), "terminal_term-1");
    let prompt = format!(
        "prose {} tail",
        format_composer_context_reference(
            reference.kind,
            reference.context_id.as_str(),
            &reference.label
        )
    );
    assert_eq!(
        remove_inline_context_reference(&prompt, &context.id),
        prompt
    );
    assert_eq!(
        remove_inline_context_reference(&prompt, reference.context_id.as_str()),
        "prose tail"
    );
}

#[test]
fn clamps_an_oversized_review_selection_so_the_record_still_encodes() {
    let build = |diff_length: usize| {
        review_comment_context_record(&ReviewCommentContext {
            id: "rc-big".into(),
            section_id: "file:a/b.ts".into(),
            section_title: "File comment".into(),
            file_path: "a/b.ts".into(),
            start_index: 0,
            end_index: 1,
            range_label: "L1".into(),
            text: "Why?".into(),
            diff: "d".repeat(diff_length),
            fence_language: None,
            pull_request: None,
        })
    };
    let at_limit = build(32_000);
    assert_eq!(at_limit.diff.len(), 32_000);
    let over_limit = build(32_001);
    assert!(turin_shared::js_string::utf16_len(&over_limit.diff) <= 32_000);
    assert!(over_limit.diff.ends_with("… truncated …"));
    let encode = |record| {
        decode_message(&OrchestrationMessageContext {
            version: turin_contracts::composer_context::ComposerContextRecordVersion,
            records: vec![ComposerContextRecord::Known(
                KnownComposerContextRecord::ReviewComment(record),
            )],
        })
    };
    encode(at_limit);
    encode(over_limit);
}

#[test]
fn keeps_a_region_only_annotations_target_summary_and_screenshot_across_a_round_trip() {
    let mut region_only = annotation();
    region_only.elements.clear();
    region_only.style_changes.clear();
    region_only.regions.push(PreviewRegionTarget {
        id: "rg_1".into(),
        rect: PreviewRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        },
    });
    let record = preview_annotation_context_record(&region_only, Some("ann_1"));
    assert_eq!(record.target_summary, "1 marked region");
    assert_eq!(
        record.screenshot_context_id.as_ref().map(|id| id.as_str()),
        Some("image_ann_1")
    );
    let reencoded =
        preview_annotation_context_record(&preview_annotation_from_record(&record), Some("ann_1"));
    assert_eq!(reencoded.target_summary, "1 marked region");
    assert_eq!(
        reencoded
            .screenshot_context_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("image_ann_1")
    );
}

fn terminal_excerpt(text: &str) -> TerminalContextRecord {
    terminal_context_record(&TerminalContextDraft {
        id: "term-1".into(),
        thread_id: thread("t"),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 1,
        line_end: 2,
        text: text.into(),
    })
}

fn known_terminal(record: TerminalContextRecord) -> ComposerContextRecord {
    ComposerContextRecord::Known(KnownComposerContextRecord::Terminal(record))
}

#[test]
fn treats_colliding_ids_with_different_payloads_as_distinct_records() {
    let base = known_terminal(terminal_excerpt("A"));
    assert!(is_same_composer_context_payload(&base, &base));
    let mut relabeled = serde_json::to_value(&base).unwrap();
    relabeled["label"] = serde_json::json!("different");
    let relabeled: ComposerContextRecord = serde_json::from_value(relabeled).unwrap();
    assert!(is_same_composer_context_payload(&base, &relabeled));
    let other = known_terminal(terminal_excerpt("B"));
    assert!(!is_same_composer_context_payload(&base, &other));
}

#[test]
fn finds_the_destination_collision_for_different_legacy_terminal_messages() {
    let legacy = |text: &str| {
        let upgraded = turin_shared::composer_context_legacy::upgrade_legacy_context_message(
            &format!(
                "Inspect this\n\n<terminal_context>\n- Terminal 1 line 1:\n  1 | {text}\n</terminal_context>"
            ),
        );
        as_known_context_record(&upgraded.records[0])
            .unwrap()
            .clone()
    };
    let first = legacy("A");
    let second = legacy("B");
    let KnownComposerContextRecord::Terminal(first_terminal) = &first else {
        panic!("expected a legacy terminal record");
    };
    let destination_id = terminal_context_reference(&terminal_context_draft_from_record(
        first_terminal,
        thread("t"),
    ))
    .context_id;
    assert_eq!(destination_id.as_str(), "terminal_legacy_terminal_1");
    assert_eq!(
        composer_context_import_lookup_ids(&second)[0],
        destination_id
    );
    assert!(!is_same_composer_context_payload(
        &ComposerContextRecord::Known(first),
        &ComposerContextRecord::Known(second)
    ));
}

#[test]
fn treats_an_imported_legacy_id_and_its_canonical_reconstruction_as_the_same_excerpt() {
    let canonical = known_terminal(terminal_excerpt("A"));
    let mut imported = serde_json::to_value(&canonical).unwrap();
    imported["contextId"] = serde_json::json!("legacy_terminal_1");
    let imported: ComposerContextRecord = serde_json::from_value(imported).unwrap();
    assert!(is_same_composer_context_payload(&canonical, &imported));
    let changed = known_terminal(terminal_excerpt("B"));
    assert!(!is_same_composer_context_payload(&canonical, &changed));
}

#[test]
fn compares_nested_annotation_element_and_source_payloads() {
    let base_record = preview_annotation_context_record(&annotation(), None);
    let base = ComposerContextRecord::Known(KnownComposerContextRecord::PreviewAnnotation(
        base_record.clone(),
    ));
    let mut relabeled = serde_json::to_value(&base).unwrap();
    relabeled["label"] = serde_json::json!("Different display label");
    let relabeled: ComposerContextRecord = serde_json::from_value(relabeled).unwrap();
    assert!(is_same_composer_context_payload(&base, &relabeled));

    let mut changed_html = serde_json::to_value(&base).unwrap();
    changed_html["elements"][0]["htmlPreview"] =
        serde_json::json!("<button id=\"pay\">Changed</button>");
    let changed_html: ComposerContextRecord = serde_json::from_value(changed_html).unwrap();
    assert!(!is_same_composer_context_payload(&base, &changed_html));

    let mut changed_source = serde_json::to_value(&base).unwrap();
    changed_source["elements"][0]["source"] = serde_json::json!({
        "functionName": "Checkout",
        "fileName": "src/Checkout.tsx",
        "lineNumber": 20,
        "columnNumber": 4
    });
    let changed_source: ComposerContextRecord = serde_json::from_value(changed_source).unwrap();
    assert!(!is_same_composer_context_payload(&base, &changed_source));
}

#[test]
fn builds_terminal_and_review_records_and_a_message_context_in_draft_order() {
    let terminal = terminal_context_record(&TerminalContextDraft {
        id: "term-1".into(),
        thread_id: thread("t"),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 3,
        line_end: 4,
        text: "\nboom\n".into(),
    });
    assert_eq!(terminal.kind, super::TerminalContextKind);
    assert_eq!(terminal.label, "Terminal 1 lines 3-4");
    assert_eq!(terminal.text, "boom");
    let review = review_comment_context_record(&ReviewCommentContext {
        id: "rc-1".into(),
        section_id: "file:a/b.ts".into(),
        section_title: "File comment".into(),
        file_path: "a/b.ts".into(),
        start_index: 3,
        end_index: 3,
        range_label: "L4".into(),
        text: "Why?".into(),
        diff: "const x = 1;".into(),
        fence_language: Some("ts".into()),
        pull_request: None,
    });
    assert_eq!(review.label, "b.ts L4");
    assert_eq!(review.fence_language.as_deref(), Some("ts"));
    let context = build_message_context(MessageContextInput {
        terminal_contexts: &[],
        review_comments: &[ReviewCommentContext {
            id: "rc-1".into(),
            section_id: "s".into(),
            section_title: "t".into(),
            file_path: "a/b.ts".into(),
            start_index: 0,
            end_index: 0,
            range_label: "L1".into(),
            text: String::new(),
            diff: String::new(),
            fence_language: None,
            pull_request: None,
        }],
        preview_annotations: &[annotation()],
        thread_contexts: &[],
        attachments: &[],
    })
    .unwrap();
    assert_eq!(
        context
            .records
            .iter()
            .map(|record| record.context_id().as_str())
            .collect::<Vec<_>>(),
        ["review-comment_rc-1", "preview-annotation_ann_1"]
    );
    assert!(
        build_message_context(MessageContextInput {
            terminal_contexts: &[],
            review_comments: &[],
            preview_annotations: &[],
            thread_contexts: &[],
            attachments: &[],
        })
        .is_none()
    );
}

#[test]
fn resolves_structured_context_directly_and_upgrades_legacy_text_otherwise() {
    let record = review_comment_context_record(&ReviewCommentContext {
        id: "rc-1".into(),
        section_id: "s".into(),
        section_title: "t".into(),
        file_path: "a/b.ts".into(),
        start_index: 3,
        end_index: 3,
        range_label: "L4".into(),
        text: String::new(),
        diff: String::new(),
        fence_language: None,
        pull_request: None,
    });
    let context = OrchestrationMessageContext {
        version: turin_contracts::composer_context::ComposerContextRecordVersion,
        records: vec![ComposerContextRecord::Known(
            KnownComposerContextRecord::ReviewComment(record),
        )],
    };
    let structured = resolve_user_message_context(UserMessageContextInput {
        text: "hi [b.ts L4](t3-context://v1/review-comment/rc-1)",
        context: Some(&context),
    });
    assert_eq!(
        structured
            .records_by_id
            .get("review-comment_rc-1")
            .unwrap()
            .kind(),
        "review-comment"
    );
    let legacy = resolve_user_message_context(UserMessageContextInput {
        text: "hi\n\n<terminal_context>\n- T line 1:\n  1 | x\n</terminal_context>",
        context: None,
    });
    assert_eq!(
        legacy.text,
        "hi\n\n[T line 1](t3-context://v1/terminal/legacy_terminal_1)"
    );
    assert_eq!(
        legacy
            .records_by_id
            .get("legacy_terminal_1")
            .unwrap()
            .kind(),
        "terminal"
    );
}

#[test]
fn binds_image_and_file_records_to_the_given_attachment_id() {
    let image = attachment_context_record(&BoundComposerAttachment {
        attachment: ComposerContextAttachment::Image(ComposerImageDraft {
            id: "img-1".into(),
            name: "shot.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 10,
        }),
        attachment_id: "pending-abc".into(),
    });
    assert_eq!(
        serde_json::to_value(&image).unwrap(),
        serde_json::json!({
            "version": 1,
            "contextId": "image_img-1",
            "kind": "image",
            "label": "shot.png",
            "attachmentId": "pending-abc",
            "name": "shot.png",
            "mimeType": "image/png",
            "sizeBytes": 10
        })
    );
    let file = attachment_context_record(&BoundComposerAttachment {
        attachment: ComposerContextAttachment::File(ComposerFileDraft {
            id: "file-1".into(),
            name: "notes.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 3,
            uploaded_attachment_id: Some("pending-def".into()),
            upload_environment_id: None,
        }),
        attachment_id: "pending-def".into(),
    });
    match &file {
        ComposerContextRecord::Known(KnownComposerContextRecord::File(record)) => {
            assert_eq!(record.context_id.as_str(), "file_file-1");
            assert_eq!(record.attachment_id.as_str(), "pending-def");
        }
        other => panic!("expected file, got {other:?}"),
    }
    let context = build_message_context(MessageContextInput {
        terminal_contexts: &[],
        review_comments: &[],
        preview_annotations: &[],
        thread_contexts: &[],
        attachments: &[BoundComposerAttachment {
            attachment: ComposerContextAttachment::File(ComposerFileDraft {
                id: "file-1".into(),
                name: "n".into(),
                mime_type: "text/plain".into(),
                size_bytes: 1,
                uploaded_attachment_id: None,
                upload_environment_id: None,
            }),
            attachment_id: "file-1".into(),
        }],
    })
    .unwrap();
    assert_eq!(
        context
            .records
            .iter()
            .map(|record| record.kind())
            .collect::<Vec<_>>(),
        ["file"]
    );
}

#[test]
fn folds_review_comment_ids_and_keeps_the_raw_id_in_the_draft_shape() {
    let record = review_comment_context_record(&ReviewCommentContext {
        id: "pull-request-finding:42".into(),
        section_id: "s".into(),
        section_title: "t".into(),
        file_path: "a/b.ts".into(),
        start_index: 0,
        end_index: 0,
        range_label: "L1".into(),
        text: String::new(),
        diff: String::new(),
        fence_language: None,
        pull_request: None,
    });
    let id = record.context_id.as_str();
    assert!(id.starts_with("review-comment_pull-request-finding-42-"));
    assert_eq!(
        id.len(),
        "review-comment_pull-request-finding-42-".len() + 16
    );
    assert!(
        id.as_bytes()[id.len() - 16..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    let context = OrchestrationMessageContext {
        version: turin_contracts::composer_context::ComposerContextRecordVersion,
        records: vec![ComposerContextRecord::Known(
            KnownComposerContextRecord::ReviewComment(record.clone()),
        )],
    };
    assert!(
        resolve_user_message_context(UserMessageContextInput {
            text: &format!("[b.ts L1](t3-context://v1/review-comment/{id})"),
            context: Some(&context),
        })
        .records_by_id
        .contains_key(id)
    );
}

#[test]
fn carries_only_records_for_chips_inside_the_selection() {
    let terminal = known_terminal(terminal_context_record(&TerminalContextDraft {
        id: "term-1".into(),
        thread_id: thread("t"),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 1,
        line_end: 1,
        text: "A".into(),
    }));
    let review = ComposerContextRecord::Known(KnownComposerContextRecord::ReviewComment(
        review_comment_context_record(&ReviewCommentContext {
            id: "rc-1".into(),
            section_id: "s".into(),
            section_title: "t".into(),
            file_path: "a/b.ts".into(),
            start_index: 3,
            end_index: 3,
            range_label: "L4".into(),
            text: String::new(),
            diff: String::new(),
            fence_language: None,
            pull_request: None,
        }),
    ));
    let environment = EnvironmentId::new("env").unwrap();
    let thread_id = thread("t");
    let message_id = MessageId::new("msg-1").unwrap();
    let records = [terminal.clone(), review.clone()];
    let fragment = selected_message_context_fragment(SelectedMessageContext {
        markdown: &format!(
            "see [b.ts L4](t3-context://v1/review-comment/{})",
            review.context_id()
        ),
        records: &records,
        environment_id: &environment,
        thread_id: Some(&thread_id),
        message_id: &message_id,
    })
    .unwrap();
    assert!(fragment.contains(review.context_id().as_str()));
    assert!(!fragment.contains(terminal.context_id().as_str()));
}

#[test]
fn references_keep_kind_scoped_ids() {
    let image = ComposerImageDraft {
        id: "img-1".into(),
        name: "shot.png".into(),
        mime_type: "image/png".into(),
        size_bytes: 10,
    };
    assert_eq!(
        image_context_reference(&image).context_id.as_str(),
        "image_img-1"
    );
    let file = file_draft("file-1");
    assert_eq!(
        file_context_reference(&file).context_id.as_str(),
        "file_file-1"
    );
    let comment = review("rc-1", "L1");
    assert_eq!(
        review_comment_context_reference(&comment)
            .context_id
            .as_str(),
        "review-comment_rc-1"
    );
    assert_eq!(
        preview_annotation_context_reference(&annotation())
            .context_id
            .as_str(),
        "preview-annotation_ann_1"
    );
    let reference = turin_contracts::environment::ScopedThreadRef {
        environment_id: EnvironmentId::new("env").unwrap(),
        thread_id: thread("thr"),
    };
    let record = thread_context_record(&reference, "My thread");
    assert_eq!(record.context_id.as_str(), "thread_thr");
    assert_eq!(record.label, "My thread");
    assert_eq!(
        thread_context_reference(&record).context_id.as_str(),
        "thread_thr"
    );
}

#[test]
fn returns_null_when_no_selected_chip_has_backing_records() {
    let terminal = known_terminal(terminal_context_record(&TerminalContextDraft {
        id: "term-1".into(),
        thread_id: thread("t"),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 1,
        line_end: 1,
        text: "A".into(),
    }));
    let review_id = review_comment_context_record(&ReviewCommentContext {
        id: "rc-1".into(),
        section_id: "s".into(),
        section_title: "t".into(),
        file_path: "a/b.ts".into(),
        start_index: 3,
        end_index: 3,
        range_label: "L4".into(),
        text: String::new(),
        diff: String::new(),
        fence_language: None,
        pull_request: None,
    })
    .context_id;
    let environment = EnvironmentId::new("env").unwrap();
    let thread_id = thread("t");
    let message_id = MessageId::new("msg-1").unwrap();
    let records = [terminal];
    assert!(
        selected_message_context_fragment(SelectedMessageContext {
            markdown: "just prose",
            records: &records,
            environment_id: &environment,
            thread_id: Some(&thread_id),
            message_id: &message_id,
        })
        .is_none()
    );
    assert!(
        selected_message_context_fragment(SelectedMessageContext {
            markdown: &format!("[b.ts L4](t3-context://v1/review-comment/{review_id})"),
            records: &[],
            environment_id: &environment,
            thread_id: Some(&thread_id),
            message_id: &message_id,
        })
        .is_none()
    );
}

// Builders return an error instead of panicking on a draft that cannot become
// a record, so the send boundary can report it. (No upstream cases: schema
// `make` throws there.)

#[test]
fn a_blank_terminal_id_is_an_error_not_a_panic() {
    let mut draft = terminal("t-1");
    draft.terminal_id = "   ".into();
    assert_eq!(
        super::terminal_context_record(&draft),
        Err(ContextRecordError(
            "The terminal id must not be blank.".into()
        ))
    );
    let mut labelless = terminal("t-2");
    labelless.terminal_label = String::new();
    assert!(super::terminal_context_record(&labelless).is_err());
}

#[test]
fn a_blank_review_section_or_file_path_is_an_error() {
    let mut comment = review("rc-1", "L1");
    comment.file_path = String::new();
    assert!(super::review_comment_context_record(&comment).is_err());
    let mut comment = review("rc-2", "L1");
    comment.section_id = " ".into();
    assert!(super::review_comment_context_record(&comment).is_err());
}

#[test]
fn an_attachment_the_contract_rejects_surfaces_as_an_error_or_no_record() {
    let bad_id = BoundComposerAttachment {
        attachment: image("img-1", "shot.png"),
        attachment_id: "not a valid id".into(),
    };
    assert!(super::attachment_context_record(&bad_id).is_err());

    let nameless = BoundComposerAttachment {
        attachment: image("img-2", "  "),
        attachment_id: "uploaded".into(),
    };
    assert!(super::attachment_context_record(&nameless).is_err());

    // A persisted file whose stored id is invalid has no record to offer.
    let mut persisted = file_draft("file-1");
    persisted.uploaded_attachment_id = Some("not a valid id".into());
    assert!(
        uploaded_attachment_context_record(&ComposerContextAttachment::File(persisted), None)
            .is_none()
    );

    // And one bad chip fails the whole context rather than dropping silently.
    let error = super::build_message_context(MessageContextInput {
        terminal_contexts: &[],
        review_comments: &[],
        preview_annotations: &[],
        thread_contexts: &[],
        attachments: &[bad_id],
    });
    assert!(error.is_err());
}
