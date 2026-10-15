//! The flat text the platform input method sees: block contents joined by
//! newlines, chips as U+FFFC, style markers and block prefixes excluded. All
//! offsets are UTF-16 code units (the document's flat coordinate).

use super::*;
use crate::composer::rich_doc::flat_to_pos;

impl EditorState {
    /// The text behind flat offsets.
    pub fn flat_text(&mut self) -> String {
        let starts = self.map().block_starts.clone();
        let mut out = String::new();
        let mut at = 0usize;
        for (index, block) in self.doc.blocks.iter().enumerate() {
            while at < starts[index] {
                out.push('\n');
                at += 1;
            }
            let piece: String = match block {
                Block::Code(code) => code.text.clone(),
                Block::Rule { .. } => String::new(),
                other => other
                    .inline()
                    .map(|inline| {
                        inline
                            .iter()
                            .map(|node| match &node.content {
                                InlineContent::Text(text) => text.clone(),
                                InlineContent::Atom(_) => "\u{FFFC}".to_owned(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            at += utf16_len(&piece);
            out.push_str(&piece);
        }
        out
    }

    /// The selection as flat offsets `(anchor, head)`.
    pub fn selection_flat(&mut self) -> (usize, usize) {
        let selection = self.selection;
        let map = self.map();
        (
            pos_to_flat(map, selection.anchor),
            pos_to_flat(map, selection.head),
        )
    }

    /// Select by flat offsets.
    pub fn set_selection_flat(&mut self, anchor: usize, head: usize) {
        let doc = self.doc.clone();
        let map = self.map().clone();
        let selection = Selection {
            anchor: flat_to_pos(&doc, &map, anchor),
            head: flat_to_pos(&doc, &map, head),
        };
        self.set_selection(selection);
    }

    /// Replace a flat range with text typed at the keyboard or committed by an
    /// input method.
    pub fn replace_flat_range(&mut self, from: usize, to: usize, text: &str) -> bool {
        let (from, to) = (from.min(to), from.max(to));
        self.set_selection_flat(from, to);
        if text.is_empty() {
            return self.delete_selection();
        }
        if text.contains('\n') {
            return self.insert_plain_text(text);
        }
        self.type_text(text)
    }
}
