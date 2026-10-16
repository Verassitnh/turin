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
    ContextClick, DraftContextInput, composer_context_details, composer_context_records_from_draft,
    context_reference_click,
};
use crate::theme::ActiveAppTheme as _;

impl ChatComposer {
    /// The records behind the references of the open draft: its attachments,
    /// and the other kinds the host fed in.
    pub(super) fn composer_context_records(
        &self,
    ) -> crate::composer::views::composer_context_presentation::ComposerDraftContextRecords {
        let images = self.draft_images();
        let files = self.draft_files();
        let mut records = composer_context_records_from_draft(&DraftContextInput {
            images: &images,
            files: &files,
            ..Default::default()
        });
        for (id, record) in &self.context_records {
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
                    .draft_images()
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
