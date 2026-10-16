//! Port of upstream/apps/web/src/components/chat/composerContextUndo.ts
//!
//! Deleting a context chip drops the attachment behind it; undoing the edit
//! puts the chip back, and this keeps the payload (file bytes, annotation and
//! its screenshot) so the restored chip still has something to send.
//!
//! Upstream's `File` objects are the shared byte buffers (`Arc<Vec<u8>>`)
//! here; a restored attachment is the same `Arc`, not a copy.

use std::collections::{HashMap, HashSet};

use indexmap::IndexSet;

use crate::composer::composer_context_records::{
    ComposerFileDraft, PreviewAnnotationPayload, file_context_reference,
    preview_annotation_context_id,
};
use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};

#[derive(Debug, Clone, PartialEq)]
pub struct RetainedPreviewAnnotation {
    pub annotation: PreviewAnnotationPayload,
    pub image: Option<ComposerImageAttachment>,
}

#[derive(Debug, Default)]
pub struct RetainedAttachmentContextPayloads {
    pub files: HashMap<String, ComposerFileAttachment>,
    pub preview_annotations: HashMap<String, RetainedPreviewAnnotation>,
}

pub struct ReconcileAttachmentContextInput<'a> {
    pub referenced_context_ids: &'a IndexSet<String>,
    pub files: &'a [ComposerFileAttachment],
    pub images: &'a [ComposerImageAttachment],
    pub preview_annotations: &'a [PreviewAnnotationPayload],
    pub retained: &'a mut RetainedAttachmentContextPayloads,
}

#[derive(Debug, Default, PartialEq)]
pub struct ReconcileAttachmentContextOutput {
    pub files_to_remove: Vec<String>,
    pub files_to_restore: Vec<ComposerFileAttachment>,
    pub annotation_ids_to_remove: Vec<String>,
    pub annotations_to_restore: Vec<RetainedPreviewAnnotation>,
}

/// `fileContextReference(file).contextId`; the draft file carries more than
/// the reference needs.
pub fn file_context_id(file: &ComposerFileAttachment) -> String {
    file_context_reference(&ComposerFileDraft {
        id: file.id.clone(),
        name: file.name.clone(),
        mime_type: file.mime_type.clone(),
        size_bytes: file.size_bytes,
        uploaded_attachment_id: file.uploaded_attachment_id.clone(),
        upload_environment_id: file.upload_environment_id.clone(),
    })
    .context_id
    .as_str()
    .to_owned()
}

pub fn reconcile_attachment_context_references(
    input: ReconcileAttachmentContextInput<'_>,
) -> ReconcileAttachmentContextOutput {
    let live_file_context_ids: HashSet<String> = input.files.iter().map(file_context_id).collect();
    let mut files_to_remove = Vec::new();
    for file in input.files {
        let context_id = file_context_id(file);
        if input.referenced_context_ids.contains(&context_id) {
            continue;
        }
        input.retained.files.insert(context_id, file.clone());
        files_to_remove.push(file.id.clone());
    }
    let files_to_restore = input
        .referenced_context_ids
        .iter()
        .filter(|context_id| !live_file_context_ids.contains(*context_id))
        .filter_map(|context_id| input.retained.files.get(context_id).cloned())
        .collect();

    let images_by_id: HashMap<&str, &ComposerImageAttachment> = input
        .images
        .iter()
        .map(|image| (image.id.as_str(), image))
        .collect();
    let live_annotation_context_ids: HashSet<String> = input
        .preview_annotations
        .iter()
        .map(|annotation| {
            preview_annotation_context_id(&annotation.id)
                .as_str()
                .to_owned()
        })
        .collect();
    let mut annotation_ids_to_remove = Vec::new();
    for annotation in input.preview_annotations {
        let context_id = preview_annotation_context_id(&annotation.id)
            .as_str()
            .to_owned();
        if input.referenced_context_ids.contains(&context_id) {
            continue;
        }
        input.retained.preview_annotations.insert(
            context_id,
            RetainedPreviewAnnotation {
                annotation: annotation.clone(),
                image: images_by_id
                    .get(annotation.id.as_str())
                    .map(|i| (*i).clone()),
            },
        );
        annotation_ids_to_remove.push(annotation.id.clone());
    }
    let annotations_to_restore = input
        .referenced_context_ids
        .iter()
        .filter(|context_id| !live_annotation_context_ids.contains(*context_id))
        .filter_map(|context_id| input.retained.preview_annotations.get(context_id).cloned())
        .collect();

    ReconcileAttachmentContextOutput {
        files_to_remove,
        files_to_restore,
        annotation_ids_to_remove,
        annotations_to_restore,
    }
}

#[cfg(test)]
#[path = "composer_context_undo_tests.rs"]
mod tests;
