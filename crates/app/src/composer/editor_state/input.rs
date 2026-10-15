//! Typed-text behaviour of the editor: surround-on-type and the markdown
//! input rules (`handleTextInput`, `listMarkerInputRule`, `taskInputRule`,
//! `bulletToTaskInputRule`, `blockquoteInputRule`, `horizontalRuleInputRule`,
//! `headingInputRule` in ComposerPromptEditorTiptap.tsx).

use std::sync::OnceLock;

use regex::Regex;

use super::*;
use crate::composer::editor_mentions::selection_touches_mention_boundary;

/// `SURROUND_CLOSE`.
fn surround_close(open: &str) -> Option<&'static str> {
    Some(match open {
        "(" => ")",
        "[" => "]",
        "{" => "}",
        "'" => "'",
        "\"" => "\"",
        "\u{201C}" => "\u{201D}",
        "`" => "`",
        "<" => ">",
        "\u{00AB}" => "\u{00BB}",
        "*" => "*",
        "_" => "_",
        _ => return None,
    })
}

struct Rules {
    bullet: Regex,
    dash: Regex,
    ordered: Regex,
    task: Regex,
    bullet_to_task: Regex,
    quote: Regex,
    rule: Regex,
    heading: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| Rules {
        bullet: Regex::new(r"^([*+])(\s)$").unwrap(),
        dash: Regex::new(r"^-([ \t]+)([^\s\[])$").unwrap(),
        ordered: Regex::new(r"^(\d+[.)])(\s)$").unwrap(),
        task: Regex::new(r"^- \[([ xX])\] $").unwrap(),
        bullet_to_task: Regex::new(r"^\[([ xX])\] $").unwrap(),
        quote: Regex::new(r"^>(\s)$").unwrap(),
        rule: Regex::new(r"^(---|\*\*\*|___)\s?$").unwrap(),
        heading: Regex::new(r"^(#{1,6})(\s)$").unwrap(),
    })
}

impl EditorState {
    /// Text typed at the keyboard (or committed by an input method): wraps a
    /// selection in a bracket pair, inserts, then runs the input rules.
    pub fn type_text(&mut self, text: &str) -> bool {
        if self.try_surround_selection(text) {
            return true;
        }
        let was_empty = self.selection.is_empty();
        if !self.insert_text(text) {
            return false;
        }
        if was_empty && self.is_rich() {
            self.try_input_rules(text);
        }
        true
    }

    /// `handleTextInput`: typing an opening bracket or quote over a selection
    /// wraps it instead of replacing it.
    fn try_surround_selection(&mut self, text: &str) -> bool {
        if self.literal || utf16_len(text) != 1 {
            return false;
        }
        let Some(closer) = surround_close(text) else {
            return false;
        };
        if self.selection.is_empty() {
            return false;
        }
        let from = self.selection.from();
        let to = self.selection.to();
        // Never wrap chips or other atoms, and never wrap styled text: the
        // default replace keeps marks intact, wrapping would drop them.
        let mut special = false;
        for block in from.block..=to.block {
            if let Some(inline) = self.doc.blocks[block].inline() {
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
                if cells[start..end.max(start)]
                    .iter()
                    .any(|cell| cell.is_atom() || !cell.marks.is_empty())
                {
                    special = true;
                }
            }
        }
        if special {
            return false;
        }
        let snapshot = self.snapshot();
        if selection_touches_mention_boundary(
            &snapshot.value,
            snapshot.selection_start,
            snapshot.selection_end,
        ) {
            return false;
        }
        self.begin_change(ChangeKind::Other);
        self.insert_text_at(to, closer, Marks::NONE);
        self.insert_text_at(from, text, Marks::NONE);
        let shift = |pos: DocPos, by_block: DocPos| {
            if pos.block == by_block.block {
                DocPos::new(pos.block, pos.offset + 1)
            } else {
                pos
            }
        };
        let new_from = DocPos::new(from.block, from.offset + 1);
        let new_to = shift(to, from);
        let new_to = if to.block == from.block { new_to } else { to };
        self.selection = Selection {
            anchor: new_from,
            head: DocPos::new(new_to.block, new_to.offset),
        };
        self.stored_marks = None;
        self.finish_change();
        true
    }

    fn try_input_rules(&mut self, typed: &str) {
        let pos = self.selection.head;
        let block = self.doc.blocks[pos.block].clone();
        let Some(inline) = block.inline() else {
            return;
        };
        let cells = to_cells(inline);
        let caret = cell_index(&cells, pos.offset);
        let before: String = cells[..caret]
            .iter()
            .map(|cell| match &cell.kind {
                CellKind::Char(c) => *c,
                CellKind::Atom(_) => '\u{FFFC}',
            })
            .collect();
        if !typed.is_empty() && !before.ends_with(typed) {
            return;
        }
        let rules = rules();
        let rest: Vec<Cell> = cells[caret..].to_vec();
        let with_rest = |text: &str| -> Inline {
            let mut cells = text_cells(text, Marks::NONE);
            cells.extend(rest.clone());
            from_cells(&cells)
        };
        let typed_state = self.entry();

        // `[ ] ` at the start of an existing bullet item turns it into a task.
        if let Block::Item {
            prefix:
                ItemPrefix::Bullet {
                    indent,
                    marker,
                    space,
                },
            ..
        } = &block
            && let Some(captures) = rules.bullet_to_task.captures(&before)
            && ["-", "*", "+"].contains(&marker.as_str())
        {
            let checked = captures[1].eq_ignore_ascii_case("x");
            let marker_space = if space.is_empty() {
                " ".to_owned()
            } else {
                space.clone()
            };
            self.doc.blocks[pos.block] = Block::Item {
                prefix: ItemPrefix::Task {
                    indent: indent.clone(),
                    checked,
                    marker_space,
                    content_space: String::new(),
                },
                content: with_rest(""),
            };
            self.selection = Selection::caret(DocPos::new(pos.block, 0));
            self.after_input_rule(typed_state);
            return;
        }
        // The remaining rules apply to top-level paragraphs only: inside an
        // item or a quote the new block would nest under a line the stored
        // draft writes flat.
        if !matches!(block, Block::Paragraph(_)) {
            return;
        }
        if let Some(captures) = rules.task.captures(&before) {
            self.doc.blocks[pos.block] = Block::Item {
                prefix: ItemPrefix::Task {
                    indent: String::new(),
                    checked: captures[1].eq_ignore_ascii_case("x"),
                    marker_space: " ".to_owned(),
                    content_space: String::new(),
                },
                content: with_rest(""),
            };
            self.selection = Selection::caret(DocPos::new(pos.block, 0));
            self.after_input_rule(typed_state);
            return;
        }
        if let Some(captures) = rules.bullet.captures(&before) {
            self.doc.blocks[pos.block] = Block::Item {
                prefix: ItemPrefix::Bullet {
                    indent: String::new(),
                    marker: captures[1].to_owned(),
                    space: captures[2].to_owned(),
                },
                content: with_rest(""),
            };
            self.selection = Selection::caret(DocPos::new(pos.block, 0));
            self.after_input_rule(typed_state);
            return;
        }
        // `- ` alone is not enough for a dash: it waits for the first character
        // after the space, which it carries into the new item. That is what
        // lets `- [ ] ` be typed whole and reach the task rule.
        if let Some(captures) = rules.dash.captures(&before) {
            let carried = captures[2].to_owned();
            self.doc.blocks[pos.block] = Block::Item {
                prefix: ItemPrefix::Bullet {
                    indent: String::new(),
                    marker: "-".to_owned(),
                    space: captures[1].to_owned(),
                },
                content: with_rest(&carried),
            };
            self.selection = Selection::caret(DocPos::new(pos.block, utf16_len(&carried)));
            self.after_input_rule(typed_state);
            return;
        }
        if let Some(captures) = rules.ordered.captures(&before) {
            self.doc.blocks[pos.block] = Block::Item {
                prefix: ItemPrefix::Ordered {
                    indent: String::new(),
                    marker: captures[1].to_owned(),
                    space: captures[2].to_owned(),
                },
                content: with_rest(""),
            };
            self.selection = Selection::caret(DocPos::new(pos.block, 0));
            self.after_input_rule(typed_state);
            return;
        }
        if let Some(captures) = rules.quote.captures(&before) {
            self.doc.blocks[pos.block] = Block::Quote {
                prefix: format!(">{}", &captures[1]),
                content: with_rest(""),
            };
            self.selection = Selection::caret(DocPos::new(pos.block, 0));
            self.after_input_rule(typed_state);
            return;
        }
        if let Some(captures) = rules.heading.captures(&before) {
            self.doc.blocks[pos.block] = Block::Heading {
                level: captures[1].len() as u8,
                space: captures[2].to_owned(),
                content: with_rest(""),
            };
            self.selection = Selection::caret(DocPos::new(pos.block, 0));
            self.after_input_rule(typed_state);
            return;
        }
        if let Some(captures) = rules.rule.captures(&before) {
            let source = captures[0].to_owned();
            if !source.starts_with("---") && !source.ends_with(char::is_whitespace) {
                return;
            }
            // The typed characters are kept as the rule's source, and a line
            // follows it so the caret has somewhere to go.
            let remainder = with_rest("");
            self.doc.blocks[pos.block] = Block::Rule { source };
            if remainder.is_empty() {
                if pos.block + 1 < self.doc.blocks.len()
                    && self.doc.blocks[pos.block + 1].is_textblock()
                {
                    self.selection = Selection::caret(DocPos::new(pos.block + 1, 0));
                } else {
                    self.doc
                        .blocks
                        .insert(pos.block + 1, Block::Paragraph(Vec::new()));
                    self.selection = Selection::caret(DocPos::new(pos.block + 1, 0));
                }
            } else {
                self.doc
                    .blocks
                    .insert(pos.block + 1, Block::Paragraph(remainder));
                self.selection = Selection::caret(DocPos::new(pos.block + 1, 0));
            }
            self.after_input_rule(typed_state);
        }
    }

    fn after_input_rule(&mut self, typed_state: HistoryEntry) {
        self.finish_change();
        self.input_rule_undo = Some(typed_state);
        self.stored_marks = None;
    }

    /// Backspace right after an input rule fired restores the typed text
    /// (`undoInputRule`).
    pub fn undo_input_rule(&mut self) -> bool {
        let Some(entry) = self.input_rule_undo.take() else {
            return false;
        };
        self.begin_change(ChangeKind::Other);
        self.doc = entry.doc;
        self.selection = entry.selection;
        self.stored_marks = None;
        self.finish_change();
        true
    }

    /// Whether Backspace would undo an input rule.
    pub fn has_input_rule_to_undo(&self) -> bool {
        self.input_rule_undo.is_some()
    }
}
