//! Per-question attachment drafts ([`crate::state::question_attachments`] keys).

use gpui_kit::{App, Global, PathPromptOptions, Window};

use crate::composer::attachment_files::{PickedFile, classify_composer_attachment_file};
use crate::composer::draft_store::{
    AddAttachmentsOptions, ComposerDraftStore, ComposerFileAttachment, ComposerImageAttachment,
};
use crate::composer::views::actions::read_picked_file;
use crate::state::question_attachments::{
    QuestionAttachmentPreparation, count_question_attachments, question_attachment_draft_id,
};

#[derive(Default)]
pub struct QuestionAttachmentDrafts {
    pub store: ComposerDraftStore,
    pub preparation: QuestionAttachmentPreparation,
}

impl Global for QuestionAttachmentDrafts {}

pub fn question_draft_attachment_count(
    cx: &App,
    environment_id: &str,
    thread_id: &str,
    request_id: &str,
    question_id: &str,
) -> usize {
    let key = question_attachment_draft_id(environment_id, thread_id, request_id, question_id);
    let drafts = cx.default_global::<QuestionAttachmentDrafts>();
    count_question_attachments(&drafts.store, &drafts.preparation, &[&key])
}

pub fn pick_question_attachments(window: &mut Window, cx: &mut App, draft_key: String) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: Some("Attach to question".into()),
    });
    cx.spawn_in(window, async move |_, cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let loaded = cx
            .background_executor()
            .spawn(async move {
                paths
                    .into_iter()
                    .filter_map(read_picked_file)
                    .collect::<Vec<_>>()
            })
            .await;
        cx.update(|cx| {
            let drafts = cx.default_global::<QuestionAttachmentDrafts>();
            let mut images = Vec::new();
            let mut files = Vec::new();
            for (picked, data) in loaded {
                let id = format!("qatt-{}", picked.name);
                match classify_composer_attachment_file(&picked) {
                    crate::composer::attachment_files::ComposerAttachmentFileKind::Image => {
                        images.push(ComposerImageAttachment::new(
                            &id,
                            &picked.name,
                            &picked.mime_type,
                            data,
                        ));
                    }
                    crate::composer::attachment_files::ComposerAttachmentFileKind::File => {
                        files.push(ComposerFileAttachment {
                            id,
                            name: picked.name.clone(),
                            mime_type: picked.mime_type.clone(),
                            size_bytes: data.len() as u64,
                            data: Some(std::sync::Arc::new(data)),
                            uploaded_attachment_id: None,
                            upload_environment_id: None,
                        });
                    }
                    _ => {}
                }
            }
            if !images.is_empty() || !files.is_empty() {
                drafts.store.add_attachments(
                    &draft_key,
                    AddAttachmentsOptions {
                        images: &images,
                        files: &files,
                    },
                );
            }
            cx.refresh_windows();
        })
        .ok();
    })
    .detach();
}
