//! Port of upstream/apps/web/src/components/chat/ChatComposer.tsx: the
//! composer that docks under the timeline.
//!
//! [`ChatComposer`] owns the prompt editor, the per-thread draft (prompt,
//! attachments), the open state of every picker, and turns the user's intent
//! into [`SendRequest`]s for the [`ComposerSource`]. What it shows comes from
//! the [`ComposerViewModel`] published by the [`ComposerStore`]. The footer,
//! menus and attachment strip live in sibling modules as further `impl`
//! blocks of this type.

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};

use crate::composer::dispatch::{
    ComposerDispatchMode, ResolveComposerDispatchModeInput, resolve_composer_dispatch_mode,
};
use crate::composer::draft_store::{
    ComposerDraftStore, ComposerFileAttachment, ComposerImageAttachment,
};
use crate::composer::editor_state::EditorSnapshot;
use crate::composer::logic::{
    ComposerSubmissionIntent, SubmissionIntentInput, composer_submission_intent_for_key,
};
use crate::composer::primary_actions::ComposerPrimaryActionsInput;
use crate::composer::prompt_history::ComposerPromptHistoryPosition;
use crate::composer::submission::{
    ComposerSubmissionInput, SubmissionTarget, submit_composer_draft,
};
use crate::composer::view_model::{
    ComposerSource, ComposerStore, ComposerViewModel, InteractionMode, SendRequest,
};
use crate::composer::views::chrome::BoundsCell;
use crate::composer::views::prompt_editor::{
    CommandKey, EditorConfig, PromptEditor, PromptEditorEvent,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{self, RADIUS_3XL, TEXT_XS, tw};

/// Composer placeholder copy (ChatComposer.tsx, default state).
pub const COMPOSER_PLACEHOLDER: &str =
    "Ask anything, @tag files/folders, $use skills, or / for commands";

/// Which floating layer is open. One at a time, as upstream's composer menus
/// close each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    ModelPicker,
    Traits,
    RuntimeMode,
    Stash,
    Queue,
    ContextMeter,
    CodeLanguage,
}

pub struct ChatComposer {
    pub(super) store: Entity<ComposerStore>,
    pub(super) editor: Entity<PromptEditor>,
    pub(super) drafts: ComposerDraftStore,
    pub(super) thread_key: String,
    pub(super) validation: Option<String>,
    pub(super) history_position: Option<ComposerPromptHistoryPosition>,
    pub(super) layer: Option<Layer>,
    pub(super) model_picker: Option<Entity<super::model_picker_view::ModelPickerView>>,
    pub(super) language_picker: Option<(
        Entity<super::language_picker_view::LanguagePickerView>,
        gpui_kit::Point<gpui_kit::Pixels>,
    )>,
    pub(super) trigger_bounds: TriggerBounds,
    pub(super) focus: FocusHandle,
    pub(super) attachment_counter: usize,
    pub(super) menu: super::menu::MenuState,
    pub(super) banner_stack_expanded: bool,
    pub(super) queue_expanded: bool,
    pub(super) stash_highlight: Option<String>,
    pub(super) editing_run_id: Option<String>,
    pub(super) pre_edit_prompt: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// The painted bounds of each footer trigger, for anchoring its layer.
#[derive(Default, Clone)]
pub struct TriggerBounds {
    pub model: BoundsCell,
    pub traits: BoundsCell,
    pub runtime: BoundsCell,
    pub stash: BoundsCell,
    pub queue: BoundsCell,
    pub meter: BoundsCell,
    pub body: BoundsCell,
}

impl ChatComposer {
    pub fn new(store: Entity<ComposerStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            PromptEditor::new(
                "",
                EditorConfig {
                    placeholder: COMPOSER_PLACEHOLDER.into(),
                    ..EditorConfig::default()
                },
                Vec::new(),
                cx,
            )
        });
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &editor,
                window,
                |this, _, event: &PromptEditorEvent, window, cx| {
                    this.on_editor_event(event, window, cx);
                },
            ),
        ];
        let mut this = Self {
            store,
            editor,
            drafts: ComposerDraftStore::new(),
            thread_key: String::new(),
            validation: None,
            history_position: None,
            layer: None,
            model_picker: None,
            language_picker: None,
            trigger_bounds: TriggerBounds::default(),
            focus: cx.focus_handle(),
            attachment_counter: 0,
            menu: Default::default(),
            banner_stack_expanded: false,
            queue_expanded: true,
            stash_highlight: None,
            editing_run_id: None,
            pre_edit_prompt: None,
            _subscriptions: subscriptions,
        };
        this.install_key_handler(cx);
        this.apply_demo(window, cx);
        // The composer takes focus when a thread opens.
        let weak = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.focus_editor(window, cx))
                .ok();
        });
        this
    }

    fn next_demo_stash_id(&mut self) -> String {
        self.attachment_counter += 1;
        format!("stash-{}", self.attachment_counter)
    }

    /// Debug aid for screenshots: `TURIN_COMPOSER_DEMO=<scenario>` preloads
    /// the prompt and opens a layer, since synthetic input is unavailable
    /// to the capture scripts.
    fn apply_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(scenario) = std::env::var("TURIN_COMPOSER_DEMO") else {
            return;
        };
        let prompt = match scenario.as_str() {
            "rich" | "language" => Some(
                "Refactor the **parser** and keep `Foo::bar` stable:\n\n- first item\n- second item\n  - nested\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n",
            ),
            "slash" => Some("/"),
            "mention" => Some("look at @src"),
            "skill" => Some("use $"),
            _ => None,
        };
        if let Some(prompt) = prompt {
            let key = "local:thread-1".to_owned();
            self.drafts.set_prompt(&key, prompt);
        }
        let overlay = scenario.clone();
        self.store.update(cx, |store, cx| {
            let mut vm = (**store.view_model()).clone();
            crate::composer::mock_source::apply_demo_scenario(&overlay, &mut vm);
            store.publish(vm, cx);
        });
        if scenario == "attach"
            && let Some(loaded) = super::actions::read_picked_file("/tmp/t17-img.png".into())
        {
            let mut files = vec![loaded];
            files.push((
                crate::composer::attachment_files::PickedFile::new("notes.txt", "text/plain", 2048),
                vec![0u8; 2048],
            ));
            self.thread_key = "local:thread-1".into();
            self.add_picked_files(files, window, cx);
        }
        if scenario == "stash" {
            for text in [
                "Investigate the flaky sidebar test",
                "Draft release notes for 0.9",
            ] {
                let id = self.next_demo_stash_id();
                self.drafts
                    .push_stash_entry(crate::composer::draft_store::PromptStashEntry {
                        id,
                        created_at: chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        prompt: text.into(),
                        images: Vec::new(),
                        files: Vec::new(),
                        dropped_image_names: Vec::new(),
                        unreadable_image_names: Vec::new(),
                        pending_image_count: 0,
                    });
            }
        }
        let layer = match scenario.as_str() {
            "traits" => Some(Layer::Traits),
            "runtime" => Some(Layer::RuntimeMode),
            "stash" => Some(Layer::Stash),
            "queue" => Some(Layer::Queue),
            "meter" => Some(Layer::ContextMeter),
            _ => None,
        };
        if let Some(layer) = layer {
            let weak = cx.weak_entity();
            window.defer(cx, move |_, cx| {
                weak.update(cx, |this, cx| this.set_layer(Some(layer), cx))
                    .ok();
            });
        }
        if scenario == "language" {
            let weak = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                weak.update(cx, |this, cx| {
                    let block =
                        this.editor
                            .read(cx)
                            .state()
                            .doc()
                            .blocks
                            .iter()
                            .position(|block| {
                                matches!(block, crate::composer::rich_doc::Block::Code(_))
                            });
                    if let Some(block) = block {
                        this.open_language_picker(
                            block,
                            gpui_kit::point(gpui_kit::px(700.), gpui_kit::px(520.)),
                            window,
                            cx,
                        );
                    }
                })
                .ok();
            });
        }
        if scenario == "picker" {
            let weak = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                weak.update(cx, |this, cx| this.open_model_picker(window, cx))
                    .ok();
            });
        }
    }

    pub fn editor(&self) -> &Entity<PromptEditor> {
        &self.editor
    }

    pub(super) fn view_model(&self, cx: &App) -> Arc<ComposerViewModel> {
        self.store.read(cx).view_model().clone()
    }

    pub(super) fn source(&self, cx: &App) -> Option<Arc<dyn ComposerSource>> {
        self.store.read(cx).source()
    }

    /// The prompt as last reported by the editor (kept in the draft).
    pub(super) fn prompt(&self, _cx: &App) -> String {
        self.drafts
            .get_composer_draft(&self.thread_key)
            .map(|draft| draft.prompt.clone())
            .unwrap_or_default()
    }

    pub(super) fn draft_images(&self) -> Vec<ComposerImageAttachment> {
        self.drafts
            .get_composer_draft(&self.thread_key)
            .map(|draft| draft.images.clone())
            .unwrap_or_default()
    }

    pub(super) fn draft_files(&self) -> Vec<ComposerFileAttachment> {
        self.drafts
            .get_composer_draft(&self.thread_key)
            .map(|draft| draft.files.clone())
            .unwrap_or_default()
    }

    pub(super) fn has_sendable_content(&self, cx: &App) -> bool {
        !self.prompt(cx).trim().is_empty()
            || !self.draft_images().is_empty()
            || !self.draft_files().is_empty()
    }

    /// Swap the editor and attachments when the thread changes (the draft is
    /// per thread; `composerDraftStore`).
    fn sync_thread(&mut self, vm: &ComposerViewModel, window: &mut Window, cx: &mut Context<Self>) {
        if vm.thread_key == self.thread_key {
            return;
        }
        let previous = std::mem::replace(&mut self.thread_key, vm.thread_key.clone());
        if !previous.is_empty() {
            let prompt = self.prompt(cx);
            self.drafts.set_prompt(&previous, &prompt);
        }
        let prompt = self
            .drafts
            .get_composer_draft(&self.thread_key)
            .map(|draft| draft.prompt.clone())
            .unwrap_or_default();
        self.validation = None;
        self.history_position = None;
        self.layer = None;
        self.editor.update(cx, |editor, cx| {
            let end = crate::composer::js_string::utf16_len(&prompt);
            editor.set_value(&prompt, end, cx);
        });
        let _ = window;
    }

    fn sync_editor_config(&mut self, vm: &ComposerViewModel, cx: &mut Context<Self>) {
        let skills = vm
            .selected_entry()
            .map(|entry| entry.skills.clone())
            .unwrap_or_default();
        let disabled = !vm.can_operate_thread;
        let rich_text = vm.settings.composer_rich_text_enabled;
        self.editor.update(cx, |editor, cx| {
            editor.set_skills(skills);
            editor.set_config(
                EditorConfig {
                    placeholder: placeholder_for(vm).into(),
                    disabled,
                    rich_text,
                    literal_text: !rich_text,
                    ..EditorConfig::default()
                },
                cx,
            );
        });
    }

    fn on_editor_event(
        &mut self,
        event: &PromptEditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PromptEditorEvent::Changed(snapshot) => {
                self.drafts.set_prompt(&self.thread_key, &snapshot.value);
                self.validation = None;
                self.after_prompt_changed(snapshot, cx);
                cx.notify();
            }
            PromptEditorEvent::OpenCodeLanguagePicker { block, anchor } => {
                self.open_language_picker(*block, *anchor, window, cx)
            }
            PromptEditorEvent::VisibleSelectionChanged => cx.notify(),
        }
    }

    pub(super) fn set_layer(&mut self, layer: Option<Layer>, cx: &mut Context<Self>) {
        self.layer = if self.layer == layer { None } else { layer };
        if self.layer != Some(Layer::ModelPicker) {
            self.model_picker = None;
        }
        if self.layer != Some(Layer::CodeLanguage) {
            self.language_picker = None;
        }
        cx.notify();
    }

    pub(super) fn close_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layer = None;
        self.model_picker = None;
        self.language_picker = None;
        self.focus_editor(window, cx);
        cx.notify();
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.clone();
        editor.update(cx, |editor, cx| editor.focus_at_end(window, cx));
    }

    // ----- Submission -----------------------------------------------------

    fn install_key_handler(&mut self, cx: &mut Context<Self>) {
        let paste_weak = cx.weak_entity();
        self.editor.update(cx, |editor, _| {
            editor.set_paste_handler(Some(std::rc::Rc::new(move |_state, item, window, cx| {
                use crate::composer::attachment_files::{
                    PickedFile, should_handle_composer_attachment_paste,
                };
                use gpui_kit::ClipboardEntry;
                let mut loaded: Vec<(PickedFile, Vec<u8>)> = Vec::new();
                let mut plain = String::new();
                for entry in item.entries() {
                    match entry {
                        ClipboardEntry::Image(image) => {
                            let extension = match image.format {
                                gpui_kit::ImageFormat::Jpeg => "jpg",
                                gpui_kit::ImageFormat::Webp => "webp",
                                gpui_kit::ImageFormat::Gif => "gif",
                                gpui_kit::ImageFormat::Svg => "svg",
                                gpui_kit::ImageFormat::Bmp => "bmp",
                                gpui_kit::ImageFormat::Tiff => "tiff",
                                _ => "png",
                            };
                            let name = format!("pasted-image.{extension}");
                            loaded.push((
                                PickedFile::new(
                                    &name,
                                    image.format.mime_type(),
                                    image.bytes.len() as u64,
                                ),
                                image.bytes.clone(),
                            ));
                        }
                        ClipboardEntry::ExternalPaths(paths) => {
                            loaded.extend(
                                paths
                                    .paths()
                                    .iter()
                                    .cloned()
                                    .filter_map(super::actions::read_picked_file),
                            );
                        }
                        ClipboardEntry::String(string) => plain.push_str(string.text()),
                    }
                }
                let files: Vec<PickedFile> = loaded.iter().map(|(file, _)| file.clone()).collect();
                if loaded.is_empty() || !should_handle_composer_attachment_paste(&files, &plain) {
                    return false;
                }
                let weak = paste_weak.clone();
                window.defer(cx, move |window, cx| {
                    weak.update(cx, |this, cx| this.add_picked_files(loaded, window, cx))
                        .ok();
                });
                true
            })));
        });
        let weak = cx.weak_entity();
        self.editor.update(cx, |editor, _| {
            editor.set_command_handler(Some(std::rc::Rc::new(move |state, key, window, cx| {
                let snapshot = state.snapshot();

                weak.update(cx, |this, cx| {
                    this.on_command_key(&snapshot, key, window, cx)
                })
                .unwrap_or(false)
            })));
        });
    }

    /// `onCommandKeyDown`: the keys the composer claims before the editor.
    fn on_command_key(
        &mut self,
        snapshot: &EditorSnapshot,
        key: &CommandKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if key.key == "Tab" && key.shift && !key.alt && !key.meta && !key.ctrl {
            // Shift+Tab flips plan/build mode.
            if !self.view_model(cx).settings.plan_mode_enabled {
                return false;
            }
            self.toggle_interaction_mode(cx);
            return true;
        }
        if self.layer == Some(Layer::Stash) && self.on_stash_key(key, window, cx) {
            return true;
        }
        if key.key == "s" && key.meta && !key.shift && !key.alt && !key.ctrl && !key.is_composing {
            let weak = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                weak.update(cx, |this, cx| this.stash_or_restore(window, cx))
                    .ok();
            });
            return true;
        }
        if self.on_menu_key(snapshot, key, window, cx) {
            return true;
        }
        if self.on_history_key(snapshot, key, window, cx) {
            return true;
        }
        let vm = self.view_model(cx);
        let event = key.as_key_event();
        let intent = composer_submission_intent_for_key(&SubmissionIntentInput {
            event: &event,
            command: None,
            is_mobile_viewport: false,
            is_draft_thread: vm.is_draft_thread,
            is_running: vm.phase == crate::composer::view_model::SessionPhase::Running,
            send_shortcut: Some(vm.settings.send_shortcut),
            prompt: &snapshot.value,
        });
        let Some(intent) = intent else {
            return self.plain_list_key(snapshot, key, window, cx);
        };
        let intent = if (key.meta || key.ctrl)
            && intent == ComposerSubmissionIntent::Foreground
            && vm.phase == crate::composer::view_model::SessionPhase::Running
        {
            ComposerSubmissionIntent::Alternate
        } else {
            intent
        };
        // The editor is mid-update: submit once it has returned.
        let weak = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.submit(intent, window, cx))
                .ok();
        });
        true
    }

    /// Plain-text mode has no list nodes, so Enter and Tab continue or indent
    /// markdown list lines in the text itself (`listContinuationForEnter` /
    /// `listIndentForTab`). Rich mode does this in the editor.
    fn plain_list_key(
        &mut self,
        snapshot: &EditorSnapshot,
        key: &CommandKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::composer::list_continuation::{
            list_continuation_for_enter, list_indent_for_tab,
        };
        let vm = self.view_model(cx);
        if vm.settings.composer_rich_text_enabled
            || key.is_composing
            || key.shift
            || key.alt
            || key.meta
            || key.ctrl
        {
            return false;
        }
        if snapshot.selection_start != snapshot.selection_end {
            return false;
        }
        let edit = match key.key.as_str() {
            "Enter" => list_continuation_for_enter(&snapshot.value, snapshot.selection_start),
            "Tab" => list_indent_for_tab(
                &snapshot.value,
                snapshot.selection_start,
                snapshot.selection_end,
            ),
            _ => None,
        };
        let Some(edit) = edit else {
            return false;
        };
        let value = snapshot.value.clone();
        let weak = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            weak.update(cx, |this, cx| {
                this.apply_prompt_replacement(
                    &value,
                    edit.start,
                    edit.end,
                    &edit.replacement,
                    true,
                    window,
                    cx,
                )
            })
            .ok();
        });
        true
    }

    /// `onSubmit`: validate, hand the draft to the source and clear it.
    pub fn submit(
        &mut self,
        intent: ComposerSubmissionIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vm = self.view_model(cx);
        let Some(source) = self.source(cx) else {
            return;
        };
        if vm.is_send_busy || vm.is_connecting || vm.environment_unavailable.is_some() {
            return;
        }
        let prompt = self.prompt(cx);
        let images = self.draft_images();
        let files = self.draft_files();
        if prompt.trim().is_empty() && images.is_empty() && files.is_empty() {
            return;
        }
        let running = vm.phase == crate::composer::view_model::SessionPhase::Running;
        let editing_queued_run = vm
            .queue
            .as_ref()
            .and_then(|queue| queue.editing_run_id.clone());
        let dispatch_mode = if editing_queued_run.is_some() {
            ComposerDispatchMode::Auto
        } else {
            resolve_composer_dispatch_mode(ResolveComposerDispatchModeInput {
                running,
                alternate_modifier: intent == ComposerSubmissionIntent::Alternate,
                active_turn_default: Some(vm.settings.follow_up_behavior),
            })
        };
        let answering = vm.pending_answer.is_some();
        let request = SendRequest {
            prompt: prompt.clone(),
            images,
            files,
            instance_id: vm.selected_instance_id.clone(),
            model: vm.selected_model.clone(),
            model_options: (!vm.selected_model_options.is_empty())
                .then(|| vm.selected_model_options.clone()),
            interaction_mode: vm.interaction_mode,
            runtime_mode: vm.runtime_mode,
            dispatch_mode,
            submission_intent: intent,
            answering_pending_user_input: answering,
            editing_queued_run,
        };
        let target = if answering {
            SubmissionTarget::PendingUserInput
        } else {
            SubmissionTarget::ProviderTurn
        };
        let mut prevented = false;
        let result = submit_composer_draft(
            &ComposerSubmissionInput {
                prompt: &prompt,
                provider_input: None,
                submission_target: target,
            },
            || prevented = true,
            || source.send(request, cx),
        );
        self.validation = result.validation_message;
        if result.did_dispatch {
            self.clear_draft(window, cx);
        }
        cx.notify();
    }

    pub(super) fn clear_draft(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.drafts.clear_composer_content(&self.thread_key);
        self.history_position = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value("", 0, cx));
    }

    pub(super) fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(source) = self.source(cx) {
            source.interrupt(cx);
        }
    }

    pub(super) fn toggle_interaction_mode(&mut self, cx: &mut Context<Self>) {
        let vm = self.view_model(cx);
        let next = match vm.interaction_mode {
            InteractionMode::Default => InteractionMode::Plan,
            InteractionMode::Plan => InteractionMode::Default,
        };
        if let Some(source) = self.source(cx) {
            source.set_interaction_mode(next, cx);
        }
    }

    pub(super) fn primary_actions_input(
        &self,
        vm: &ComposerViewModel,
        compact: bool,
        cx: &App,
    ) -> ComposerPrimaryActionsInput {
        let prompt_has_text = !self.prompt(cx).trim().is_empty();
        ComposerPrimaryActionsInput {
            compact,
            can_operate_thread: vm.can_operate_thread,
            pending_action: vm.pending_answer.as_ref().map(|answer| {
                crate::composer::primary_actions::PendingActionState {
                    question_index: answer.question_index,
                    is_last_question: answer.is_last_question,
                    can_advance: answer.can_advance,
                    is_responding: answer.is_responding,
                    is_complete: answer.is_complete,
                }
            }),
            is_running: vm.phase == crate::composer::view_model::SessionPhase::Running,
            can_interrupt: vm.can_interrupt,
            follow_up_behavior: vm.settings.follow_up_behavior,
            alternate_shortcut_label: vm.shortcut_labels.get("composer.sendAlternate").cloned(),
            show_plan_follow_up_prompt: vm.show_plan_follow_up_prompt,
            prompt_has_text,
            is_send_busy: vm.is_send_busy,
            send_disabled_reason: vm.send_disabled_reason.clone(),
            is_connecting: vm.is_connecting,
            is_environment_unavailable: vm.environment_unavailable.is_some(),
            is_preparing_worktree: vm.is_preparing_worktree,
            has_sendable_content: self.has_sendable_content(cx),
            can_resume: vm.can_resume,
            is_editing_queued_message: vm
                .queue
                .as_ref()
                .is_some_and(|queue| queue.editing_run_id.is_some()),
            compact_before_send_tokens: vm.resume_compaction_tokens,
            keep_full_history: vm.keep_full_history,
            alternate_modifier: false,
        }
    }
}

fn placeholder_for(vm: &ComposerViewModel) -> String {
    if let Some(answer) = &vm.pending_answer {
        if answer.is_choice_only {
            return "Select an option above, or type to add a note".to_owned();
        }
        return "Type your own answer, or select an option above".to_owned();
    }
    if vm.show_plan_follow_up_prompt {
        return "Add feedback to refine the plan, or leave this blank to implement it".to_owned();
    }
    if vm.phase == crate::composer::view_model::SessionPhase::Disconnected {
        return "Ask for follow-up changes or attach images".to_owned();
    }
    COMPOSER_PLACEHOLDER.to_owned()
}

impl Focusable for ChatComposer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for ChatComposer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vm = self.view_model(cx);
        self.sync_thread(&vm, window, cx);
        self.sync_queue_edit(&vm, window, cx);
        self.sync_editor_config(&vm, cx);
        let theme = cx.app_theme().clone();
        let dark = theme.is_dark();
        // Light: `--card` at the 80% glass opacity; dark: `--surface-raised`.
        // Flattened over the canvas: GPUI paints box shadows under the
        // element (CSS clips them to outside the border box), so a
        // translucent fill would show its own drop shadow through it.
        let glass = if dark {
            theme.surface_raised
        } else {
            theme.card
        };
        let surface = color_mix(MixSpace::Srgb, glass, 0.8, theme.background);
        let outline = if dark {
            with_alpha(tw::WHITE, 0.05)
        } else {
            with_alpha(tw::BLACK, 0.08)
        };

        if self.menu.is_open() {
            self.refresh_menu_items(cx);
        }
        let menu_drawer = self.render_menu_drawer(window, cx);
        let drawers = self.render_drawers(&vm, window, cx);
        let attachments = self.render_attachments(&vm, cx);
        let footer = self.render_footer(&vm, window, cx);
        let layers = self.render_layers(&vm, window, cx);
        let validation =
            crate::composer::submission::get_composer_prompt_length_validation_message(
                &self.prompt(cx),
            )
            .or_else(|| self.validation.clone());

        let frame = div()
            .id("chat-composer")
            .relative()
            .drag_over::<gpui_kit::ExternalPaths>(|style, _, _, cx| style.bg(cx.app_theme().accent))
            .on_drop(
                cx.listener(|this, paths: &gpui_kit::ExternalPaths, window, cx| {
                    let loaded: Vec<_> = paths
                        .paths()
                        .iter()
                        .cloned()
                        .filter_map(super::actions::read_picked_file)
                        .collect();
                    this.add_picked_files(loaded, window, cx);
                }),
            )
            .w_full()
            .rounded(RADIUS_3XL)
            .bg(surface)
            .border_1()
            .border_color(outline)
            .shadow(if dark {
                Vec::new()
            } else {
                tokens::shadow_composer(false)
            })
            .child(
                // Body: `px-4 pt-4 pb-2`.
                div()
                    .relative()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(super::chrome::track_bounds(&self.trigger_bounds.body))
                    .children(attachments)
                    .child(self.editor.clone()),
            )
            .when_some(validation, |this, message| {
                // ComposerPromptLengthValidation: `px-3 pb-2 text-xs text-destructive sm:px-4`.
                this.child(
                    div()
                        .px_4()
                        .pb_2()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.destructive)
                        .child(SharedString::from(message)),
                )
            })
            .child(footer)
            .children(layers);
        div()
            .w_full()
            .flex()
            .flex_col()
            .children(menu_drawer)
            .children(drawers)
            .child(frame)
    }
}
