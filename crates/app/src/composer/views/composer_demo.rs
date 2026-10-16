//! Screenshot fixtures for the composer chips (`TURIN_COMPOSER_DEMO=chips`):
//! a prompt holding a mention and one reference of each context kind, the
//! payloads behind them, two generated images and a file.

use std::sync::Arc;

use gpui_kit::Context;
use turin_contracts::base_schemas::{EnvironmentId, ThreadId};
use turin_contracts::environment::ScopedThreadRef;
use turin_shared::composer_context_references::format_composer_context_reference;

use crate::composer::composer_context_records::{
    ComposerFileDraft, ComposerImageDraft, PreviewAnnotationPayload, ReviewCommentContext,
    TerminalContextDraft, file_context_reference, image_context_reference,
    preview_annotation_context_reference, review_comment_context_reference,
    terminal_context_reference, thread_context_record, thread_context_reference,
};
use crate::composer::draft_store::{
    AddAttachmentsOptions, ComposerFileAttachment, ComposerImageAttachment,
};
use crate::composer::views::chat_composer::ChatComposer;
use crate::composer::views::composer_context_presentation::{
    ComposerDraftContextRecord, ComposerDraftContextRecords,
};

/// A PNG gradient, so the thumbnails have something to crop.
fn demo_png(width: u32, height: u32, from: [u8; 3], to: [u8; 3]) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        let mix = |a: u8, b: u8| {
            let t = (x as f32 / width as f32 + y as f32 / height as f32) / 2.;
            (a as f32 + (b as f32 - a as f32) * t) as u8
        };
        image::Rgba([
            mix(from[0], to[0]),
            mix(from[1], to[1]),
            mix(from[2], to[2]),
            255,
        ])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode the demo image");
    out
}

impl ChatComposer {
    /// `TURIN_COMPOSER_DEMO=chips`.
    pub(super) fn apply_chips_demo(&mut self, cx: &mut Context<Self>) {
        let key = "local:thread-1".to_owned();

        let terminal = TerminalContextDraft {
            id: "term-1".into(),
            thread_id: ThreadId::new("thread-1").expect("thread id"),
            created_at: "2026-10-15T12:00:00.000Z".into(),
            terminal_id: "default".into(),
            terminal_label: "Terminal 1".into(),
            line_start: 3,
            line_end: 4,
            text: "error[E0382]: borrow of moved value: `prompt`\n  --> src/composer.rs:42:18"
                .into(),
        };
        let review = ReviewCommentContext {
            id: "review-1".into(),
            section_id: "turn-diff:1".into(),
            section_title: "Turn 1".into(),
            file_path: "crates/app/src/composer/footer.rs".into(),
            start_index: 11,
            end_index: 13,
            range_label: "+12 to +14".into(),
            text: "The **empty** model list falls through to the unavailable badge.".into(),
            diff: "@@ -10,3 +10,5 @@\n+    let models = entry.models.as_slice();\n+    if models.is_empty() { return unavailable(); }".into(),
            fence_language: Some("rust".into()),
            pull_request: None,
        };
        let annotation = PreviewAnnotationPayload {
            id: "ann-1".into(),
            page_url: "http://localhost:3000/checkout".into(),
            page_title: Some("Checkout".into()),
            comment: "Make this button bigger".into(),
            elements: Vec::new(),
            regions: Vec::new(),
            strokes: Vec::new(),
            style_changes: Vec::new(),
            screenshot: None,
            created_at: "2026-10-15T12:00:00.000Z".into(),
        };
        let thread = thread_context_record(
            &ScopedThreadRef {
                environment_id: EnvironmentId::new("local").expect("environment id"),
                thread_id: ThreadId::new("thread-2").expect("thread id"),
            },
            "Fix flaky sidebar test",
        );

        let screenshot = ComposerImageAttachment::new(
            "img-1",
            "checkout-screenshot.png",
            "image/png",
            demo_png(640, 360, [214, 78, 98], [86, 70, 196]),
        );
        let diagram = ComposerImageAttachment::new(
            "img-2",
            "architecture.png",
            "image/png",
            demo_png(480, 480, [38, 166, 154], [240, 201, 92]),
        );
        let notes = ComposerFileAttachment {
            id: "file-1".into(),
            name: "release-notes.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 2048,
            data: Some(Arc::new(vec![b'x'; 2048])),
            uploaded_attachment_id: None,
            upload_environment_id: None,
        };
        let image_draft = |image: &ComposerImageAttachment| ComposerImageDraft {
            id: image.id.clone(),
            name: image.name.clone(),
            mime_type: image.mime_type.clone(),
            size_bytes: image.size_bytes,
        };
        let file_draft = ComposerFileDraft {
            id: notes.id.clone(),
            name: notes.name.clone(),
            mime_type: notes.mime_type.clone(),
            size_bytes: notes.size_bytes,
            uploaded_attachment_id: None,
            upload_environment_id: None,
        };

        let reference =
            |kind: &str, id: &str, label: &str| format_composer_context_reference(kind, id, label);
        let terminal_ref = terminal_context_reference(&terminal);
        let review_ref = review_comment_context_reference(&review);
        let annotation_ref = preview_annotation_context_reference(&annotation);
        let thread_ref = thread_context_reference(&thread);
        let screenshot_ref = image_context_reference(&image_draft(&screenshot));
        let notes_ref = file_context_reference(&file_draft);
        let prompt = format!(
            "Compare @crates/app/src/composer/footer.rs with {} and {}, then check {} against {}. See {} and {} too, and {}. ",
            reference(
                "terminal",
                terminal_ref.context_id.as_str(),
                &terminal_ref.label
            ),
            reference(
                "review-comment",
                review_ref.context_id.as_str(),
                &review_ref.label
            ),
            reference(
                "preview-annotation",
                annotation_ref.context_id.as_str(),
                &annotation_ref.label
            ),
            reference("thread", thread_ref.context_id.as_str(), &thread_ref.label),
            reference(
                "image",
                screenshot_ref.context_id.as_str(),
                &screenshot_ref.label
            ),
            reference("file", notes_ref.context_id.as_str(), &notes_ref.label),
            reference(
                "terminal",
                terminal_ref.context_id.as_str(),
                &terminal_ref.label
            ),
        );

        self.drafts.add_images(
            &key,
            vec![screenshot, diagram],
            AddAttachmentsOptions::default(),
        );
        self.drafts
            .add_files(&key, vec![notes], AddAttachmentsOptions::default());
        self.drafts.set_prompt(&key, &prompt);

        let mut records = ComposerDraftContextRecords::new();
        records.insert(
            terminal_ref.context_id.as_str().to_owned(),
            ComposerDraftContextRecord::Terminal(terminal),
        );
        records.insert(
            review_ref.context_id.as_str().to_owned(),
            ComposerDraftContextRecord::ReviewComment(review),
        );
        records.insert(
            annotation_ref.context_id.as_str().to_owned(),
            ComposerDraftContextRecord::PreviewAnnotation {
                record: annotation,
                screenshot: None,
            },
        );
        records.insert(
            thread_ref.context_id.as_str().to_owned(),
            ComposerDraftContextRecord::Thread(thread),
        );
        *self.context_records.borrow_mut() = records;
        cx.notify();
    }
}
