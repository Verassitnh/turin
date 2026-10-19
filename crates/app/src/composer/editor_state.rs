//! Port of upstream/apps/web/src/components/ComposerPromptEditorTiptap.tsx
//!
//! The rich prompt editor's state machine: document, selection, stored marks,
//! undo history and every editing command, with no UI attached.
//!
//! Upstream builds this from Tiptap/ProseMirror (`ComposerPromptEditorTiptap.tsx`
//! plus the command helpers in `composer-code-block.ts`,
//! `composer-rich-text-doc.ts` and `composer-undo-grouping.ts`). The commands
//! keep upstream's names and rules; the document is the flat line model of
//! [`super::rich_doc`], so a "transaction" is a mutation of [`Doc`] followed by
//! a history entry.
//!
//! Offsets are UTF-16 code units in block content (chips count 1).
//!
//! Port of upstream/apps/web/src/composer-undo-grouping.ts.

use std::time::{Duration, Instant};

use crate::composer::code_block::{
    CODE_BLOCK_INDENT, IndentDirection, indent_lines, leading_whitespace,
};
use crate::composer::js_string::utf16_len;
use crate::composer::list_continuation::next_ordered_marker_text;
use crate::composer::logic::{
    CursorDirection, clamp_collapsed_composer_cursor, detect_composer_trigger,
    is_collapsed_cursor_adjacent_to_inline_token,
};
use crate::composer::rich_doc::{
    Atom, Block, BuildOptions, CodeBlock, Doc, DocMap, DocPos, Inline, InlineContent, InlineNode,
    ItemPrefix, Marks, SkillMeta, build_blocks, build_doc, collapsed_to_flat, flat_to_collapsed,
    flat_to_markdown, flat_to_pos, is_closing_fence, list_depths, normalize_inline,
    parse_opening_fence, pos_to_flat, serialize_doc,
};
use crate::composer::rich_text::RichTextMark;

/// `COMPOSER_UNDO_GROUP_DELAY`.
pub const COMPOSER_UNDO_GROUP_DELAY: Duration = Duration::from_millis(1000);

/// `ComposerChangeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Insert,
    Delete,
    Other,
}

// ── Cells: one entry per character or chip ─────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum CellKind {
    Char(char),
    Atom(Atom),
}

#[derive(Debug, Clone, PartialEq)]
struct Cell {
    kind: CellKind,
    marks: Marks,
}

impl Cell {
    fn units(&self) -> usize {
        match &self.kind {
            CellKind::Char(c) => c.len_utf16(),
            CellKind::Atom(_) => 1,
        }
    }

    fn is_atom(&self) -> bool {
        matches!(self.kind, CellKind::Atom(_))
    }
}

fn to_cells(inline: &[InlineNode]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for node in inline {
        match &node.content {
            InlineContent::Text(text) => {
                cells.extend(text.chars().map(|c| Cell {
                    kind: CellKind::Char(c),
                    marks: node.marks,
                }));
            }
            InlineContent::Atom(atom) => cells.push(Cell {
                kind: CellKind::Atom(atom.clone()),
                marks: node.marks,
            }),
        }
    }
    cells
}

fn from_cells(cells: &[Cell]) -> Inline {
    let mut inline: Inline = Vec::new();
    for cell in cells {
        match &cell.kind {
            CellKind::Char(c) => {
                if let Some(last) = inline.last_mut()
                    && last.marks == cell.marks
                    && let InlineContent::Text(text) = &mut last.content
                {
                    text.push(*c);
                } else {
                    inline.push(InlineNode::text(c.to_string(), cell.marks));
                }
            }
            CellKind::Atom(atom) => inline.push(InlineNode::atom(atom.clone(), cell.marks)),
        }
    }
    inline
}

/// Cell index at (or just before) a UTF-16 offset.
fn cell_index(cells: &[Cell], offset: usize) -> usize {
    let mut units = 0;
    for (index, cell) in cells.iter().enumerate() {
        if units >= offset {
            return index;
        }
        units += cell.units();
        if units > offset {
            return index;
        }
    }
    cells.len()
}

fn cell_offset(cells: &[Cell], index: usize) -> usize {
    cells[..index.min(cells.len())]
        .iter()
        .map(Cell::units)
        .sum()
}

fn text_cells(text: &str, marks: Marks) -> Vec<Cell> {
    text.chars()
        .map(|c| Cell {
            kind: CellKind::Char(c),
            marks,
        })
        .collect()
}

fn byte_index(text: &str, utf16_offset: usize) -> usize {
    let mut units = 0;
    for (at, c) in text.char_indices() {
        if units >= utf16_offset {
            return at;
        }
        units += c.len_utf16();
        if units > utf16_offset {
            return at;
        }
    }
    text.len()
}

fn code_slice(text: &str, from: usize, to: usize) -> &str {
    &text[byte_index(text, from)..byte_index(text, to)]
}

// ── Selection and snapshots ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: DocPos,
    pub head: DocPos,
}

impl Selection {
    pub fn caret(pos: DocPos) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn from(&self) -> DocPos {
        self.anchor.min(self.head)
    }

    pub fn to(&self) -> DocPos {
        self.anchor.max(self.head)
    }
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    doc: Doc,
    selection: Selection,
}

/// Where the caret sits at an edge between differently styled text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeStop {
    /// Typing continues the styled text before the edge.
    Before,
    /// Typing continues the text after the edge.
    After,
    /// The user toggled marks by hand: neither.
    Neither,
}

/// What the owner of the draft reads after a change (`readSnapshot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSnapshot {
    pub value: String,
    /// Collapsed caret (chips count 1).
    pub cursor: usize,
    pub expanded_cursor: usize,
    /// Selection in markdown offsets.
    pub selection_start: usize,
    pub selection_end: usize,
    /// Context ids referenced by the value (`contextIds`); empty for literal
    /// answers, which hold no chips.
    pub context_ids: Vec<String>,
    /// `cursorAdjacentToMention` in `onChange`: no `@` / `$` / `/` menu for
    /// literal answers, inside a fence, or with the caret against a chip.
    pub suppress_trigger: bool,
}

pub struct EditorState {
    doc: Doc,
    selection: Selection,
    stored_marks: Option<Marks>,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    previous_kind: Option<ChangeKind>,
    last_change: Option<Instant>,
    group_delay: Duration,
    map_cache: Option<DocMap>,
    /// The state before the last input rule fired, restored by Backspace.
    input_rule_undo: Option<HistoryEntry>,
    /// Rich text styling on (marks and block syntax) or plain.
    rich: bool,
    literal: bool,
    citation_comments: citations::CitationComments,
}

impl EditorState {
    pub fn new(doc: Doc, rich: bool, literal: bool) -> Self {
        let mut doc = doc;
        if doc.blocks.is_empty() {
            doc = Doc::empty();
        }
        let selection = Selection::caret(DocPos::new(0, 0));
        Self {
            doc,
            selection,
            stored_marks: None,
            undo: Vec::new(),
            redo: Vec::new(),
            previous_kind: None,
            last_change: None,
            group_delay: COMPOSER_UNDO_GROUP_DELAY,
            map_cache: None,
            input_rule_undo: None,
            rich,
            literal,
            citation_comments: citations::CitationComments::default(),
        }
    }

    pub fn from_value(
        value: &str,
        skill_label_for: &dyn Fn(&str) -> SkillMeta,
        rich: bool,
        literal: bool,
    ) -> Self {
        let doc = build_doc(value, skill_label_for, build_options(rich, literal));
        let mut state = Self::new(doc, rich, literal);
        let end = state.end_pos();
        state.selection = Selection::caret(end);
        state
    }

    pub fn set_group_delay(&mut self, delay: Duration) {
        self.group_delay = delay;
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn is_rich(&self) -> bool {
        self.rich && !self.literal
    }

    pub fn is_literal(&self) -> bool {
        self.literal
    }

    pub fn stored_marks(&self) -> Option<Marks> {
        self.stored_marks
    }

    pub fn map(&mut self) -> &DocMap {
        if self.map_cache.is_none() {
            self.map_cache = Some(serialize_doc(&self.doc));
        }
        self.map_cache.as_ref().expect("map")
    }

    pub fn value(&mut self) -> String {
        self.map().value.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.doc.blocks.len() == 1
            && self.doc.blocks[0].doc_len() == 0
            && matches!(self.doc.blocks[0], Block::Paragraph(_))
    }

    fn end_pos(&self) -> DocPos {
        let last = self.doc.blocks.len() - 1;
        DocPos::new(last, self.doc.blocks[last].doc_len())
    }

    /// Clamp a position to a valid textblock position.
    fn clamp(&self, pos: DocPos) -> DocPos {
        let block = pos.block.min(self.doc.blocks.len() - 1);
        if !self.doc.blocks[block].is_textblock() {
            // A rule owns no characters: the position belongs to the next
            // textblock, or the previous one at the end.
            if let Some(next) =
                (block + 1..self.doc.blocks.len()).find(|b| self.doc.blocks[*b].is_textblock())
            {
                return DocPos::new(next, 0);
            }
            if let Some(prev) = (0..block)
                .rev()
                .find(|b| self.doc.blocks[*b].is_textblock())
            {
                return DocPos::new(prev, self.doc.blocks[prev].doc_len());
            }
            return DocPos::new(0, 0);
        }
        DocPos::new(block, pos.offset.min(self.doc.blocks[block].doc_len()))
    }

    pub fn set_selection(&mut self, selection: Selection) {
        let anchor = self.clamp(selection.anchor);
        let head = self.clamp(selection.head);
        let next = Selection { anchor, head };
        if next != self.selection {
            // A moved caret drops marks the user toggled by hand.
            self.stored_marks = None;
            self.input_rule_undo = None;
        }
        self.selection = next;
    }

    pub fn set_caret(&mut self, pos: DocPos) {
        self.set_selection(Selection::caret(pos));
    }

    // ── History ────────────────────────────────────────────────────────────

    fn entry(&self) -> HistoryEntry {
        HistoryEntry {
            doc: self.doc.clone(),
            selection: self.selection,
        }
    }

    /// Start (or continue) an undo group before a mutation of `kind`
    /// (`groupUndoByChangeKind`): a new step begins when the kind of change
    /// switches, for one-off changes, and after a pause.
    fn begin_change(&mut self, kind: ChangeKind) -> HistoryEntry {
        let before = self.entry();
        let now = Instant::now();
        let paused = self
            .last_change
            .is_some_and(|at| now.duration_since(at) > self.group_delay);
        let new_group = self.undo.is_empty()
            || paused
            || self
                .previous_kind
                .is_none_or(|previous| previous != kind || kind == ChangeKind::Other);
        if new_group {
            self.undo.push(before.clone());
        }
        self.redo.clear();
        self.previous_kind = Some(kind);
        self.last_change = Some(now);
        before
    }

    fn finish_change(&mut self) {
        // A rule cannot hold the caret, so a document never ends in one: an
        // empty line follows it, which the draft does not write.
        if matches!(self.doc.blocks.last(), Some(Block::Rule { .. })) {
            self.doc.blocks.push(Block::Paragraph(Vec::new()));
        }
        if self.doc.blocks.is_empty() {
            self.doc = Doc::empty();
        }
        self.map_cache = None;
        let last = self.doc.blocks.len() - 1;
        let anchor = self.selection.anchor;
        let head = self.selection.head;
        let fit = |pos: DocPos, doc: &Doc| {
            let block = pos.block.min(last);
            DocPos::new(block, pos.offset.min(doc.blocks[block].doc_len()))
        };
        self.selection = Selection {
            anchor: fit(anchor, &self.doc),
            head: fit(head, &self.doc),
        };
        self.input_rule_undo = None;
    }

    /// Drop the undo entry `begin_change` pushed when the command turned out to
    /// change nothing.
    fn abort_change(&mut self, before: &HistoryEntry, kind: ChangeKind) {
        let _ = kind;
        if self
            .undo
            .last()
            .is_some_and(|last| last.doc == before.doc && last.selection == before.selection)
            && self.doc == before.doc
        {
            self.undo.pop();
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.entry());
        self.doc = entry.doc;
        self.selection = entry.selection;
        self.stored_marks = None;
        // Undo and redo themselves count as one-off, so typing after them
        // starts a fresh step.
        self.previous_kind = Some(ChangeKind::Other);
        self.finish_change();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.entry());
        self.doc = entry.doc;
        self.selection = entry.selection;
        self.stored_marks = None;
        self.previous_kind = Some(ChangeKind::Other);
        self.finish_change();
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ── Controlled value ───────────────────────────────────────────────────

    /// Replace the document with `value` (history recall, chip insertion,
    /// stash restore…) and place the caret at a collapsed cursor.
    pub fn set_value(
        &mut self,
        value: &str,
        collapsed_cursor: Option<usize>,
        skill_label_for: &dyn Fn(&str) -> SkillMeta,
    ) {
        if self.map().value != value {
            self.begin_change(ChangeKind::Other);
            self.doc = build_doc(
                value,
                skill_label_for,
                build_options(self.rich, self.literal),
            );
            self.finish_change();
        }
        let cursor = collapsed_cursor.unwrap_or_else(|| {
            let map = self.map();
            flat_to_collapsed(map, map.doc_length)
        });
        self.set_collapsed_cursor(cursor);
    }

    pub fn set_collapsed_cursor(&mut self, cursor: usize) {
        let value = self.value();
        let bounded = if self.literal {
            cursor.min(utf16_len(&value))
        } else {
            clamp_collapsed_composer_cursor(&value, cursor, false)
        };
        let doc = self.doc.clone();
        let map = self.map().clone();
        let flat = collapsed_to_flat(&map, bounded);
        let pos = flat_to_pos(&doc, &map, flat);
        self.set_selection(Selection::caret(pos));
    }

    /// `readSnapshot`.
    pub fn snapshot(&mut self) -> EditorSnapshot {
        let selection = self.selection;
        let doc = self.doc.clone();
        let map = self.map().clone();
        let _ = &doc;
        let head_flat = pos_to_flat(&map, selection.head);
        let from_flat = pos_to_flat(&map, selection.from());
        let to_flat = pos_to_flat(&map, selection.to());
        let value_len = utf16_len(&map.value);
        let cursor = flat_to_collapsed(&map, head_flat);
        let expanded = flat_to_markdown(&map, head_flat).min(value_len);
        let start = flat_to_markdown(&map, from_flat).min(value_len);
        let end = flat_to_markdown(&map, to_flat).min(value_len);
        let cursor = if self.literal {
            expanded
        } else {
            clamp_collapsed_composer_cursor(&map.value, cursor, false)
        };
        // A fence holds no chips, so nothing in it should summon the mention or
        // command menu: `@` in code is a decorator, not a file.
        let in_code_block = matches!(self.doc.blocks[selection.from().block], Block::Code(_));
        let suppress_trigger = self.literal
            || in_code_block
            || is_collapsed_cursor_adjacent_to_inline_token(
                &map.value,
                cursor,
                CursorDirection::Left,
            )
            || is_collapsed_cursor_adjacent_to_inline_token(
                &map.value,
                cursor,
                CursorDirection::Right,
            );
        EditorSnapshot {
            value: map.value.clone(),
            cursor,
            expanded_cursor: expanded,
            selection_start: start.min(end),
            selection_end: start.max(end),
            context_ids: if self.literal {
                Vec::new()
            } else {
                map.context_ids.clone()
            },
            suppress_trigger,
        }
    }

    // ── Reading ────────────────────────────────────────────────────────────

    /// The line's text up to the caret, chips as U+FFFC (`textBetween`).
    pub fn line_before_caret(&self) -> String {
        let pos = self.selection.head;
        match &self.doc.blocks[pos.block] {
            Block::Code(code) => code_slice(&code.text, 0, pos.offset).to_owned(),
            other => {
                let cells = to_cells(other.inline().map_or(&[][..], |v| v));
                let end = cell_index(&cells, pos.offset);
                cells[..end]
                    .iter()
                    .map(|cell| match &cell.kind {
                        CellKind::Char(c) => *c,
                        CellKind::Atom(_) => '\u{FFFC}',
                    })
                    .collect()
            }
        }
    }

    pub fn in_code_block(&self) -> bool {
        matches!(self.doc.blocks[self.selection.head.block], Block::Code(_))
    }

    pub fn selection_in_one_code_block(&self) -> bool {
        self.selection.anchor.block == self.selection.head.block && self.in_code_block()
    }

    /// Whether the caret is in a list item (`isActive("listItem"|"taskItem")`).
    pub fn in_list_item(&self) -> bool {
        matches!(
            self.doc.blocks[self.selection.head.block],
            Block::Item { .. }
        )
    }

    pub fn in_task_item(&self) -> bool {
        matches!(
            self.doc.blocks[self.selection.head.block],
            Block::Item {
                prefix: ItemPrefix::Task { .. },
                ..
            }
        )
    }

    // ── Marks ──────────────────────────────────────────────────────────────

    fn caret_cells(&self) -> Option<(Vec<Cell>, usize)> {
        let pos = self.selection.head;
        let inline = self.doc.blocks[pos.block].inline()?;
        let cells = to_cells(inline);
        let index = cell_index(&cells, pos.offset);
        Some((cells, index))
    }

    /// `$from.marks()`: the marks of the node before the caret, or after it at
    /// the start of a block.
    fn default_marks(&self) -> Marks {
        match self.caret_cells() {
            Some((cells, index)) => {
                if index > 0 {
                    cells[index - 1].marks
                } else {
                    cells.first().map_or(Marks::NONE, |c| c.marks)
                }
            }
            None => Marks::NONE,
        }
    }

    /// The marks typed text takes.
    pub fn current_marks(&self) -> Marks {
        self.stored_marks.unwrap_or_else(|| self.default_marks())
    }

    /// `styledEdge`.
    fn styled_edge(&self) -> Option<(Marks, Marks, Marks)> {
        if !self.selection.is_empty() {
            return None;
        }
        let (cells, index) = self.caret_cells()?;
        let before = if index > 0 {
            cells[index - 1].marks
        } else {
            Marks::NONE
        };
        let after = cells.get(index).map_or(Marks::NONE, |c| c.marks);
        if before == after {
            return None;
        }
        Some((before, after, self.current_marks()))
    }

    /// Which stop of a styled edge the caret is on, or `None` away from edges.
    pub fn caret_edge_stop(&self) -> Option<EdgeStop> {
        let (before, after, current) = self.styled_edge()?;
        Some(if current == before {
            EdgeStop::Before
        } else if current == after {
            EdgeStop::After
        } else {
            EdgeStop::Neither
        })
    }

    /// `caretTakesMarksBefore`: true when the caret sits on a styled edge and
    /// types with the marks before it.
    pub fn caret_takes_marks_before(&self) -> bool {
        self.styled_edge()
            .is_some_and(|(before, _, current)| current == before)
    }

    /// `stepCaretAcrossStyledEdge`: moves the caret to the other stop of the
    /// styled edge it sits on, toward `direction`. Returns false when there is
    /// no stop to take, so the arrow key moves the caret as usual.
    pub fn step_caret_across_styled_edge(&mut self, direction: i32) -> bool {
        let Some((before, after, current)) = self.styled_edge() else {
            return false;
        };
        // Marks the user toggled by hand (neither stop) are theirs: move as usual.
        if current != before && current != after {
            return false;
        }
        let target = if direction < 0 { before } else { after };
        if current == target {
            return false;
        }
        self.stored_marks = Some(target);
        true
    }

    /// Toggle a mark on the selection, or in the stored marks for a caret
    /// (Mod-B, Mod-I, Mod-Shift-S, Mod-E).
    pub fn toggle_mark(&mut self, mark: RichTextMark) -> bool {
        if !self.is_rich() || self.in_code_block() {
            return false;
        }
        if self.selection.is_empty() {
            let current = self.current_marks();
            self.stored_marks = Some(if current.has(mark) {
                current.without(mark)
            } else {
                current.with(mark)
            });
            return true;
        }
        let from = self.selection.from();
        let to = self.selection.to();
        // Active when every selected character carries the mark.
        let mut all = true;
        let mut any = false;
        self.for_each_inline_range(from, to, |cells, start, end| {
            for cell in &cells[start..end] {
                any = true;
                if !cell.marks.has(mark) {
                    all = false;
                }
            }
        });
        if !any {
            return false;
        }
        let before = self.begin_change(ChangeKind::Other);
        self.apply_to_inline_ranges(from, to, |cell| {
            cell.marks = if all {
                cell.marks.without(mark)
            } else {
                cell.marks.with(mark)
            };
        });
        self.finish_change();
        let _ = before;
        true
    }

    fn for_each_inline_range(
        &self,
        from: DocPos,
        to: DocPos,
        mut visit: impl FnMut(&[Cell], usize, usize),
    ) {
        for block in from.block..=to.block {
            let Some(inline) = self.doc.blocks[block].inline() else {
                continue;
            };
            let cells = to_cells(inline);
            let start = if block == from.block {
                cell_index(&cells, from.offset)
            } else {
                0
            };
            let end = if block == to.block {
                cell_index(&cells, to.offset)
            } else {
                cells.len()
            };
            visit(&cells, start, end.max(start));
        }
    }

    fn apply_to_inline_ranges(
        &mut self,
        from: DocPos,
        to: DocPos,
        mut edit: impl FnMut(&mut Cell),
    ) {
        for block in from.block..=to.block {
            let Some(inline) = self.doc.blocks[block].inline() else {
                continue;
            };
            let mut cells = to_cells(inline);
            let start = if block == from.block {
                cell_index(&cells, from.offset)
            } else {
                0
            };
            let end = if block == to.block {
                cell_index(&cells, to.offset)
            } else {
                cells.len()
            };
            for cell in &mut cells[start..end.max(start)] {
                edit(cell);
            }
            if let Some(slot) = self.doc.blocks[block].inline_mut() {
                *slot = from_cells(&cells);
            }
        }
    }

    // ── Core edits ─────────────────────────────────────────────────────────

    /// Replace `[from, to]` with nothing, merging the ends. Returns the caret
    /// position at the join.
    fn delete_range_raw(&mut self, from: DocPos, to: DocPos) -> DocPos {
        if from >= to {
            return from;
        }
        if from.block == to.block {
            match &mut self.doc.blocks[from.block] {
                Block::Code(code) => {
                    let start = byte_index(&code.text, from.offset);
                    let end = byte_index(&code.text, to.offset);
                    code.text.replace_range(start..end, "");
                }
                other => {
                    if let Some(inline) = other.inline_mut() {
                        let mut cells = to_cells(inline);
                        let start = cell_index(&cells, from.offset);
                        let end = cell_index(&cells, to.offset);
                        cells.drain(start..end.max(start));
                        *inline = from_cells(&cells);
                    }
                }
            }
            return from;
        }
        // Tail of the first block, head of the last, everything between gone.
        let last = self.doc.blocks[to.block].clone();
        self.truncate_block(from.block, from.offset);
        let remainder = match &last {
            Block::Code(code) => {
                Remainder::Text(code_slice(&code.text, to.offset, utf16_len(&code.text)).to_owned())
            }
            other => {
                let cells = to_cells(other.inline().map_or(&[][..], |v| v));
                let start = cell_index(&cells, to.offset);
                Remainder::Cells(cells[start..].to_vec())
            }
        };
        self.doc.blocks.drain(from.block + 1..=to.block);
        self.append_remainder(from.block, remainder);
        from
    }

    fn truncate_block(&mut self, block: usize, offset: usize) {
        match &mut self.doc.blocks[block] {
            Block::Code(code) => {
                let at = byte_index(&code.text, offset);
                code.text.truncate(at);
            }
            other => {
                if let Some(inline) = other.inline_mut() {
                    let mut cells = to_cells(inline);
                    let at = cell_index(&cells, offset);
                    cells.truncate(at);
                    *inline = from_cells(&cells);
                }
            }
        }
    }

    fn append_remainder(&mut self, block: usize, remainder: Remainder) {
        match (&mut self.doc.blocks[block], remainder) {
            (Block::Code(code), Remainder::Text(text)) => code.text.push_str(&text),
            (Block::Code(code), Remainder::Cells(cells)) => {
                for cell in cells {
                    match cell.kind {
                        CellKind::Char(c) => code.text.push(c),
                        CellKind::Atom(atom) => code.text.push_str(&atom.source()),
                    }
                }
            }
            (other, Remainder::Text(text)) => {
                if let Some(inline) = other.inline_mut() {
                    let mut cells = to_cells(inline);
                    cells.extend(text_cells(&text, Marks::NONE));
                    *inline = from_cells(&cells);
                }
            }
            (other, Remainder::Cells(extra)) => {
                if let Some(inline) = other.inline_mut() {
                    let mut cells = to_cells(inline);
                    cells.extend(extra);
                    *inline = from_cells(&cells);
                }
            }
        }
    }

    /// Delete the selection; returns the collapsed position.
    fn delete_selection_raw(&mut self) -> DocPos {
        let from = self.selection.from();
        let to = self.selection.to();
        let pos = self.delete_range_raw(from, to);
        self.selection = Selection::caret(pos);
        pos
    }

    pub fn delete_selection(&mut self) -> bool {
        if self.selection.is_empty() {
            return false;
        }
        self.begin_change(ChangeKind::Delete);
        self.delete_selection_raw();
        self.stored_marks = None;
        self.finish_change();
        true
    }

    /// Insert typed text (no newlines) at the selection, taking the caret's
    /// marks (`insertText`).
    pub fn insert_text(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        if text.contains('\n') {
            return self.insert_plain_text(text);
        }
        let marks = self.current_marks();
        let had_selection = !self.selection.is_empty();
        self.begin_change(if had_selection {
            ChangeKind::Other
        } else {
            ChangeKind::Insert
        });
        let pos = if had_selection {
            self.delete_selection_raw()
        } else {
            self.selection.head
        };
        self.insert_text_at(pos, text, marks);
        let new_pos = DocPos::new(pos.block, pos.offset + utf16_len(text));
        self.selection = Selection::caret(new_pos);
        // Typing keeps the marks it started with until the caret moves.
        self.stored_marks = if marks == self.default_marks() {
            None
        } else {
            Some(marks)
        };
        self.finish_change();
        true
    }

    fn insert_text_at(&mut self, pos: DocPos, text: &str, marks: Marks) {
        match &mut self.doc.blocks[pos.block] {
            Block::Code(code) => {
                let at = byte_index(&code.text, pos.offset);
                code.text.insert_str(at, text);
            }
            other => {
                if let Some(inline) = other.inline_mut() {
                    let mut cells = to_cells(inline);
                    let at = cell_index(&cells, pos.offset);
                    let inserted = text_cells(text, marks);
                    cells.splice(at..at, inserted);
                    *inline = from_cells(&cells);
                }
            }
        }
    }

    /// Insert text that may hold newlines literally: in a fence as lines of
    /// code, elsewhere as paragraph splits (a verbatim paste).
    pub fn insert_plain_text(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let marks = self.current_marks();
        self.begin_change(ChangeKind::Other);
        let pos = self.delete_selection_raw();
        let lines: Vec<&str> = text.split('\n').collect();
        if self.in_code_block_at(pos) {
            self.insert_text_at(pos, text, Marks::NONE);
            self.selection = Selection::caret(DocPos::new(pos.block, pos.offset + utf16_len(text)));
        } else {
            let mut at = pos;
            for (index, line) in lines.iter().enumerate() {
                if index > 0 {
                    at = self.split_block_at(at);
                }
                self.insert_text_at(at, line, marks);
                at = DocPos::new(at.block, at.offset + utf16_len(line));
            }
            self.selection = Selection::caret(at);
        }
        self.stored_marks = None;
        self.finish_change();
        true
    }

    fn in_code_block_at(&self, pos: DocPos) -> bool {
        matches!(self.doc.blocks[pos.block], Block::Code(_))
    }

    /// Insert a chip at the selection.
    pub fn insert_atom(&mut self, atom: Atom) -> bool {
        if self.in_code_block() {
            return self.insert_text(&atom.source());
        }
        let marks = self.current_marks();
        self.begin_change(ChangeKind::Other);
        let pos = self.delete_selection_raw();
        if let Some(inline) = self.doc.blocks[pos.block].inline_mut() {
            let mut cells = to_cells(inline);
            let at = cell_index(&cells, pos.offset);
            cells.insert(
                at,
                Cell {
                    kind: CellKind::Atom(atom),
                    marks,
                },
            );
            *inline = from_cells(&cells);
        }
        self.selection = Selection::caret(DocPos::new(pos.block, pos.offset + 1));
        self.finish_change();
        true
    }

    /// Split the block at `pos` into two of the same kind (`splitBlock`); the
    /// first keeps the position, the second starts empty of any prefix change.
    /// Returns the start of the new block.
    fn split_block_at(&mut self, pos: DocPos) -> DocPos {
        let block = self.doc.blocks[pos.block].clone();
        let (first, second) = match block {
            Block::Code(mut code) => {
                // A generic split would cut the fence in two; callers use the
                // code-block newline instead.
                let at = byte_index(&code.text, pos.offset);
                let rest = code.text.split_off(at);
                let mut next = code.clone();
                next.text = rest;
                (Block::Code(code), Block::Code(next))
            }
            other => {
                let inline = other.inline().cloned().unwrap_or_default();
                let cells = to_cells(&inline);
                let at = cell_index(&cells, pos.offset);
                let head = from_cells(&cells[..at]);
                let tail = from_cells(&cells[at..]);
                (
                    with_inline(&other, head),
                    self.second_half(&other, tail, at == cells.len()),
                )
            }
        };
        self.doc.blocks[pos.block] = first;
        self.doc.blocks.insert(pos.block + 1, second);
        DocPos::new(pos.block + 1, 0)
    }

    /// The block a split leaves after the caret: a heading split at its end
    /// continues as a paragraph (ProseMirror's default content type); list
    /// items continue as the next item.
    fn second_half(&self, original: &Block, tail: Inline, at_end: bool) -> Block {
        match original {
            Block::Heading { .. } if at_end => Block::Paragraph(tail),
            Block::Item { prefix, .. } => Block::Item {
                prefix: next_item_prefix(prefix),
                content: tail,
            },
            other => with_inline(other, tail),
        }
    }

    // ── Enter ──────────────────────────────────────────────────────────────

    /// Enter that was not claimed by sending (`handleKeyDown` for Enter after
    /// the code-block rules): list split/lift, quote lift, paragraph split.
    pub fn enter(&mut self) -> bool {
        if self.in_code_block() && self.selection_in_one_code_block() {
            return self.enter_in_code_block();
        }
        if self.is_rich() && self.convert_code_fence_on_enter() {
            return true;
        }
        self.begin_change(ChangeKind::Other);
        let carried = self.current_marks();
        let pos = if self.selection.is_empty() {
            self.selection.head
        } else {
            self.delete_selection_raw()
        };
        let block = self.doc.blocks[pos.block].clone();
        if self.is_rich() {
            match &block {
                Block::Item { .. } => {
                    if self.split_or_lift_list_item(pos) {
                        self.stored_marks = None;
                        self.finish_change();
                        return true;
                    }
                }
                Block::Quote { .. } if block.doc_len() == 0 => {
                    self.doc.blocks[pos.block] = Block::Paragraph(Vec::new());
                    self.selection = Selection::caret(DocPos::new(pos.block, 0));
                    self.finish_change();
                    return true;
                }
                _ => {}
            }
        }
        let next = self.split_block_at(pos);
        self.renumber_after(next.block);
        self.selection = Selection::caret(next);
        // splitBlockKeepMarks: typing continues with the marks at the split.
        self.stored_marks = if carried.is_empty() {
            None
        } else {
            Some(carried)
        };
        self.finish_change();
        true
    }

    fn enter_in_code_block(&mut self) -> bool {
        if !self.selection.is_empty() {
            // A ranged selection is replaced by the newline.
        }
        if self.exit_code_block_on_closing_fence() {
            return true;
        }
        if self.exit_code_block_on_trailing_blank_lines() {
            return true;
        }
        if self.indented_newline_in_code_block() {
            return true;
        }
        self.insert_text_at_selection_in_code("\n")
    }

    fn insert_text_at_selection_in_code(&mut self, text: &str) -> bool {
        self.begin_change(ChangeKind::Insert);
        let pos = self.delete_selection_raw();
        self.insert_text_at(pos, text, Marks::NONE);
        self.selection = Selection::caret(DocPos::new(pos.block, pos.offset + utf16_len(text)));
        self.finish_change();
        true
    }

    /// `indentedNewlineInCodeBlock`.
    pub fn indented_newline_in_code_block(&mut self) -> bool {
        if !self.selection_in_one_code_block() {
            return false;
        }
        let from = self.selection.from();
        let Block::Code(code) = &self.doc.blocks[from.block] else {
            return false;
        };
        let before = code_slice(&code.text, 0, from.offset);
        let line = before.rsplit('\n').next().unwrap_or("");
        let indent = leading_whitespace(line).to_owned();
        // Nothing to carry, so let the default newline handle it.
        if indent.is_empty() {
            return false;
        }
        self.insert_text_at_selection_in_code(&format!("\n{indent}"))
    }

    /// `exitCodeBlockOnTrailingBlankLines`: two blank lines at the end of a
    /// fence leave it.
    pub fn exit_code_block_on_trailing_blank_lines(&mut self) -> bool {
        let pos = self.selection.head;
        if !self.selection.is_empty() {
            return false;
        }
        let Block::Code(code) = &self.doc.blocks[pos.block] else {
            return false;
        };
        if pos.offset != utf16_len(&code.text) {
            return false;
        }
        let Some(trailing_len) = trailing_blank_lines(&code.text).map(str::len) else {
            return false;
        };
        self.begin_change(ChangeKind::Other);
        if let Block::Code(code) = &mut self.doc.blocks[pos.block] {
            let keep = code.text.len() - trailing_len;
            code.text.truncate(keep);
            // An unclosed fence runs to the end of the draft; the prose after
            // it would be read back as code, so leaving it writes the closing fence.
            if code.close.is_empty() {
                code.close = format!("\n{}", code.fence);
            }
        }
        // `exitCode`: a new paragraph after the block, caret inside it.
        self.doc
            .blocks
            .insert(pos.block + 1, Block::Paragraph(Vec::new()));
        self.selection = Selection::caret(DocPos::new(pos.block + 1, 0));
        self.stored_marks = None;
        self.finish_change();
        true
    }

    /// The picker's choice for a fence's language (`updateAttributes` on the
    /// code block node), keeping the rest of the info string.
    pub fn set_code_block_language(&mut self, block: usize, language: &str) -> bool {
        let Some(Block::Code(code)) = self.doc.blocks.get(block) else {
            return false;
        };
        let next =
            crate::composer::code_languages::with_info_string_language(&code.language, language);
        if next == code.language {
            return false;
        }
        self.begin_change(ChangeKind::Other);
        if let Block::Code(code) = &mut self.doc.blocks[block] {
            code.language = next;
        }
        self.finish_change();
        true
    }

    /// `exitCodeBlockOnClosingFence`.
    pub fn exit_code_block_on_closing_fence(&mut self) -> bool {
        if !self.selection_in_one_code_block() || !self.selection.is_empty() {
            return false;
        }
        let pos = self.selection.head;
        let Block::Code(code) = &self.doc.blocks[pos.block] else {
            return false;
        };
        let caret = byte_index(&code.text, pos.offset);
        let line_start = code.text[..caret].rfind('\n').map_or(0, |at| at + 1);
        let newline_after = code.text[caret..].find('\n').map(|at| caret + at);
        let line_end = newline_after.unwrap_or(code.text.len());
        let line = &code.text[line_start..line_end];
        if caret != line_end || !is_closing_fence(line, &code.fence) {
            return false;
        }
        let line = line.to_owned();
        let body = code.text[..line_start.saturating_sub(1)].to_owned();
        let rest: Vec<String> = match newline_after {
            Some(at) => code.text[at + 1..].split('\n').map(str::to_owned).collect(),
            None => Vec::new(),
        };
        let mut closed = code.clone();
        closed.text = body;
        closed.close = format!("\n{line}");
        self.begin_change(ChangeKind::Other);
        let mut replacement = vec![Block::Code(closed), Block::Paragraph(Vec::new())];
        replacement.extend(rest.into_iter().map(|text| {
            Block::Paragraph(if text.is_empty() {
                Vec::new()
            } else {
                vec![InlineNode::text(text, Marks::NONE)]
            })
        }));
        self.doc.blocks.splice(pos.block..=pos.block, replacement);
        self.selection = Selection::caret(DocPos::new(pos.block + 1, 0));
        self.stored_marks = None;
        self.finish_change();
        true
    }

    /// `convertCodeFenceOnEnter`: Enter on a line that is nothing but an
    /// opening fence turns it into a code block.
    pub fn convert_code_fence_on_enter(&mut self) -> bool {
        let pos = self.selection.head;
        if !self.selection.is_empty() {
            return false;
        }
        let Block::Paragraph(inline) = &self.doc.blocks[pos.block] else {
            return false;
        };
        if pos.offset != inline_units(inline) {
            return false;
        }
        // The parser's own grammar, read from the line's stored source, so a
        // line Enter turns into a fence is exactly one the stored draft reads
        // back as a fence, info string and all.
        let source = serialize_doc(&Doc {
            blocks: vec![Block::Paragraph(inline.clone())],
        })
        .value;
        let Some((fence, language)) = parse_opening_fence(&source) else {
            return false;
        };
        self.begin_change(ChangeKind::Other);
        self.doc.blocks[pos.block] = Block::Code(CodeBlock {
            close: format!("\n{fence}"),
            fence,
            language,
            text: String::new(),
        });
        self.selection = Selection::caret(DocPos::new(pos.block, 0));
        self.stored_marks = None;
        self.finish_change();
        true
    }

    /// `indentCodeBlock`: Tab/Shift+Tab inside a fence.
    pub fn indent_code_block(&mut self, direction: IndentDirection) -> bool {
        if !self.selection_in_one_code_block() {
            return false;
        }
        let from = self.selection.from();
        let to = self.selection.to();
        let block = from.block;
        let Block::Code(code) = &self.doc.blocks[block] else {
            return false;
        };
        let text = code.text.clone();
        if self.selection.is_empty() && direction == IndentDirection::In {
            return self.insert_text_at_selection_in_code(CODE_BLOCK_INDENT);
        }
        let start_offset = byte_index(&text, from.offset);
        let end_offset = byte_index(&text, to.offset);
        // Grow the range to whole lines so indenting is stable no matter where
        // in the first and last lines the selection happens to start and end.
        let line_start = if start_offset == 0 {
            0
        } else {
            text[..start_offset].rfind('\n').map_or(0, |at| at + 1)
        };
        // A selection ending at the start of a line does not touch that line.
        let search_from = start_offset.max(end_offset.saturating_sub(1));
        let newline_after = text[search_from.min(text.len())..]
            .find('\n')
            .map(|at| search_from + at);
        let line_end = newline_after.unwrap_or(text.len());
        let original: Vec<&str> = text[line_start..line_end].split('\n').collect();
        let next = indent_lines(&original, direction);
        if next.join("\n") == original.join("\n") {
            return true;
        }
        self.begin_change(ChangeKind::Other);
        let joined = next.join("\n");
        let first_delta = next[0].chars().map(char::len_utf16).sum::<usize>() as isize
            - original[0].chars().map(char::len_utf16).sum::<usize>() as isize;
        let total_delta =
            utf16_len(&joined) as isize - utf16_len(&text[line_start..line_end]) as isize;
        if let Block::Code(code) = &mut self.doc.blocks[block] {
            code.text.replace_range(line_start..line_end, &joined);
        }
        let line_start_units = utf16_len(&text[..line_start]);
        let next_from =
            (from.offset as isize + first_delta).max(line_start_units as isize) as usize;
        let next_to = ((to.offset as isize + total_delta).max(next_from as isize)) as usize;
        self.selection = Selection {
            anchor: DocPos::new(block, next_from),
            head: DocPos::new(block, next_to),
        };
        self.finish_change();
        true
    }

    // ── Lists ──────────────────────────────────────────────────────────────

    /// Split a list item at the caret, or on an empty item leave one level of
    /// nesting (`splitOrLiftListItem`). The item at the caret ends up written
    /// with the indent and marker of its new siblings.
    fn split_or_lift_list_item(&mut self, pos: DocPos) -> bool {
        let Block::Item { prefix, content } = self.doc.blocks[pos.block].clone() else {
            return false;
        };
        if content.is_empty() {
            return self.lift_list_item(pos.block);
        }
        let cells = to_cells(&content);
        let at = cell_index(&cells, pos.offset);
        let head = from_cells(&cells[..at]);
        let tail = from_cells(&cells[at..]);
        self.doc.blocks[pos.block] = Block::Item {
            prefix: prefix.clone(),
            content: head,
        };
        let new_prefix = next_item_prefix(&prefix);
        self.doc.blocks.insert(
            pos.block + 1,
            Block::Item {
                prefix: new_prefix,
                content: tail,
            },
        );
        self.renumber_after(pos.block + 1);
        self.selection = Selection::caret(DocPos::new(pos.block + 1, 0));
        true
    }

    /// Leave one level of nesting: an item under a parent takes the parent's
    /// indent and sits after it as a sibling; a top-level item becomes a
    /// paragraph.
    fn lift_list_item(&mut self, block: usize) -> bool {
        let depths = list_depths(&self.doc);
        let depth = depths[block];
        let Block::Item { prefix, content } = self.doc.blocks[block].clone() else {
            return false;
        };
        if depth == 0 {
            self.doc.blocks[block] = Block::Paragraph(content);
            self.selection = Selection::caret(DocPos::new(block, 0));
            return true;
        }
        // The nearest earlier item one level up is the new sibling.
        let Some(sibling_index) = (0..block)
            .rev()
            .find(|i| depths[*i] + 1 == depth && matches!(self.doc.blocks[*i], Block::Item { .. }))
        else {
            return false;
        };
        let Block::Item {
            prefix: sibling, ..
        } = &self.doc.blocks[sibling_index]
        else {
            return false;
        };
        // A task under a bullet has no list to lift into.
        let sibling_is_task = matches!(sibling, ItemPrefix::Task { .. });
        let is_task = matches!(prefix, ItemPrefix::Task { .. });
        if sibling_is_task != is_task {
            return false;
        }
        let aligned = align_with_sibling(&prefix, sibling, true);
        self.doc.blocks[block] = Block::Item {
            prefix: aligned,
            content,
        };
        self.renumber_after(block);
        self.selection = Selection::caret(DocPos::new(block, 0));
        true
    }

    /// Counts the items after `index` on from its marker, so an item inserted
    /// in the middle of an ordered list pushes the rest down.
    fn renumber_after(&mut self, index: usize) {
        let Block::Item {
            prefix: ItemPrefix::Ordered { indent, marker, .. },
            ..
        } = &self.doc.blocks[index]
        else {
            return;
        };
        let indent = indent.clone();
        let mut previous = marker.clone();
        let depths = list_depths(&self.doc);
        let depth = depths[index];
        #[allow(clippy::needless_range_loop)]
        for next in index + 1..self.doc.blocks.len() {
            let Block::Item { prefix, .. } = &mut self.doc.blocks[next] else {
                break;
            };
            if depths[next] < depth {
                break;
            }
            if depths[next] > depth {
                continue;
            }
            let ItemPrefix::Ordered {
                marker,
                indent: item_indent,
                ..
            } = prefix
            else {
                break;
            };
            if *item_indent != indent {
                break;
            }
            previous = next_ordered_marker_text(&previous);
            if *marker != previous {
                *marker = previous.clone();
            }
        }
    }

    /// `listIndentForTab`-style indent for list items (two spaces), used by
    /// Tab in a list item.
    pub fn indent_list_item(&mut self, outdent: bool) -> bool {
        let block = self.selection.head.block;
        let Block::Item { prefix, .. } = &self.doc.blocks[block] else {
            return false;
        };
        let indent = prefix.indent().to_owned();
        let next = if outdent {
            if let Some(rest) = indent.strip_prefix("  ") {
                rest.to_owned()
            } else if let Some(rest) = indent.strip_prefix('\t') {
                rest.to_owned()
            } else {
                return false;
            }
        } else {
            format!("  {indent}")
        };
        self.begin_change(ChangeKind::Other);
        if let Block::Item { prefix, .. } = &mut self.doc.blocks[block] {
            prefix.set_indent(next);
        }
        self.finish_change();
        true
    }

    /// Toggle a task item's checkbox.
    pub fn toggle_task(&mut self, block: usize) -> bool {
        let Some(Block::Item {
            prefix: ItemPrefix::Task { checked, .. },
            ..
        }) = self.doc.blocks.get_mut(block)
        else {
            return false;
        };
        let _ = checked;
        self.begin_change(ChangeKind::Other);
        if let Some(Block::Item {
            prefix: ItemPrefix::Task { checked, .. },
            ..
        }) = self.doc.blocks.get_mut(block)
        {
            *checked = !*checked;
        }
        self.finish_change();
        true
    }

    // ── Backspace / Delete ─────────────────────────────────────────────────

    /// Backspace. Returns false when the editor has nothing to do.
    pub fn delete_backward(&mut self) -> bool {
        if self.selection.is_empty() && self.undo_input_rule() {
            return true;
        }
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let pos = self.selection.head;
        if pos.offset > 0 {
            let from = self.prev_cursor_pos(pos);
            self.begin_change(ChangeKind::Delete);
            self.delete_range_raw(from, pos);
            self.selection = Selection::caret(from);
            self.stored_marks = None;
            self.finish_change();
            return true;
        }
        self.backspace_at_block_start(pos)
    }

    fn backspace_at_block_start(&mut self, pos: DocPos) -> bool {
        let block = pos.block;
        let before = if block > 0 {
            Some(self.doc.blocks[block - 1].clone())
        } else {
            None
        };
        let current = self.doc.blocks[block].clone();
        match current {
            Block::Code(code) if self.is_rich() => {
                // Backspace at the start of a fence removes the fence and keeps
                // its lines as paragraphs.
                self.begin_change(ChangeKind::Other);
                let paragraphs: Vec<Block> = code
                    .text
                    .split('\n')
                    .map(|line| {
                        Block::Paragraph(if line.is_empty() {
                            Vec::new()
                        } else {
                            vec![InlineNode::text(line, Marks::NONE)]
                        })
                    })
                    .collect();
                self.doc.blocks.splice(block..=block, paragraphs);
                self.selection = Selection::caret(DocPos::new(block, 0));
                self.finish_change();
                true
            }
            Block::Code(_) => false,
            Block::Item { .. } => {
                // Backspace at the start of an item's text leaves one level of
                // nesting, or the list itself at the top, written at its new
                // depth. A lift with nowhere to go joins the line above.
                let snapshot = self.entry();
                self.begin_change(ChangeKind::Other);
                if self.lift_list_item(block) {
                    self.finish_change();
                    return true;
                }
                self.abort_change(&snapshot, ChangeKind::Other);
                self.join_with_previous(block)
            }
            Block::Heading { content, .. } | Block::Quote { content, .. } if before.is_none() => {
                self.begin_change(ChangeKind::Other);
                self.doc.blocks[block] = Block::Paragraph(content);
                self.finish_change();
                true
            }
            Block::Quote { content, prefix } => {
                // The first line of a quote lifts out of it; later lines join.
                let joins_quote =
                    matches!(&before, Some(Block::Quote { prefix: p, .. }) if *p == prefix);
                if joins_quote {
                    self.join_with_previous(block)
                } else {
                    self.begin_change(ChangeKind::Other);
                    self.doc.blocks[block] = Block::Paragraph(content);
                    self.finish_change();
                    true
                }
            }
            _ => self.join_with_previous(block),
        }
    }

    /// Join the block at `block` onto the text of the one before it.
    fn join_with_previous(&mut self, block: usize) -> bool {
        if block == 0 {
            return false;
        }
        match self.doc.blocks[block - 1].clone() {
            Block::Rule { .. } => {
                // The rule goes; the caret stays where it was.
                self.begin_change(ChangeKind::Delete);
                self.doc.blocks.remove(block - 1);
                self.selection = Selection::caret(DocPos::new(block - 1, 0));
                self.finish_change();
                true
            }
            Block::Code(code) => {
                // Pulling text into the code would drop the fence from the
                // draft: the caret enters the block instead.
                self.set_caret(DocPos::new(block - 1, utf16_len(&code.text)));
                true
            }
            previous => {
                let end = previous.doc_len();
                let current = self.doc.blocks[block].clone();
                let remainder = match &current {
                    Block::Code(code) => Remainder::Text(code.text.clone()),
                    other => Remainder::Cells(to_cells(other.inline().map_or(&[][..], |v| v))),
                };
                self.begin_change(ChangeKind::Delete);
                self.doc.blocks.remove(block);
                self.append_remainder(block - 1, remainder);
                self.selection = Selection::caret(DocPos::new(block - 1, end));
                self.stored_marks = None;
                self.finish_change();
                true
            }
        }
    }

    /// Delete (forward).
    pub fn delete_forward(&mut self) -> bool {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let pos = self.selection.head;
        let len = self.doc.blocks[pos.block].doc_len();
        if pos.offset < len {
            let to = self.next_cursor_pos(pos);
            self.begin_change(ChangeKind::Delete);
            self.delete_range_raw(pos, to);
            self.selection = Selection::caret(pos);
            self.stored_marks = None;
            self.finish_change();
            return true;
        }
        // End of a block: pull the next line's text up onto this one. Before a
        // fence, anywhere, it does nothing.
        let next = pos.block + 1;
        if next >= self.doc.blocks.len() {
            return false;
        }
        match self.doc.blocks[next].clone() {
            Block::Code(_) => false,
            Block::Rule { .. } => {
                self.begin_change(ChangeKind::Delete);
                self.doc.blocks.remove(next);
                self.finish_change();
                true
            }
            _ if self.in_code_block() => false,
            next_block => {
                let remainder =
                    Remainder::Cells(to_cells(next_block.inline().map_or(&[][..], |v| v)));
                self.begin_change(ChangeKind::Delete);
                self.doc.blocks.remove(next);
                self.append_remainder(pos.block, remainder);
                self.finish_change();
                true
            }
        }
    }

    // ── Movement ───────────────────────────────────────────────────────────

    /// The position one character before `pos` (across block boundaries).
    pub fn prev_cursor_pos(&self, pos: DocPos) -> DocPos {
        if pos.offset > 0 {
            return match &self.doc.blocks[pos.block] {
                Block::Code(code) => {
                    let text = &code.text;
                    let at = byte_index(text, pos.offset);
                    let previous = text[..at].chars().next_back().map_or(0, char::len_utf16);
                    DocPos::new(pos.block, pos.offset - previous)
                }
                other => {
                    let cells = to_cells(other.inline().map_or(&[][..], |v| v));
                    let index = cell_index(&cells, pos.offset);
                    if index == 0 {
                        DocPos::new(pos.block, 0)
                    } else {
                        DocPos::new(pos.block, cell_offset(&cells, index - 1))
                    }
                }
            };
        }
        match (0..pos.block)
            .rev()
            .find(|b| self.doc.blocks[*b].is_textblock())
        {
            Some(previous) => DocPos::new(previous, self.doc.blocks[previous].doc_len()),
            None => pos,
        }
    }

    pub fn next_cursor_pos(&self, pos: DocPos) -> DocPos {
        let len = self.doc.blocks[pos.block].doc_len();
        if pos.offset < len {
            return match &self.doc.blocks[pos.block] {
                Block::Code(code) => {
                    let at = byte_index(&code.text, pos.offset);
                    let next = code.text[at..].chars().next().map_or(0, char::len_utf16);
                    DocPos::new(pos.block, pos.offset + next)
                }
                other => {
                    let cells = to_cells(other.inline().map_or(&[][..], |v| v));
                    let index = cell_index(&cells, pos.offset);
                    if index >= cells.len() {
                        DocPos::new(pos.block, len)
                    } else {
                        DocPos::new(pos.block, cell_offset(&cells, index + 1))
                    }
                }
            };
        }
        match (pos.block + 1..self.doc.blocks.len()).find(|b| self.doc.blocks[*b].is_textblock()) {
            Some(next) => DocPos::new(next, 0),
            None => pos,
        }
    }

    /// Word boundary before `pos` (Alt+Left / Alt+Backspace).
    pub fn prev_word_pos(&self, pos: DocPos) -> DocPos {
        if pos.offset == 0 {
            return self.prev_cursor_pos(pos);
        }
        let chars = self.block_chars(pos.block);
        let mut index = char_index_for_offset(&chars, pos.offset);
        while index > 0 && chars[index - 1].0.is_whitespace() {
            index -= 1;
        }
        if index > 0 {
            let word = is_word_char(chars[index - 1].0);
            while index > 0
                && !chars[index - 1].0.is_whitespace()
                && is_word_char(chars[index - 1].0) == word
            {
                index -= 1;
            }
        }
        DocPos::new(pos.block, offset_for_char_index(&chars, index))
    }

    pub fn next_word_pos(&self, pos: DocPos) -> DocPos {
        let len = self.doc.blocks[pos.block].doc_len();
        if pos.offset >= len {
            return self.next_cursor_pos(pos);
        }
        let chars = self.block_chars(pos.block);
        let mut index = char_index_for_offset(&chars, pos.offset);
        while index < chars.len() && chars[index].0.is_whitespace() {
            index += 1;
        }
        if index < chars.len() {
            let word = is_word_char(chars[index].0);
            while index < chars.len()
                && !chars[index].0.is_whitespace()
                && is_word_char(chars[index].0) == word
            {
                index += 1;
            }
        }
        DocPos::new(pos.block, offset_for_char_index(&chars, index))
    }

    /// One `(char, units)` per cell; chips are U+FFFC.
    fn block_chars(&self, block: usize) -> Vec<(char, usize)> {
        match &self.doc.blocks[block] {
            Block::Code(code) => code.text.chars().map(|c| (c, c.len_utf16())).collect(),
            other => to_cells(other.inline().map_or(&[][..], |v| v))
                .iter()
                .map(|cell| match &cell.kind {
                    CellKind::Char(c) => (*c, c.len_utf16()),
                    CellKind::Atom(_) => ('\u{FFFC}', 1),
                })
                .collect(),
        }
    }

    pub fn move_caret(&mut self, to: DocPos, extend: bool) {
        let head = self.clamp(to);
        let anchor = if extend { self.selection.anchor } else { head };
        let next = Selection { anchor, head };
        self.set_selection(next);
    }

    pub fn select_all(&mut self) {
        let end = self.end_pos();
        self.set_selection(Selection {
            anchor: DocPos::new(0, 0),
            head: end,
        });
    }

    /// Delete back to the previous word (Alt+Backspace) or line start
    /// (Cmd+Backspace).
    pub fn delete_to(&mut self, target: DocPos) -> bool {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let pos = self.selection.head;
        if target == pos {
            return false;
        }
        let (from, to) = if target < pos {
            (target, pos)
        } else {
            (pos, target)
        };
        self.begin_change(ChangeKind::Delete);
        self.delete_range_raw(from, to);
        self.selection = Selection::caret(from);
        self.stored_marks = None;
        self.finish_change();
        true
    }

    // ── Clipboard ──────────────────────────────────────────────────────────

    /// The stored markdown of the selection, as copy and cut put it on the
    /// clipboard (`serializeSelection`).
    pub fn selected_markdown(&self) -> String {
        if self.selection.is_empty() {
            return String::new();
        }
        let from = self.selection.from();
        let to = self.selection.to();
        let slice = self.slice_doc(from, to);
        serialize_doc(&slice).value
    }

    /// A document holding just the selected part: a range inside one block
    /// loses its marker; across blocks the blocks keep theirs.
    fn slice_doc(&self, from: DocPos, to: DocPos) -> Doc {
        let cut = |block: &Block, start: Option<usize>, end: Option<usize>| -> Block {
            match block {
                Block::Code(code) => {
                    let s = start.unwrap_or(0);
                    let e = end.unwrap_or_else(|| utf16_len(&code.text));
                    Block::Code(CodeBlock {
                        text: code_slice(&code.text, s, e).to_owned(),
                        ..code.clone()
                    })
                }
                Block::Rule { .. } => block.clone(),
                other => {
                    let cells = to_cells(other.inline().map_or(&[][..], |v| v));
                    let s = start.map_or(0, |o| cell_index(&cells, o));
                    let e = end.map_or(cells.len(), |o| cell_index(&cells, o));
                    with_inline(other, from_cells(&cells[s..e.max(s)]))
                }
            }
        };
        if from.block == to.block {
            let block = &self.doc.blocks[from.block];
            let sliced = cut(block, Some(from.offset), Some(to.offset));
            return Doc {
                blocks: vec![match sliced {
                    Block::Code(code) => {
                        Block::Paragraph(vec![InlineNode::text(code.text, Marks::NONE)])
                    }
                    other => Block::Paragraph(other.inline().cloned().unwrap_or_default()),
                }],
            };
        }
        let mut blocks = Vec::new();
        for index in from.block..=to.block {
            let start = (index == from.block).then_some(from.offset);
            let end = (index == to.block).then_some(to.offset);
            blocks.push(cut(&self.doc.blocks[index], start, end));
        }
        Doc { blocks }
    }

    /// Cut: copy then delete.
    pub fn cut_selection(&mut self) -> Option<String> {
        if self.selection.is_empty() {
            return None;
        }
        let text = self.selected_markdown();
        self.begin_change(ChangeKind::Other);
        self.delete_selection_raw();
        self.stored_marks = None;
        self.finish_change();
        Some(text)
    }

    /// Paste text at the selection (`handlePaste` after the token fix-ups):
    /// in a fence it is verbatim; elsewhere it is parsed as markdown.
    pub fn paste(
        &mut self,
        text: &str,
        skill_label_for: &dyn Fn(&str) -> SkillMeta,
        path_query_active: bool,
    ) -> bool {
        if text.is_empty() {
            return false;
        }
        if self.in_code_block() {
            return self.insert_plain_text(text);
        }
        let mut text = text.to_owned();
        if !self.literal {
            // Complete chips at paste boundaries just as autocomplete does.
            let probe = format!("{text}\n");
            let tokens =
                crate::composer::editor_mentions::collect_composer_prompt_inline_tokens(&probe);
            if let Some(last) = tokens.last()
                && last.is_mention_or_skill()
                && last.end() == utf16_len(&text)
            {
                text.push(' ');
            }
            if let Some(first) = tokens.first()
                && first.is_mention_or_skill()
                && first.start() == 0
            {
                let snapshot = self.snapshot();
                let offset = snapshot.selection_start;
                if offset > 0 {
                    let units: Vec<u16> = snapshot.value.encode_utf16().collect();
                    if let Some(previous) = units.get(offset - 1)
                        && !char::from_u32(u32::from(*previous))
                            .is_some_and(crate::composer::js_string::is_js_whitespace_char)
                    {
                        text = format!(" {text}");
                    }
                }
            }
        }
        let line_before = self.line_before_caret();
        let literal_length = if self.literal {
            0
        } else {
            crate::composer::logic::pasted_path_query_length(&line_before, &text, path_query_active)
        };
        let nested = self.selection.is_empty()
            && matches!(
                self.doc.blocks[self.selection.head.block],
                Block::Item { .. } | Block::Quote { .. }
            )
            || matches!(
                self.doc.blocks[self.selection.from().block],
                Block::Item { .. } | Block::Quote { .. }
            );
        let options = BuildOptions {
            styling: self.rich && !self.literal,
            blocks: !nested,
            literal_text: self.literal,
            literal_length,
        };
        let blocks = build_blocks(&text, skill_label_for, options);
        self.insert_blocks(blocks)
    }

    /// `insertMarkdownParagraphs` / tiptap `insertContent` for built blocks.
    pub fn insert_blocks(&mut self, blocks: Vec<Block>) -> bool {
        if blocks.is_empty() {
            return false;
        }
        self.begin_change(ChangeKind::Other);
        let pos = self.delete_selection_raw();
        self.stored_marks = None;
        // A single paragraph inserts its inline content at the caret.
        if blocks.len() == 1
            && let Block::Paragraph(inline) = &blocks[0]
        {
            if inline.is_empty() {
                self.abort_change(
                    &HistoryEntry {
                        doc: self.doc.clone(),
                        selection: self.selection,
                    },
                    ChangeKind::Other,
                );
                return false;
            }
            let units = inline_units(inline);
            if let Block::Code(_) = self.doc.blocks[pos.block] {
                // handled by `paste`
            } else if let Some(target) = self.doc.blocks[pos.block].inline_mut() {
                let mut cells = to_cells(target);
                let at = cell_index(&cells, pos.offset);
                let inserted = to_cells(inline);
                cells.splice(at..at, inserted);
                *target = from_cells(&cells);
            }
            self.selection = Selection::caret(DocPos::new(pos.block, pos.offset + units));
            self.finish_change();
            return true;
        }
        let current = self.doc.blocks[pos.block].clone();
        let len = current.doc_len();
        let count = blocks.len();
        let last_len = blocks[count - 1].doc_len();
        let empty_text_block = len == 0
            && matches!(
                current,
                Block::Paragraph(_)
                    | Block::Heading { .. }
                    | Block::Quote { .. }
                    | Block::Item { .. }
            );
        if empty_text_block {
            self.doc.blocks.splice(pos.block..=pos.block, blocks);
        } else if pos.offset == 0 {
            for (index, block) in blocks.into_iter().enumerate() {
                self.doc.blocks.insert(pos.block + index, block);
            }
        } else if pos.offset >= len {
            for (index, block) in blocks.into_iter().enumerate() {
                self.doc.blocks.insert(pos.block + 1 + index, block);
            }
        } else {
            // Split the block around the paste.
            let (head, tail) = match &current {
                Block::Code(_) => return false,
                other => {
                    let cells = to_cells(other.inline().map_or(&[][..], |v| v));
                    let at = cell_index(&cells, pos.offset);
                    (
                        with_inline(other, from_cells(&cells[..at])),
                        with_inline(other, from_cells(&cells[at..])),
                    )
                }
            };
            let mut replacement = vec![head];
            replacement.extend(blocks);
            replacement.push(tail);
            self.doc.blocks.splice(pos.block..=pos.block, replacement);
        }
        // Caret at the end of the pasted content.
        let first_new = if empty_text_block || pos.offset == 0 {
            pos.block
        } else {
            pos.block + 1
        };
        let mut last_index = first_new + count - 1;
        // A paste ending in a rule leaves the caret on the line after it,
        // which is added when there is none.
        if matches!(self.doc.blocks[last_index], Block::Rule { .. }) {
            if last_index + 1 >= self.doc.blocks.len()
                || !self.doc.blocks[last_index + 1].is_textblock()
            {
                self.doc
                    .blocks
                    .insert(last_index + 1, Block::Paragraph(Vec::new()));
            }
            last_index += 1;
            self.selection = Selection::caret(DocPos::new(last_index, 0));
        } else {
            let at = self.doc.blocks[last_index]
                .doc_len()
                .min(last_len.max(self.doc.blocks[last_index].doc_len()));
            self.selection = Selection::caret(DocPos::new(last_index, at));
        }
        self.finish_change();
        true
    }

    /// Replace a markdown-offset range of the stored prompt (autocomplete,
    /// list continuation…), rebuilding the document: a store rewrite.
    pub fn replace_markdown_range(
        &mut self,
        start: usize,
        end: usize,
        replacement: &str,
        skill_label_for: &dyn Fn(&str) -> SkillMeta,
    ) -> usize {
        let value = self.value();
        let units: Vec<u16> = value.encode_utf16().collect();
        let start = start.min(units.len());
        let end = end.clamp(start, units.len());
        let next = format!(
            "{}{}{}",
            String::from_utf16_lossy(&units[..start]),
            replacement,
            String::from_utf16_lossy(&units[end..])
        );
        let expanded_cursor = start + utf16_len(replacement);
        let cursor = crate::composer::logic::collapse_expanded_composer_cursor(
            &next,
            expanded_cursor,
            self.literal,
        );
        self.set_value(&next, Some(cursor), skill_label_for);
        cursor
    }

    /// `detectComposerTrigger` at the caret, in the stored text.
    pub fn trigger(&mut self) -> Option<crate::composer::logic::ComposerTrigger> {
        let snapshot = self.snapshot();
        if snapshot.suppress_trigger {
            return None;
        }
        detect_composer_trigger(&snapshot.value, snapshot.expanded_cursor)
    }
}

enum Remainder {
    Text(String),
    Cells(Vec<Cell>),
}

fn build_options(rich: bool, literal: bool) -> BuildOptions {
    BuildOptions {
        styling: rich && !literal,
        blocks: true,
        literal_text: literal,
        literal_length: 0,
    }
}

fn with_inline(block: &Block, inline: Inline) -> Block {
    let inline = normalize_inline(inline);
    match block {
        Block::Paragraph(_) => Block::Paragraph(inline),
        Block::Heading { level, space, .. } => Block::Heading {
            level: *level,
            space: space.clone(),
            content: inline,
        },
        Block::Quote { prefix, .. } => Block::Quote {
            prefix: prefix.clone(),
            content: inline,
        },
        Block::Item { prefix, .. } => Block::Item {
            prefix: prefix.clone(),
            content: inline,
        },
        other => other.clone(),
    }
}

fn inline_units(inline: &[InlineNode]) -> usize {
    crate::composer::rich_doc::inline_len(inline)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn char_index_for_offset(chars: &[(char, usize)], offset: usize) -> usize {
    let mut units = 0;
    for (index, (_, len)) in chars.iter().enumerate() {
        if units >= offset {
            return index;
        }
        units += len;
    }
    chars.len()
}

fn offset_for_char_index(chars: &[(char, usize)], index: usize) -> usize {
    chars[..index.min(chars.len())]
        .iter()
        .map(|(_, len)| len)
        .sum()
}

/// `/\n[ \t]*\n[ \t]*$/` at the end of `text`: returns the matched tail.
fn trailing_blank_lines(text: &str) -> Option<&str> {
    let trimmed = text.trim_end_matches([' ', '\t']);
    let without_last = trimmed.strip_suffix('\n')?;
    let before = without_last.trim_end_matches([' ', '\t']);
    let start = before.strip_suffix('\n')?.len();
    Some(&text[start..])
}

/// The prefix a split leaves on the new item: tasks start unchecked, ordered
/// markers count up, bullets repeat (`splitListItem` + `alignListItemWithSiblings`).
fn next_item_prefix(prefix: &ItemPrefix) -> ItemPrefix {
    match prefix {
        ItemPrefix::Task {
            indent,
            marker_space,
            ..
        } => ItemPrefix::Task {
            indent: indent.clone(),
            checked: false,
            marker_space: marker_space.clone(),
            content_space: String::new(),
        },
        ItemPrefix::Ordered { indent, marker, .. } => ItemPrefix::Ordered {
            indent: indent.clone(),
            marker: next_ordered_marker_text(marker),
            space: " ".to_owned(),
        },
        ItemPrefix::Bullet { indent, marker, .. } => ItemPrefix::Bullet {
            indent: indent.clone(),
            marker: marker.clone(),
            space: " ".to_owned(),
        },
    }
}

/// `alignListItemWithSiblings` for a lifted item: the sibling's indent and
/// marker (an ordered marker counts on from the sibling it follows).
fn align_with_sibling(item: &ItemPrefix, sibling: &ItemPrefix, after_sibling: bool) -> ItemPrefix {
    match (item, sibling) {
        (
            ItemPrefix::Task {
                checked,
                content_space,
                ..
            },
            ItemPrefix::Task {
                indent,
                marker_space,
                ..
            },
        ) => ItemPrefix::Task {
            indent: indent.clone(),
            checked: *checked,
            marker_space: marker_space.clone(),
            content_space: content_space.clone(),
        },
        (ItemPrefix::Bullet { space, .. }, ItemPrefix::Bullet { indent, marker, .. })
        | (ItemPrefix::Ordered { space, .. }, ItemPrefix::Bullet { indent, marker, .. }) => {
            ItemPrefix::Bullet {
                indent: indent.clone(),
                marker: marker.clone(),
                space: space.clone(),
            }
        }
        (ItemPrefix::Bullet { space, .. }, ItemPrefix::Ordered { indent, marker, .. })
        | (ItemPrefix::Ordered { space, .. }, ItemPrefix::Ordered { indent, marker, .. }) => {
            ItemPrefix::Ordered {
                indent: indent.clone(),
                marker: if after_sibling {
                    next_ordered_marker_text(marker)
                } else {
                    marker.clone()
                },
                space: space.clone(),
            }
        }
        (other, _) => other.clone(),
    }
}

mod citations;
mod ime;
mod input;
mod keys;
pub use citations::{CitationCommentRequest, CitationDraftSave};
pub use keys::EditorKey;

#[cfg(test)]
mod tests;
