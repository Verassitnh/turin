//! Port of upstream/apps/web/src/components/ComposerPromptEditorTiptap.tsx
//! Port of upstream/apps/web/src/components/ComposerPromptEditor.tsx
//!
//! The rich prompt editor view: port of ComposerPromptEditorTiptap.tsx's
//! surface, built on [`EditorState`].
//!
//! The view paints each block with [`InlineFlow`] (the chat markdown line
//! engine, extended with highlight/caret/probe hooks), forwards keyboard,
//! pointer and input-method events to the state machine, and reports changes
//! to its host. The host (the chat composer) sees a [`PromptEditorEvent`] after
//! every change and may claim keys first through the command handler, which is
//! upstream's `onCommandKeyDown`.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UTF16Selection, WeakEntity, Window,
    canvas, div, point, px, rems,
};

use crate::components::chat::markdown::code_highlight::highlight_code;
use crate::components::chat::markdown::inline_flow::{
    FlowCaret, FlowHighlight, FlowProbe, FlowSpan, FlowSpanStyle, InlineFlow, WhiteSpace,
};
use crate::components::chat::page_scroll_controller::{
    PageScrollKey, PageScrollKeyEvent, get_timeline_page_scroll_key,
};
use crate::composer::catalog::ProviderSkill;
use crate::composer::citation_comment_dismissal::{
    AssistantCitationCommentDismissal, resolve_assistant_citation_comment_dismissal,
};
use crate::composer::code_languages::{code_language_label, language_of_info_string};
use crate::composer::editor_state::{
    CitationCommentRequest, CitationDraftSave, EdgeStop, EditorKey, EditorSnapshot, EditorState,
    Selection,
};
use crate::composer::rich_doc::{
    Atom, Block, CodeBlock, DocPos, InlineContent, ItemPrefix, Marks, SkillMeta, list_depths,
};
use crate::composer::rich_text::RichTextMark;
use crate::composer::views::block_flow::{
    BlockFlow, BlockTypography, CaretSide, FlowEnv, MarkerReveal, OffsetMap, Seg, SegKind,
    build_inline_flow,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{LEADING_RELAXED, RADIUS_LG, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

gpui_kit::actions!(
    composer_editor,
    [
        /// StarterKit's Mod-B bold in the rich composer. Bound in the editor's
        /// key context so it outranks `sidebar.toggle` there; plain mode
        /// passes it on (AppSidebarLayout's `isRichTextBoldShortcut` carve-out).
        ToggleBold,
    ]
);

const KEY_CONTEXT: &str = "ComposerEditor";

/// The editor's own chords.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        "cmd-b",
        ToggleBold,
        Some(KEY_CONTEXT),
    )]);
}

/// What the host is told.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptEditorEvent {
    /// The document or the caret changed.
    Changed(EditorSnapshot),
    /// A range selection appeared or moved (`onVisibleSelectionChange`).
    VisibleSelectionChanged,
    /// The language control of a code block was clicked.
    OpenCodeLanguagePicker { block: usize, anchor: Point<Pixels> },
    /// PageUp/PageDown the editor cannot scroll any further, for the timeline
    /// (`onPageScrollKeyDown`).
    PageScrollKeyDown(PageScrollKey),
    /// A key went up (`onPageScrollKeyUp`).
    PageScrollKeyUp(String),
    /// A modifier changed, the page key no longer scrolls, or focus left
    /// (`onPageScrollRelease`).
    PageScrollRelease,
    /// A file mention chip was clicked (`actions.openMention(path)`).
    OpenMention(String),
    /// A citation comment was saved with Cmd/Ctrl+Enter
    /// (`onCitationSubmitAndSend`).
    CitationSubmitAndSend,
    /// A new controlled value replaced the document while a citation comment
    /// was unsaved; the comment was saved into `replaced_value`, giving
    /// `saved_value` (`onCitationDraftSave`).
    CitationDraftSaved(CitationDraftSave),
}

impl EventEmitter<PromptEditorEvent> for PromptEditor {}

/// A key the host may claim before the editor acts (`onCommandKeyDown`).
#[derive(Debug, Clone)]
pub struct CommandKey {
    pub key: String,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    pub ctrl: bool,
    pub is_composing: bool,
    /// Auto-repeat of a held key (`KeyDownEvent::is_held`).
    pub repeat: bool,
    /// The caret sits on the first / last visual line (for history recall);
    /// only filled for ArrowUp / ArrowDown.
    pub caret_on_first_line: bool,
    pub caret_on_last_line: bool,
}

impl CommandKey {
    fn from_event(event: &KeyDownEvent) -> Self {
        let modifiers = event.keystroke.modifiers;
        Self {
            key: normalize_key(&event.keystroke.key),
            shift: modifiers.shift,
            alt: modifiers.alt,
            meta: modifiers.platform,
            ctrl: modifiers.control,
            is_composing: false,
            repeat: event.is_held,
            caret_on_first_line: false,
            caret_on_last_line: false,
        }
    }

    pub fn as_key_event(&self) -> crate::composer::logic::ComposerKeyEvent {
        crate::composer::logic::ComposerKeyEvent {
            key: self.key.clone(),
            meta_key: self.meta,
            ctrl_key: self.ctrl,
            shift_key: self.shift,
            alt_key: self.alt,
            is_composing: self.is_composing,
            repeat: false,
        }
    }
}

/// GPUI key names → the DOM `KeyboardEvent.key` values the composer logic reads.
fn normalize_key(key: &str) -> String {
    match key {
        "enter" => "Enter",
        "tab" => "Tab",
        "escape" => "Escape",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        other => other,
    }
    .to_owned()
}

pub type CommandKeyHandler =
    Rc<dyn Fn(&mut EditorState, &CommandKey, &mut Window, &mut App) -> bool>;
pub type BuildContextFragment = Rc<dyn Fn(&[String]) -> Option<String>>;
pub type ImportContextFragment = Rc<
    dyn Fn(
        &turin_contracts::composer_context_clipboard::ComposerContextClipboardFragment,
    ) -> std::collections::HashMap<String, String>,
>;
pub type PasteHandler = Rc<dyn Fn(&mut EditorState, &ClipboardItem, &mut Window, &mut App) -> bool>;

#[derive(Clone)]
pub struct EditorConfig {
    pub placeholder: SharedString,
    pub disabled: bool,
    pub rich_text: bool,
    pub literal_text: bool,
    /// Height bounds in rems: the composer body is `min-h-19.5` up to 13rem;
    /// a one-line field (the picker search) uses its own.
    pub min_height: f32,
    pub max_height: f32,
    /// Overrides the prompt font size (px).
    pub font_size: Option<f32>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            placeholder: SharedString::default(),
            disabled: false,
            rich_text: true,
            literal_text: false,
            min_height: 4.875,
            max_height: 13.,
            font_size: None,
        }
    }
}

type Flows = Rc<RefCell<Vec<Option<Rc<BlockFlow>>>>>;

pub struct PromptEditor {
    state: EditorState,
    focus: FocusHandle,
    scroll: ScrollHandle,
    config: EditorConfig,
    skills: Vec<ProviderSkill>,
    probes: Rc<RefCell<Vec<Option<FlowProbe>>>>,
    flows: Flows,
    command_handler: Option<CommandKeyHandler>,
    paste_handler: Option<PasteHandler>,
    last_snapshot: Option<EditorSnapshot>,
    blink_on: bool,
    blink_epoch: u64,
    selecting: bool,
    click_count: usize,
    last_click: Option<(std::time::Instant, Point<Pixels>)>,
    marked: Option<Range<usize>>,
    /// The x a run of vertical moves aims for.
    goal_x: Option<Pixels>,
    scroll_caret_into_view: bool,
    path_query_active: bool,
    was_focused: bool,
    /// How many draft context records back the prompt (`contextRecords.size`).
    context_record_count: usize,
    /// The open citation's comment field (`AssistantCitationCommentEditor`).
    comment_editor: Option<(String, Entity<PromptEditor>, Subscription)>,
    /// Frames repainted while waiting for the chip's box to anchor to.
    comment_anchor_retries: u8,
    build_context_fragment: Option<BuildContextFragment>,
    import_context_fragment: Option<ImportContextFragment>,
}

impl PromptEditor {
    pub fn new(
        value: &str,
        config: EditorConfig,
        skills: Vec<ProviderSkill>,
        cx: &mut Context<Self>,
    ) -> Self {
        let label_for = skill_label_fn(&skills);
        let state =
            EditorState::from_value(value, &*label_for, config.rich_text, config.literal_text);
        let mut this = Self {
            state,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            config,
            skills,
            probes: Rc::default(),
            flows: Rc::default(),
            command_handler: None,
            paste_handler: None,
            last_snapshot: None,
            blink_on: true,
            blink_epoch: 0,
            selecting: false,
            click_count: 0,
            last_click: None,
            marked: None,
            goal_x: None,
            scroll_caret_into_view: false,
            path_query_active: false,
            was_focused: false,
            context_record_count: 0,
            comment_editor: None,
            comment_anchor_retries: 0,
            build_context_fragment: None,
            import_context_fragment: None,
        };
        this.last_snapshot = Some(this.state.snapshot());
        this
    }

    pub fn state(&self) -> &EditorState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut EditorState {
        &mut self.state
    }

    pub fn snapshot(&mut self) -> EditorSnapshot {
        self.state.snapshot()
    }

    pub fn set_command_handler(&mut self, handler: Option<CommandKeyHandler>) {
        self.command_handler = handler;
    }

    pub fn set_paste_handler(&mut self, handler: Option<PasteHandler>) {
        self.paste_handler = handler;
    }

    /// `buildContextClipboardFragment`: the structured clipboard payload for
    /// the referenced ids, or `None` to copy text only.
    pub fn set_build_context_fragment(&mut self, build: Option<BuildContextFragment>) {
        self.build_context_fragment = build;
    }

    /// `importContextFragment`: imports a structured paste's records and
    /// returns the ids that changed.
    pub fn set_import_context_fragment(&mut self, import: Option<ImportContextFragment>) {
        self.import_context_fragment = import;
    }

    pub fn set_config(&mut self, config: EditorConfig, cx: &mut Context<Self>) {
        if config.rich_text != self.config.rich_text
            || config.literal_text != self.config.literal_text
        {
            // Extensions are creation-time upstream: a mode flip rebuilds the
            // editor from the same controlled value.
            let snapshot = self.state.snapshot();
            let label_for = skill_label_fn(&self.skills);
            self.state = EditorState::from_value(
                &snapshot.value,
                &*label_for,
                config.rich_text,
                config.literal_text,
            );
            self.state.set_collapsed_cursor(snapshot.cursor);
        }
        self.config = config;
        cx.notify();
    }

    pub fn set_skills(&mut self, skills: Vec<ProviderSkill>) {
        self.skills = skills;
    }

    pub fn set_path_query_active(&mut self, active: bool) {
        self.path_query_active = active;
    }

    pub fn is_empty(&self) -> bool {
        self.state.is_empty()
    }

    /// The number of draft context records (`contextRecords.size`); the
    /// placeholder hides while any exist.
    pub fn set_context_record_count(&mut self, count: usize, cx: &mut Context<Self>) {
        if self.context_record_count != count {
            self.context_record_count = count;
            cx.notify();
        }
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Controlled value/cursor from the store (history recall, chip
    /// insertion…). A no-op when both already match.
    ///
    /// An unsaved citation comment is saved into the replaced value first
    /// (`saveCitationDraft`); the save is returned and emitted so the owner
    /// of the replaced value can keep it.
    pub fn set_value(
        &mut self,
        value: &str,
        cursor: usize,
        cx: &mut Context<Self>,
    ) -> Option<CitationDraftSave> {
        let snapshot = self.state.snapshot();
        if snapshot.value == value && snapshot.cursor == cursor {
            return None;
        }
        let label_for = skill_label_fn(&self.skills);
        let saved = self
            .state
            .apply_controlled_value(value, cursor, &*label_for);
        if let Some(saved) = &saved {
            cx.emit(PromptEditorEvent::CitationDraftSaved(saved.clone()));
        }
        self.sync_comment_editor(None, cx);
        self.after_change(cx);
        saved
    }

    // ── Citation comments ──────────────────────────────────────────────────

    /// `requestCitationComment`: open the comment editor of a citation just
    /// inserted through the store, now or once its value is applied.
    pub fn request_citation_comment(
        &mut self,
        request: CitationCommentRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.request_citation_comment(request);
        self.sync_comment_editor(Some(window), cx);
        cx.notify();
    }

    /// Open a citation's comment editor (the chip's comment trigger).
    fn open_citation_comment(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.config.disabled {
            return;
        }
        self.state.set_citation_comment_open(key, true);
        self.sync_comment_editor(Some(window), cx);
        cx.notify();
    }

    /// Mount or drop the comment field to match the open citation.
    fn sync_comment_editor(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let open = self
            .state
            .open_citation_comment()
            .map(|open| open.key.clone());
        if self.comment_editor.as_ref().map(|(key, ..)| key) == open.as_ref() {
            return;
        }
        self.comment_editor = None;
        let Some(key) = open else {
            return;
        };
        let Some((_, citation)) = self.state.open_citation() else {
            return;
        };
        let initial = citation.comment.clone().unwrap_or_default();
        let config = EditorConfig {
            placeholder: "Add an optional comment...".into(),
            disabled: false,
            rich_text: false,
            literal_text: true,
            // `min-h-16 max-h-40`, `sm:text-sm`.
            min_height: 4.,
            max_height: 10.,
            font_size: Some(14.),
        };
        let field =
            gpui_kit::AppContext::new(cx, |cx| PromptEditor::new(&initial, config, Vec::new(), cx));
        let weak = cx.weak_entity();
        field.update(cx, |field, _| {
            field.set_command_handler(Some(Rc::new(move |state, key, window, cx| {
                comment_field_key(&weak, state, key, window, cx)
            })));
        });
        let draft_key = key.clone();
        let subscription = cx.subscribe(&field, move |this, _, event: &PromptEditorEvent, cx| {
            if let PromptEditorEvent::Changed(snapshot) = event {
                this.state
                    .set_citation_comment_draft(&draft_key, &snapshot.value);
                cx.notify();
            }
        });
        if let Some(window) = window {
            field.update(cx, |field, cx| field.focus_at_end(window, cx));
        }
        self.comment_editor = Some((key, field, subscription));
    }

    /// `onSubmit` / `onSubmitAndSend`: save, close, and put the caret back
    /// after the chip (`onRestoreFocus`).
    fn submit_citation_comment(
        &mut self,
        comment: &str,
        send: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.state.open_citation_comment().map(|o| o.key.clone()) else {
            return;
        };
        if crate::composer::js_string::utf16_len(comment)
            > turin_contracts::assistant_citations::ASSISTANT_CITATION_MAX_COMMENT_LENGTH
        {
            return;
        }
        if !self.state.save_citation_comment(&key, comment) {
            return;
        }
        self.state.set_citation_comment_open(&key, false);
        self.state.caret_after_citation(&key);
        self.sync_comment_editor(None, cx);
        window.focus(&self.focus, cx);
        self.after_change(cx);
        if send {
            cx.emit(PromptEditorEvent::CitationSubmitAndSend);
        }
    }

    /// `onCancel`: a fresh insertion is removed with its spaces; otherwise the
    /// popover closes and the caret returns after the chip.
    fn cancel_citation_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.state.open_citation_comment().map(|o| o.key.clone()) else {
            return;
        };
        if !self.state.remove_citation_on_cancel(&key) {
            self.state.set_citation_comment_open(&key, false);
            self.state.caret_after_citation(&key);
        }
        self.sync_comment_editor(None, cx);
        window.focus(&self.focus, cx);
        self.after_change(cx);
    }

    /// A press outside the popover (`settleDraftOnClose("outside-press")`).
    fn dismiss_citation_comment(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.state.open_citation_comment().map(|o| o.key.clone()) else {
            return;
        };
        let draft = self
            .comment_editor
            .as_ref()
            .map(|(_, field, _)| field.read(cx).state().doc().clone())
            .map(|doc| crate::composer::rich_doc::serialize_doc(&doc).value);
        let saved = self.state.open_citation().and_then(|(_, c)| c.comment);
        match resolve_assistant_citation_comment_dismissal(
            "outside-press",
            draft.as_deref(),
            saved.as_deref(),
        ) {
            AssistantCitationCommentDismissal::KeepOpen => return,
            AssistantCitationCommentDismissal::Commit { comment } => {
                self.state.save_citation_comment(&key, &comment);
            }
            AssistantCitationCommentDismissal::Close => {}
        }
        self.state.set_citation_comment_open(&key, false);
        self.sync_comment_editor(None, cx);
        self.after_change(cx);
    }

    /// The comment popover (`PopoverPopup side="top" align="end" width="md"
    /// padding="compact"`), anchored to the chip.
    fn citation_comment_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, field, _) = self.comment_editor.as_ref()?;
        let (pos, _) = self.state.open_citation()?;
        let anchor = self.atom_bounds(pos)?;
        let theme = cx.app_theme().clone();
        let comment = crate::composer::rich_doc::serialize_doc(field.read(cx).state().doc()).value;
        let too_long = crate::composer::js_string::utf16_len(&comment)
            > turin_contracts::assistant_citations::ASSISTANT_CITATION_MAX_COMMENT_LENGTH;
        let weak = cx.weak_entity();
        let cancel = weak.clone();
        let save = weak.clone();
        let content = crate::composer::views::chrome::glass_surface(cx)
            .id("citation-comment-popover")
            .w(rems(20.))
            .py(rems(0.5))
            .px(rems(0.75))
            .on_mouse_down_out(move |_, _, cx| {
                weak.update(cx, |this, cx| this.dismiss_citation_comment(cx))
                    .ok();
            })
            .child(div().px(rems(0.25)).py(rems(0.375)).child(field.clone()))
            .when(too_long, |this| {
                this.child(
                    div()
                        .pt(rems(0.25))
                        .text_size(TEXT_XS.size)
                        .line_height(TEXT_XS.line_height)
                        .text_color(theme.destructive)
                        .child(format!(
                            "Comments can contain up to {} characters.",
                            format_thousands(
                                turin_contracts::assistant_citations::ASSISTANT_CITATION_MAX_COMMENT_LENGTH
                            )
                        )),
                )
            })
            .child(
                div()
                    .mt(rems(0.5))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(rems(0.5))
                    .child(
                        Button::new("citation-comment-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Xs)
                            .label("Cancel")
                            .on_click(move |_, window, cx| {
                                cancel
                                    .update(cx, |this, cx| this.cancel_citation_comment(window, cx))
                                    .ok();
                            }),
                    )
                    .child(
                        Button::new("citation-comment-save")
                            .size(ButtonSize::Xs)
                            .label(if too_long { "Shorten comment" } else { "Save" })
                            .disabled(too_long)
                            .on_click(move |_, window, cx| {
                                save.update(cx, |this, cx| {
                                    let comment = this
                                        .comment_editor
                                        .as_ref()
                                        .map(|(_, field, _)| {
                                            crate::composer::rich_doc::serialize_doc(
                                                field.read(cx).state().doc(),
                                            )
                                            .value
                                        })
                                        .unwrap_or_default();
                                    this.submit_citation_comment(&comment, false, window, cx)
                                })
                                .ok();
                            }),
                    ),
            );
        Some(crate::composer::views::chrome::floating(
            anchor, true, true, content,
        ))
    }

    /// The painted box of the chip at `pos`, from its block's line boxes.
    fn atom_bounds(&self, pos: DocPos) -> Option<Bounds<Pixels>> {
        let flow = self.block_flow_for(pos.block)?;
        let probe = self.probes.borrow().get(pos.block).cloned().flatten()?;
        let seg = flow
            .map
            .segs
            .iter()
            .find(|seg| seg.kind == SegKind::Atom && seg.doc_start == pos.offset)?;
        let start = probe.caret_bounds(seg.byte_start, px(1.))?;
        let end = probe.caret_bounds(seg.byte_start + seg.byte_len, px(1.))?;
        if (start.origin.y - end.origin.y).abs() > px(1.) {
            return Some(start);
        }
        Some(Bounds::from_corners(
            start.origin,
            point(end.origin.x, end.bottom()),
        ))
    }

    /// The chip under `position`, if any.
    fn chip_at(&self, position: Point<Pixels>) -> Option<(DocPos, Atom)> {
        let pos = self.pos_for_point(position)?;
        let inline = self.state.doc().blocks.get(pos.block)?.inline()?;
        let mut offset = 0usize;
        for node in inline {
            if let InlineContent::Atom(atom) = &node.content
                && (offset + 1 == pos.offset || offset == pos.offset)
            {
                let at = DocPos::new(pos.block, offset);
                if self
                    .atom_bounds(at)
                    .is_some_and(|bounds| bounds.contains(&position))
                {
                    return Some((at, atom.clone()));
                }
            }
            offset += node.doc_len();
        }
        None
    }

    /// Place the caret at a collapsed cursor and focus.
    pub fn focus_at(&mut self, cursor: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.state.set_collapsed_cursor(cursor);
        window.focus(&self.focus, cx);
        self.after_change(cx);
    }

    pub fn focus_at_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.state.snapshot().value;
        let end = crate::composer::logic::collapse_expanded_composer_cursor(
            &value,
            crate::composer::js_string::utf16_len(&value),
            self.config.literal_text,
        );
        self.focus_at(end, window, cx);
    }

    /// Repaint and report after the host edited the state directly.
    /// The language picker's choice for the fence at `block`.
    pub fn set_code_block_language(
        &mut self,
        block: usize,
        language: &str,
        cx: &mut Context<Self>,
    ) {
        if self.state.set_code_block_language(block, language) {
            self.after_change(cx);
        }
    }

    /// The info-string language of the fence at `block`.
    pub fn code_block_language(&self, block: usize) -> Option<String> {
        match self.state.doc().blocks.get(block) {
            Some(Block::Code(code)) => Some(
                crate::composer::code_languages::language_of_info_string(&code.language).to_owned(),
            ),
            _ => None,
        }
    }

    pub fn after_external_edit(&mut self, cx: &mut Context<Self>) {
        self.after_change(cx);
    }

    fn skill_label(&self) -> Rc<dyn Fn(&str) -> SkillMeta> {
        skill_label_fn(&self.skills)
    }

    fn restart_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.blink_on = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(530))
                        .await;
                    let alive = weak
                        .update(cx, |this, cx| {
                            if this.blink_epoch != epoch {
                                return false;
                            }
                            this.blink_on = !this.blink_on;
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !alive {
                        break;
                    }
                }
            })
            .detach();
    }

    /// Report the state to the host and repaint.
    fn after_change(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.state.snapshot();
        let previous = self.last_snapshot.replace(snapshot.clone());
        self.scroll_caret_into_view = true;
        if previous.as_ref() != Some(&snapshot) {
            use crate::composer::selection::{
                ComposerSelectionRange, did_composer_selection_change_visibly,
            };
            let visible = did_composer_selection_change_visibly(
                previous
                    .as_ref()
                    .map_or(ComposerSelectionRange { start: 0, end: 0 }, |p| {
                        ComposerSelectionRange {
                            start: p.selection_start,
                            end: p.selection_end,
                        }
                    }),
                Some(ComposerSelectionRange {
                    start: snapshot.selection_start,
                    end: snapshot.selection_end,
                }),
            );
            cx.emit(PromptEditorEvent::Changed(snapshot));
            if visible {
                cx.emit(PromptEditorEvent::VisibleSelectionChanged);
            }
        }
        cx.notify();
    }

    fn block_flow_for(&self, index: usize) -> Option<Rc<BlockFlow>> {
        self.flows.borrow().get(index).cloned().flatten()
    }

    fn caret_side(&self) -> CaretSide {
        match self.state.caret_edge_stop() {
            Some(EdgeStop::Before) => CaretSide::InsideBefore,
            Some(EdgeStop::After) => CaretSide::InsideAfter,
            _ => CaretSide::Plain,
        }
    }

    // ── Keys ───────────────────────────────────────────────────────────────

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.config.disabled {
            return;
        }
        let mut key = CommandKey::from_event(event);
        key.is_composing = self.marked.is_some();
        let modifiers = event.keystroke.modifiers;
        let command = modifiers.platform;

        // The editor's own rules first (`handleKeyDown` before
        // `onCommandKeyDown`): styled-edge stops, chip skipping, Enter during
        // composition, and Tab / Enter that belong to a fence.
        let editor_key = EditorKey {
            key: key.key.clone(),
            shift: key.shift,
            alt: key.alt,
            meta: key.meta,
            ctrl: key.ctrl,
            is_composing: key.is_composing,
        };
        if self.state.handle_key_down(&editor_key) {
            if !matches!(key.key.as_str(), "ArrowUp" | "ArrowDown") {
                self.goal_x = None;
            }
            cx.stop_propagation();
            self.restart_blink(window, cx);
            self.after_change(cx);
            return;
        }
        if matches!(key.key.as_str(), "PageUp" | "PageDown") {
            self.on_page_key(&key, cx);
            return;
        }

        if matches!(key.key.as_str(), "ArrowUp" | "ArrowDown") {
            key.caret_on_first_line = self.is_caret_on_visual_edge(true);
            key.caret_on_last_line = self.is_caret_on_visual_edge(false);
        }

        // Keys the host may claim first (menu navigation, send, history, stash).
        if (matches!(
            key.key.as_str(),
            "Enter" | "Tab" | "Escape" | "ArrowUp" | "ArrowDown"
        ) || (command && matches!(key.key.as_str(), "s" | "Backspace")))
            && let Some(handler) = self.command_handler.clone()
            && handler(&mut self.state, &key, window, cx)
        {
            cx.stop_propagation();
            self.restart_blink(window, cx);
            self.after_change(cx);
            return;
        }

        let mut handled = true;
        let mut vertical = false;
        match key.key.as_str() {
            "Enter" => {
                if modifiers.platform || modifiers.control {
                    handled = false;
                } else {
                    self.state.enter();
                }
            }
            "Tab" => {
                if self.state.selection_in_one_code_block() {
                    self.state.indent_code_block(if key.shift {
                        crate::composer::code_block::IndentDirection::Out
                    } else {
                        crate::composer::code_block::IndentDirection::In
                    });
                } else if self.state.in_list_item() && self.state.is_rich() {
                    handled = self.state.indent_list_item(key.shift);
                } else {
                    handled = false;
                }
            }
            "Escape" => handled = false,
            "Backspace" => {
                if command {
                    let pos = self.state.selection().head;
                    self.state.delete_to(DocPos::new(pos.block, 0));
                } else if modifiers.alt {
                    let target = self.state.prev_word_pos(self.state.selection().head);
                    self.state.delete_to(target);
                } else {
                    self.state.delete_backward();
                }
            }
            "Delete" => {
                if modifiers.alt {
                    let target = self.state.next_word_pos(self.state.selection().head);
                    self.state.delete_to(target);
                } else {
                    self.state.delete_forward();
                }
            }
            "ArrowLeft" | "ArrowRight" => {
                let forward = key.key == "ArrowRight";
                let head = self.state.selection().head;
                if command {
                    let target = self.line_edge(head, forward);
                    self.state.move_caret(target, key.shift);
                } else if modifiers.alt {
                    let target = if forward {
                        self.state.next_word_pos(head)
                    } else {
                        self.state.prev_word_pos(head)
                    };
                    self.state.move_caret(target, key.shift);
                } else if !key.shift && !self.state.selection().is_empty() {
                    let selection = self.state.selection();
                    let edge = if forward {
                        selection.to()
                    } else {
                        selection.from()
                    };
                    self.state.move_caret(edge, false);
                } else {
                    let target = if forward {
                        self.state.next_cursor_pos(head)
                    } else {
                        self.state.prev_cursor_pos(head)
                    };
                    self.state.move_caret(target, key.shift);
                }
            }
            "ArrowUp" | "ArrowDown" => {
                vertical = true;
                let forward = key.key == "ArrowDown";
                if command {
                    let target = if forward {
                        self.doc_end()
                    } else {
                        DocPos::new(0, 0)
                    };
                    self.state.move_caret(target, key.shift);
                } else {
                    self.move_vertical(forward, key.shift);
                }
            }
            "Home" | "End" => {
                let head = self.state.selection().head;
                let target = self.line_edge(head, key.key == "End");
                self.state.move_caret(target, key.shift);
            }
            _ => {
                if command {
                    match key.key.as_str() {
                        "a" => self.state.select_all(),
                        "c" => self.copy(cx),
                        "x" => self.cut(cx),
                        "v" => {
                            self.paste(window, cx);
                        }
                        "z" => {
                            if key.shift {
                                self.state.redo();
                            } else {
                                self.state.undo();
                            }
                        }
                        "y" => {
                            self.state.redo();
                        }
                        "i" => {
                            self.state.toggle_mark(RichTextMark::Italic);
                        }
                        "e" => {
                            self.state.toggle_mark(RichTextMark::Code);
                        }
                        "s" if key.shift => {
                            self.state.toggle_mark(RichTextMark::Strike);
                        }
                        _ => handled = false,
                    }
                } else {
                    handled = false;
                }
            }
        }
        if handled {
            if !vertical {
                self.goal_x = None;
            }
            cx.stop_propagation();
            self.restart_blink(window, cx);
            self.after_change(cx);
        }
    }

    /// PageUp/PageDown (`EditorContent` `onKeyDown`): the editor keeps the key
    /// while it can scroll that way; past its edge the timeline takes it.
    fn on_page_key(&mut self, key: &CommandKey, cx: &mut Context<Self>) {
        let client_height = f64::from(self.scroll.bounds().size.height);
        let max_offset = f64::from(self.scroll.max_offset().y);
        let scroll_top = -f64::from(self.scroll.offset().y);
        let page_key = get_timeline_page_scroll_key(&PageScrollKeyEvent {
            alt_key: key.alt,
            ctrl_key: key.ctrl,
            meta_key: key.meta,
            shift_key: key.shift,
            default_prevented: false,
            is_composing: key.is_composing,
            key: &key.key,
            key_code: 0,
            client_height,
            scroll_height: client_height + max_offset,
            scroll_top,
        });
        match page_key {
            Some(page_key) => {
                cx.stop_propagation();
                cx.emit(PromptEditorEvent::PageScrollKeyDown(page_key));
            }
            None => {
                cx.emit(PromptEditorEvent::PageScrollRelease);
                // The browser scrolls the editor itself by a page.
                if !(key.alt || key.ctrl || key.meta || key.shift) && max_offset > 0. {
                    let direction = if key.key == "PageUp" { -1. } else { 1. };
                    let next = (scroll_top + direction * client_height).clamp(0., max_offset);
                    let mut offset = self.scroll.offset();
                    offset.y = px(-(next as f32));
                    self.scroll.set_offset(offset);
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }

    fn doc_end(&self) -> DocPos {
        let doc = self.state.doc();
        let last = doc.blocks.len() - 1;
        DocPos::new(last, doc.blocks[last].doc_len())
    }

    /// The start or end of the visual line holding `pos`.
    fn line_edge(&self, pos: DocPos, end: bool) -> DocPos {
        let probe = self.probes.borrow().get(pos.block).cloned().flatten();
        let (Some(probe), Some(flow)) = (probe, self.block_flow_for(pos.block)) else {
            return DocPos::new(
                pos.block,
                if end {
                    self.state.doc().blocks[pos.block].doc_len()
                } else {
                    0
                },
            );
        };
        let byte = flow.map.doc_to_byte(pos.offset, CaretSide::Plain);
        let line = probe.line_for_index(byte);
        let range = probe.line_visible_range(line);
        let target = if end { range.end } else { range.start };
        DocPos::new(pos.block, flow.map.byte_to_doc(target))
    }

    fn move_vertical(&mut self, forward: bool, extend: bool) {
        let head = self.state.selection().head;
        let probes = self.probes.borrow().clone();
        let (Some(Some(probe)), Some(flow)) =
            (probes.get(head.block), self.block_flow_for(head.block))
        else {
            return;
        };
        let byte = flow.map.doc_to_byte(head.offset, CaretSide::Plain);
        let line = probe.line_for_index(byte);
        let x = self
            .goal_x
            .or_else(|| probe.caret_bounds(byte, px(1.)).map(|b| b.origin.x))
            .unwrap_or_default();
        self.goal_x = Some(x);
        let lines = probe.line_count();
        let target_line = if forward {
            line + 1
        } else {
            line.wrapping_sub(1)
        };
        if target_line < lines {
            let target_byte = probe.index_for_x_on_line(target_line, x);
            self.state.move_caret(
                DocPos::new(head.block, flow.map.byte_to_doc(target_byte)),
                extend,
            );
            return;
        }
        // Into the neighbouring text block.
        let blocks = self.state.doc().blocks.len();
        let neighbours: Vec<usize> = if forward {
            (head.block + 1..blocks).collect()
        } else {
            (0..head.block).rev().collect()
        };
        for index in neighbours {
            if let (Some(Some(other)), Some(other_flow)) =
                (probes.get(index), self.block_flow_for(index))
            {
                let line = if forward { 0 } else { other.line_count() - 1 };
                let target_byte = other.index_for_x_on_line(line, x);
                self.state.move_caret(
                    DocPos::new(index, other_flow.map.byte_to_doc(target_byte)),
                    extend,
                );
                return;
            }
        }
        // The first or last line: the caret goes to the document edge.
        let edge = if forward {
            self.doc_end()
        } else {
            DocPos::new(0, 0)
        };
        self.state.move_caret(edge, extend);
    }

    /// True when a collapsed caret sits on the first (`edge_start`) or last
    /// visual line, counting soft wraps (`isCaretOnVisualEdge`).
    pub fn is_caret_on_visual_edge(&mut self, edge_start: bool) -> bool {
        let snapshot = self.state.snapshot();
        if snapshot.value.is_empty() {
            return true;
        }
        let units: Vec<u16> = snapshot.value.encode_utf16().collect();
        let caret = snapshot.expanded_cursor.min(units.len());
        let has_newline = if edge_start {
            units[..caret].contains(&0x0a)
        } else {
            units[caret..].contains(&0x0a)
        };
        if has_newline {
            return false;
        }
        let head = self.state.selection().head;
        let probes = self.probes.borrow().clone();
        let (Some(Some(probe)), Some(flow)) =
            (probes.get(head.block), self.block_flow_for(head.block))
        else {
            return false;
        };
        let byte = flow.map.doc_to_byte(head.offset, CaretSide::Plain);
        let line = probe.line_for_index(byte);
        if edge_start {
            line == 0
        } else {
            line + 1 >= probe.line_count()
        }
    }

    // ── Clipboard ──────────────────────────────────────────────────────────

    /// `handleCopyCut`: the selection's markdown, plus the structured payload
    /// of the context records it references.
    fn clipboard_item(&self, text: String) -> ClipboardItem {
        let ids = if self.state.is_literal() {
            Vec::new()
        } else {
            crate::composer::rich_doc::collect_inline_context_ids(&text)
        };
        let fragment = (!ids.is_empty())
            .then_some(self.build_context_fragment.as_ref())
            .flatten()
            .and_then(|build| build(&ids));
        match fragment {
            Some(fragment) => ClipboardItem::new_string_with_metadata(text, fragment),
            None => ClipboardItem::new_string(text),
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.state.selected_markdown();
        if !text.is_empty() {
            cx.write_to_clipboard(self.clipboard_item(text));
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if self.config.disabled {
            return;
        }
        if let Some(text) = self.state.cut_selection() {
            cx.write_to_clipboard(self.clipboard_item(text));
        }
    }

    /// Paste the clipboard: the host may claim files/images; text goes through
    /// the markdown-aware paste.
    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        if let Some(handler) = self.paste_handler.clone()
            && handler(&mut self.state, &item, window, cx)
        {
            return true;
        }
        let Some((text, metadata)) = item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::String(string) => {
                Some((string.text().to_owned(), string.metadata.clone()))
            }
            _ => None,
        }) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        // A fence takes the clipboard verbatim; elsewhere a structured paste
        // brings its context records and relinks to them.
        let text = match &self.import_context_fragment {
            Some(import) if !self.state.is_literal() && !self.state.in_code_block() => {
                crate::composer::inline_token_paste::import_pasted_composer_text(
                    crate::composer::inline_token_paste::PastedClipboard {
                        text: &text,
                        fragment: metadata.as_deref(),
                        html: None,
                    },
                    Some(&**import),
                )
            }
            _ => text,
        };
        let label_for = self.skill_label();
        self.state.paste(&text, &*label_for, self.path_query_active)
    }

    /// Insert text at the end of the draft (a drop, a redirected paste).
    pub fn insert_text_at_end(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let end = self.doc_end();
        self.state.set_caret(end);
        let label_for = self.skill_label();
        let inserted = self.state.paste(text, &*label_for, false);
        if inserted {
            window.focus(&self.focus, cx);
            self.after_change(cx);
        }
        inserted
    }

    // ── Pointer ────────────────────────────────────────────────────────────

    fn pos_for_point(&self, position: Point<Pixels>) -> Option<DocPos> {
        let probes = self.probes.borrow();
        let mut best: Option<(usize, Pixels)> = None;
        for (index, probe) in probes.iter().enumerate() {
            let Some(probe) = probe else { continue };
            let bounds = probe.bounds();
            let distance = if position.y < bounds.top() {
                bounds.top() - position.y
            } else if position.y > bounds.bottom() {
                position.y - bounds.bottom()
            } else {
                px(0.)
            };
            if best.is_none_or(|(_, d)| distance < d) {
                best = Some((index, distance));
            }
        }
        let (index, _) = best?;
        let byte = probes[index].as_ref()?.index_for_point(position)?;
        let flow = self.block_flow_for(index)?;
        Some(DocPos::new(index, flow.map.byte_to_doc(byte)))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config.disabled {
            return;
        }
        window.focus(&self.focus, cx);
        // Chips are buttons: a mention opens its file, a citation its comment.
        if event.click_count == 1
            && !event.modifiers.shift
            && let Some((pos, atom)) = self.chip_at(event.position)
        {
            match atom {
                Atom::Mention { path, .. } => {
                    self.state.set_caret(DocPos::new(pos.block, pos.offset + 1));
                    cx.emit(PromptEditorEvent::OpenMention(path));
                    self.after_change(cx);
                    return;
                }
                Atom::Citation { cite_key, .. } => {
                    self.state.set_caret(DocPos::new(pos.block, pos.offset + 1));
                    self.after_change(cx);
                    self.open_citation_comment(&cite_key, window, cx);
                    return;
                }
                _ => {}
            }
        }
        let Some(pos) = self.pos_for_point(event.position) else {
            let end = self.doc_end();
            self.state.move_caret(end, event.modifiers.shift);
            self.after_change(cx);
            return;
        };
        let now = std::time::Instant::now();
        let repeated = self.last_click.is_some_and(|(at, p)| {
            now.duration_since(at) < Duration::from_millis(500)
                && (p.x - event.position.x).abs() < px(4.)
                && (p.y - event.position.y).abs() < px(4.)
        });
        self.click_count = if repeated { self.click_count + 1 } else { 1 };
        self.last_click = Some((now, event.position));
        if event.modifiers.shift {
            self.state.move_caret(pos, true);
        } else {
            match self.click_count {
                2 => {
                    let end = self.state.next_word_pos(pos);
                    let start = self.state.prev_word_pos(end);
                    self.state.set_selection(Selection {
                        anchor: start.min(pos),
                        head: end.max(pos),
                    });
                }
                3.. => {
                    let len = self.state.doc().blocks[pos.block].doc_len();
                    self.state.set_selection(Selection {
                        anchor: DocPos::new(pos.block, 0),
                        head: DocPos::new(pos.block, len),
                    });
                }
                _ => self.state.move_caret(pos, false),
            }
        }
        self.selecting = true;
        self.goal_x = None;
        self.restart_blink(window, cx);
        self.after_change(cx);
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting || !event.dragging() {
            return;
        }
        if let Some(pos) = self.pos_for_point(event.position) {
            let anchor = self.state.selection().anchor;
            self.state.set_selection(Selection { anchor, head: pos });
            self.after_change(cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    // ── Rendering ──────────────────────────────────────────────────────────

    /// Highlight and caret for one block from the selection.
    fn decorations(
        &self,
        index: usize,
        flow: &BlockFlow,
        focused: bool,
        theme: &AppTheme,
    ) -> (Vec<FlowHighlight>, Option<FlowCaret>) {
        let selection = self.state.selection();
        let (from, to) = (selection.from(), selection.to());
        let side = self.caret_side();
        let mut highlights = Vec::new();
        if !selection.is_empty() && index >= from.block && index <= to.block {
            let start_doc = if index == from.block { from.offset } else { 0 };
            let end_doc = if index == to.block {
                to.offset
            } else {
                flow.map.doc_len
            };
            let range = flow.map.doc_to_byte(start_doc, CaretSide::InsideAfter)
                ..flow.map.doc_to_byte(end_doc, CaretSide::InsideBefore);
            highlights.push(FlowHighlight {
                range,
                color: with_alpha(theme.info, 0.30).into(),
                through_line_end: index < to.block,
            });
        }
        let caret =
            (focused && selection.is_empty() && self.blink_on && selection.head.block == index)
                .then(|| FlowCaret {
                    index: flow.map.doc_to_byte(selection.head.offset, side),
                    color: theme.foreground,
                    width: px(1.),
                });
        (highlights, caret)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_block(
        &self,
        index: usize,
        block: &Block,
        depth: usize,
        env: &FlowEnv<'_>,
        focused: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<Rc<BlockFlow>>, Option<FlowProbe>) {
        let theme = env.theme;
        let selection = self.state.selection();
        let reveal = MarkerReveal {
            caret: focused.then_some(selection.head),
            selection: Some((selection.from(), selection.to())),
            block: index,
            current_marks: self.state.current_marks(),
            enabled: self.state.is_rich() && focused,
            _marker: std::marker::PhantomData,
        };
        let base = BlockTypography {
            size: env.size,
            line_height: LEADING_RELAXED,
            weight: FontWeight::NORMAL,
            color: theme.foreground,
        };
        let rem = env.rem;
        match block {
            Block::Code(code) => {
                let flow = Rc::new(code_flow(env, code));
                let probe = FlowProbe::default();
                let (highlights, caret) = self.decorations(index, &flow, focused, theme);
                let element = self.code_frame(
                    index,
                    code,
                    &flow,
                    probe.clone(),
                    highlights,
                    caret,
                    env,
                    cx,
                );
                (element, Some(flow), Some(probe))
            }
            Block::Rule { .. } => {
                let element = div().h(px(1.)).w_full().bg(theme.border).into_any_element();
                (element, None, None)
            }
            other => {
                let inline = other.inline().expect("inline block");
                let typography = match other {
                    Block::Heading { level, .. } => {
                        let size = match level {
                            1 => rem * 1.25,
                            2 => rem * 1.125,
                            3 => rem,
                            _ => rem * 0.875,
                        };
                        BlockTypography {
                            size,
                            line_height: 1.3,
                            weight: FontWeight::SEMIBOLD,
                            color: if *level == 6 {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            },
                        }
                    }
                    _ => base,
                };
                let flow = Rc::new(build_inline_flow(env, inline, typography, &reveal));
                let probe = FlowProbe::default();
                let (highlights, caret) = self.decorations(index, &flow, focused, theme);
                let text = InlineFlow::new(
                    ElementId::NamedInteger("composer-block".into(), index as u64),
                    flow.flow.clone(),
                    WhiteSpace::PreWrap,
                )
                .strut(env.family.clone(), typography.size, typography.line_height)
                .selectable(false)
                .highlights(highlights)
                .caret(caret)
                .probe(probe.clone());
                let element = match other {
                    Block::Item { prefix, .. } => {
                        self.item_row(index, prefix, depth, text, env, cx)
                    }
                    Block::Quote { .. } => div()
                        .w_full()
                        .border_l_2()
                        .border_color(theme.border)
                        .pl(rems(0.8))
                        .child(text)
                        .into_any_element(),
                    _ => div().w_full().child(text).into_any_element(),
                };
                (element, Some(flow), Some(probe))
            }
        }
    }

    fn item_row(
        &self,
        index: usize,
        prefix: &ItemPrefix,
        depth: usize,
        text: InlineFlow,
        env: &FlowEnv<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = env.theme;
        match prefix {
            ItemPrefix::Task { checked, .. } => {
                let checked = *checked;
                let line = env.size * LEADING_RELAXED;
                let box_size = rems(0.95);
                div()
                    .flex()
                    .w_full()
                    .items_start()
                    .gap(rems(0.5))
                    .pl(rems(1.375 * depth as f32))
                    .child(
                        div()
                            .id(ElementId::NamedInteger(
                                "composer-task".into(),
                                index as u64,
                            ))
                            .flex()
                            .flex_none()
                            .h(line)
                            .items_center()
                            .cursor(CursorStyle::PointingHand)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.state.toggle_task(index);
                                    window.focus(&this.focus, cx);
                                    this.after_change(cx);
                                }),
                            )
                            .child(
                                div()
                                    .size(box_size)
                                    .rounded(px(3.))
                                    .border_1()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(checked, |this| {
                                        this.bg(theme.info).border_color(theme.info)
                                    })
                                    .when(!checked, |this| {
                                        this.bg(theme.background).border_color(theme.input)
                                    })
                                    .when(checked, |this| {
                                        this.child(
                                            Icon::new(IconName::Check)
                                                .size(rems(0.625))
                                                .color(gpui_kit::white()),
                                        )
                                    }),
                            ),
                    )
                    .child(div().flex_1().min_w_0().child(text))
                    .into_any_element()
            }
            other => {
                let marker: SharedString = match other {
                    ItemPrefix::Ordered { marker, .. } => marker.clone().into(),
                    _ => match depth {
                        0 => "•",
                        1 => "◦",
                        _ => "▪",
                    }
                    .into(),
                };
                let gutter = rems(1.25 * (depth as f32 + 1.));
                div()
                    .flex()
                    .w_full()
                    .items_start()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .w(gutter)
                            .justify_end()
                            .pr(env.size * 0.45)
                            .font_family(env.family.clone())
                            .text_size(env.size)
                            .line_height(env.size * LEADING_RELAXED)
                            .text_color(theme.foreground)
                            .child(marker),
                    )
                    .child(div().flex_1().min_w_0().child(text))
                    .into_any_element()
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn code_frame(
        &self,
        index: usize,
        code: &CodeBlock,
        flow: &Rc<BlockFlow>,
        probe: FlowProbe,
        highlights: Vec<FlowHighlight>,
        caret: Option<FlowCaret>,
        env: &FlowEnv<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = env.theme;
        let dark = theme.is_dark();
        let language = language_of_info_string(&code.language).to_owned();
        let label = code_language_label(&language);
        let (bg, border) = if dark {
            (
                with_alpha(theme.input, 0.32).into(),
                gpui_kit::transparent_black(),
            )
        } else {
            (theme.secondary, with_alpha(theme.border, 0.7).into())
        };
        let header_color = with_alpha(theme.foreground, 0.72);
        let code_size = px(theme.fonts.size_code);
        let text = InlineFlow::new(
            ElementId::NamedInteger("composer-code".into(), index as u64),
            flow.flow.clone(),
            WhiteSpace::PreWrap,
        )
        .strut(env.mono.clone(), code_size, 1.375)
        .selectable(false)
        .highlights(highlights)
        .caret(caret)
        .probe(probe);
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(border)
            .bg(bg)
            .child(
                div()
                    .flex()
                    .items_center()
                    .pt(env.rem * 0.375)
                    .pr(env.rem * 0.375)
                    .pl(env.rem * 0.75)
                    .child(
                        div()
                            .id(ElementId::NamedInteger(
                                "composer-code-language".into(),
                                index as u64,
                            ))
                            .flex()
                            .items_center()
                            // ComposerCodeBlockLanguagePicker: Button `ghost-muted`,
                            // size `xs` (`gap-1`, `sm:text-xs`, `font-medium`).
                            .gap(rems(0.25))
                            .h(rems(1.5))
                            .px(rems(0.5))
                            .ml(rems(-0.5))
                            .rounded(px(6.))
                            .cursor(CursorStyle::PointingHand)
                            .text_color(theme.muted_foreground)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(TEXT_XS.size)
                            .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(PromptEditorEvent::OpenCodeLanguagePicker {
                                        block: index,
                                        anchor: event.position,
                                    });
                                }),
                            )
                            .child(
                                Icon::new(IconName::FileText)
                                    .size(rems(0.875))
                                    .color(header_color.into()),
                            )
                            .child(div().max_w(rems(10.)).truncate().child(label))
                            .child(
                                // `opacity-60` on the label color.
                                Icon::new(IconName::ChevronDown)
                                    .size(rems(0.875))
                                    .color(with_alpha(theme.muted_foreground, 0.6).into()),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .px(rems(0.75))
                    .pt(rems(0.25))
                    .pb(rems(0.5))
                    .child(text),
            )
            .into_any_element()
    }

    /// The vertical gap CSS margin collapsing leaves between two blocks.
    fn gap_between(
        prev: Option<(&Block, usize)>,
        next: (&Block, usize),
        rem: Pixels,
        first: bool,
    ) -> Pixels {
        let list = rem * 0.65;
        let before = |block: &Block| match block {
            Block::Heading { .. } => {
                if first {
                    px(0.)
                } else {
                    rem * 1.25
                }
            }
            Block::Item { prefix, .. } => {
                if matches!(prefix, ItemPrefix::Task { .. }) {
                    px(0.)
                } else {
                    list
                }
            }
            Block::Quote { .. } | Block::Rule { .. } | Block::Code(_) => list,
            Block::Paragraph(_) => px(0.),
        };
        let after = |block: &Block| match block {
            Block::Heading { .. } => rem * 0.5,
            Block::Item { prefix, .. } => {
                if matches!(prefix, ItemPrefix::Task { .. }) {
                    px(0.)
                } else {
                    list
                }
            }
            Block::Quote { .. } | Block::Rule { .. } | Block::Code(_) => list,
            Block::Paragraph(_) => px(0.),
        };
        let Some((prev_block, prev_depth)) = prev else {
            return before(next.0);
        };
        // Items of one list run touch; leaving or entering a nested list opens
        // the nested list's margin.
        if let (Block::Item { prefix: a, .. }, Block::Item { prefix: b, .. }) = (prev_block, next.0)
        {
            let task = |p: &ItemPrefix| matches!(p, ItemPrefix::Task { .. });
            return if next.1 > prev_depth {
                if task(b) { px(0.) } else { list }
            } else if next.1 < prev_depth {
                if task(a) { px(0.) } else { list }
            } else if task(a) == task(b) {
                px(0.)
            } else {
                list
            };
        }
        if let (Block::Quote { prefix: a, .. }, Block::Quote { prefix: b, .. }) =
            (prev_block, next.0)
            && a == b
        {
            return px(0.);
        }
        after(prev_block).max(before(next.0))
    }
}

/// Highlighted code flow for a fence body.
fn code_flow(env: &FlowEnv<'_>, code: &CodeBlock) -> BlockFlow {
    let theme = env.theme;
    let language = language_of_info_string(&code.language);
    let highlighted = highlight_code(&code.text, language, theme.is_dark());
    let code_size = px(theme.fonts.size_code);
    let base = FlowSpanStyle {
        family: env.mono.clone(),
        weight: FontWeight::NORMAL,
        italic: false,
        size: code_size,
        line_height: 1.375,
        color: highlighted.foreground,
        strikethrough: false,
        chrome: None,
        link: None,
        background: None,
        atomic: false,
    };
    let text = code.text.as_str();
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    for (range, color) in &highlighted.tokens {
        if range.start < cursor || range.end > text.len() {
            continue;
        }
        if range.start > cursor {
            spans.push(FlowSpan {
                len: range.start - cursor,
                style: base.clone(),
            });
        }
        spans.push(FlowSpan {
            len: range.end - range.start,
            style: FlowSpanStyle {
                color: *color,
                ..base.clone()
            },
        });
        cursor = range.end;
    }
    if cursor < text.len() {
        spans.push(FlowSpan {
            len: text.len() - cursor,
            style: base.clone(),
        });
    }
    let doc_len = crate::composer::js_string::utf16_len(text);
    BlockFlow {
        flow: crate::components::chat::markdown::inline_flow::FlowText {
            text: text.to_owned().into(),
            spans,
            links: Vec::new(),
        },
        map: OffsetMap {
            segs: vec![Seg {
                kind: SegKind::Text,
                doc_start: 0,
                doc_len,
                byte_start: 0,
                byte_len: text.len(),
                text: text.to_owned(),
            }],
            doc_len,
            byte_len: text.len(),
        },
    }
}

/// `toLocaleString()` for a count (en-US grouping).
fn format_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Keys of the citation comment field (`AssistantCitationCommentEditor`):
/// Enter saves, Cmd/Ctrl+Enter saves and sends, Shift+Enter is a new line,
/// Escape cancels.
fn comment_field_key(
    parent: &WeakEntity<PromptEditor>,
    state: &mut EditorState,
    key: &CommandKey,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    if key.is_composing {
        return false;
    }
    let parent = parent.clone();
    match key.key.as_str() {
        "Enter" if !key.shift => {
            let comment = state.value();
            let send = key.meta || key.ctrl;
            window.defer(cx, move |window, cx| {
                parent
                    .update(cx, |this, cx| {
                        this.submit_citation_comment(&comment, send, window, cx)
                    })
                    .ok();
            });
            true
        }
        "Escape" => {
            window.defer(cx, move |window, cx| {
                parent
                    .update(cx, |this, cx| this.cancel_citation_comment(window, cx))
                    .ok();
            });
            true
        }
        _ => false,
    }
}

fn skill_label_fn(skills: &[ProviderSkill]) -> Rc<dyn Fn(&str) -> SkillMeta> {
    let skills = skills.to_vec();
    Rc::new(move |name: &str| {
        let normalized = name.strip_prefix('$').unwrap_or(name);
        match skills.iter().find(|skill| skill.name == normalized) {
            None => SkillMeta {
                label: crate::composer::skills::format_provider_skill_display_name(
                    normalized, None,
                ),
                description: None,
            },
            Some(skill) => SkillMeta {
                label: crate::composer::skills::format_provider_skill_display_name(
                    &skill.name,
                    skill.display_name.as_deref(),
                ),
                description: skill
                    .short_description
                    .as_deref()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .or(skill
                        .description
                        .as_deref()
                        .map(str::trim)
                        .filter(|d| !d.is_empty()))
                    .map(str::to_owned),
            },
        }
    })
}

impl Focusable for PromptEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PromptEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let focused = self.focus.is_focused(window);
        if focused != self.was_focused {
            self.was_focused = focused;
            if focused {
                self.restart_blink(window, cx);
            } else {
                // `onBlur={onPageScrollRelease}`.
                cx.emit(PromptEditorEvent::PageScrollRelease);
            }
        }
        // Anchored to the chip's box from the last painted frame; until one
        // exists, paint again so the popover appears.
        let comment_popover = self.citation_comment_popover(cx);
        if comment_popover.is_some() || self.comment_editor.is_none() {
            self.comment_anchor_retries = 0;
        } else if self.comment_anchor_retries < 3 {
            self.comment_anchor_retries += 1;
            cx.on_next_frame(window, |_, _, cx| cx.notify());
        }
        let env = self.env(&theme, window);
        let doc = self.state.doc().clone();
        let depths = list_depths(&doc);
        let mut flows: Vec<Option<Rc<BlockFlow>>> = Vec::with_capacity(doc.blocks.len());
        let mut probes: Vec<Option<FlowProbe>> = Vec::with_capacity(doc.blocks.len());
        let mut rows: Vec<AnyElement> = Vec::with_capacity(doc.blocks.len());
        let mut previous: Option<usize> = None;
        for (index, block) in doc.blocks.iter().enumerate() {
            let (element, flow, probe) =
                self.render_block(index, block, depths[index], &env, focused, window, cx);
            let gap = Self::gap_between(
                previous.map(|p| (&doc.blocks[p], depths[p])),
                (block, depths[index]),
                env.rem,
                index == 0,
            );
            rows.push(div().w_full().mt(gap).child(element).into_any_element());
            flows.push(flow);
            probes.push(probe);
            previous = Some(index);
        }
        // The last block's own bottom margin.
        let bottom = doc.blocks.last().map_or(px(0.), |last| match last {
            Block::Item { prefix, .. } if !matches!(prefix, ItemPrefix::Task { .. }) => {
                env.rem * 0.65
            }
            Block::Quote { .. } | Block::Rule { .. } | Block::Code(_) => env.rem * 0.65,
            _ => px(0.),
        });
        *self.flows.borrow_mut() = flows;
        *self.probes.borrow_mut() = probes;

        if self.scroll_caret_into_view {
            self.scroll_caret_into_view = false;
            cx.on_next_frame(window, |this, _window, cx| this.scroll_to_caret(cx));
        }

        let entity = cx.entity();
        let focus = self.focus.clone();
        // `isEmpty && contextRecords.size === 0 && placeholder`.
        let empty = self.state.is_empty()
            && self.context_record_count == 0
            && !self.config.placeholder.is_empty();
        let size = px(self.config.font_size.unwrap_or(theme.fonts.size_prompt));
        let line_height = size * LEADING_RELAXED;
        div()
            .id("composer-editor")
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &ToggleBold, window, cx| {
                if !this.state.is_rich() {
                    // Plain mode has no marks: the chord keeps its app meaning.
                    cx.propagate();
                    return;
                }
                // The rich composer claims the chord even where bold cannot
                // apply (a fence, a read-only editor).
                if !this.config.disabled && this.state.toggle_mark(RichTextMark::Bold) {
                    this.restart_blink(window, cx);
                    this.after_change(cx);
                }
            }))
            .track_focus(&self.focus)
            .relative()
            .w_full()
            // `-m-1 p-1`: the padding keeps the caret off the clip edge.
            .m(px(-4.))
            .overflow_hidden()
            .cursor(CursorStyle::IBeam)
            .font_family(theme.fonts.composer.clone())
            .text_size(size)
            .line_height(line_height)
            .text_color(theme.foreground)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key_down(event, window, cx)
            }))
            .on_key_up(cx.listener(|_, event: &KeyUpEvent, _, cx| {
                cx.emit(PromptEditorEvent::PageScrollKeyUp(normalize_key(
                    &event.keystroke.key,
                )));
            }))
            .on_modifiers_changed(cx.listener(|_, _: &ModifiersChangedEvent, _, cx| {
                cx.emit(PromptEditorEvent::PageScrollRelease);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(event, window, cx)
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.on_mouse_move(event, window, cx)
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    this.on_mouse_up(event, window, cx)
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    this.on_mouse_up(event, window, cx)
                }),
            )
            .child(
                div()
                    .id("composer-editor-scroll")
                    .w_full()
                    .min_h(rems(self.config.min_height))
                    .max_h(rems(self.config.max_height))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p(px(4.))
                    .child(div().flex().flex_col().w_full().children(rows).pb(bottom)),
            )
            .children(comment_popover)
            .when(empty, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(4.))
                        .left(px(4.))
                        .right(px(4.))
                        .text_color(with_alpha(theme.placeholder, 0.75))
                        .child(self.config.placeholder.clone()),
                )
            })
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

impl PromptEditor {
    fn env<'a>(&self, theme: &'a AppTheme, window: &Window) -> FlowEnv<'a> {
        FlowEnv {
            theme,
            rem: window.rem_size(),
            size: px(self.config.font_size.unwrap_or(theme.fonts.size_prompt)),
            family: theme.fonts.composer.clone().into(),
            mono: theme.fonts.mono.clone().into(),
            sans: theme.fonts.sans.clone().into(),
        }
    }

    fn scroll_to_caret(&mut self, cx: &mut Context<Self>) {
        let head = self.state.selection().head;
        let probe = self.probes.borrow().get(head.block).cloned().flatten();
        let (Some(probe), Some(flow)) = (probe, self.block_flow_for(head.block)) else {
            return;
        };
        let byte = flow.map.doc_to_byte(head.offset, self.caret_side());
        let Some(caret) = probe.caret_bounds(byte, px(1.)) else {
            return;
        };
        let viewport = self.scroll.bounds();
        let mut offset = self.scroll.offset();
        let pad = px(4.);
        if caret.top() < viewport.top() + pad {
            offset.y += viewport.top() + pad - caret.top();
        } else if caret.bottom() > viewport.bottom() - pad {
            offset.y -= caret.bottom() - viewport.bottom() + pad;
        } else {
            return;
        }
        offset.y = offset.y.min(px(0.));
        self.scroll.set_offset(point(offset.x, offset.y));
        cx.notify();
    }
}

// ── Platform text input ────────────────────────────────────────────────────

fn utf16_range_to_flat(range: &Range<usize>) -> (usize, usize) {
    (range.start, range.end)
}

impl EntityInputHandler for PromptEditor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.state.flat_text();
        let units: Vec<u16> = text.encode_utf16().collect();
        let start = range.start.min(units.len());
        let end = range.end.clamp(start, units.len());
        adjusted_range.replace(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let (anchor, head) = self.state.selection_flat();
        Some(UTF16Selection {
            range: anchor.min(head)..anchor.max(head),
            reversed: head < anchor,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.clone()
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config.disabled {
            return;
        }
        let (from, to) = match range.as_ref().or(self.marked.as_ref()) {
            Some(range) => utf16_range_to_flat(range),
            None => {
                let (anchor, head) = self.state.selection_flat();
                (anchor.min(head), anchor.max(head))
            }
        };
        self.marked = None;
        self.state.replace_flat_range(from, to, text);
        self.goal_x = None;
        self.restart_blink(window, cx);
        self.after_change(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config.disabled {
            return;
        }
        let (from, to) = match range.as_ref().or(self.marked.as_ref()) {
            Some(range) => utf16_range_to_flat(range),
            None => {
                let (anchor, head) = self.state.selection_flat();
                (anchor.min(head), anchor.max(head))
            }
        };
        self.state.replace_flat_range(from, to, new_text);
        let units = crate::composer::js_string::utf16_len(new_text);
        self.marked = (units > 0).then_some(from..from + units);
        if let Some(selected) = new_selected_range {
            self.state
                .set_selection_flat(from + selected.start, from + selected.end);
        }
        self.restart_blink(window, cx);
        self.after_change(cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let _ = range;
        let head = self.state.selection().head;
        let probe = self.probes.borrow().get(head.block).cloned().flatten()?;
        let flow = self.block_flow_for(head.block)?;
        let byte = flow.map.doc_to_byte(head.offset, CaretSide::Plain);
        probe.caret_bounds(byte, px(1.)).or(Some(element_bounds))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let pos = self.pos_for_point(position)?;
        let map = self.state.map();
        Some(crate::composer::rich_doc::pos_to_flat(map, pos))
    }
}

#[allow(dead_code)]
fn _assert_marks(_: Marks) {}

#[allow(dead_code)]
fn _entity_type(_: Entity<PromptEditor>) {}
