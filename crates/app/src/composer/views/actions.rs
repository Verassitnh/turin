//! Composer interactions that sit beside the footer: attachments (picker,
//! paste, strip), the model picker lifecycle and prompt history recall.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, AppContext as _, Context, CursorStyle, Image, ImageFormat, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, PathPromptOptions, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Window, div, img, px, rems,
};

use crate::components::chat::chat_media;
use crate::components::chat::expanded_image::{PreviewAttachment, build_expanded_image_preview};
use crate::composer::attachment_files::{
    ComposerAttachmentFileKind, PickedFile, classify_composer_attachment_file,
    format_attachment_size, infer_image_mime_type_from_name,
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
use crate::composer::views::language_picker_view::{LanguagePickerEvent, LanguagePickerView};
use crate::composer::views::model_picker_view::{ModelPickerEvent, ModelPickerView};
use crate::composer::views::prompt_editor::CommandKey;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_3XS, TEXT_XS};
use crate::ui::{Icon, IconName, Tooltip};

fn image_format(mime: &str) -> Option<ImageFormat> {
    Some(match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        "image/svg+xml" => ImageFormat::Svg,
        "image/bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

fn guess_mime(name: &str) -> String {
    if let Some(mime) = infer_image_mime_type_from_name(name) {
        return mime.to_owned();
    }
    match name.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("txt" | "md" | "log") => "text/plain",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
    .to_owned()
}

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
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .filter_map(read_picked_file)
                        .collect::<Vec<_>>()
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.add_picked_files(loaded, window, cx)
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn add_picked_files(
        &mut self,
        loaded: Vec<(PickedFile, Vec<u8>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut images = Vec::new();
        let mut files = Vec::new();
        let mut rejected = Vec::new();
        for (picked, data) in loaded {
            let id = format!("att-{}-{}", picked.name, self.next_attachment_id());
            match classify_composer_attachment_file(&picked) {
                ComposerAttachmentFileKind::Image => {
                    images.push(ComposerImageAttachment::new(
                        &id,
                        &picked.name,
                        &picked.mime_type,
                        data,
                    ));
                }
                ComposerAttachmentFileKind::File => files.push(ComposerFileAttachment {
                    id,
                    name: picked.name.clone(),
                    mime_type: picked.mime_type.clone(),
                    size_bytes: picked.size_bytes,
                    data: Some(Arc::new(data)),
                    uploaded_attachment_id: None,
                    upload_environment_id: None,
                }),
                ComposerAttachmentFileKind::UnsupportedImage => rejected.push(picked.name.clone()),
            }
        }
        if !rejected.is_empty() {
            self.validation = Some(format!("Unsupported image format: {}", rejected.join(", ")));
        }
        let key = self.thread_key.clone();
        self.drafts
            .add_images(&key, images, AddAttachmentsOptions::default());
        self.drafts
            .add_files(&key, files, AddAttachmentsOptions::default());
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn next_attachment_id(&mut self) -> usize {
        self.attachment_counter += 1;
        self.attachment_counter
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
                let picture = image_format(&image.mime_type)
                    .map(|format| Arc::new(Image::from_bytes(format, image.data.as_ref().clone())));
                div()
                    .id(("composer-image", image.id.len()))
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
                    .child(match picture {
                        Some(picture) => img(picture)
                            .size_full()
                            .rounded(RADIUS_LG)
                            .object_fit(ObjectFit::Cover)
                            .into_any_element(),
                        None => div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_1()
                            .text_size(TEXT_3XS.size)
                            .text_color(theme.secondary_label)
                            .child(name.clone())
                            .into_any_element(),
                    })
                    .child(self.remove_button(
                        ("remove-image", image.id.len()),
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
                    .id(("composer-file", file.id.len()))
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
                            .id(("remove-file", file.id.len()))
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

pub(super) fn read_picked_file(path: PathBuf) -> Option<(PickedFile, Vec<u8>)> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let data = std::fs::read(&path).ok()?;
    let picked = PickedFile::new(&name, &guess_mime(&name), data.len() as u64);
    Some((picked, data))
}

// ----- Stash and queued-message editing ---------------------------------------

use crate::composer::draft_store::PromptStashEntry;

impl ChatComposer {
    fn next_stash_id(&mut self) -> String {
        self.attachment_counter += 1;
        format!("stash-{}", self.attachment_counter)
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
