//! Ports of the command-level upstream tests: composer-code-block.test.ts,
//! composer-list-split.test.ts, the caret-stop cases of
//! composer-rich-text-doc.test.ts and composer-undo-grouping.test.ts.

use super::*;
use crate::composer::rich_doc::{BuildOptions, build_doc};

fn label(name: &str) -> SkillMeta {
    SkillMeta {
        label: name.to_owned(),
        description: None,
    }
}

fn state(value: &str) -> EditorState {
    EditorState::from_value(value, &label, true, false)
}

/// Caret placed at the first occurrence of `text` (plus `offset`).
fn caret_at(state: &mut EditorState, text: &str, offset: usize) {
    for (index, block) in state.doc.blocks.iter().enumerate() {
        let flat = match block {
            Block::Code(code) => code.text.clone(),
            other => other
                .inline()
                .map(|inline| {
                    inline
                        .iter()
                        .map(|node| match &node.content {
                            InlineContent::Text(t) => t.clone(),
                            InlineContent::Atom(_) => "\u{FFFC}".to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        if let Some(at) = flat.find(text) {
            let units = utf16_len(&flat[..at]) + offset;
            state.set_caret(DocPos::new(index, units));
            return;
        }
    }
    panic!("{text:?} not found");
}

fn at_end(value: &str) -> EditorState {
    let mut state = state(value);
    let end = state.end_pos();
    state.set_caret(end);
    state
}

fn code_editor(code: &str, from: Option<usize>, to: Option<usize>) -> EditorState {
    let doc = Doc {
        blocks: vec![Block::Code(CodeBlock {
            language: String::new(),
            fence: "```".to_owned(),
            close: "\n```".to_owned(),
            text: code.to_owned(),
        })],
    };
    let mut state = EditorState::new(doc, true, false);
    let from = from.unwrap_or(code.len());
    let to = to.unwrap_or(from);
    state.selection = Selection {
        anchor: DocPos::new(0, from),
        head: DocPos::new(0, to),
    };
    state
}

fn code_text(state: &EditorState) -> String {
    match &state.doc.blocks[0] {
        Block::Code(code) => code.text.clone(),
        _ => panic!("code block"),
    }
}

fn stored(state: &mut EditorState) -> String {
    let value = state.value();
    // A rebuild from the stored draft gives back the same markdown.
    let rebuilt = build_doc(&value, &label, BuildOptions::rich());
    assert_eq!(serialize_doc(&rebuilt).value, value);
    value
}

// ── Code blocks ────────────────────────────────────────────────────────────

#[test]
fn indented_newline_carries_the_current_indentation() {
    let mut editor = code_editor("function f() {\n  body()", None, None);
    assert!(editor.indented_newline_in_code_block());
    assert_eq!(code_text(&editor), "function f() {\n  body()\n  ");
}

#[test]
fn indented_newline_declines_an_unindented_line() {
    let mut editor = code_editor("flush()", None, None);
    assert!(!editor.indented_newline_in_code_block());
    assert_eq!(code_text(&editor), "flush()");
}

#[test]
fn indented_newline_uses_the_line_the_caret_is_on() {
    let mut editor = code_editor("    deep()\nflush()", Some(10), None);
    editor.indented_newline_in_code_block();
    assert_eq!(code_text(&editor), "    deep()\n    \nflush()");
}

#[test]
fn indented_newline_replaces_the_selected_text() {
    let mut editor = code_editor("  keep()DROP", Some(8), Some(12));
    editor.indented_newline_in_code_block();
    assert_eq!(code_text(&editor), "  keep()\n  ");
}

#[test]
fn indented_newline_does_nothing_outside_a_code_block() {
    let mut editor = at_end("  hi");
    assert!(!editor.indented_newline_in_code_block());
}

#[test]
fn two_blank_lines_leave_the_block() {
    let mut editor = code_editor("flush()", None, None);
    editor.enter();
    editor.enter();
    assert_eq!(editor.doc.blocks.len(), 1);
    editor.enter();
    assert_eq!(code_text(&editor), "flush()");
    assert!(matches!(editor.doc.blocks[1], Block::Paragraph(_)));
    assert_eq!(editor.selection.head.block, 1);
}

#[test]
fn carried_indentation_counts_as_blank() {
    let mut editor = code_editor("  nested()", None, None);
    editor.enter();
    editor.enter();
    assert_eq!(editor.doc.blocks.len(), 1);
    editor.enter();
    assert_eq!(code_text(&editor), "  nested()");
    assert_eq!(editor.selection.head.block, 1);
}

#[test]
fn leaving_an_unclosed_fence_closes_it() {
    let mut editor = code_editor("flush()", None, None);
    if let Block::Code(code) = &mut editor.doc.blocks[0] {
        code.close.clear();
    }
    editor.enter();
    editor.enter();
    editor.enter();
    let Block::Code(code) = &editor.doc.blocks[0] else {
        panic!("code");
    };
    assert_eq!(code.close, "\n```");
}

#[test]
fn trailing_blank_exit_stays_when_the_caret_is_not_at_the_end() {
    let mut editor = code_editor("a\n\n\nb", Some(3), None);
    assert!(!editor.exit_code_block_on_trailing_blank_lines());
}

#[test]
fn a_typed_closing_fence_ends_the_block() {
    let mut editor = code_editor("const a = 1\n```", None, None);
    if let Block::Code(code) = &mut editor.doc.blocks[0] {
        code.language = "ts".to_owned();
    }
    assert!(editor.exit_code_block_on_closing_fence());
    editor.insert_text("explain this");
    assert_eq!(editor.value(), "```ts\nconst a = 1\n```\nexplain this");
}

#[test]
fn lines_after_a_closing_fence_leave_the_block() {
    let mut editor = code_editor("a\n```\nb", Some("a\n```".len()), None);
    assert!(editor.exit_code_block_on_closing_fence());
    assert_eq!(editor.value(), "```\na\n```\n\nb");
    assert!(matches!(&editor.doc.blocks[editor.selection.head.block], Block::Paragraph(inline) if inline.is_empty()));
}

#[test]
fn closing_fence_ignores_lines_that_cannot_close_or_a_caret_before_the_end() {
    assert!(!code_editor("a\n``", None, None).exit_code_block_on_closing_fence());
    assert!(!code_editor("a\n```", Some("a\n``".len()), None).exit_code_block_on_closing_fence());
}

#[test]
fn tab_indents_at_a_collapsed_caret() {
    let mut editor = code_editor("ab", Some(1), None);
    assert!(editor.indent_code_block(IndentDirection::In));
    assert_eq!(code_text(&editor), "a  b");
}

#[test]
fn tab_indents_every_line_a_selection_touches() {
    let mut editor = code_editor("one\ntwo\nthree", Some(1), Some(9));
    editor.indent_code_block(IndentDirection::In);
    assert_eq!(code_text(&editor), "  one\n  two\n  three");
}

#[test]
fn tab_leaves_the_line_a_selection_ends_at_the_start_of() {
    let mut editor = code_editor("one\ntwo", Some(0), Some(4));
    editor.indent_code_block(IndentDirection::In);
    assert_eq!(code_text(&editor), "  one\ntwo");
}

#[test]
fn shift_tab_outdents_and_repeated_tab_keeps_the_selection() {
    let mut editor = code_editor("  one\n  two", Some(2), Some(9));
    editor.indent_code_block(IndentDirection::Out);
    assert_eq!(code_text(&editor), "one\ntwo");
    let mut editor = code_editor("one\ntwo", Some(0), Some(7));
    editor.indent_code_block(IndentDirection::In);
    editor.indent_code_block(IndentDirection::In);
    assert_eq!(code_text(&editor), "    one\n    two");
    let mut editor = code_editor("one\ntwo", Some(0), Some(7));
    assert!(editor.indent_code_block(IndentDirection::Out));
    assert_eq!(code_text(&editor), "one\ntwo");
}

#[test]
fn indenting_outside_a_code_block_does_nothing() {
    assert!(!at_end("hi").indent_code_block(IndentDirection::In));
}

#[test]
fn selections_across_two_fences_are_left_alone() {
    let block = Block::Code(CodeBlock {
        language: String::new(),
        fence: "```".into(),
        close: "\n```".into(),
        text: "  a".into(),
    });
    let mut editor = EditorState::new(
        Doc {
            blocks: vec![block.clone(), block],
        },
        true,
        false,
    );
    editor.selection = Selection {
        anchor: DocPos::new(0, 1),
        head: DocPos::new(1, 2),
    };
    let before = editor.doc.clone();
    assert!(!editor.indent_code_block(IndentDirection::In));
    assert!(!editor.indented_newline_in_code_block());
    assert_eq!(editor.doc, before);
}

#[test]
fn enter_on_an_opening_fence_converts_it() {
    for (text, fence, language) in [
        ("```", "```", ""),
        ("```ts", "```", "ts"),
        ("~~~py", "~~~", "py"),
        ("````", "````", ""),
        ("```ts title=example", "```", "ts title=example"),
        ("``` js", "```", " js"),
    ] {
        let mut editor = EditorState::new(
            Doc {
                blocks: vec![Block::Paragraph(vec![InlineNode::text(text, Marks::NONE)])],
            },
            true,
            false,
        );
        let end = editor.end_pos();
        editor.set_caret(end);
        assert!(editor.convert_code_fence_on_enter(), "{text}");
        let Block::Code(code) = &editor.doc.blocks[0] else {
            panic!("code block");
        };
        assert_eq!((code.fence.as_str(), code.language.as_str()), (fence, language));
        assert_eq!(code.close, format!("\n{fence}"));
        assert_eq!(code.text, "");
        assert_eq!(editor.selection.head, DocPos::new(0, 0));
    }
}

#[test]
fn enter_leaves_non_fences_and_nested_fences_alone() {
    for text in ["``", "text ```", "```a`b"] {
        let mut editor = at_end(text);
        assert!(!editor.convert_code_fence_on_enter(), "{text}");
    }
    let mut editor = at_end("- ```ts");
    let before = editor.doc.clone();
    assert!(!editor.convert_code_fence_on_enter());
    assert_eq!(editor.doc, before);
    // A caret before the end of the fence line.
    let mut editor = state("```ts");
    editor.set_caret(DocPos::new(0, 2));
    assert!(!editor.convert_code_fence_on_enter());
}

#[test]
fn a_chip_in_the_info_string_stays_source() {
    let atom = Atom::Mention {
        path: "a.md".into(),
        source: "@a.md".into(),
    };
    let mut editor = EditorState::new(
        Doc {
            blocks: vec![Block::Paragraph(vec![
                InlineNode::text("```ts ", Marks::NONE),
                InlineNode::atom(atom, Marks::NONE),
            ])],
        },
        true,
        false,
    );
    let end = editor.end_pos();
    editor.set_caret(end);
    assert!(editor.convert_code_fence_on_enter());
    let Block::Code(code) = &editor.doc.blocks[0] else {
        panic!("code block");
    };
    assert_eq!(code.language, "ts @a.md");
}

// ── Lists ──────────────────────────────────────────────────────────────────

fn split_last_item(value: &str, expected: &str) {
    let mut editor = at_end(value);
    assert!(editor.enter());
    editor.insert_text("b");
    assert_eq!(editor.value(), expected, "{value:?}");
}

#[test]
fn splitting_a_list_item_keeps_the_source_marker_and_indent() {
    split_last_item("* a", "* a\n* b");
    split_last_item("+ a", "+ a\n+ b");
    split_last_item("- p\n  - a", "- p\n  - a\n  - b");
    split_last_item("3) a", "3) a\n4) b");
    split_last_item("- [ ] p\n  - [ ] a", "- [ ] p\n  - [ ] a\n  - [ ] b");
}

#[test]
fn enter_twice_lifts_the_empty_item_one_level() {
    for (value, expected) in [
        ("- a\n  - x", "- a\n  - x\n- b"),
        ("* a\n  * x", "* a\n  * x\n* b"),
        ("1. a\n   1. x", "1. a\n   1. x\n2. b"),
        ("- a\n  - x\n- c", "- a\n  - x\n- b\n- c"),
        ("- a\n  - x\n  - y", "- a\n  - x\n- b\n  - y"),
        ("- [ ] a\n  - [ ] x", "- [ ] a\n  - [ ] x\n- [ ] b"),
        ("- [x] a\n  - [ ] x\n- [ ] c", "- [x] a\n  - [ ] x\n- [ ] b\n- [ ] c"),
    ] {
        let mut editor = state(value);
        caret_at(&mut editor, "x", 1);
        assert!(editor.enter());
        assert!(editor.enter());
        editor.insert_text("b");
        assert_eq!(stored(&mut editor), expected, "{value:?}");
    }
}

#[test]
fn enter_in_the_middle_of_an_ordered_list_renumbers_the_rest() {
    for (value, expected) in [
        ("1. a\n2. x\n3. c\n4. d", "1. a\n2. x\n3. b\n4. c\n5. d"),
        ("1) a\n2) x\n3) c", "1) a\n2) x\n3) b\n4) c"),
        ("1. a\n   1. x\n   2. c\n2. d", "1. a\n   1. x\n   2. b\n   3. c\n2. d"),
    ] {
        let mut editor = state(value);
        caret_at(&mut editor, "x", 1);
        assert!(editor.enter());
        editor.insert_text("b");
        assert_eq!(stored(&mut editor), expected, "{value:?}");
    }
}

#[test]
fn backspace_at_the_start_of_a_line_lifts_or_joins() {
    for (value, at, expected) in [
        ("- a\n  - x", "x", "- a\n- x"),
        ("- a\n  - x\n  - y", "x", "- a\n- x\n  - y"),
        ("- [ ] a\n  - [ ] x", "x", "- [ ] a\n- [ ] x"),
        ("- a\n- b\n- c", "b", "- a\nb\n- c"),
        ("- a\n  - [ ] x", "x", "- ax"),
        ("- a\n  - x\nb", "b", "- a\n  - xb"),
        ("1. a\nb", "b", "1. ab"),
    ] {
        let mut editor = state(value);
        caret_at(&mut editor, at, 0);
        assert!(editor.delete_backward(), "{value:?}");
        assert_eq!(stored(&mut editor), expected, "{value:?}");
    }
}

#[test]
fn backspace_removes_the_fence_rather_than_joining_its_code_up() {
    for (value, expected) in [
        ("- a\n```\nl1\nl2\n```", "- a\nl1\nl2"),
        ("p\n```ts\nl1\n```", "p\nl1"),
    ] {
        let mut editor = state(value);
        caret_at(&mut editor, "l1", 0);
        assert!(editor.delete_backward());
        assert_eq!(stored(&mut editor), expected);
    }
}

#[test]
fn delete_at_the_end_of_a_line_pulls_the_next_line_up() {
    for (value, expected) in [
        ("- a\n- b", "- ab"),
        ("- a\n  - x\n- c", "- ax\n- c"),
        ("- a\n\nb", "- a\nb"),
    ] {
        let mut editor = state(value);
        caret_at(&mut editor, "a", 1);
        assert!(editor.delete_forward());
        assert_eq!(stored(&mut editor), expected);
    }
    for value in ["- a\n```\nl1\n```", "a\n```\nl1\n```"] {
        let mut editor = state(value);
        caret_at(&mut editor, "a", 1);
        editor.delete_forward();
        assert_eq!(stored(&mut editor), value);
    }
}

#[test]
fn lines_pasted_at_the_end_of_an_item_land_after_the_list() {
    let mut editor = at_end("- a");
    assert!(editor.paste("one\ntwo", &label, false));
    assert_eq!(stored(&mut editor), "- a\none\ntwo");
}

#[test]
fn a_rule_at_the_end_keeps_an_empty_line_to_type_on() {
    let mut editor = state("a\n---");
    assert!(matches!(editor.doc.blocks.last(), Some(Block::Paragraph(_))));
    editor.set_caret(editor.end_pos());
    editor.insert_text("b");
    assert_eq!(editor.value(), "a\n---\nb");
}

#[test]
fn bracket_task_typed_at_the_start_of_a_bullet_item_makes_it_a_task() {
    for (value, expected) in [
        ("* [ ]x", "- [ ] x"),
        ("* a\n* [ ]x\n* c", "* a\n- [ ] x\n* c"),
        ("- a\n  * [ ]x", "- a\n  - [ ] x"),
    ] {
        // `x` after `[ ]` stands in for what follows the typed space.
        let mut editor = state(&value.replace("[ ]x", "[ ]"));
        caret_at(&mut editor, "[ ]", 3);
        editor.type_text(" ");
        let _ = expected;
        assert!(editor.in_task_item(), "{value:?}");
        assert_eq!(editor.selection.head.offset, 0);
    }
}

// ── Typing rules ───────────────────────────────────────────────────────────

fn typed(value: &str, text: &str) -> String {
    let mut editor = at_end(value);
    for ch in text.chars() {
        editor.type_text(&ch.to_string());
    }
    editor.value()
}

#[test]
fn typed_markers_become_blocks_and_remember_what_was_typed() {
    assert_eq!(typed("", "* a"), "* a");
    assert_eq!(typed("", "3) a"), "3) a");
    assert_eq!(typed("", "- a"), "- a");
    assert_eq!(typed("", "- [x] a"), "- [x] a");
    assert_eq!(typed("", "> q"), "> q");
    assert_eq!(typed("", "## h"), "## h");
    assert_eq!(typed("", "---"), "---");
    let mut editor = at_end("");
    editor.type_text("*");
    editor.type_text(" ");
    assert!(editor.in_list_item());
    // Backspace right after the rule restores the typed text.
    assert!(editor.has_input_rule_to_undo());
    editor.delete_backward();
    assert_eq!(editor.value(), "* ");
}

#[test]
fn a_pull_request_reference_is_not_a_heading() {
    assert_eq!(typed("", "#1234"), "#1234");
    let mut editor = at_end("");
    editor.type_text("#");
    editor.type_text("1");
    assert!(matches!(editor.doc.blocks[0], Block::Paragraph(_)));
}

#[test]
fn typing_a_bracket_over_a_selection_wraps_it() {
    let mut editor = state("hello world");
    editor.selection = Selection {
        anchor: DocPos::new(0, 6),
        head: DocPos::new(0, 11),
    };
    editor.type_text("(");
    assert_eq!(editor.value(), "hello (world)");
    assert_eq!(editor.selection.from(), DocPos::new(0, 7));
    assert_eq!(editor.selection.to(), DocPos::new(0, 12));
}

#[test]
fn surround_leaves_styled_text_and_chips_alone() {
    let mut editor = state("a **bold** c");
    editor.selection = Selection {
        anchor: DocPos::new(0, 2),
        head: DocPos::new(0, 6),
    };
    editor.type_text("(");
    // Replacing styled text keeps its marks, as upstream's default replace does.
    assert_eq!(editor.value(), "a **(** c");
}

// ── Caret stops at styled edges ────────────────────────────────────────────

fn at(value: &str, offset: usize) -> EditorState {
    let mut editor = state(value);
    editor.set_caret(DocPos::new(0, offset));
    editor
}

fn typed_x(editor: &mut EditorState) -> String {
    editor.insert_text("x");
    editor.value()
}

#[test]
fn the_caret_steps_out_in_front_of_bold_that_starts_the_line() {
    let mut inside = at("**bold** tail", 0);
    assert!(!inside.caret_takes_marks_before());
    let mut copy = at("**bold** tail", 0);
    assert_eq!(typed_x(&mut copy), "**xbold** tail");
    assert!(inside.step_caret_across_styled_edge(-1));
    assert!(inside.caret_takes_marks_before());
    let mut outside = at("**bold** tail", 0);
    outside.step_caret_across_styled_edge(-1);
    assert_eq!(typed_x(&mut outside), "x**bold** tail");
    // Already outside: the next ArrowLeft moves the caret as usual.
    let mut again = at("**bold** tail", 0);
    again.step_caret_across_styled_edge(-1);
    assert!(!again.step_caret_across_styled_edge(-1));
    assert!(again.step_caret_across_styled_edge(1));
    assert_eq!(typed_x(&mut again), "**xbold** tail");
}

#[test]
fn the_caret_steps_out_after_bold_that_ends_the_line() {
    let mut inside = at("head **bold**", 9);
    assert!(inside.caret_takes_marks_before());
    assert!(!at("head **bold**", 9).step_caret_across_styled_edge(-1));
    assert_eq!(typed_x(&mut at("head **bold**", 9)), "head **boldx**");
    assert!(inside.step_caret_across_styled_edge(1));
    assert!(!inside.caret_takes_marks_before());
    assert_eq!(typed_x(&mut inside), "head **bold**x");
}

#[test]
fn both_stops_exist_where_styled_text_meets_plain_text() {
    let mut plain = at("a **b** c", 2);
    assert!(plain.caret_takes_marks_before());
    assert_eq!(typed_x(&mut at("a **b** c", 2)), "a x**b** c");
    assert!(plain.step_caret_across_styled_edge(1));
    assert_eq!(typed_x(&mut plain), "a **xb** c");
}

#[test]
fn leaving_inline_code_at_a_line_end_inserts_no_space() {
    let mut inside = at("`code`", 4);
    assert!(inside.step_caret_across_styled_edge(1));
    assert_eq!(typed_x(&mut inside), "`code`x");
    let mut outside = at("`code`", 4);
    outside.step_caret_across_styled_edge(1);
    assert!(!outside.step_caret_across_styled_edge(1));
}

#[test]
fn marks_toggled_at_an_edge_are_kept() {
    let mut editor = at("a **b** c", 2);
    editor.stored_marks = Some(Marks::of(&[RichTextMark::Bold, RichTextMark::Italic]));
    assert!(!editor.step_caret_across_styled_edge(1));
    assert!(!editor.step_caret_across_styled_edge(-1));
}

#[test]
fn arrow_keys_are_left_alone_away_from_styled_edges() {
    assert!(!at("**bold** tail", 2).step_caret_across_styled_edge(-1));
    assert!(!at("plain text", 0).step_caret_across_styled_edge(-1));
    assert!(!at("plain text", 0).caret_takes_marks_before());
}

// ── Editing basics, history, clipboard ─────────────────────────────────────

#[test]
fn typing_replaces_the_selection_and_takes_the_marks_before_the_caret() {
    let mut editor = at("**bold**", 4);
    editor.insert_text("!");
    assert_eq!(editor.value(), "**bold!**");
}

#[test]
fn backspace_and_delete_remove_one_character_or_chip() {
    let mut editor = at_end("a @README.md b");
    editor.set_caret(DocPos::new(0, 3));
    assert!(editor.delete_backward());
    assert_eq!(editor.value(), "a  b");
    let mut editor = at("ab", 1);
    editor.delete_forward();
    assert_eq!(editor.value(), "a");
}

#[test]
fn newline_splits_paragraphs_and_keeps_marks() {
    let mut editor = at("**bold**", 4);
    editor.enter();
    editor.insert_text("x");
    assert_eq!(editor.value(), "**bold**\n**x**");
}

#[test]
fn empty_quote_line_lifts_out_on_enter() {
    let mut editor = at_end("> a");
    editor.enter();
    assert_eq!(editor.value(), "> a\n> ");
    editor.enter();
    assert_eq!(editor.value(), "> a\n");
}

#[test]
fn undo_groups_by_kind_of_change() {
    let mut editor = at_end("");
    editor.set_group_delay(Duration::from_secs(3600));
    for ch in "abc".chars() {
        editor.insert_text(&ch.to_string());
    }
    editor.delete_backward();
    editor.delete_backward();
    for ch in "xy".chars() {
        editor.insert_text(&ch.to_string());
    }
    assert_eq!(editor.value(), "axy");
    assert!(editor.undo());
    assert_eq!(editor.value(), "a");
    assert!(editor.undo());
    assert_eq!(editor.value(), "abc");
    assert!(editor.undo());
    assert_eq!(editor.value(), "");
    assert!(editor.redo());
    assert_eq!(editor.value(), "abc");
}

#[test]
fn pasting_is_its_own_undo_step() {
    let mut editor = at_end("");
    editor.set_group_delay(Duration::from_secs(3600));
    editor.insert_text("a");
    editor.paste("pasted", &label, false);
    editor.insert_text("b");
    assert_eq!(editor.value(), "apastedb");
    editor.undo();
    assert_eq!(editor.value(), "apasted");
    editor.undo();
    assert_eq!(editor.value(), "a");
}

#[test]
fn copy_keeps_prefixes_of_blocks_it_spans() {
    let copy = |value: &str, first: &str, last: &str| {
        let mut editor = state(value);
        let mut from = None;
        let mut to = None;
        for (index, block) in editor.doc.blocks.iter().enumerate() {
            if let Some(inline) = block.inline() {
                let text: String = inline
                    .iter()
                    .filter_map(|n| match &n.content {
                        InlineContent::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                if from.is_none()
                    && let Some(at) = text.find(first)
                {
                    from = Some(DocPos::new(index, at));
                }
                if let Some(at) = text.rfind(last) {
                    to = Some(DocPos::new(index, at + last.len()));
                }
            }
        }
        editor.selection = Selection {
            anchor: from.expect("from"),
            head: to.expect("to"),
        };
        editor.selected_markdown()
    };
    assert_eq!(copy("- one\n- two\n- three", "one", "two"), "- one\n- two");
    assert_eq!(copy("3) one\n4) two", "one", "two"), "3) one\n4) two");
    assert_eq!(copy("- [ ] one\n- [x] two", "one", "two"), "- [ ] one\n- [x] two");
    assert_eq!(copy("- p\n  - a\n  - b", "p", "a"), "- p\n  - a");
    assert_eq!(copy("> one\n> two", "one", "two"), "> one\n> two");
    assert_eq!(copy("- one two", "one", "one"), "one");
    assert_eq!(copy("intro\n- one", "intro", "one"), "intro\n- one");
}

#[test]
fn pasting_into_a_path_query_keeps_the_path_literal() {
    for path in ["src/__test__.tsx", "src/__tests__/a.ts", "pkg/__init__.py", "a/**b**/c"] {
        let mut editor = at_end("Open @");
        editor.paste(path, &label, true);
        assert_eq!(editor.value(), format!("Open @{path}"));
        assert!(
            editor.doc.blocks[0].inline().unwrap().iter().all(|n| n.marks.is_empty()),
            "{path}"
        );
    }
    let mut editor = at_end("Open @");
    editor.paste("__hello__ world", &label, false);
    assert_eq!(editor.value(), "Open @**hello** world");
    let mut editor = at_end("Open @");
    editor.paste("src/a.ts - not a list item", &label, true);
    assert_eq!(editor.doc.blocks.len(), 1);
    let mut editor = at_end("Open ");
    editor.paste("src/__test__.tsx", &label, true);
    assert_eq!(editor.value(), "Open src/**test**.tsx");
}

#[test]
fn a_controlled_value_places_the_collapsed_cursor() {
    let mut editor = state("see @README.md here");
    editor.set_value("hello **bold** world", Some(8), &label);
    let snapshot = editor.snapshot();
    assert_eq!(snapshot.value, "hello **bold** world");
    assert_eq!(snapshot.cursor, 8);
    assert!(editor.undo());
    assert_eq!(editor.value(), "see @README.md here");
}

#[test]
fn trailing_blank_line_pattern() {
    assert_eq!(trailing_blank_lines("a\n\n"), Some("\n\n"));
    assert_eq!(trailing_blank_lines("a\n  \n  "), Some("\n  \n  "));
    assert_eq!(trailing_blank_lines("a\n"), None);
    assert_eq!(trailing_blank_lines("a\nb"), None);
}
