//! The composer's side of `composerContextPresentation`: the records behind
//! the draft's context references, and what a click on a reference chip
//! opens (an image, a pull request, or the details popover).

use gpui_kit::{
    AnyElement, Bounds, Context, ParentElement as _, Pixels, Point, Styled as _, Window, div, rems,
    size,
};

use crate::components::chat::chat_media;
use crate::components::chat::expanded_image::{PreviewAttachment, build_expanded_image_preview};
use crate::composer::views::chat_composer::{ChatComposer, Layer};
use crate::composer::views::chrome::{floating, glass_surface};
use crate::composer::views::composer_context_presentation::{
    ComposerDraftContextRecord, ContextClick, DraftContextInput, composer_context_details,
    composer_context_records_from_draft, context_reference_click,
    uploaded_context_record_from_draft,
};
use crate::theme::ActiveAppTheme as _;

fn draft_context_wire_record(
    entry: &ComposerDraftContextRecord,
) -> Option<turin_contracts::composer_context::ComposerContextRecord> {
    use crate::composer::composer_context_records::{
        preview_annotation_context_record, review_comment_context_record, terminal_context_record,
    };
    use turin_contracts::composer_context::{
        ComposerContextRecord, KnownComposerContextRecord,
    };

    if let Some(record) = uploaded_context_record_from_draft(entry) {
        return Some(record);
    }
    match entry {
        ComposerDraftContextRecord::Terminal(draft) => {
            terminal_context_record(draft)
                .ok()
                .map(|record| {
                    ComposerContextRecord::Known(KnownComposerContextRecord::Terminal(record))
                })
        }
        ComposerDraftContextRecord::ReviewComment(draft) => {
            review_comment_context_record(draft)
                .ok()
                .map(|record| {
                    ComposerContextRecord::Known(KnownComposerContextRecord::ReviewComment(record))
                })
        }
        ComposerDraftContextRecord::Thread(record) => Some(ComposerContextRecord::Known(
            KnownComposerContextRecord::Thread(record.clone()),
        )),
        ComposerDraftContextRecord::PreviewAnnotation {
            record,
            screenshot,
        } => {
            let screenshot = screenshot.as_ref().map(|image| image.id.as_str());
            Some(ComposerContextRecord::Known(
                KnownComposerContextRecord::PreviewAnnotation(preview_annotation_context_record(
                    record,
                    screenshot,
                )),
            ))
        }
        ComposerDraftContextRecord::Image { .. } | ComposerDraftContextRecord::File { .. } => None,
    }
}

impl ChatComposer {
    /// Keeps the prompt editor's context copy/paste hooks aligned with [`Self::context_records`].
    pub(super) fn sync_context_editor(&mut self, cx: &mut Context<Self>) {
        use std::rc::Rc;

        use turin_contracts::composer_context_clipboard::ComposerContextClipboardFragment;
        use turin_shared::composer_context_clipboard::encode_composer_context_fragment;

        use crate::composer::views::prompt_editor::BuildContextFragment;
        use crate::state::local_environment_id;

        let count = self.composer_context_records().len();
        self.editor.update(cx, |editor, cx| {
            editor.set_context_record_count(count, cx);
        });

        let merged = self.composer_context_records();
        let environment_id =
            turin_contracts::base_schemas::EnvironmentId::new(local_environment_id())
                .expect("environment id");
        let build: BuildContextFragment = Rc::new(move |ids: &[String]| {
            let records: Vec<_> = ids
                .iter()
                .filter_map(|id| merged.get(id))
                .filter_map(draft_context_wire_record)
                .collect();
            if records.is_empty() {
                return None;
            }
            encode_composer_context_fragment(&ComposerContextClipboardFragment {
                version: turin_contracts::composer_context_clipboard::ComposerContextClipboardVersion,
                source: turin_contracts::composer_context_clipboard::ComposerContextClipboardSource {
                    environment_id: environment_id.clone(),
                    thread_id: None,
                    message_id: None,
                },
                records,
            })
        });
        use crate::composer::composer_context_records::{
            composer_context_draft_from_record, composer_context_import_lookup_ids,
        };
        use crate::composer::views::prompt_editor::ImportContextFragment;
        use crate::state::models::parse_scoped_thread_key;

        let thread_id = parse_scoped_thread_key(&self.thread_key)
            .map(|(_, thread)| thread)
            .unwrap_or_else(|| {
                turin_contracts::ThreadId::new("thread-unknown").expect("thread id")
            });
        let store = self.context_records.clone();
        let import: ImportContextFragment = Rc::new(move |fragment| {
            let mut rewritten = std::collections::HashMap::new();
            let mut target = store.borrow_mut();
            for record in &fragment.records {
                let id = record.context_id().as_str().to_owned();
                if let Some(draft) = composer_context_draft_from_record(record, thread_id.clone()) {
                    target.insert(id.clone(), draft);
                    if let Some(known) =
                        crate::composer::composer_context_records::as_known_context_record(record)
                    {
                        for alt in composer_context_import_lookup_ids(known) {
                            rewritten.insert(alt.as_str().to_owned(), id.clone());
                        }
                    }
                }
                rewritten.entry(id.clone()).or_insert(id);
            }
            rewritten
        });
        self.editor.update(cx, |editor, _| {
            editor.set_build_context_fragment(Some(build));
            editor.set_import_context_fragment(Some(import));
        });
    }

    /// The records behind the references of the open draft: its attachments,
    /// and the other kinds the host fed in.
    pub(super) fn composer_context_records(
        &self,
    ) -> crate::composer::views::composer_context_presentation::ComposerDraftContextRecords {
        let images = self.draft_images(cx);
        let files = self.draft_files(cx);
        let mut records = composer_context_records_from_draft(&DraftContextInput {
            images: &images,
            files: &files,
            ..Default::default()
        });
        for (id, record) in self.context_records.borrow().iter() {
            records.entry(id.clone()).or_insert_with(|| record.clone());
        }
        records
    }

    /// A click on a context reference chip in the prompt.
    pub(super) fn open_context_reference(
        &mut self,
        context_id: &str,
        anchor: Point<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let records = self.composer_context_records();
        match context_reference_click(context_id, &records) {
            ContextClick::ExpandImage(id) => {
                let gallery: Vec<PreviewAttachment> = self
                    .draft_images(cx)
                    .iter()
                    .map(|image| PreviewAttachment {
                        id: image.id.clone(),
                        name: image.name.clone(),
                        kind: "image".into(),
                        mime_type: image.mime_type.clone(),
                        preview_url: Some(image.id.clone()),
                        bytes: Some(image.data.as_ref().clone()),
                    })
                    .collect();
                if let Some(preview) = build_expanded_image_preview(&gallery, &id) {
                    chat_media::open_preview(preview, cx);
                }
            }
            ContextClick::OpenPullRequest(url) => cx.open_url(&url),
            ContextClick::Details => {
                self.context_details = Some((context_id.to_owned(), anchor));
                self.layer = Some(Layer::ContextDetails);
                cx.notify();
            }
            // No file, video or thread surface in the composer yet: the
            // host's router owns them (`openFile`, `expandVideo`,
            // `ThreadContextChip`'s link).
            ContextClick::ExpandVideo(_)
            | ContextClick::OpenFile(_)
            | ContextClick::OpenThread(_)
            | ContextClick::Nothing => {}
        }
    }

    /// The details popover of the clicked reference.
    pub(super) fn context_details_layer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (context_id, anchor) = self.context_details.clone()?;
        let records = self.composer_context_records();
        let theme = cx.app_theme().clone();
        let details = composer_context_details(
            records.get(&context_id)?,
            &format!("composer-context-{context_id}"),
            &theme,
        )?;
        Some(floating(
            Bounds::new(anchor, size(gpui_kit::px(0.), gpui_kit::px(0.))),
            true,
            false,
            glass_surface(cx)
                .w(rems(24.))
                .py_2()
                .px_3()
                .child(div().child(details)),
        ))
    }
}
