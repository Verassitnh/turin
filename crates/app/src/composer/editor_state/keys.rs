//! The model half of `handleKeyDown` in ComposerPromptEditorTiptap.tsx: the
//! steps that run before the host's `onCommandKeyDown` and need no layout.
//!
//! Visual-line work (Home/End, Up/Down between wrapped lines) stays in the
//! view, which owns the line boxes; it runs only when this returns false,
//! the way upstream leaves those keys to the browser.

use super::*;
use crate::composer::code_block::IndentDirection;

/// One keydown, with the DOM `KeyboardEvent.key` name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorKey {
    pub key: String,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    pub ctrl: bool,
    /// An input method is composing (`isComposing` / keyCode 229).
    pub is_composing: bool,
}

impl EditorKey {
    pub fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            ..Self::default()
        }
    }

    fn unmodified(&self) -> bool {
        !self.shift && !self.alt && !self.meta && !self.ctrl && !self.is_composing
    }
}

impl EditorState {
    /// The first textblock position (`TextSelection.atStart`).
    fn start_pos(&self) -> DocPos {
        self.clamp(DocPos::new(0, 0))
    }

    /// Runs the editor's own key rules ahead of the host. True when the key
    /// was consumed (upstream returns true and prevents the default).
    pub fn handle_key_down(&mut self, key: &EditorKey) -> bool {
        let arrow = matches!(
            key.key.as_str(),
            "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown"
        );
        if arrow && key.unmodified() && self.selection.is_empty() {
            let head = self.selection.head;
            let direction = if matches!(key.key.as_str(), "ArrowLeft" | "ArrowUp") {
                -1
            } else {
                1
            };
            let horizontal = matches!(key.key.as_str(), "ArrowLeft" | "ArrowRight");
            // At the document's ends, native Up/Down can cross a marker without
            // changing the document position. Record the stop so the caret does
            // not land back inside the styling.
            let at_document_edge = head
                == if direction < 0 {
                    self.start_pos()
                } else {
                    self.end_pos()
                };
            // Take the other stop of a styled edge before skipping a chip, so
            // the plain stop between styled text and a chip stays reachable.
            if (horizontal || at_document_edge) && self.step_caret_across_styled_edge(direction) {
                return true;
            }
            if horizontal && self.adjacent_is_atom(direction) {
                let offset = if direction < 0 {
                    head.offset - 1
                } else {
                    head.offset + 1
                };
                self.set_caret(DocPos::new(head.block, offset));
                return true;
            }
        }
        // Enter while an input method composes confirms the composition.
        if key.key == "Enter" && key.is_composing {
            return true;
        }
        // Inside a fence Tab belongs to the code, not to the composer's focus
        // order or its autocomplete.
        if key.key == "Tab" && !key.meta && !key.ctrl && self.selection_in_one_code_block() {
            self.indent_code_block(if key.shift {
                IndentDirection::Out
            } else {
                IndentDirection::In
            });
            return true;
        }
        // A fence is multi-line by definition, so Enter and Shift+Enter belong
        // to the code rather than to sending or splitting; on a line that is
        // only an opening fence they open the block. Sending from inside a
        // fence is still Cmd/Ctrl+Enter, which falls through to the host.
        if key.key == "Enter" && self.is_rich() && !key.meta && !key.ctrl && !key.is_composing {
            if self.selection_in_one_code_block() {
                return self.enter_in_code_block();
            }
            if self.convert_code_fence_on_enter() {
                return true;
            }
        }
        false
    }

    /// Whether the node beside a collapsed caret, toward `direction`, is a chip.
    fn adjacent_is_atom(&self, direction: i32) -> bool {
        let Some((cells, index)) = self.caret_cells() else {
            return false;
        };
        let cell = if direction < 0 {
            index.checked_sub(1).and_then(|i| cells.get(i))
        } else {
            cells.get(index)
        };
        cell.is_some_and(Cell::is_atom)
    }
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/web/src/components/ComposerPromptEditorTiptap.navigation.test.tsx
    //!
    //! `press` returns whether the editor claimed the key, which upstream
    //! reads from `handleKeyDown`; a false result is the key left to native
    //! navigation (the view's own movement in Turin).

    use super::*;

    const DRAFT: &str = "> **Assistant and reasoning text still save intermediate full snapshots**, using the existing 50 ms batching. Compaction still preserves their `turn-item.updated` events. [Code](/Users/yashsingh/p/projects/t3code/apps/server/src/orchestration-v2/ProjectionMaintenance.ts:235)";

    fn no_skills(name: &str) -> SkillMeta {
        SkillMeta {
            label: name.to_owned(),
            description: None,
        }
    }

    /// `mountEditor`: the controlled value with `cursor={0}`.
    fn mount(value: &str, rich: bool) -> EditorState {
        let mut state = EditorState::from_value(value, &no_skills, rich, false);
        state.set_collapsed_cursor(0);
        state
    }

    fn press(state: &mut EditorState, key: EditorKey) -> bool {
        state.handle_key_down(&key)
    }

    fn arrow(key: &str) -> EditorKey {
        EditorKey::new(key)
    }

    #[test]
    fn keeps_up_before_the_opening_bold_marker_in_a_quote_including_while_typing() {
        let mut state = mount(DRAFT, true);
        let position = state.selection().head;
        assert!(press(&mut state, arrow("ArrowUp")));
        assert_eq!(state.selection().head, position);
        // The caret paints before the revealed `**` (the outside stop).
        assert_eq!(state.caret_edge_stop(), Some(EdgeStop::Before));
        // A repeated Up stays at the outside stop, ready for native navigation.
        assert!(!press(&mut state, arrow("ArrowUp")));
        state.insert_text("x");
        assert_eq!(state.value(), DRAFT.replacen("> **", "> x**", 1));
    }

    #[test]
    fn keeps_down_after_a_closing_marker_including_while_typing() {
        let mut state = mount("> **bold**", true);
        let end = state.end_pos();
        state.set_caret(end);
        assert!(press(&mut state, arrow("ArrowDown")));
        assert!(!press(&mut state, arrow("ArrowDown")));
        state.insert_text("x");
        assert_eq!(state.value(), "> **bold**x");
    }

    #[test]
    fn leaves_vertical_navigation_between_paragraphs_to_the_browser() {
        let mut state = mount("**first**\n**second**", true);
        state.set_caret(DocPos::new(1, 0));
        assert!(!press(&mut state, arrow("ArrowUp")));
        state.set_caret(DocPos::new(0, 5));
        assert!(!press(&mut state, arrow("ArrowDown")));
    }

    #[test]
    fn preserves_modified_up_behavior() {
        let modified = [
            EditorKey {
                shift: true,
                ..arrow("ArrowUp")
            },
            EditorKey {
                alt: true,
                ..arrow("ArrowUp")
            },
            EditorKey {
                meta: true,
                ..arrow("ArrowUp")
            },
            EditorKey {
                ctrl: true,
                ..arrow("ArrowUp")
            },
            EditorKey {
                is_composing: true,
                ..arrow("ArrowUp")
            },
        ];
        for key in modified {
            let mut state = mount(DRAFT, true);
            assert!(!press(&mut state, key.clone()), "{key:?}");
            state.insert_text("x");
            assert_eq!(state.value(), DRAFT.replacen("> **", "> **x", 1), "{key:?}");
        }
    }

    #[test]
    fn leaves_literal_markdown_navigation_to_the_browser_in_plain_mode() {
        let mut state = mount(DRAFT, false);
        // ProseMirror position 5 is flat offset 4: after `> **`.
        state.set_caret(DocPos::new(0, 4));
        assert!(!press(&mut state, arrow("ArrowUp")));
        state.insert_text("x");
        assert_eq!(state.value(), DRAFT.replacen("> **", "> **x", 1));
    }

    // ── The rest of handleKeyDown's pre-host rules ─────────────────────────

    #[test]
    fn horizontal_arrows_step_over_a_chip_in_one_press() {
        let mut state = mount("see @a.md now", true);
        state.set_caret(DocPos::new(0, 4));
        assert!(press(&mut state, arrow("ArrowRight")));
        assert_eq!(state.selection().head, DocPos::new(0, 5));
        assert!(press(&mut state, arrow("ArrowLeft")));
        assert_eq!(state.selection().head, DocPos::new(0, 4));
        // Plain text is left to native movement.
        assert!(!press(&mut state, arrow("ArrowLeft")));
    }

    #[test]
    fn enter_inside_a_fence_makes_a_line_before_the_host_sees_it() {
        let mut state = mount("```\ncode\n```", true);
        state.set_caret(DocPos::new(0, 4));
        assert!(press(&mut state, EditorKey::new("Enter")));
        assert_eq!(state.value(), "```\ncode\n\n```");
        // Cmd/Ctrl+Enter still reaches the host (send).
        assert!(!press(
            &mut state,
            EditorKey {
                meta: true,
                ..EditorKey::new("Enter")
            }
        ));
    }

    #[test]
    fn enter_on_an_opening_fence_line_opens_the_block() {
        let mut state = mount("```ts", true);
        assert!(press(&mut state, EditorKey::new("Enter")));
        assert!(state.in_code_block());
    }

    #[test]
    fn tab_inside_a_fence_indents_the_code() {
        let mut state = mount("```\ncode\n```", true);
        state.set_caret(DocPos::new(0, 0));
        assert!(press(&mut state, EditorKey::new("Tab")));
        assert!(
            state
                .value()
                .contains(&format!("\n{CODE_BLOCK_INDENT}code"))
        );
        assert!(press(
            &mut state,
            EditorKey {
                shift: true,
                ..EditorKey::new("Tab")
            }
        ));
        assert_eq!(state.value(), "```\ncode\n```");
    }

    #[test]
    fn enter_while_composing_is_swallowed() {
        let mut state = mount("hello", true);
        assert!(press(
            &mut state,
            EditorKey {
                is_composing: true,
                ..EditorKey::new("Enter")
            }
        ));
        assert_eq!(state.value(), "hello");
    }
}
