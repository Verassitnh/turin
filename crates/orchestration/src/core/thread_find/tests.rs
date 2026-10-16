//! Behaviour of the thread-find text extraction and match selection.

use serde_json::json;
use turin_contracts::orchestration_v2::OrchestrationV2SearchThreadInput;

use super::*;

fn skill(name: &str) -> InlineSkill {
    InlineSkill {
        name: name.to_owned(),
        display_name: None,
    }
}

#[test]
fn block_elements_split_segments_and_soft_newlines_join_with_a_space() {
    let segments =
        markdown_thread_find_text("first line\ncontinued\n\nsecond", false, None, &[], false);
    assert_eq!(segments, vec!["first line continued", "second"]);
}

#[test]
fn user_messages_break_a_segment_at_every_newline() {
    let segments = searchable_message_segments(true, "one\ntwo", false, true, None, &[]);
    assert_eq!(segments, vec!["one", "two"]);
}

#[test]
fn inline_wrappers_do_not_split_a_phrase() {
    let segments =
        markdown_thread_find_text("a **bold** and `code` phrase", false, None, &[], false);
    assert_eq!(segments, vec!["a bold and code phrase"]);
}

#[test]
fn fenced_code_keeps_its_own_lines() {
    let segments =
        markdown_thread_find_text("```\nlet a = 1;\nlet b = 2;\n```", false, None, &[], false);
    assert_eq!(segments, vec!["let a = 1;\nlet b = 2;"]);
}

#[test]
fn skill_tokens_show_their_display_name_in_paragraphs_only() {
    let skills = [skill("code-review")];
    let shown = markdown_thread_find_text("$code-review now", true, None, &skills, true);
    assert_eq!(shown, vec!["Code Review now"]);
    let literal = markdown_thread_find_text("`$code-review`", true, None, &skills, true);
    assert_eq!(literal, vec!["$code-review"]);
}

#[test]
fn an_empty_finished_assistant_message_reads_as_the_placeholder() {
    assert_eq!(
        searchable_message_segments(false, "", false, false, None, &[]),
        vec!["(empty response)"]
    );
    assert!(searchable_message_segments(false, "", true, false, None, &[]).is_empty());
}

#[test]
fn plans_lead_with_their_title() {
    let segments = searchable_plan_segments("# Ship it\n\nstep one", None);
    assert_eq!(segments[0], "Ship it");
    assert!(segments.iter().any(|segment| segment == "step one"));
}

fn docs(counts: &[usize]) -> Vec<FindDocument> {
    counts
        .iter()
        .enumerate()
        .map(|(index, count)| FindDocument {
            entry_id: format!("entry:{index}"),
            run_id: None,
            count: *count,
        })
        .collect()
}

fn input(value: serde_json::Value) -> OrchestrationV2SearchThreadInput {
    let mut base = json!({"threadId": "thread:find", "query": "x"});
    base.as_object_mut()
        .expect("object")
        .extend(value.as_object().expect("object").clone());
    serde_json::from_value(base).expect("input decodes")
}

#[test]
fn selection_walks_occurrences_across_documents_and_wraps_with_the_offset() {
    let documents = docs(&[2, 0, 3]);
    let first = select_match(&documents, &input(json!({})));
    assert_eq!(
        (first.active_index, first.document, first.occurrence),
        (0, Some(0), 0)
    );

    let by_index = select_match(&documents, &input(json!({"index": 3})));
    assert_eq!((by_index.document, by_index.occurrence), (Some(2), 1));

    // Relative to an entry, a negative offset wraps to the last match.
    let wrapped = select_match(
        &documents,
        &input(json!({"start": {"entryId": "entry:0", "occurrence": 0}, "offset": -1})),
    );
    assert_eq!(
        (wrapped.active_index, wrapped.document, wrapped.occurrence),
        (4, Some(2), 2)
    );
}

#[test]
fn navigation_lists_matching_entries_with_their_first_ordinal() {
    let documents = docs(&[2, 0, 3]);
    let selected = select_match(&documents, &input(json!({})));
    let result = result_for_selection(&selected, &documents, 7);
    let navigation = result.navigation.expect("navigation");
    assert_eq!(navigation.len(), 2);
    assert_eq!((navigation[1].start_index, navigation[1].count), (2, 3));
    assert_eq!(result.total_matches, 5);
    assert_eq!(result.snapshot_sequence, 7);
}
