//! Composer interactions that sit beside the footer: attachments (picker,
//! paste, strip), the model picker lifecycle and prompt history recall.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, AppContext as _, Context, CursorStyle, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px, rems,
};

use crate::components::chat::chat_media;
use crate::components::chat::expanded_image::{PreviewAttachment, build_expanded_image_preview};
use crate::composer::attachment_files::{
    FileAttachmentCapabilityState, PickedFile, file_attachment_staging_limit,
    format_attachment_size,
};
use crate::composer::attachment_intake::{
    Intake, load_path, load_paths, plan_staging, prepare_image,
};
use crate::composer::draft_store::{
    AddAttachmentsOptions, ComposerFileAttachment, ComposerImageAttachment,
};
use crate::composer::editor_state::EditorSnapshot;
use crate::composer::prompt_history::{
    HistoryDirection, build_composer_prompt_history_entries, step_composer_prompt_history,
};
use crate::composer::view_model::{ComposerViewModel, SessionPhase};
use crate::composer::views::chat_composer::{ChatComposer, Layer};
use crate::composer::views::image_thumbnail::ComposerImageThumbnail;
use crate::composer::views::language_picker_view::{LanguagePickerEvent, LanguagePickerView};
use crate::composer::views::model_picker_view::{ModelPickerEvent, ModelPickerView};
use crate::composer::views::prompt_editor::CommandKey;
use crate::state::helpers::image_compression::ImageFile;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_3XS, TEXT_XS};
use crate::ui::toast::{ToastOptions, toast_manager};
use crate::ui::toast_logic::ToastType;
use crate::ui::{Icon, IconName, Tooltip};

impl ChatComposer {
    // ----- Model picker ---------------------------------------------------

    pub(super) fn open_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layer == Some(Layer::ModelPicker) {
            self.close_layer(window, cx);
            return;
        }
        let store = self.store.clone();
        let picker = cx.new(|cx| ModelPickerView::new(store, window, cx));
        cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &ModelPickerEvent, window, cx| match event {
                ModelPickerEvent::Select { instance_id, slug } => {
                    if let Some(source) = this.source(cx) {
                        source.select_model(instance_id, slug, cx);
                    }
                    this.close_layer(window, cx);
                }
                ModelPickerEvent::ToggleFavorite { instance_id, slug } => {
                    if let Some(source) = this.source(cx) {
                        source.toggle_favorite_model(instance_id, slug, cx);
                    }
                }
                ModelPickerEvent::OpenSetup(instance_id) => {
                    if let Some(source) = this.source(cx) {
                        source.open_provider_setup(instance_id, cx);
                    }
                    this.close_layer(window, cx);
                }
                ModelPickerEvent::Close => this.close_layer(window, cx),
            },
        )
        .detach();
        picker.update(cx, |picker, cx| picker.focus_search(window, cx));
        self.model_picker = Some(picker);
        self.layer = Some(Layer::ModelPicker);
        cx.notify();
    }

    // ----- Code block language ---------------------------------------------

    pub(super) fn open_language_picker(
        &mut self,
        block: usize,
        anchor: gpui_kit::Point<gpui_kit::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(language) = self.editor.read(cx).code_block_language(block) else {
            return;
        };
        let picker = cx.new(|cx| LanguagePickerView::new(&language, window, cx));
        cx.subscribe_in(
            &picker,
            window,
            move |this, _, event: &LanguagePickerEvent, window, cx| {
                if let LanguagePickerEvent::Select(language) = event {
                    let language = language.clone();
                    this.editor.update(cx, |editor, cx| {
                        editor.set_code_block_language(block, &language, cx)
                    });
                }
                this.close_layer(window, cx);
            },
        )
        .detach();
        self.language_picker = Some((picker, anchor));
        self.layer = Some(Layer::CodeLanguage);
        cx.notify();
    }

    // ----- Attachments ----------------------------------------------------

    /// The attach button: a native file dialog, then each file is read and
    /// classified (`addComposerAttachments`).
    pub(super) fn pick_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let intake = cx
                .background_executor()
                .spawn(async move { load_paths(paths) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.add_picked_files(intake, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// `addComposerFiles`: stages what the draft may take and reports the rest.
    /// Images are prepared (HEIC conversion, downscaling to the wire cap) off
    /// the UI thread and land in the draft when ready.
    pub(super) fn add_picked_files(
        &mut self,
        intake: Intake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Intake { loaded, mut errors } = intake;
        let vm = self.view_model(cx);
        let capability = FileAttachmentCapabilityState {
            attachment_uploads_capability_known: vm.attachment_uploads_capability_known,
            supports_attachment_uploads: vm.supports_attachment_uploads,
            max_file_attachment_bytes: vm.max_file_attachment_bytes,
        };
        let key = self.thread_key.clone();
        let reserved = self
            .drafts
            .get_composer_draft(&key)
            .map_or(0, |draft| draft.images.len() + draft.files.len())
            + self.pending_image_compressions;
        let staged = plan_staging(loaded, reserved, &capability);
        errors.extend(staged.errors);
        // The last error wins, as `setThreadError(error)` does.
        self.validation = errors.pop();
        let files: Vec<ComposerFileAttachment> = staged
            .files
            .into_iter()
            .map(|(picked, data)| ComposerFileAttachment {
                id: format!("att-{}-{}", picked.name, self.next_attachment_id()),
                name: picked.name.clone(),
                mime_type: picked.mime_type.clone(),
                size_bytes: data.len() as u64,
                data: Some(Arc::new(data)),
                uploaded_attachment_id: None,
                upload_environment_id: None,
            })
            .collect();
        self.drafts
            .add_files(&key, files, AddAttachmentsOptions::default());
        if !staged.images.is_empty() {
            let images: Vec<(String, PickedFile, Vec<u8>)> = staged
                .images
                .into_iter()
                .map(|(picked, data)| {
                    (
                        format!("att-{}-{}", picked.name, self.next_attachment_id()),
                        picked,
                        data,
                    )
                })
                .collect();
            let count = images.len();
            self.pending_image_compressions += count;
            cx.spawn_in(window, async move |this, cx| {
                let prepared = cx
                    .background_executor()
                    .spawn(async move {
                        images
                            .into_iter()
                            .map(|(id, picked, data)| (id, prepare_image(picked, data)))
                            .collect::<Vec<_>>()
                    })
                    .await;
                this.update_in(cx, |this, _, cx| {
                    this.pending_image_compressions =
                        this.pending_image_compressions.saturating_sub(count);
                    let mut ready = Vec::new();
                    let mut failure = None;
                    for (id, outcome) in prepared {
                        match outcome {
                            Ok((picked, data)) => ready.push(ComposerImageAttachment::new(
                                &id,
                                &picked.name,
                                &picked.mime_type,
                                data,
                            )),
                            Err(error) => failure = Some(error),
                        }
                    }
                    this.drafts
                        .add_images(&key, ready, AddAttachmentsOptions::default());
                    // Only failures are reported: a success must not clear
                    // an error something else set meanwhile.
                    if failure.is_some() {
                        this.validation = failure;
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        self.focus_editor(window, cx);
        cx.notify();
    }

    /// What [`fold_pasted_text`](crate::composer::pasted_text::fold_pasted_text)
    /// needs from the composer.
    pub(super) fn paste_fold_facts(&self, cx: &gpui_kit::App) -> PasteFoldFacts {
        let vm = self.view_model(cx);
        let draft = self.drafts.get_composer_draft(&self.thread_key);
        PasteFoldFacts {
            reserved_attachments: draft
                .as_ref()
                .map_or(0, |draft| draft.images.len() + draft.files.len())
                + self.pending_image_compressions,
            staging_limit: file_attachment_staging_limit(&FileAttachmentCapabilityState {
                attachment_uploads_capability_known: vm.attachment_uploads_capability_known,
                supports_attachment_uploads: vm.supports_attachment_uploads,
                max_file_attachment_bytes: vm.max_file_attachment_bytes,
            }),
            file_names: draft
                .map(|draft| draft.files.iter().map(|file| file.name.clone()).collect())
                .unwrap_or_default(),
        }
    }

    /// Carries out a [`PasteFold`](crate::composer::pasted_text::PasteFold)
    /// other than `Inline`: stages the text as a file and says so, or refuses.
    pub(super) fn apply_paste_fold(
        &mut self,
        fold: crate::composer::pasted_text::PasteFold,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::composer::pasted_text::{PASTED_TEXT_MIME_TYPE, PasteFold, large_paste_toast};
        let (title, description, kind) = match fold {
            PasteFold::Inline => return,
            PasteFold::Refuse { title, description } => {
                (title.to_owned(), description.to_owned(), ToastType::Error)
            }
            PasteFold::Attach { name } => {
                let bytes = text.into_bytes();
                let size = bytes.len() as u64;
                let picked = PickedFile::new(&name, PASTED_TEXT_MIME_TYPE, size);
                self.add_picked_files(Intake::from_loaded(vec![(picked, bytes)]), window, cx);
                let (title, description) = large_paste_toast(&name, size);
                (title, description, ToastType::Info)
            }
        };
        toast_manager(cx).update(cx, |store, cx| {
            store.add(
                ToastOptions::new(title)
                    .type_(kind)
                    .description(description),
                cx,
            );
        });
    }

    fn next_attachment_id(&mut self) -> usize {
        crate::composer::draft_store::next_composer_serial()
    }

    pub(super) fn render_attachments(
        &mut self,
        _vm: &ComposerViewModel,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let images = self.draft_images();
        let files = self.draft_files();
        if images.is_empty() && files.is_empty() {
            return None;
        }
        let theme = cx.app_theme().clone();
        let gallery: Vec<PreviewAttachment> = images
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
        let thumbs = images
            .iter()
            .map(|image| {
                let id = image.id.clone();
                let name = image.name.clone();
                let gallery = gallery.clone();
                let fallback = div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_1()
                    .text_size(TEXT_3XS.size)
                    .text_color(theme.secondary_label)
                    .child(name.clone())
                    .into_any_element();
                div()
                    .id(("composer-image", attachment_key(&image.id)))
                    .group("attachment")
                    .relative()
                    .size(rems(4.))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.8))
                    .bg(theme.background)
                    .cursor(CursorStyle::PointingHand)
                    .on_click({
                        let id = id.clone();
                        move |_, _, cx| {
                            if let Some(preview) = build_expanded_image_preview(&gallery, &id) {
                                chat_media::open_preview(preview, cx);
                            }
                        }
                    })
                    .child(ComposerImageThumbnail {
                        id: SharedString::from(format!("composer-thumbnail-{}", image.id)),
                        file: ImageFile {
                            name: image.name.clone(),
                            mime_type: image.mime_type.clone(),
                            bytes: image.data.clone(),
                            last_modified: None,
                        },
                        fallback,
                    })
                    .child(self.remove_button(
                        ("remove-image", attachment_key(&image.id)),
                        format!("Remove {name}"),
                        {
                            let id = id.clone();
                            move |this: &mut ChatComposer, cx: &mut Context<ChatComposer>| {
                                let key = this.thread_key.clone();
                                this.drafts.remove_image(&key, &id);
                                cx.notify();
                            }
                        },
                        cx,
                    ))
            })
            .collect::<Vec<_>>();
        let chips = files
            .iter()
            .map(|file| {
                let id = file.id.clone();
                let name = file.name.clone();
                div()
                    .id(("composer-file", attachment_key(&file.id)))
                    .flex()
                    .h(rems(2.))
                    .max_w(rems(16.))
                    .items_center()
                    .gap(rems(0.375))
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.8))
                    .bg(theme.background)
                    .pl_2()
                    .pr_1()
                    .text_size(TEXT_XS.size)
                    .child(
                        Icon::new(IconName::File)
                            .size(rems(0.875))
                            .color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(file.name.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground)
                            .child(format_attachment_size(file.size_bytes)),
                    )
                    .child(
                        div()
                            .id(("remove-file", attachment_key(&file.id)))
                            .flex()
                            .size(rems(1.25))
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor(CursorStyle::PointingHand)
                            .hover(|style| style.bg(with_alpha(theme.foreground, 0.1)))
                            .child(
                                Icon::new(IconName::X)
                                    .size(rems(0.75))
                                    .color(theme.muted_foreground),
                            )
                            .tooltip({
                                let label = format!("Remove {name}");
                                move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let key = this.thread_key.clone();
                                this.drafts.remove_file(&key, &id);
                                cx.notify();
                            })),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .mb_3()
                .flex()
                .flex_wrap()
                .max_w_full()
                .gap_2()
                .children(thumbs)
                .children(chips)
                .into_any_element(),
        )
    }

    /// `Button variant="media-close" size="icon-xs"`: a small dark disc, shown on hover.
    fn remove_button(
        &self,
        id: (&'static str, usize),
        label: String,
        on_remove: impl Fn(&mut ChatComposer, &mut Context<ChatComposer>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .absolute()
            .top(px(4.))
            .right(px(4.))
            .flex()
            .size(rems(1.25))
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(gpui_kit::hsla(0., 0., 0., 0.6))
            .cursor(CursorStyle::PointingHand)
            .opacity(0.)
            .group_hover("attachment", |style| style.opacity(1.))
            .child(
                Icon::new(IconName::X)
                    .size(rems(0.75))
                    .color(gpui_kit::white()),
            )
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| on_remove(this, cx)))
    }

    // ----- Prompt history ------------------------------------------------

    /// `navigatePromptHistory`: ArrowUp/Down at a visual edge recall earlier prompts.
    pub(super) fn on_history_key(
        &mut self,
        snapshot: &EditorSnapshot,
        key: &CommandKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let direction = match key.key.as_str() {
            "ArrowUp" => HistoryDirection::Backward,
            "ArrowDown" => HistoryDirection::Forward,
            _ => return false,
        };
        if key.shift || key.alt || key.meta || key.ctrl || key.is_composing {
            return false;
        }
        let vm = self.view_model(cx);
        if vm.has_pending_approval || vm.pending_answer.is_some() {
            return false;
        }
        if !self.draft_images().is_empty() || !self.draft_files().is_empty() {
            return false;
        }
        if self.history_position.is_none() && !snapshot.value.is_empty() {
            return false;
        }
        let at_edge = self.editor.update(cx, |editor, _| {
            editor.is_caret_on_visual_edge(direction == HistoryDirection::Backward)
        });
        if !at_edge {
            return false;
        }
        let entries = build_composer_prompt_history_entries(&vm.prompt_history_messages);
        let Some(step) = step_composer_prompt_history(
            direction,
            &entries,
            self.history_position.as_ref(),
            &snapshot.value,
        ) else {
            return false;
        };
        self.history_position = step.position;
        let weak = cx.weak_entity();
        let prompt = step.prompt;
        window.defer(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.replace_prompt(&prompt, window, cx))
                .ok();
        });
        true
    }

    /// Replace the whole prompt, caret at the end, keeping the recall position.
    pub(super) fn replace_prompt(
        &mut self,
        prompt: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = self.history_position.clone();
        let end = crate::composer::js_string::utf16_len(prompt);
        self.editor
            .update(cx, |editor, cx| editor.set_value(prompt, end, cx));
        self.history_position = position;
    }

    pub(super) fn is_running(&self, vm: &ComposerViewModel) -> bool {
        vm.phase == SessionPhase::Running
    }
}

/// One file for attachment; `None` when it is refused (see [`load_path`]).
pub fn read_picked_file(path: PathBuf) -> Option<(PickedFile, Vec<u8>)> {
    load_path(&path).ok()
}

/// See [`ChatComposer::paste_fold_facts`].
pub(super) struct PasteFoldFacts {
    pub reserved_attachments: usize,
    pub staging_limit: Option<u64>,
    pub file_names: Vec<String>,
}

// ----- Stash and queued-message editing ---------------------------------------

use crate::composer::draft_store::PromptStashEntry;

impl ChatComposer {
    fn next_stash_id(&mut self) -> String {
        format!(
            "stash-{}",
            crate::composer::draft_store::next_composer_serial()
        )
    }

    /// ⌘S: stash the draft; on an empty composer restore the lone entry or
    /// open the menu when there are several.
    pub(super) fn stash_or_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let images = self.draft_images();
        let files = self.draft_files();
        let prompt = self.prompt(cx);
        if !prompt.trim().is_empty() || !images.is_empty() || !files.is_empty() {
            let entry = PromptStashEntry {
                id: self.next_stash_id(),
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                prompt,
                images,
                files,
                dropped_image_names: Vec::new(),
                unreadable_image_names: Vec::new(),
                pending_image_count: 0,
            };
            self.drafts.push_stash_entry(entry);
            self.clear_draft(window, cx);
            return;
        }
        let entries = self.drafts.stash_entries();
        match entries.len() {
            0 => {}
            1 => {
                let id = entries[0].id.clone();
                self.restore_stash_entry(&id, window, cx);
            }
            _ => {
                self.stash_highlight = None;
                self.set_layer(Some(Layer::Stash), cx);
            }
        }
    }

    /// `restoreStashEntry`: append the prompt after what is already typed and
    /// bring the attachments back.
    pub(super) fn restore_stash_entry(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.drafts.remove_stash_entry(id) else {
            return;
        };
        let current = self.prompt(cx);
        let restored = entry.prompt.clone();
        let next = if restored.is_empty() {
            current
        } else if current.trim().is_empty() {
            restored
        } else {
            format!("{}\n\n{restored}", current.trim_end())
        };
        let key = self.thread_key.clone();
        self.drafts
            .add_images(&key, entry.images, AddAttachmentsOptions::default());
        self.drafts
            .add_files(&key, entry.files, AddAttachmentsOptions::default());
        self.replace_prompt(&next, window, cx);
        self.layer = None;
        self.stash_highlight = None;
        self.focus_editor(window, cx);
        cx.notify();
    }

    pub(super) fn delete_stash_entry(&mut self, id: &str, cx: &mut Context<Self>) {
        self.drafts.remove_stash_entry(id);
        if self.drafts.stash_entries().is_empty() {
            self.layer = None;
        }
        cx.notify();
    }

    /// Keys while the stash menu is open (a capture-phase window listener upstream).
    pub(super) fn on_stash_key(
        &mut self,
        key: &CommandKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let entries: Vec<String> = self
            .drafts
            .stash_entries()
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        let highlighted = self
            .stash_highlight
            .clone()
            .or_else(|| entries.first().cloned());
        match key.key.as_str() {
            "Escape" => {
                // The editor is mid-update: close once it has returned.
                let weak = cx.weak_entity();
                window.defer(cx, move |window, cx| {
                    weak.update(cx, |this, cx| this.close_layer(window, cx))
                        .ok();
                });
                true
            }
            "ArrowDown" | "ArrowUp" if !entries.is_empty() => {
                let current = highlighted
                    .as_ref()
                    .and_then(|id| entries.iter().position(|entry| entry == id));
                let down = key.key == "ArrowDown";
                let base = current.map_or(if down { -1 } else { 0 }, |index| index as i32);
                let next =
                    (base + if down { 1 } else { -1 }).rem_euclid(entries.len() as i32) as usize;
                self.stash_highlight = Some(entries[next].clone());
                cx.notify();
                true
            }
            "Enter" => {
                let Some(id) = highlighted else {
                    return false;
                };
                let weak = cx.weak_entity();
                window.defer(cx, move |window, cx| {
                    weak.update(cx, |this, cx| this.restore_stash_entry(&id, window, cx))
                        .ok();
                });
                true
            }
            "Backspace" if key.meta || key.ctrl => {
                if let Some(id) = highlighted {
                    self.delete_stash_entry(&id, cx);
                }
                true
            }
            _ => false,
        }
    }

    /// Take back a draft whose send failed, unless the user already typed
    /// something new.
    pub(super) fn sync_restore(
        &mut self,
        vm: &ComposerViewModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(restore) = vm.restore_draft.as_ref() else {
            return;
        };
        if restore.nonce == self.restored_nonce {
            return;
        }
        self.restored_nonce = restore.nonce;
        if restore.merge {
            let merged = crate::composer::view_model::merge_restored_prompt(
                &self.prompt(cx),
                &restore.prompt,
            );
            self.replace_prompt(&merged, window, cx);
        } else if self.has_sendable_content(cx) {
            return;
        } else {
            self.replace_prompt(&restore.prompt, window, cx);
        }
        let key = self.thread_key.clone();
        self.drafts.add_images(
            &key,
            restore.images.clone(),
            crate::composer::draft_store::AddAttachmentsOptions::default(),
        );
        self.drafts.add_files(
            &key,
            restore.files.clone(),
            crate::composer::draft_store::AddAttachmentsOptions::default(),
        );
        cx.notify();
    }

    /// Load a queued message into the composer while it is edited, and bring
    /// the previous draft back afterwards (`onEditQueuedRun`).
    pub(super) fn sync_queue_edit(
        &mut self,
        vm: &ComposerViewModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editing = vm
            .queue
            .as_ref()
            .and_then(|queue| queue.editing_run_id.clone());
        if editing == self.editing_run_id {
            return;
        }
        let previous = std::mem::replace(&mut self.editing_run_id, editing.clone());
        match editing {
            Some(run_id) => {
                if previous.is_none() {
                    self.pre_edit_prompt = Some(self.prompt(cx));
                }
                let text = vm
                    .queue
                    .as_ref()
                    .and_then(|queue| queue.queued.iter().find(|run| run.run_id == run_id))
                    .map(|run| run.text.clone())
                    .unwrap_or_default();
                self.replace_prompt(&text, window, cx);
                self.focus_editor(window, cx);
            }
            None => {
                if let Some(prompt) = self.pre_edit_prompt.take() {
                    self.replace_prompt(&prompt, window, cx);
                }
            }
        }
    }
}

/// A per-attachment element id. Hashing the whole id keeps two attachments with
/// same-length names (e.g. two `pasted-image.png`) from sharing one GPUI element id.
fn attachment_key(id: &str) -> usize {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish() as usize
}
