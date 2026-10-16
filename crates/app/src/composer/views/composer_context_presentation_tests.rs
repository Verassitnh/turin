//! Ports composerContextPresentation.test.ts (the three
//! `composerContextRecordsFromDraft` cases) and covers the tooltip text.

use turin_contracts::base_schemas::ThreadId;
use turin_contracts::composer_context::{ComposerContextRecord, KnownComposerContextRecord};

use super::*;
use crate::composer::attachment_upload_state::ReadyAttachmentUpload;
use crate::composer::composer_context_records::{
    BoundComposerAttachment, MessageContextInput, build_message_context,
};

fn annotation(id: &str) -> PreviewAnnotationPayload {
    PreviewAnnotationPayload {
        id: id.into(),
        page_url: "http://localhost:3000/".into(),
        page_title: Some("Example".into()),
        comment: String::new(),
        elements: Vec::new(),
        regions: Vec::new(),
        strokes: Vec::new(),
        style_changes: Vec::new(),
        // The draft stores annotations without their data URL.
        screenshot: Some(()),
        created_at: "2026-10-08T00:00:00.000Z".into(),
    }
}

fn image(id: &str, name: &str, preview: &[u8]) -> ComposerImageAttachment {
    ComposerImageAttachment::new(id, name, "image/png", preview.to_vec())
}

fn file(id: &str, uploaded: Option<&str>) -> ComposerFileAttachment {
    ComposerFileAttachment {
        id: id.into(),
        name: "file.txt".into(),
        mime_type: "text/plain".into(),
        size_bytes: 4,
        data: Some(Arc::new(b"test".to_vec())),
        uploaded_attachment_id: uploaded.map(str::to_owned),
        upload_environment_id: None,
    }
}

#[test]
fn pairs_a_picked_element_with_its_crop_which_the_draft_keeps_as_an_image() {
    let crop = image("pick-1", "preview-annotation-pick-1.png", b"x");
    let records = composer_context_records_from_draft(&DraftContextInput {
        preview_annotations: &[annotation("pick-1")],
        images: std::slice::from_ref(&crop),
        ..Default::default()
    });
    match records.get("preview-annotation_pick-1") {
        Some(ComposerDraftContextRecord::PreviewAnnotation {
            screenshot: Some(screenshot),
            ..
        }) => assert_eq!(screenshot.name, "preview-annotation-pick-1.png"),
        other => panic!("expected the annotation with its crop, got {other:?}"),
    }
}

#[test]
fn recovers_the_uploaded_record_when_clipboard_data_points_at_an_attachment_already_in_the_draft() {
    let records = composer_context_records_from_draft(&DraftContextInput {
        files: &[file("file-1", Some("attachment-1"))],
        ..Default::default()
    });
    let entry = records.get("file_file-1").expect("the file's record");
    match uploaded_context_record_from_draft(entry) {
        Some(ComposerContextRecord::Known(KnownComposerContextRecord::File(record))) => {
            assert_eq!(record.context_id.as_str(), "file_file-1");
            assert_eq!(record.attachment_id.as_str(), "attachment-1");
        }
        other => panic!("expected a file record, got {other:?}"),
    }
}

#[test]
fn resolves_each_wire_reference_to_its_own_backing_draft_even_when_producer_ids_collide() {
    let id = "same.id:1";
    let terminal = TerminalContextDraft {
        id: id.into(),
        thread_id: ThreadId::new("t1").unwrap(),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal".into(),
        line_start: 1,
        line_end: 1,
        text: "output".into(),
    };
    let shot = image(id, "shot.png", b"x");
    let mut plain_file = file(id, None);
    plain_file.data = None;
    plain_file.size_bytes = 1;
    let records = composer_context_records_from_draft(&DraftContextInput {
        terminal_contexts: std::slice::from_ref(&terminal),
        images: std::slice::from_ref(&shot),
        files: std::slice::from_ref(&plain_file),
        ..Default::default()
    });
    let message = build_message_context(MessageContextInput {
        terminal_contexts: std::slice::from_ref(&terminal),
        review_comments: &[],
        preview_annotations: &[],
        thread_contexts: &[],
        attachments: &[
            BoundComposerAttachment {
                attachment: ComposerContextAttachment::Image(image_draft(&shot)),
                attachment_id: "uploaded-image".into(),
            },
            BoundComposerAttachment {
                attachment: ComposerContextAttachment::File(file_draft(&plain_file)),
                attachment_id: "uploaded-file".into(),
            },
        ],
    })
    .expect("context")
    .expect("records");
    assert_eq!(records.len(), 3);
    for record in &message.records {
        let kind = match record {
            ComposerContextRecord::Known(KnownComposerContextRecord::Terminal(_)) => "terminal",
            ComposerContextRecord::Known(KnownComposerContextRecord::Image(_)) => "image",
            ComposerContextRecord::Known(KnownComposerContextRecord::File(_)) => "file",
            other => panic!("unexpected record {other:?}"),
        };
        let entry = records
            .get(record.context_id().as_str())
            .expect("a draft behind every reference");
        let draft_kind = match entry {
            ComposerDraftContextRecord::Terminal(_) => "terminal",
            ComposerDraftContextRecord::Image { .. } => "image",
            ComposerDraftContextRecord::File { .. } => "file",
            other => panic!("unexpected draft {other:?}"),
        };
        assert_eq!(draft_kind, kind);
    }
}

#[test]
fn only_attachments_have_an_uploaded_record() {
    let thread = ThreadId::new("t1").unwrap();
    let terminal = TerminalContextDraft {
        id: "t".into(),
        thread_id: thread,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal".into(),
        line_start: 1,
        line_end: 1,
        text: "output".into(),
    };
    assert!(
        uploaded_context_record_from_draft(&ComposerDraftContextRecord::Terminal(terminal))
            .is_none()
    );
    let shot = image("img", "shot.png", b"x");
    let ready = AttachmentUploadState::Ready(ReadyAttachmentUpload {
        environment_id: "env".into(),
        attachment_id: "uploaded".into(),
    });
    assert!(
        uploaded_context_record_from_draft(&ComposerDraftContextRecord::Image {
            record: shot,
            upload: Some(ready),
        })
        .is_some()
    );
}

#[test]
fn an_attachment_chip_suffix_names_the_upload_state() {
    assert_eq!(upload_status_suffix(None), None);
    let uploading = AttachmentUploadState::Uploading {
        environment_id: "env".into(),
        progress: 0.5,
        previous: None,
    };
    assert_eq!(
        upload_status_suffix(Some(&uploading)),
        Some(format_attachment_upload_progress(0.5))
    );
    let failed = AttachmentUploadState::Failed {
        environment_id: "env".into(),
        reason: "Too large".into(),
        attachment_id: None,
        previous: None,
    };
    assert_eq!(
        upload_status_suffix(Some(&failed)).as_deref(),
        Some("upload failed")
    );
    assert_eq!(
        attachment_tooltip("a.txt", 2048, Some(&failed)),
        format!("a.txt\n{}\n\nToo large", format_attachment_size(2048))
    );
}

#[test]
fn the_annotation_tooltip_lists_its_targets() {
    let mut note = annotation("a");
    note.comment = "  Make it bigger ".into();
    assert_eq!(
        preview_annotation_tooltip(&note),
        "Example\n\nMake it bigger"
    );
    note.page_title = None;
    note.comment.clear();
    assert_eq!(preview_annotation_tooltip(&note), "http://localhost:3000/");
}

#[test]
fn a_terminal_range_reads_as_line_or_lines() {
    assert_eq!(terminal_line_range_text(4, 4), "Line 4");
    assert_eq!(terminal_line_range_text(4, 9), "Lines 4–9");
}

fn review(id: &str, file_path: &str, diff: &str) -> ReviewCommentContext {
    ReviewCommentContext {
        id: id.into(),
        section_id: "turn-diff:1".into(),
        section_title: "Turn 1".into(),
        file_path: file_path.into(),
        start_index: 0,
        end_index: 0,
        range_label: "+1".into(),
        text: "note".into(),
        diff: diff.into(),
        fence_language: None,
        pull_request: None,
    }
}

fn terminal(id: &str, text: &str) -> TerminalContextDraft {
    TerminalContextDraft {
        id: id.into(),
        thread_id: ThreadId::new("t1").unwrap(),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal".into(),
        line_start: 1,
        line_end: 1,
        text: text.into(),
    }
}

#[test]
fn a_click_on_an_attachment_reference_opens_it() {
    let shot = image("img-1", "shot.png", b"x");
    let video = ComposerFileAttachment {
        name: "clip.mp4".into(),
        mime_type: "video/mp4".into(),
        ..file("vid-1", None)
    };
    let mut missing = file("gone-1", None);
    missing.data = None;
    let records = composer_context_records_from_draft(&DraftContextInput {
        images: std::slice::from_ref(&shot),
        files: &[file("doc-1", None), video, missing],
        ..Default::default()
    });
    assert_eq!(
        context_reference_click("image_img-1", &records),
        ContextClick::ExpandImage("img-1".into())
    );
    assert_eq!(
        context_reference_click("file_doc-1", &records),
        ContextClick::OpenFile("doc-1".into())
    );
    assert_eq!(
        context_reference_click("file_vid-1", &records),
        ContextClick::ExpandVideo("vid-1".into())
    );
    // A file that was not saved with the draft has nothing to open.
    assert_eq!(
        context_reference_click("file_gone-1", &records),
        ContextClick::Nothing
    );
    assert_eq!(
        context_reference_click("file_unknown", &records),
        ContextClick::Nothing
    );
}

#[test]
fn a_click_on_text_context_opens_its_details() {
    let live = terminal("t-live", "output");
    let expired = terminal("t-expired", "\n\n");
    let comment = review("r-1", "src/a.rs", "@@ -1 +1 @@");
    let note = annotation("ann-1");
    let records = composer_context_records_from_draft(&DraftContextInput {
        terminal_contexts: &[live, expired],
        review_comments: std::slice::from_ref(&comment),
        preview_annotations: std::slice::from_ref(&note),
        ..Default::default()
    });
    assert_eq!(
        context_reference_click("terminal_t-live", &records),
        ContextClick::Details
    );
    // An expired excerpt has no text to show.
    assert_eq!(
        context_reference_click("terminal_t-expired", &records),
        ContextClick::Nothing
    );
    assert_eq!(
        context_reference_click("review-comment_r-1", &records),
        ContextClick::Details
    );
    assert_eq!(
        context_reference_click("preview-annotation_ann-1", &records),
        ContextClick::Details
    );
}
