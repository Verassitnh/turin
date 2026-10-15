//! The rich prompt editor view: port of ComposerPromptEditorTiptap.tsx's
//! surface, built on [`EditorState`].
//!
//! The view paints each block with [`InlineFlow`] (the chat markdown line
//! engine, extended with highlights/caret/probe hooks), forwards keyboard,
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
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, FontWeight,
    Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, UTF16Selection, Window, canvas, div, point, px, rems,
};

use crate::components::chat::markdown::code_highlight::highlight_code;
use crate::components::chat::markdown::inline_flow::{
    FlowCaret, FlowHighlight, FlowProbe, FlowSpan, FlowSpanStyle, FlowText, InlineFlow, WhiteSpace,
};
use crate::composer::catalog::ProviderSkill;
use crate::composer::editor_state::{EditorSnapshot, EditorState, Selection};
use crate::composer::rich_doc::{Block, CodeBlock, DocPos, ItemPrefix, Marks, SkillMeta, list_depths};
use crate::composer::rich_text::RichTextMark;
use crate::composer::views::block_flow::{
    BlockFlow, BlockTypography, CaretSide, FlowEnv, MarkerReveal, build_inline_flow, plain_flow,
};
use crate::composer::code_languages::{code_language_label, language_of_info_string};
use crate::theme::color::{with_alpha};
use crate::theme::tokens::{LEADING_RELAXED, RADIUS_LG};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName};

/// What the host is told.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptEditorEvent {
    /// The document or the caret changed.
    Changed(EditorSnapshot),
    /// A range selection appeared or moved (`onVisibleSelectionChange`).
    VisibleSelectionChanged,
    Focused,
    Blurred,
    /// The language chip of a code block was clicked.
    OpenCodeLanguagePicker { block: usize },
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

pub type CommandKeyHandler = Rc<dyn Fn(&mut EditorState, &CommandKey, &mut Window, &mut App) -> bool>;
pub type PasteHandler = Rc<dyn Fn(&mut EditorState, &ClipboardItem, &mut Window, &mut App) -> bool>;

#[derive(Clone)]
pub struct EditorConfig {
    pub placeholder: SharedString,
    pub disabled: bool,
    pub rich_text: bool,
    pub literal_text: bool,
    /// Resting layout: a single clamped line.
    pub resting: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            placeholder: SharedString::default(),
            disabled: false,
            rich_text: true,
            literal_text: false,
            resting: false,
        }
    }
}

pub struct PromptEditor {
    state: EditorState,
    focus: FocusHandle,
    scroll: ScrollHandle,
    config: EditorConfig,
    skills: Vec<ProviderSkill>,
    probes: Rc<RefCell<Vec<Option<FlowProbe>>>>,
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
}

impl PromptEditor {
    pub fn new(value: &str, config: EditorConfig, skills: Vec<ProviderSkill>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _ = window;
        let label_for = skill_label_fn(&skills);
        let state = EditorState::from_value(value, &*label_for, config.rich_text, config.literal_text);
        let mut this = Self {
            state,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            config,
            skills,
            probes: Rc::default(),
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

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
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

    pub fn set_config(&mut self, config: EditorConfig, cx: &mut Context<Self>) {
        if config.rich_text != self.config.rich_text || config.literal_text != self.config.literal_text {
            // Extensions are creation-time upstream: a mode flip rebuilds the
            // editor from the same controlled value.
            let snapshot = self.state.snapshot();
            let label_for = skill_label_fn(&self.skills);
            self.state = EditorState::from_value(&snapshot.value, &*label_for, config.rich_text, config.literal_text);
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

    /// Controlled value/cursor from the store (history recall, chip
    /// insertion…). A no-op when both already match.
    pub fn set_value(&mut self, value: &str, cursor: usize, cx: &mut Context<Self>) {
        let snapshot = self.state.snapshot();
        if snapshot.value == value && snapshot.cursor == cursor {
            return;
        }
        let label_for = skill_label_fn(&self.skills);
        self.state.set_value(value, Some(cursor), &*label_for);
        self.after_change(cx);
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

    /// Apply an edit made to the state outside a key (the host's list
    /// continuation, a stash restore…).
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
                    cx.background_executor().timer(Duration::from_millis(530)).await;
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
            let visible = crate::composer::selection::did_composer_selection_change_visibly(
                previous.as_ref().map_or(
                    crate::composer::selection::ComposerSelectionRange { start: 0, end: 0 },
                    |p| crate::composer::selection::ComposerSelectionRange { start: p.selection_start, end: p.selection_end },
                ),
                Some(crate::composer::selection::ComposerSelectionRange {
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

    fn env<'a>(&self, theme: &'a AppTheme, window: &Window) -> FlowEnv<'a> {
        FlowEnv {
            theme,
            rem: window.rem_size(),
            size: px(theme.fonts.size_prompt),
            family: theme.fonts.composer.clone().into(),
            mono: theme.fonts.mono.clone().into(),
            sans: theme.fonts.sans.clone().into(),
        }
    }

    // ── Keys ───────────────────────────────────────────────────────────────

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.config.disabled {
            return;
        }
        let key = CommandKey::from_event(event);
        let modifiers = event.keystroke.modifiers;
        let command = modifiers.platform;

        // Keys the host may claim first (menu navigation, send, history).
        if matches!(key.key.as_str(), "Enter" | "Tab" | "Escape" | "ArrowUp" | "ArrowDown")
            && let Some(handler) = self.command_handler.clone()
            && handler(&mut self.state, &key, window, cx)
        {
            cx.stop_propagation();
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
                    if !self.state.indent_list_item(key.shift) {
                        handled = false;
                    }
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
                let target = if command {
                    self.line_edge(head, forward)
                } else if modifiers.alt {
                    if forward { self.state.next_word_pos(head) } else { self.state.prev_word_pos(head) }
                } else if !key.shift && !self.state.selection().is_empty() {
                    let selection = self.state.selection();
                    let edge = if forward { selection.to() } else { selection.from() };
                    self.state.move_caret(edge, false);
                    self.goal_x = None;
                    cx.stop_propagation();
                    self.after_change(cx);
                    return;
                } else if !key.shift && self.state.step_caret_across_styled_edge(if forward { 1 } else { -1 }) {
                    self.goal_x = None;
                    cx.stop_propagation();
                    self.after_change(cx);
                    return;
                } else if forward {
                    self.state.next_cursor_pos(head)
                } else {
                    self.state.prev_cursor_pos(head)
                };
                self.state.move_caret(target, key.shift);
                self.goal_x = None;
            }
            "ArrowUp" | "ArrowDown" => {
                vertical = true;
                let forward = key.key == "ArrowDown";
                if command {
                    let target = if forward { self.doc_end() } else { DocPos::new(0, 0) };
                    self.state.move_caret(target, key.shift);
                } else {
                    self.move_vertical(forward, key.shift);
                }
            }
            "Home" | "End" => {
                let head = self.state.selection().head;
                let target = self.line_edge(head, key.key == "End");
                self.state.move_caret(target, key.shift);
                self.goal_x = None;
            }
            "PageUp" | "PageDown" => handled = false,
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

    fn doc_end(&self) -> DocPos {
        let doc = self.state.doc();
        let last = doc.blocks.len() - 1;
        DocPos::new(last, doc.blocks[last].doc_len())
    }

    /// The start or end of the visual line holding `pos`.
    fn line_edge(&self, pos: DocPos, end: bool) -> DocPos {
        let probes = self.probes.borrow();
        let Some(Some(probe)) = probes.get(pos.block) else {
            return DocPos::new(pos.block, if end { self.state.doc().blocks[pos.block].doc_len() } else { 0 });
        };
        let Some(flow) = self.block_flow_for(pos.block) else {
            return pos;
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
        let Some(Some(probe)) = probes.get(head.block) else {
            return;
        };
        let Some(flow) = self.block_flow_for(head.block) else {
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
        let target_line = if forward { line + 1 } else { line.wrapping_sub(1) };
        if target_line < lines {
            let target_byte = probe.index_for_x_on_line(target_line, x);
            self.state.move_caret(DocPos::new(head.block, flow.map.byte_to_doc(target_byte)), extend);
            return;
        }
        // Into the neighbouring text block.
        let blocks = self.state.doc().blocks.len();
        let neighbours: Box<dyn Iterator<Item = usize>> = if forward {
            Box::new(head.block + 1..blocks)
        } else {
            Box::new((0..head.block).rev())
        };
        for index in neighbours {
            if let Some(Some(other)) = probes.get(index) {
                let Some(other_flow) = self.block_flow_for(index) else {
                    continue;
                };
                let line = if forward { 0 } else { other.line_count() - 1 };
                let target_byte = other.index_for_x_on_line(line, x);
                self.state.move_caret(DocPos::new(index, other_flow.map.byte_to_doc(target_byte)), extend);
                return;
            }
        }
        // The first or last line: the caret goes to the document edge.
        let edge = if forward { self.doc_end() } else { DocPos::new(0, 0) };
        self.state.move_caret(edge, extend);
    }

    /// True when a collapsed caret sits on the first (`start`) or last
    /// (`end`) visual line, counting soft wraps (`isCaretOnVisualEdge`).
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
        let Some(Some(probe)) = probes.get(head.block) else {
            return false;
        };
        let Some(flow) = self.block_flow_for(head.block) else {
            return false;
        };
        let byte = flow.map.doc_to_byte(head.offset, CaretSide::Plain);
        let line = probe.line_for_index(byte);
        if edge_start { line == 0 } else { line + 1 >= probe.line_count() }
    }

    // ── Clipboard ──────────────────────────────────────────────────────────

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.state.selected_markdown();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.state.cut_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
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
        let text = item
            .entries()
            .iter()
            .find_map(|entry| match entry {
                ClipboardEntry::String(string) => Some(string.text().to_owned()),
                _ => None,
            })
            .unwrap_or_default();
        if text.is_empty() {
            return false;
        }
        let label_for = self.skill_label();
        self.state.paste(&text, &*label_for, self.path_query_active)
    }

    /// Insert text at the end of the draft (a drop, a redirected paste).
    pub fn insert_text_at_end(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
        let doc = self.state.doc();
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
        let probe = probes[index].as_ref()?;
        let byte = probe.index_for_point(position)?;
        let flow = self.block_flow_for(index)?;
        let _ = doc;
        Some(DocPos::new(index, flow.map.byte_to_doc(byte)))
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.config.disabled {
            return;
        }
        window.focus(&self.focus, cx);
        let Some(pos) = self.pos_for_point(event.position) else {
            let end = self.doc_end();
            self.state.move_caret(end, event.modifiers.shift);
            self.after_change(cx);
            return;
        };
        let now = std::time::Instant::now();
        let repeated = self
            .last_click
            .is_some_and(|(at, p)| now.duration_since(at) < Duration::from_millis(500) && (p.x - event.position.x).abs() < px(4.) && (p.y - event.position.y).abs() < px(4.));
        self.click_count = if repeated { self.click_count + 1 } else { 1 };
        self.last_click = Some((now, event.position));
        if event.modifiers.shift {
            self.state.move_caret(pos, true);
        } else {
            match self.click_count {
                2 => {
                    let start = self.state.prev_word_pos(self.state.next_word_pos(pos));
                    let end = self.state.next_word_pos(start);
                    let start = if start > pos { pos } else { start };
                    self.state.set_selection(Selection { anchor: start, head: end.max(pos) });
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

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
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

    fn block_flow_for(&self, index: usize) -> Option<BlockFlowCache> {
        self.flows.borrow().get(index).cloned().flatten()
    }
}

fn skill_label_fn(skills: &[ProviderSkill]) -> Rc<dyn Fn(&str) -> SkillMeta> {
    let skills = skills.to_vec();
    Rc::new(move |name: &str| {
        let normalized = name.strip_prefix('$').unwrap_or(name);
        let found = skills.iter().find(|skill| skill.name == normalized);
        match found {
            None => SkillMeta {
                label: crate::composer::skills::format_provider_skill_display_name(normalized, None),
                description: None,
            },
            Some(skill) => SkillMeta {
                label: crate::composer::skills::format_provider_skill_display_name(&skill.name, skill.display_name.as_deref()),
                description: skill
                    .short_description
                    .as_deref()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .or(skill.description.as_deref().map(str::trim).filter(|d| !d.is_empty()))
                    .map(str::to_owned),
            },
        }
    })
}
