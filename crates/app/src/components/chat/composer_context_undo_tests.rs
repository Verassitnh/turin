//! Port of upstream/apps/web/src/components/chat/composerContextUndo.test.ts
//!
//! `PreviewAnnotationPayload.screenshot` is `Option<()>` in Turin (the
//! screenshot bytes ride on the image attachment), so the `dataUrl`
//! assertion is covered by whole-payload equality and the shared image
//! buffer check.

use std::sync::Arc;

use turin_contracts::composer_context::KnownComposerContextRecord;

use super::*;
use crate::composer::composer_context_records::{
    BoundComposerAttachment, ComposerContextAttachment, ComposerImageDraft, MessageContextInput,
    as_known_context_record, build_message_context,
};

fn file() -> ComposerFileAttachment {
    ComposerFileAttachment {
        id: "file-1".into(),
        name: "notes.txt".into(),
        mime_type: "text/plain".into(),
        size_bytes: 5,
        data: Some(Arc::new(b"notes".to_vec())),
        uploaded_attachment_id: None,
        upload_environment_id: None,
    }
}

fn image() -> ComposerImageAttachment {
    ComposerImageAttachment::new(
        "annotation-1",
        "annotation.png",
        "image/png",
        b"binary".to_vec(),
    )
}

fn annotation() -> PreviewAnnotationPayload {
    PreviewAnnotationPayload {
        id: "annotation-1".into(),
        page_url: "https://example.com".into(),
        page_title: Some("Example".into()),
        comment: "Fix this".into(),
        elements: Vec::new(),
        regions: Vec::new(),
        strokes: Vec::new(),
        style_changes: Vec::new(),
        screenshot: Some(()),
        created_at: "2026-01-01T00:00:00.000Z".into(),
    }
}

fn ids(values: &[&str]) -> IndexSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn context_attachment_of_file(file: &ComposerFileAttachment) -> ComposerContextAttachment {
    ComposerContextAttachment::File(ComposerFileDraft {
        id: file.id.clone(),
        name: file.name.clone(),
        mime_type: file.mime_type.clone(),
        size_bytes: file.size_bytes,
        uploaded_attachment_id: file.uploaded_attachment_id.clone(),
        upload_environment_id: file.upload_environment_id.clone(),
    })
}

fn context_attachment_of_image(image: &ComposerImageAttachment) -> ComposerContextAttachment {
    ComposerContextAttachment::Image(ComposerImageDraft {
        id: image.id.clone(),
        name: image.name.clone(),
        mime_type: image.mime_type.clone(),
        size_bytes: image.size_bytes,
    })
}

#[test]
fn restores_file_bytes_after_deleting_and_undoing_its_chip() {
    let mut retained = RetainedAttachmentContextPayloads::default();
    let file = file();
    let removed = reconcile_attachment_context_references(ReconcileAttachmentContextInput {
        referenced_context_ids: &ids(&[]),
        files: std::slice::from_ref(&file),
        images: &[],
        preview_annotations: &[],
        retained: &mut retained,
    });
    assert_eq!(removed.files_to_remove, ["file-1"]);

    let restored = reconcile_attachment_context_references(ReconcileAttachmentContextInput {
        referenced_context_ids: &ids(&[file_context_id(&file).as_str()]),
        files: &[],
        images: &[],
        preview_annotations: &[],
        retained: &mut retained,
    });
    assert_eq!(restored.files_to_restore, std::slice::from_ref(&file));
    assert!(Arc::ptr_eq(
        restored.files_to_restore[0].data.as_ref().unwrap(),
        file.data.as_ref().unwrap()
    ));
    let context = build_message_context(MessageContextInput {
        terminal_contexts: &[],
        review_comments: &[],
        preview_annotations: &[],
        thread_contexts: &[],
        attachments: &[BoundComposerAttachment {
            attachment: context_attachment_of_file(&restored.files_to_restore[0]),
            attachment_id: "uploaded-file".into(),
        }],
    })
    .unwrap();
    assert_eq!(context.records.len(), 1);
    let record = &context.records[0];
    assert_eq!(record.kind(), "file");
    assert_eq!(record.context_id().as_str(), "file_file-1");
    match as_known_context_record(record) {
        Some(KnownComposerContextRecord::File(file)) => {
            assert_eq!(file.attachment_id.as_str(), "uploaded-file")
        }
        other => panic!("expected a file record, got {other:?}"),
    }
}

#[test]
fn restores_an_annotation_and_its_screenshot_attachment_after_undo() {
    let mut retained = RetainedAttachmentContextPayloads::default();
    let (image, annotation) = (image(), annotation());
    let removed = reconcile_attachment_context_references(ReconcileAttachmentContextInput {
        referenced_context_ids: &ids(&[]),
        files: &[],
        images: std::slice::from_ref(&image),
        preview_annotations: std::slice::from_ref(&annotation),
        retained: &mut retained,
    });
    assert_eq!(removed.annotation_ids_to_remove, ["annotation-1"]);

    let restored = reconcile_attachment_context_references(ReconcileAttachmentContextInput {
        referenced_context_ids: &ids(&[preview_annotation_context_id(&annotation.id).as_str()]),
        files: &[],
        images: &[],
        preview_annotations: &[],
        retained: &mut retained,
    });
    assert_eq!(
        restored.annotations_to_restore,
        [RetainedPreviewAnnotation {
            annotation: annotation.clone(),
            image: Some(image.clone()),
        }]
    );
    assert_eq!(
        restored.annotations_to_restore[0].annotation.screenshot,
        Some(())
    );
    assert!(Arc::ptr_eq(
        &restored.annotations_to_restore[0]
            .image
            .as_ref()
            .unwrap()
            .data,
        &image.data
    ));
    let restored_annotations: Vec<_> = restored
        .annotations_to_restore
        .iter()
        .map(|entry| entry.annotation.clone())
        .collect();
    let context = build_message_context(MessageContextInput {
        terminal_contexts: &[],
        review_comments: &[],
        preview_annotations: &restored_annotations,
        thread_contexts: &[],
        attachments: &[BoundComposerAttachment {
            attachment: context_attachment_of_image(
                restored.annotations_to_restore[0].image.as_ref().unwrap(),
            ),
            attachment_id: "uploaded-screenshot".into(),
        }],
    })
    .unwrap();
    assert_eq!(context.records.len(), 2);
    assert_eq!(context.records[0].kind(), "preview-annotation");
    match as_known_context_record(&context.records[0]) {
        Some(KnownComposerContextRecord::PreviewAnnotation(record)) => assert_eq!(
            record.screenshot_context_id.as_ref().map(|id| id.as_str()),
            Some("image_annotation-1")
        ),
        other => panic!("expected a preview annotation, got {other:?}"),
    }
    assert_eq!(context.records[1].kind(), "image");
    assert_eq!(
        context.records[1].context_id().as_str(),
        "image_annotation-1"
    );
    match as_known_context_record(&context.records[1]) {
        Some(KnownComposerContextRecord::Image(image)) => {
            assert_eq!(image.attachment_id.as_str(), "uploaded-screenshot")
        }
        other => panic!("expected an image record, got {other:?}"),
    }
}
