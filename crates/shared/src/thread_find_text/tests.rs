//! Port of upstream/packages/shared/src/threadFindText.test.ts, plus the segment-count half of the
//! `highlights the indexed occurrences` cases in apps/web/src/components/ChatMarkdown.test.tsx
//! (the DOM half belongs to the app's find highlighter).

use pretty_assertions::assert_eq;
use serde_json::json;
use turin_contracts::assistant_citations::AssistantCitation;

use super::*;
use crate::assistant_citations::serialize_assistant_citation;
use crate::thread_search::count_thread_search_occurrences;

fn message(
    role: OrchestrationV2ConversationMessageRole,
    text: &str,
    has_context: bool,
) -> SearchableMessage<'_> {
    SearchableMessage {
        role,
        text,
        streaming: false,
        has_context,
    }
}

fn user(text: &str) -> Option<Vec<String>> {
    searchable_message_segments(
        &message(OrchestrationV2ConversationMessageRole::User, text, false),
        None,
        &[],
    )
}

fn assistant(text: &str, cwd: Option<&str>) -> Vec<String> {
    searchable_message_segments(
        &message(
            OrchestrationV2ConversationMessageRole::Assistant,
            text,
            false,
        ),
        cwd,
        &[],
    )
    .expect("assistant messages are indexed")
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn excludes_review_attachments_rendered_as_context_chips() {
    let text = [
        "Before **review**",
        r#"<review_comment sectionId="turn:2" sectionTitle="Turn 2" filePath="hidden.ts" startIndex="3" endIndex="14" rangeLabel="L4">"#,
        "Keep **this literal** comment.",
        "```diff",
        "+ hidden patch content",
        "```",
        "</review_comment>",
        "After review",
    ]
    .join("\n");
    assert_eq!(
        user(&text),
        Some(strings(&["Before review", " After review"]))
    );
}

#[test]
fn keeps_malformed_review_tags_visible_matching_the_message_renderer() {
    let text = "<review_comment>not a valid attachment</review_comment>";
    assert_eq!(user(text), Some(strings(&[text])));
}

#[test]
fn excludes_structured_context_chips_without_joining_text_across_them() {
    let segments = searchable_message_segments(
        &message(
            OrchestrationV2ConversationMessageRole::User,
            "before[hidden label](t3-context://v1/terminal/terminal_1)after",
            true,
        ),
        None,
        &[],
    );
    assert_eq!(segments, Some(strings(&["before", "after"])));
}

#[test]
fn does_not_upgrade_literal_legacy_tags_in_messages_with_structured_context() {
    let text =
        "<terminal_context>\n- Terminal 1 line 12:\n  12 | visible output\n</terminal_context>";
    let with_context = searchable_message_segments(
        &message(OrchestrationV2ConversationMessageRole::User, text, true),
        None,
        &[],
    )
    .unwrap();
    assert!(with_context.join("\n").contains("visible output"));
    assert_eq!(user(text), Some(Vec::new()));
}

#[test]
fn keeps_context_reference_syntax_inside_code_searchable() {
    assert_eq!(
        user("`[label](t3-context://v1/terminal/terminal_1)`"),
        Some(strings(&["[label](t3-context://v1/terminal/terminal_1)"]))
    );
}

#[test]
fn excludes_repeated_legacy_attachments_containing_literal_context_tags() {
    let context = "<terminal_context>\n- Terminal 1 line 12:\n  12 | <terminal_context>literal</terminal_context>\n</terminal_context>";
    assert_eq!(
        user(&format!("Fix this\n\n{context}\n\n{context}")),
        Some(strings(&["Fix this"]))
    );
}

#[test]
fn searches_displayed_prose_and_file_chip_labels_without_hidden_paths() {
    assert_eq!(
        assistant(
            "[important description](/tmp/actual.ts). `/tmp/inline-example.ts:42`",
            None
        ),
        strings(&["important description actual.ts. inline-example.ts · L42"])
    );
    assert_eq!(
        assistant("[label](src/main.ts#L3C2)", Some("/workspace/repo")),
        strings(&["label main.ts · L3:C2"])
    );
    assert_eq!(
        assistant("[label](src/main.ts#L3C2)", None),
        strings(&["label"])
    );
}

#[test]
fn includes_the_same_parent_suffixes_for_duplicate_filenames_as_the_renderer() {
    assert_eq!(
        assistant(
            "[first](src/main.ts) and `/workspace/repo/tests/main.ts:2`",
            Some("/workspace/repo")
        ),
        strings(&["first main.ts · repo/src and main.ts · repo/tests · L2"])
    );
    assert_eq!(
        assistant(
            "[first](src/main.ts) and `/workspace/repo/src/main.ts:2`",
            Some("/workspace/repo")
        ),
        strings(&["first main.ts and main.ts · L2"])
    );
}

#[test]
fn keeps_fence_paths_literal_and_indexes_file_chip_labels_in_user_messages() {
    assert_eq!(
        assistant("```text\n/tmp/file.ts:42\n```", None),
        strings(&["/tmp/file.ts:42\n"])
    );
    assert_eq!(user("`/tmp/file.ts:42`"), Some(strings(&["file.ts · L42"])));
}

#[test]
fn indexes_nested_disclosure_summaries_and_bodies_in_rendered_order() {
    assert_eq!(
        assistant(
            "<details><summary>Outer</summary><p>first</p><details><summary>Inner</summary><p>second</p></details></details>",
            None
        ),
        strings(&["Outer", "first", "Inner", "second"])
    );
}

#[test]
fn uses_insight_line_breaks_without_splitting_ordinary_assistant_prose() {
    assert_eq!(
        assistant("★ Insight ─────\nfirst line\nsecond line", None),
        strings(&["★ Insight ─────", " first line", " second line"])
    );
    assert_eq!(
        assistant("first line\nsecond line", None),
        strings(&["first line second line"])
    );
}

#[test]
fn insight_line_breaks_follow_the_multiline_regex() {
    use super::should_preserve_assistant_line_breaks as preserve;
    // A bare line matches because `\s` consumes the line terminator.
    assert!(preserve("\u{2605} Insight\na\nb"));
    assert!(preserve("x\n\u{2605} Insight\na\nb"));
    assert!(preserve("\u{2605} Insight\r\na\r\nb"));
    // `m` also starts lines after a lone CR, U+2028 and U+2029.
    assert!(preserve("x\r\u{2605} Insight\ra\rb"));
    assert!(preserve("x\u{2028}\u{2605} Insight\u{2028}a"));
    assert!(preserve("x\u{2029}\u{2605} Insight\u{2029}a"));
    assert!(preserve("\u{2605} Insight \u{2500}\u{2500}"));
    assert!(preserve("\u{2605} Insight\u{2500}"));
    // Not at a line start, no separator, or end of text.
    assert!(!preserve("x \u{2605} Insight\na"));
    assert!(!preserve("\u{2605} Insights\na"));
    assert!(!preserve("\u{2605} Insight"));
    assert!(!preserve("\u{2605} Insight\u{0085}a"));
    assert_eq!(
        assistant("\u{2605} Insight\na\nb", None),
        strings(&["\u{2605} Insight", " a", " b"])
    );
}

fn skills() -> Vec<InlineSkill> {
    vec![InlineSkill {
        name: "test-t3-app".to_owned(),
        display_name: Some("T3 App Testing".to_owned()),
    }]
}

#[test]
fn indexes_skill_labels_in_prose_but_leaves_links_and_code_literal() {
    let segments = searchable_message_segments(
        &message(
            OrchestrationV2ConversationMessageRole::Assistant,
            "Use $test-t3-app now.\n\n`$test-t3-app`\n\n[$test-t3-app](https://example.com)",
            false,
        ),
        None,
        &skills(),
    );
    assert_eq!(
        segments,
        Some(strings(&[
            "Use T3 App Testing now.",
            "$test-t3-app",
            "$test-t3-app"
        ]))
    );
}

fn citation(extra: Option<&str>) -> AssistantCitation {
    let mut value = json!({
        "version": 1,
        "environmentId": "environment",
        "threadId": "thread",
        "messageId": "message",
        "text": "cited   needle",
        "start": 0,
        "end": 14,
        "prefix": "",
        "suffix": "",
    });
    if let Some(comment) = extra {
        value["comment"] = json!(comment);
    }
    serde_json::from_value(value).unwrap()
}

#[test]
fn indexes_the_citation_chip_label_instead_of_its_link_text() {
    let text = format!(
        "Please fix {} thanks",
        serialize_assistant_citation(&citation(None))
    );
    assert_eq!(
        user(&text),
        Some(strings(&["Please fix cited needle thanks"]))
    );
    let commented = serialize_assistant_citation(&citation(Some("my note")));
    assert_eq!(user(&commented), Some(strings(&["my note"])));
}

// ChatMarkdown.test.tsx: `highlights the indexed occurrences of $query in $text`.

const ARTIFACT_TEMPLATE_DIRECTIVE: &str = r#"::artifact-template{skill_name="artifact-template-hello-world" skill_directory="/Users/test/.codex/skills/artifact-template-hello-world" display_name="Hello World" artifact_kind="document"}"#;

fn occurrences(text: &str, query: &str, is_user: bool) -> usize {
    let role = if is_user {
        OrchestrationV2ConversationMessageRole::User
    } else {
        OrchestrationV2ConversationMessageRole::Assistant
    };
    searchable_message_segments(&message(role, text, false), None, &skills())
        .unwrap_or_default()
        .iter()
        .map(|segment| count_thread_search_occurrences(segment, query))
        .sum()
}

#[test]
fn counts_the_indexed_occurrences_the_renderer_highlights() {
    let cases: &[(&str, &str, usize, bool)] = &[
        (
            "```mermaid\ngraph TD; SearchSourceAlpha-->B\n```",
            "SearchSourceAlpha",
            1,
            false,
        ),
        // Insight breaks split the phrase across lines.
        (
            "★ Insight ─────\nfirst line\nsecond line",
            "first line second",
            0,
            false,
        ),
        (
            "```ts title=\"src/needle.ts\"\nconst a = 1;\n```",
            "needle",
            0,
            false,
        ),
        ("```weirdlang\nconst a = 1;\n```", "weirdlang", 0, false),
        ("Use $test-t3-app now", "T3 App Testing", 1, false),
        ("`/tmp/file.ts:42`", "file.ts · L42", 1, true),
        (
            "> [!NOTE]\n> Searchable alert",
            "Searchable alert",
            1,
            false,
        ),
        (
            "<details><summary>Folded</summary><p>Hidden needle</p></details>",
            "Hidden needle",
            1,
            false,
        ),
        (ARTIFACT_TEMPLATE_DIRECTIVE, "Hello World", 1, false),
        (ARTIFACT_TEMPLATE_DIRECTIVE, "Document template", 1, false),
        (ARTIFACT_TEMPLATE_DIRECTIVE, "World Document", 0, false),
        (ARTIFACT_TEMPLATE_DIRECTIVE, "Use template", 0, false),
    ];
    for (text, query, count, is_user) in cases {
        assert_eq!(
            occurrences(text, query, *is_user),
            *count,
            "{query} in {text:?}"
        );
    }
}

/// `golden.json` holds inputs and the segments upstream's `searchableMessageSegments` produced
/// for them (run with the real unified/parse5 pipeline, with the `test-t3-app` skill), so the
/// reimplemented pipeline is checked against the thing it replaces. `turinSegments` marks the one
/// intended difference: upstream skips `remarkNormalizeListItemIndentation` here (see PORTING.md
/// Deviations) and Turin applies it, as the renderer does.
#[test]
fn matches_the_segments_the_upstream_pipeline_produces() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Golden {
        role: String,
        text: String,
        cwd: Option<String>,
        segments: Vec<String>,
        turin_segments: Option<Vec<String>>,
    }
    let cases: Vec<Golden> = serde_json::from_str(include_str!("golden.json")).unwrap();
    assert!(cases.len() > 100);
    for case in cases {
        let role = if case.role == "user" {
            OrchestrationV2ConversationMessageRole::User
        } else {
            OrchestrationV2ConversationMessageRole::Assistant
        };
        let actual = searchable_message_segments(
            &message(role, &case.text, false),
            case.cwd.as_deref(),
            &skills(),
        )
        .unwrap();
        let expected = case.turin_segments.as_ref().unwrap_or(&case.segments);
        assert_eq!(&actual, expected, "{} message {:?}", case.role, case.text);
    }
}

#[test]
fn plan_segments_start_with_the_title_and_index_skill_tokens_as_written() {
    assert_eq!(
        searchable_plan_segments("# Plan: ship it\n\nUse $test-t3-app now.", None),
        strings(&["Plan: ship it", "Use $test-t3-app now."])
    );
    assert_eq!(
        searchable_plan_segments("no heading here", None)[0],
        "Proposed plan"
    );
}

#[test]
fn system_messages_are_not_indexed() {
    let segments = searchable_message_segments(
        &message(
            OrchestrationV2ConversationMessageRole::System,
            "hidden",
            false,
        ),
        None,
        &[],
    );
    assert_eq!(segments, None);
}

#[test]
fn streaming_assistant_messages_have_no_empty_response_fallback() {
    let streaming = SearchableMessage {
        role: OrchestrationV2ConversationMessageRole::Assistant,
        text: "",
        streaming: true,
        has_context: false,
    };
    assert_eq!(
        searchable_message_segments(&streaming, None, &[]),
        Some(Vec::new())
    );
}

#[test]
fn applies_list_item_indentation_normalization_like_the_renderer() {
    // Upstream's find pipeline never runs this plugin; the renderer does, so find shows prose.
    assert_eq!(
        assistant("-       over indented", None),
        strings(&["over indented"])
    );
}

#[test]
fn block_tags_are_shared_with_the_renderer_side_walker() {
    assert!(is_thread_find_block_tag("p"));
    assert!(is_thread_find_block_tag("br"));
    assert!(!is_thread_find_block_tag("span"));
    assert!(!is_thread_find_block_tag("code"));
}

// Expected values below come from running upstream `searchableMessageSegments`.

#[test]
fn unresolved_leaf_directives_are_paragraphs_of_their_own() {
    assert_eq!(
        user("x\n::artifact-template{artifact_kind=\"bogus\" display_name=\"A *b* _c_\"}\ny"),
        Some(strings(&[
            "x",
            r#"::artifact-template{artifact_kind="bogus" display_name="A *b* _c_"}"#,
            "y"
        ]))
    );
    assert_eq!(
        user("para\n::artifact-template\nafter"),
        Some(strings(&["para", "::artifact-template", "after"]))
    );
    assert_eq!(
        user("Use $test-t3-app\n::artifact-template{a=1}\nnow"),
        Some(strings(&[
            "Use $test-t3-app",
            r#"::artifact-template{a="1"}"#,
            "now"
        ]))
    );
}

#[test]
fn unresolved_leaf_directives_are_rebuilt_from_the_parsed_node() {
    assert_eq!(
        assistant("x\n::artifact-template{}\ny", None),
        strings(&["x", "::artifact-template", "y"])
    );
    assert_eq!(
        assistant("x\n::artifact-template{ a=\"1\"   b }  \ny", None),
        strings(&["x", r#"::artifact-template{a="1" b=""}"#, "y"])
    );
    assert_eq!(
        assistant(
            "x\n::artifact-template[lab *e*]{.a #b .c a='q\"r'}\ny",
            None
        ),
        strings(&[
            "x",
            r#"::artifact-template[lab e]{class="a c" id="b" a="q\"r"}"#,
            "y"
        ])
    );
    assert_eq!(
        assistant("> x\n> ::artifact-template{a=\"&amp; <b>\"}\n> y", None),
        strings(&["x", r#"::artifact-template{a="& <b>"}"#, "y"])
    );
}

#[test]
fn unresolved_leaf_directives_follow_list_tightness() {
    // A tight list item unwraps its paragraphs, so the pieces share one segment.
    assert_eq!(
        assistant("- x\n  ::artifact-template{a=\"1\"}\n  y", None),
        strings(&[r#"x ::artifact-template{a="1"} y"#])
    );
    assert_eq!(
        assistant("- x\n\n  ::artifact-template{a=\"1\"}\n  y", None),
        strings(&["x", r#"::artifact-template{a="1"}"#, "y"])
    );
}

#[test]
fn raw_html_is_parsed_with_scripting_disabled() {
    assert_eq!(
        assistant("<noscript><p>x</p></noscript>y", None),
        strings(&["x", "y"])
    );
}
