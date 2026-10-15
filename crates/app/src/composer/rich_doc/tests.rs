//! Port of upstream/apps/web/src/composer-rich-text-doc.test.ts (the cases
//! that exercise the document model; ProseMirror schema checks, hard breaks
//! and the caret-stop cases are covered in `editor_state`).

use super::*;
use crate::composer::logic::collapse_expanded_composer_cursor;

fn label(name: &str) -> SkillMeta {
    SkillMeta {
        label: name.to_owned(),
        description: None,
    }
}

fn round_trip(value: &str) -> DocMap {
    serialize_doc(&build_doc(value, &label, BuildOptions::rich()))
}

fn round_trip_plain(value: &str) -> DocMap {
    serialize_doc(&build_doc(value, &label, BuildOptions::plain()))
}

fn assert_round_trips(values: &[&str]) {
    for value in values {
        assert_eq!(round_trip(value).value, *value, "round trip of {value:?}");
    }
}

fn assert_collapsed_round_trip(value: &str) {
    let map = round_trip(value);
    assert_eq!(map.value, value);
    for flat in 0..=map.doc_length {
        assert_eq!(
            collapsed_to_flat(&map, flat_to_collapsed(&map, flat)),
            flat,
            "{value:?} at {flat}"
        );
    }
}

#[test]
fn canonicalizes_skill_aliases_while_preserving_amounts() {
    for prefix in ["€", "£", "¥", "₹", "₩", "₿", "\u{11FDD}"] {
        let value = format!("Use {prefix}my-skill for {prefix}20 please");
        let expected = format!("Use $my-skill for {prefix}20 please");
        assert_eq!(round_trip(&value).value, expected);
        assert_eq!(round_trip_plain(&value).value, expected);
    }
}

#[test]
fn round_trips_through_the_document() {
    assert_round_trips(&[
        "",
        "\n\n",
        "plain text",
        "hello **bold** world",
        "a *italic* word and `code` here",
        "struck ~~out~~ now",
        "**`x`**",
        "*`x`*",
        "~~`x`~~",
        "**a `code` c**",
        "***bold italic*** keeps nesting",
        "line one\nline two",
        "trailing newline\n",
        "1. foo\n2. asdf\n",
        "- [ ] buy milk",
        "-   [ ]  buy milk",
        "\t-\t[x]\t\titem",
        "- [ ]  ",
        "- [ ]\n  - [ ] child",
        "  - [ ] first\n - [ ] second\n  - [ ] child",
        "**before @README.md after**",
        "*a **b** c*",
        "*a**b***",
        "**a*b***",
        "**a *b* c**",
        "literal \u{FFFC} **before @README.md after**",
        "- [x] done\n- [ ] next",
        "- [ ] parent\n  - [ ] child\n  - [ ] sibling\n- [ ] uncle",
        "- [ ] empty task follows\n- [ ]",
        "- [ ] **bold** task with @README.md",
        "para\n- [ ] task\npara",
        "- [ ]No space stays literal",
        "-[ ] also literal",
        "@README.md explain this",
        "@\"docs/My File.md\" and $my-skill please",
        "snake_case stays literal",
        "unmatched ** stays literal",
        "**bold** then @README.md then *italic*",
    ]);
}

#[test]
fn maps_editable_positions() {
    for value in [
        "",
        "\n",
        "text\n\n",
        "- [ ]\n- [ ] next\n",
        "- [ ] parent\n  - [ ] child\n- [ ]",
        "para\n- [ ] task\npara",
        "**before @README.md after**",
    ] {
        let doc = build_doc(value, &label, BuildOptions::rich());
        let map = serialize_doc(&doc);
        for flat in 0..=map.doc_length {
            let pos = flat_to_pos(&doc, &map, flat);
            assert!(doc.blocks[pos.block].is_textblock(), "{value:?} at {flat}");
            assert_eq!(pos_to_flat(&map, pos), flat, "{value:?} at {flat}");
            assert_eq!(collapsed_to_flat(&map, flat_to_collapsed(&map, flat)), flat);
        }
    }
}

#[test]
fn applies_a_shared_mark_to_text_on_both_sides_of_a_chip() {
    let doc = build_doc("**before @README.md after**", &label, BuildOptions::rich());
    let Block::Paragraph(inline) = &doc.blocks[0] else {
        panic!("paragraph");
    };
    assert_eq!(inline.len(), 3);
    assert!(inline.iter().all(|node| node.marks.has(RichTextMark::Bold)));
    assert_eq!(serialize_doc(&doc).value, "**before @README.md after**");
}

fn doc_of(parts: &[(&str, &[RichTextMark])]) -> Doc {
    Doc {
        blocks: vec![Block::Paragraph(
            parts
                .iter()
                .map(|(text, marks)| InlineNode::text(*text, Marks::of(marks)))
                .collect(),
        )],
    }
}

#[test]
fn preserves_crossing_mark_ranges_through_controlled_rebuilds() {
    use RichTextMark::*;
    let cases: [([RichTextMark; 1], [RichTextMark; 2], [RichTextMark; 1]); 4] = [
        ([Bold], [Bold, Italic], [Italic]),
        ([Italic], [Bold, Italic], [Bold]),
        ([Bold], [Bold, Strike], [Strike]),
        ([Strike], [Bold, Strike], [Bold]),
    ];
    for (first, second, third) in cases {
        let doc = doc_of(&[("a", &first), ("b", &second), ("c", &third)]);
        let serialized = serialize_doc(&doc).value;
        let rebuilt = build_doc(&serialized, &label, BuildOptions::rich());
        assert_eq!(rebuilt, doc, "{serialized}");
        assert_eq!(serialize_doc(&rebuilt).value, serialized);
    }
}

#[test]
fn keeps_boundary_whitespace_outside_emphasis() {
    use RichTextMark::*;
    type Run<'a> = (&'a str, Vec<RichTextMark>);
    let cases: Vec<(Vec<Run>, &str)> = vec![
        (vec![("hello ", vec![Bold])], "**hello** "),
        (vec![("  ", vec![Bold])], "  "),
        (vec![(" left ", vec![Bold, Italic])], " ***left*** "),
        (
            vec![
                ("one ", vec![Bold]),
                (" two ", vec![Bold, Italic]),
                (" three", vec![Bold]),
            ],
            "**one  *two*  three**",
        ),
        (
            vec![("hello ", vec![Bold]), ("world ", vec![Bold, Italic])],
            "**hello *world*** ",
        ),
        (vec![(" hello ", vec![Code])], "` hello `"),
        (vec![(" hello ", vec![Bold, Code])], "**` hello `**"),
        (vec![(" ", vec![Bold, Code])], "**` `**"),
    ];
    for (parts, expected) in cases {
        let parts: Vec<(&str, &[RichTextMark])> =
            parts.iter().map(|(t, m)| (*t, m.as_slice())).collect();
        let doc = doc_of(&parts);
        let map = serialize_doc(&doc);
        assert_eq!(map.value, expected);
        let rebuilt = build_doc(&map.value, &label, BuildOptions::rich());
        assert_eq!(serialize_doc(&rebuilt).value, map.value);
        for flat in 0..=map.doc_length {
            assert_eq!(collapsed_to_flat(&map, flat_to_collapsed(&map, flat)), flat);
        }
    }
}

#[test]
fn keeps_chip_sources_canonical_through_the_document() {
    let map = round_trip("explain @README.md with **care**\nsecond line *here*");
    assert_eq!(
        map.value,
        "explain @README.md with **care**\nsecond line *here*"
    );
    assert!(map.runs.iter().any(|run| run.kind == RunKind::Token));
}

#[test]
fn normalizes_uppercase_checkboxes_to_lowercase() {
    assert_eq!(round_trip("- [X] done").value, "- [x] done");
}

#[test]
fn round_trips_byte_identically_in_plain_mode() {
    for value in [
        "plain text",
        "hello **bold** stays literal",
        "a *italic* stays literal",
        "some `code` stays literal",
        "struck ~~out~~ stays literal",
        "- [ ] stays a paragraph",
        "- [x] stays a paragraph",
        "line one\nline two",
        "@README.md explain this",
        "**bold** then @README.md then *italic*",
        "- one\n- two",
        "1. first\n2. second",
        "- parent\n  - child",
        "* star\n+ plus",
        "-no space stays literal",
        "> quoted",
        "> a\n>b",
        "---",
        "- - -",
        "# Heading",
        "#1234",
        "```ts\nconst a = 1;\n```",
    ] {
        assert_eq!(round_trip_plain(value).value, value);
    }
}

#[test]
fn round_trips_lists() {
    assert_round_trips(&[
        "- one\n- two",
        "* star\n* star two",
        "+ plus",
        "* star\n+ plus",
        "1. first\n2. second",
        "1) paren\n2) paren",
        "3. starts at three\n4. four",
        "01. zero padded\n02. two",
        "- parent\n  - child\n  - sibling\n- uncle",
        "1. ordered\n   - bullet child\n2. next",
        "- outer\n  1. inner ordered\n  2. more\n- outer again",
        "-   wide space item",
        "-",
        "1.",
        "- \n- second",
        "- **bold** item with @README.md",
        "- [ ] task\n- bullet after",
        "- bullet\n- [x] task after",
        "para\n- item\npara",
        "\t- tab indented\n\t- again",
        "  - leading indent only",
        "- item\n\n- after a blank",
        "- item one\n```ts\ncode\n```\n- item two",
        "-no space stays literal",
        "1.no space stays literal",
        "10. ten\n11. eleven",
    ]);
}

#[test]
fn maps_every_list_offset_through_collapsed_coordinates_and_back() {
    for value in [
        "- one\n- two",
        "1. a\n   - b\n2. c",
        "- **bold** @README.md tail",
        "-",
    ] {
        assert_collapsed_round_trip(value);
    }
}

#[test]
fn clamps_offsets_inside_a_list_marker_to_the_start_of_the_item_text() {
    let map = round_trip("- item");
    for collapsed in 0..="- ".len() {
        assert_eq!(collapsed_to_flat(&map, collapsed), 0);
    }
    assert_eq!(collapsed_to_flat(&map, "- it".len()), 2);
    assert_eq!(flat_to_collapsed(&map, 0), "- ".len());
}

#[test]
fn round_trips_quotes_rules_and_headings() {
    assert_round_trips(&[
        "> quoted",
        "> line one\n> line two",
        ">no space",
        ">  two spaces",
        "> a\n>b",
        ">",
        "> **bold** and @README.md inside",
        "> - looks like a list but stays quote text",
        "> > nested stays literal inside the quote",
        "before\n> quoted\nafter",
        "> quote\n\n> another",
        "- item\n> quote after list",
        "---",
        "***",
        "___",
        "- - -",
        "* * *",
        "-----",
        "---   ",
        "text\n---\nmore",
        "- item\n---\n- item two",
        "```\n---\n```",
        "--",
        "-- -",
        "---text",
        "# Heading",
        "## Two",
        "###### Six",
        "####### seven hashes stays a paragraph",
        "#  two spaces",
        "#\tTab",
        "# Trailing hashes stay literal #",
        "#1234",
        "#1234 is a pull request, not a heading",
        "# Heading with **bold** and @README.md",
        "#",
        "# ",
        "text\n# Heading\ntext",
        "# Heading\n- item\n> quote\n---",
    ]);
}

#[test]
fn parses_rules_ahead_of_lists_and_emphasis() {
    let blocks = build_blocks(
        "- - -\n***\n___\ntext\n---\n- item",
        &label,
        BuildOptions::rich(),
    );
    let kinds: Vec<&str> = blocks
        .iter()
        .map(|block| match block {
            Block::Rule { .. } => "rule",
            Block::Paragraph(_) => "paragraph",
            Block::Item { .. } => "item",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["rule", "rule", "rule", "paragraph", "rule", "item"]);
}

#[test]
fn parses_the_blocks_it_renders_as_the_right_node_types() {
    let doc = build_doc(
        "# Title\n> quote\n---\n#1234 ref\n- - -",
        &label,
        BuildOptions::rich(),
    );
    assert!(matches!(doc.blocks[0], Block::Heading { level: 1, .. }));
    assert!(matches!(doc.blocks[1], Block::Quote { .. }));
    assert!(matches!(doc.blocks[2], Block::Rule { .. }));
    assert!(matches!(doc.blocks[3], Block::Paragraph(_)));
    assert!(matches!(doc.blocks[4], Block::Rule { .. }));
    // The empty line a final rule keeps for the caret.
    assert!(matches!(&doc.blocks[5], Block::Paragraph(inline) if inline.is_empty()));
}

#[test]
fn gives_the_caret_a_line_after_the_final_rule() {
    for (value, expected) in [("---", "---"), ("a\n---", "a\n---"), ("---\n", "---")] {
        let doc = build_doc(value, &label, BuildOptions::rich());
        let map = serialize_doc(&doc);
        assert_eq!(map.value, expected);
        let pos = flat_to_pos(&doc, &map, map.doc_length);
        assert!(doc.blocks[pos.block].is_textblock());
    }
}

#[test]
fn maps_every_block_offset_through_collapsed_coordinates_and_back() {
    for value in [
        "> a\n> b",
        "> **q** @README.md",
        "text\n---\nmore",
        "---\ntext",
        "# Heading text",
        "## **b** @README.md",
        "```ts\nconst a = 1;\n```",
        "```\n```",
        "before\n```ts\ncode\n```\nafter",
    ] {
        assert_collapsed_round_trip(value);
    }
}

#[test]
fn clamps_offsets_inside_a_quote_or_heading_marker_to_the_start_of_the_text() {
    for (value, prefix) in [("> quoted", "> "), ("# Heading", "# ")] {
        let map = round_trip(value);
        for collapsed in 0..=prefix.len() {
            assert_eq!(collapsed_to_flat(&map, collapsed), 0);
        }
        assert_eq!(flat_to_collapsed(&map, 0), prefix.len());
    }
}

#[test]
fn round_trips_fenced_blocks() {
    assert_round_trips(&[
        "```\ncode\n```",
        "```ts\nconst a = 1;\n```",
        "```ts\nconst a = 1;\n```\n",
        "before\n```ts\nconst a = 1;\n```\nafter",
        "```\n```",
        "```ts\nline one\nline two\nline three\n```",
        "```ts\n  indented\n    deeper\n```",
        "```js title=example\ncode\n```",
        "~~~py\ncode\n~~~",
        "````\n```\n````",
        "```ts\ncode without a closing fence",
        "```",
        "```ts\n**not bold** and @README.md stay literal\n```",
        "```ts\ncode\n````",
        "- [ ] task\n```ts\ncode\n```\n- [ ] after",
        "  ```ts\nindented fence stays a paragraph\n  ```",
    ]);
}

#[test]
fn reads_blocks_written_with_crlf_line_endings() {
    let value = "# Title\r\n> quote\r\n---\r\n```ts\r\nconst a = 1;\r\n```\r\nafter";
    let doc = build_doc(value, &label, BuildOptions::rich());
    assert!(matches!(doc.blocks[0], Block::Heading { .. }));
    assert!(matches!(doc.blocks[1], Block::Quote { .. }));
    assert!(matches!(doc.blocks[2], Block::Rule { .. }));
    assert!(matches!(doc.blocks[3], Block::Code(_)));
    assert!(matches!(doc.blocks[4], Block::Paragraph(_)));
    assert_eq!(serialize_doc(&doc).value, value);
}

#[test]
fn keeps_the_end_of_the_code_inside_the_fence_rather_than_after_it() {
    let map = round_trip("```ts\nfunc();\n```");
    let end_of_code = "func();".len();
    assert_eq!(flat_to_collapsed(&map, end_of_code), "```ts\nfunc();".len());
    assert_eq!(flat_to_markdown(&map, end_of_code), "```ts\nfunc();".len());
    assert_eq!(
        collapsed_to_flat(&map, flat_to_collapsed(&map, end_of_code)),
        end_of_code
    );
}

#[test]
fn keeps_the_caret_inside_an_empty_fence() {
    let map = round_trip("before\n```\n```");
    let inside = "before\n".len();
    assert_eq!(flat_to_collapsed(&map, inside), "before\n```".len());
    assert_eq!(
        collapsed_to_flat(&map, flat_to_collapsed(&map, inside)),
        inside
    );
}

#[test]
fn still_places_the_end_of_an_inline_mark_after_its_markers() {
    let map = round_trip("a **bold** c");
    assert_eq!(flat_to_markdown(&map, 6), 10);
}

#[test]
fn clamps_offsets_inside_a_fence_to_the_edge_of_the_code() {
    let value = "```ts\nab\n```";
    let map = round_trip(value);
    assert_eq!(map.value, value);
    for collapsed in 0..="```ts\n".len() {
        assert_eq!(collapsed_to_flat(&map, collapsed), 0);
    }
    assert_eq!(collapsed_to_flat(&map, "```ts\na".len()), 1);
    for collapsed in "```ts\nab".len()..=value.len() {
        assert_eq!(collapsed_to_flat(&map, collapsed), 2);
    }
}

#[test]
fn canonicalizes_the_empty_fence() {
    assert_eq!(round_trip("```\n\n```").value, "```\n```");
    assert_eq!(round_trip("```\n").value, "```");
}

#[test]
fn keeps_chips_in_a_fence_info_string_as_source_and_later_chips_aligned() {
    let value = "```@README.md\ncode\n```\n$my-skill after";
    assert_eq!(round_trip(value).value, value);
    let doc = build_doc(value, &label, BuildOptions::rich());
    let Block::Paragraph(inline) = &doc.blocks[1] else {
        panic!("paragraph after the fence");
    };
    assert!(
        matches!(&inline[0].content, InlineContent::Atom(Atom::Skill { name, .. }) if name == "my-skill")
    );
    assert!(inline[1].is_text());

    let value = "``` @a.md\nsee @b.md\n```";
    assert_eq!(round_trip(value).value, value);
    let doc = build_doc(value, &label, BuildOptions::rich());
    let Block::Code(code) = &doc.blocks[0] else {
        panic!("code block");
    };
    assert_eq!(code.language, " @a.md");
}

#[test]
fn gives_a_bare_list_item_a_space_once_it_has_text() {
    let item = |marker: &str| Doc {
        blocks: vec![Block::Item {
            prefix: ItemPrefix::Bullet {
                indent: String::new(),
                marker: marker.to_owned(),
                space: String::new(),
            },
            content: vec![InlineNode::text("text", Marks::NONE)],
        }],
    };
    assert_eq!(serialize_doc(&item("-")).value, "- text");
    assert_eq!(round_trip("-").value, "-");
}

#[test]
fn writes_code_holding_a_closing_fence_inside_a_longer_fence() {
    let doc = Doc {
        blocks: vec![Block::Code(CodeBlock {
            language: String::new(),
            fence: "```".to_owned(),
            close: "\n```".to_owned(),
            text: "initial\n```\nsecret".to_owned(),
        })],
    };
    let stored = serialize_doc(&doc).value;
    assert_eq!(stored, "````\ninitial\n```\nsecret\n````");
    assert_eq!(
        build_doc(&stored, &label, BuildOptions::rich())
            .blocks
            .len(),
        1
    );
    assert_eq!(round_trip(&stored).value, stored);
}

#[test]
fn closes_an_unclosed_fence_that_has_a_block_after_it() {
    let doc = Doc {
        blocks: vec![
            Block::Code(CodeBlock {
                language: String::new(),
                fence: "```".to_owned(),
                close: String::new(),
                text: "code".to_owned(),
            }),
            Block::Paragraph(vec![InlineNode::text("after", Marks::NONE)]),
        ],
    };
    assert_eq!(serialize_doc(&doc).value, "```\ncode\n```\nafter");
    assert_eq!(round_trip("```\ncode").value, "```\ncode");
}

#[test]
fn counts_a_chip_source_in_a_fence_as_one_cursor_position() {
    let value = "```\nsee @b.md now\n```\nafter";
    let map = round_trip(value);
    assert_eq!(map.value, value);
    for flat in 0..=map.doc_length {
        assert_eq!(
            flat_to_collapsed(&map, flat),
            collapse_expanded_composer_cursor(value, flat_to_markdown(&map, flat), false)
        );
    }
    let after_chip = "see @b.md".len();
    assert_eq!(
        collapsed_to_flat(&map, flat_to_collapsed(&map, after_chip)),
        after_chip
    );
    let end = map.doc_length;
    assert_eq!(collapsed_to_flat(&map, flat_to_collapsed(&map, end)), end);
}

#[test]
fn maps_every_document_offset_through_collapsed_coordinates_and_back() {
    assert_collapsed_round_trip("hi **bold** @README.md bye");
}

#[test]
fn resolves_a_collapsed_cursor_identically_in_both_modes() {
    for value in [
        "plain prose with no styling at all",
        "a chip @README.md counts one character in both modes",
        "$my-skill leads the line",
        "trailing newline\n",
    ] {
        let rich = round_trip(value);
        let plain = round_trip_plain(value);
        assert_eq!(rich.value, value);
        assert_eq!(plain.value, value);
        for collapsed in 0..=value.len() {
            assert_eq!(
                flat_to_markdown(&rich, collapsed_to_flat(&rich, collapsed)),
                flat_to_markdown(&plain, collapsed_to_flat(&plain, collapsed)),
                "{value:?} at {collapsed}"
            );
        }
    }
}

#[test]
fn clamps_a_cursor_that_was_sitting_inside_a_marker_onto_the_styled_text() {
    let value = "a **bold** c";
    let plain = round_trip_plain(value);
    let rich = round_trip(value);
    assert_eq!(flat_to_markdown(&plain, collapsed_to_flat(&plain, 3)), 3);
    assert_eq!(flat_to_markdown(&rich, collapsed_to_flat(&rich, 3)), 4);
}

#[test]
fn maps_markdown_offsets_at_styled_edges_onto_document_text() {
    let map = round_trip("a **bold** c");
    assert_eq!(flat_to_markdown(&map, 2), 4);
    assert_eq!(flat_to_markdown(&map, 6), 10);
    assert_eq!(collapsed_to_flat(&map, 3), 2);
    assert_eq!(collapsed_to_flat(&map, 9), 6);
}

#[test]
fn literal_text_keeps_every_line_verbatim() {
    let options = BuildOptions {
        literal_text: true,
        ..BuildOptions::rich()
    };
    let doc = build_doc("**not bold** @README.md \n- [ ] x\n", &label, options);
    assert_eq!(doc.blocks.len(), 3);
    assert_eq!(
        serialize_doc(&doc).value,
        "**not bold** @README.md \n- [ ] x\n"
    );
}

#[test]
fn derives_list_depths_from_indentation() {
    let doc = build_doc(
        "- a\n  - b\n    - c\n  - d\n- e\npara\n- f",
        &label,
        BuildOptions::rich(),
    );
    assert_eq!(list_depths(&doc), [0, 1, 2, 1, 0, 0, 0]);
}

#[test]
fn a_pasted_path_continuation_stays_literal() {
    // The paste continues `@src/`: its leading non-space run reaches file
    // search unstyled, the rest is parsed as markdown.
    let options = BuildOptions {
        literal_length: "__init__.py".len(),
        ..BuildOptions::rich()
    };
    let doc = build_doc("__init__.py **rest**", &label, options);
    let Block::Paragraph(inline) = &doc.blocks[0] else {
        panic!("paragraph");
    };
    assert_eq!(inline[0], InlineNode::text("__init__.py ", Marks::NONE));
    assert!(inline[1].marks.has(RichTextMark::Bold));
    assert_eq!(serialize_doc(&doc).value, "__init__.py **rest**");
}
