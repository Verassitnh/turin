//! Port of upstream/apps/web/src/components/chat/composerPromptHistory.ts
//!
//! Terminal-style prompt recall for the composer. ArrowUp on an empty composer
//! walks back through the active thread's sent prompts, ArrowDown walks
//! forward and restores the unsent draft past the newest entry. History is
//! per thread and text only, derived from the thread's user messages.
//!
//! Context references (`t3-context://` links) are removed with a light
//! matcher: their producers are not part of Turin yet.

use std::sync::OnceLock;

use regex::Regex;

/// `ATTACHMENT_ONLY_BOOTSTRAP_PROMPT` (packages/shared composerContextReferences).
pub const ATTACHMENT_ONLY_BOOTSTRAP_PROMPT: &str = "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";

/// `PLAN_IMPLEMENTATION_PROMPT_PREFIX` (proposedPlan.ts).
pub const PLAN_IMPLEMENTATION_PROMPT_PREFIX: &str = "PLEASE IMPLEMENT THIS PLAN:\n";

const CLAUDE_ULTRATHINK_PREFIX: &str = "Ultrathink:\n";

#[derive(Debug, Clone)]
pub struct ComposerPromptHistoryMessage {
    pub id: String,
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerPromptHistoryEntry {
    pub id: String,
    pub prompt: String,
}

/// Active recall. `entry_id` is resolved against the current entries on every
/// step; `recalled` is the text put in the composer: once the composer no
/// longer matches it, the user has edited or sent and browsing is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerPromptHistoryPosition {
    pub entry_id: String,
    pub recalled: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerPromptHistoryStep {
    pub position: Option<ComposerPromptHistoryPosition>,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    Backward,
    Forward,
}

fn review_comment_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<review_comment\b[^>]*>[\s\S]*?</review_comment>").unwrap())
}

fn legacy_context_patterns() -> &'static [(&'static str, Regex); 3] {
    static RE: OnceLock<[(&'static str, Regex); 3]> = OnceLock::new();
    RE.get_or_init(|| {
        let make = |tag: &'static str| {
            (
                tag,
                Regex::new(&format!(r"\n*<{tag}>\n([\s\S]*?)\n</{tag}>\s*$")).unwrap(),
            )
        };
        [make("terminal_context"), make("element_context"), make("preview_annotation")]
    })
}

fn context_reference_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!?\[[^\]]*\]\(t3-context://v1/[^)\s]+\)").unwrap())
}

/// Prefer the id. A consecutive duplicate collapse can retire the recalled id
/// while the same text lives on under a newer one, so fall back to the newest
/// entry with matching text.
fn find_active(entries: &[ComposerPromptHistoryEntry], position: &ComposerPromptHistoryPosition) -> Option<usize> {
    entries
        .iter()
        .position(|entry| entry.id == position.entry_id)
        .or_else(|| entries.iter().rposition(|entry| entry.prompt == position.recalled))
}

/// Drop only the review comments appended at send time, which sit at the end.
fn strip_trailing_review_comments(prompt: &str) -> String {
    let mut cut = prompt.len();
    let matches: Vec<(usize, usize)> = review_comment_pattern()
        .find_iter(prompt)
        .map(|m| (m.start(), m.end()))
        .collect();
    for (start, end) in matches.into_iter().rev() {
        if !prompt[end..cut].trim().is_empty() {
            break;
        }
        cut = start;
    }
    if cut == prompt.len() {
        prompt.to_owned()
    } else {
        prompt[..cut].trim_end().to_owned()
    }
}

fn is_label_prefix_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '@' || c == '.' || c == '-'
}

/// `(?<![\w@.-])label(?![\d-])`: the first whole label.
fn find_whole_label(text: &str, label: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find(label) {
        let start = from + found;
        let end = start + label.len();
        let before_ok = text[..start].chars().next_back().is_none_or(|c| !is_label_prefix_char(c));
        let after_ok = text[end..].chars().next().is_none_or(|c| !(c.is_ascii_digit() || c == '-'));
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Inline terminal chips are sent as `@terminal-1:12-13` labels in the text
/// with their content in the trailing block. Once the block is stripped the
/// label points at nothing, so remove it too. Each block header removes one
/// label (the first match) and the single space beside it.
fn strip_inline_terminal_labels(prompt: &str, headers: &[String]) -> String {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let header_re = HEADER.get_or_init(|| Regex::new(r"^(.+?) lines? (\d+(?:-\d+)?)$").unwrap());
    let mut result = prompt.to_owned();
    for header in headers {
        let Some(captures) = header_re.captures(header) else {
            continue;
        };
        let name = captures[1].trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-");
        let label = format!("@{name}:{}", &captures[2]);
        let Some(index) = find_whole_label(&result, &label) else {
            continue;
        };
        let mut start = index;
        let mut end = index + label.len();
        if result[end..].starts_with(' ') {
            end += 1;
        } else if result[..start].ends_with(' ') {
            start -= 1;
        }
        result.replace_range(start..end, "");
    }
    result
}

/// `recallableComposerPrompt`: reduce a sent message to the text the user
/// typed. Send-time appends (terminal and element context blocks, preview
/// annotations, review comments, the Claude ultrathink prefix) are stripped so
/// a recalled prompt never carries stale context from another turn.
pub fn recallable_composer_prompt(message_text: &str) -> String {
    let mut prompt = turin_contracts::base_schemas::js_trim(message_text).to_owned();
    if let Some(rest) = prompt.strip_prefix(CLAUDE_ULTRATHINK_PREFIX) {
        prompt = rest.to_owned();
    }

    static HEADERS: OnceLock<Regex> = OnceLock::new();
    static OPEN_TAG: OnceLock<Regex> = OnceLock::new();
    static KEYED: OnceLock<Regex> = OnceLock::new();
    let headers_re = HEADERS.get_or_init(|| Regex::new(r"(?m)^- (.+):$").unwrap());
    let open_tag_re = OPEN_TAG.get_or_init(|| Regex::new(r"(?m)^<preview_annotation>\s*$").unwrap());
    let keyed_re = KEYED.get_or_init(|| Regex::new(r"(?m)^(?:Id|Page|Comment|Targets): ").unwrap());

    while !prompt.is_empty() {
        let without_review = strip_trailing_review_comments(&prompt);
        if without_review != prompt {
            prompt = without_review;
            continue;
        }
        // The leftmost match across the three tags, like the JS alternation.
        let legacy = legacy_context_patterns()
            .iter()
            .filter_map(|(tag, re)| re.captures(&prompt).map(|c| (*tag, c)))
            .min_by_key(|(_, c)| c.get(0).map_or(usize::MAX, |m| m.start()));
        if let Some((tag, captures)) = legacy {
            let body = captures[1].to_owned();
            let start = captures.get(0).map_or(0, |m| m.start());
            if tag == "preview_annotation" && open_tag_re.is_match(&body) && !keyed_re.is_match(&body) {
                break;
            }
            let headers: Vec<String> = headers_re.captures_iter(&body).map(|c| c[1].to_owned()).collect();
            if tag != "preview_annotation" && headers.is_empty() {
                break;
            }
            prompt = prompt[..start].trim_end().to_owned();
            if tag == "terminal_context" {
                prompt = strip_inline_terminal_labels(&prompt, &headers);
            }
            continue;
        }
        break;
    }

    // Recall is text-only: never create dangling chips without their backing records.
    let references: Vec<(usize, usize)> = context_reference_pattern()
        .find_iter(&prompt)
        .map(|m| (m.start(), m.end()))
        .collect();
    for (mut start, mut end) in references.into_iter().rev() {
        if prompt[end..].starts_with(' ') {
            end += 1;
        } else if prompt[..start].ends_with(' ') {
            start -= 1;
        }
        prompt.replace_range(start..end, "");
    }

    // App-composed sends are not text the user typed, so they are not history.
    let trimmed = turin_contracts::base_schemas::js_trim(&prompt);
    if trimmed == ATTACHMENT_ONLY_BOOTSTRAP_PROMPT || trimmed.starts_with(PLAN_IMPLEMENTATION_PROMPT_PREFIX) {
        return String::new();
    }
    trimmed.to_owned()
}

/// `buildComposerPromptHistoryEntries`: oldest first. Consecutive identical
/// prompts collapse into the newest one, like shell `HISTCONTROL=ignoredups`.
/// Image-only sends have no text and are skipped.
pub fn build_composer_prompt_history_entries(messages: &[ComposerPromptHistoryMessage]) -> Vec<ComposerPromptHistoryEntry> {
    let mut entries: Vec<ComposerPromptHistoryEntry> = Vec::new();
    for message in messages {
        if message.role != "user" {
            continue;
        }
        let prompt = recallable_composer_prompt(&message.text);
        if prompt.is_empty() {
            continue;
        }
        if let Some(previous) = entries.last_mut()
            && previous.prompt == prompt
        {
            *previous = ComposerPromptHistoryEntry {
                id: message.id.clone(),
                prompt,
            };
            continue;
        }
        entries.push(ComposerPromptHistoryEntry {
            id: message.id.clone(),
            prompt,
        });
    }
    entries
}

/// `stepComposerPromptHistory`: `None` when the key should fall through to
/// normal caret movement.
pub fn step_composer_prompt_history(
    direction: HistoryDirection,
    entries: &[ComposerPromptHistoryEntry],
    position: Option<&ComposerPromptHistoryPosition>,
    current_prompt: &str,
) -> Option<ComposerPromptHistoryStep> {
    let active_index = position
        .filter(|position| position.recalled == current_prompt)
        .and_then(|position| find_active(entries, position));
    let step = |entry: &ComposerPromptHistoryEntry| ComposerPromptHistoryStep {
        position: Some(ComposerPromptHistoryPosition {
            entry_id: entry.id.clone(),
            recalled: entry.prompt.clone(),
        }),
        prompt: entry.prompt.clone(),
    };
    match direction {
        HistoryDirection::Backward => {
            if active_index.is_none() && !current_prompt.is_empty() {
                return None;
            }
            let target = match active_index {
                None => entries.len().checked_sub(1)?,
                Some(index) => index.checked_sub(1)?,
            };
            entries.get(target).map(step)
        }
        HistoryDirection::Forward => {
            let index = active_index?;
            Some(match entries.get(index + 1) {
                Some(entry) => step(entry),
                None => ComposerPromptHistoryStep {
                    position: None,
                    prompt: String::new(),
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: &str, text: &str) -> ComposerPromptHistoryMessage {
        ComposerPromptHistoryMessage {
            id: id.into(),
            role: role.into(),
            text: text.into(),
        }
    }

    fn entries() -> Vec<ComposerPromptHistoryEntry> {
        build_composer_prompt_history_entries(&[
            message("m1", "user", "first"),
            message("a1", "assistant", "reply"),
            message("m2", "user", "second"),
            message("m3", "user", "third"),
        ])
    }

    fn pos(id: &str, recalled: &str) -> ComposerPromptHistoryPosition {
        ComposerPromptHistoryPosition {
            entry_id: id.into(),
            recalled: recalled.into(),
        }
    }

    fn backward(position: Option<&ComposerPromptHistoryPosition>, current: &str) -> Option<ComposerPromptHistoryStep> {
        step_composer_prompt_history(HistoryDirection::Backward, &entries(), position, current)
    }

    fn forward(position: Option<&ComposerPromptHistoryPosition>, current: &str) -> Option<ComposerPromptHistoryStep> {
        step_composer_prompt_history(HistoryDirection::Forward, &entries(), position, current)
    }

    #[test]
    fn strips_an_annotation_whose_comment_contains_a_literal_opening_tag() {
        for comment in ["Render <preview_annotation>", "Render\n<preview_annotation>\nhere", "Render &lt;preview_annotation&gt;"] {
            let sent = format!("Prompt\n<preview_annotation>\nComment: {comment}\n</preview_annotation>");
            assert_eq!(recallable_composer_prompt(&sent), "Prompt");
        }
    }

    #[test]
    fn preserves_a_malformed_annotation_containing_a_nested_opening_tag() {
        let text = "Prompt\n<preview_annotation>\nouter literal\n<preview_annotation>\ninner\n</preview_annotation>";
        assert_eq!(recallable_composer_prompt(text), text);
    }

    #[test]
    fn does_not_strip_a_terminal_label_embedded_in_ordinary_prose() {
        assert_eq!(
            recallable_composer_prompt("email@build:7\n<terminal_context>\n- Build line 7:\n  output\n</terminal_context>"),
            "email@build:7"
        );
    }

    #[test]
    fn strips_legacy_send_time_context_blocks_and_the_ultrathink_prefix() {
        let sent = "Ultrathink:\nInvestigate this\n\n<terminal_context>\n- Terminal 1 lines 12-13:\n  12 | git status\n  13 | On branch main\n</terminal_context>\n\n<element_context>\n- <button>:\n  url: https://example.com\n</element_context>";
        assert_eq!(recallable_composer_prompt(sent), "Investigate this");
    }

    #[test]
    fn removes_only_the_matching_legacy_terminal_label() {
        for (typed, expected) in [
            ("Look at @terminal-1:4 please", "Look at please"),
            (
                "@terminal-1:4 typed twice: @terminal-1:4\n    indented  code",
                "typed twice: @terminal-1:4\n    indented  code",
            ),
            (
                "see @terminal-1:40 and @terminal-1:4-12 then @terminal-1:4",
                "see @terminal-1:40 and @terminal-1:4-12 then",
            ),
        ] {
            let sent = format!("{typed}\n\n<terminal_context>\n- Terminal 1 line 4:\n  4 | ls\n</terminal_context>");
            assert_eq!(recallable_composer_prompt(&sent), expected);
        }
    }

    #[test]
    fn strips_only_review_comments_appended_at_the_end() {
        let block = "<review_comment filePath=\"src/app.ts\">Keep this configurable.</review_comment>";
        assert_eq!(recallable_composer_prompt(&format!("Please update this.\n\n{block}")), "Please update this.");
        let mid = format!("Before\n\n{block}\n\nAfter");
        assert_eq!(recallable_composer_prompt(&mid), mid);
        assert_eq!(recallable_composer_prompt(&format!("{mid}\n\n{block}")), mid);
    }

    #[test]
    fn strips_a_trailing_preview_annotation_including_its_nested_element_context() {
        let sent = "Fix this\n\n<preview_annotation>\nPage: Example\n<element_context>\n- <button>:\n  html: Save\n</element_context>\n</preview_annotation>";
        assert_eq!(recallable_composer_prompt(sent), "Fix this");
    }

    #[test]
    fn removes_canonical_references_without_restoring_dangling_chips() {
        let sent = "Look at [Terminal](t3-context://v1/terminal/term-1) please\n    indented  code\n![image](t3-context://v1/image/img-1)";
        assert_eq!(recallable_composer_prompt(sent), "Look at please\n    indented  code");
        assert_eq!(recallable_composer_prompt("![image](t3-context://v1/image/img-1)"), "");
    }

    #[test]
    fn returns_an_empty_string_for_app_composed_sends() {
        assert_eq!(recallable_composer_prompt("   "), "");
        assert_eq!(recallable_composer_prompt(ATTACHMENT_ONLY_BOOTSTRAP_PROMPT), "");
        assert_eq!(
            recallable_composer_prompt(&format!("{PLAN_IMPLEMENTATION_PROMPT_PREFIX}# Plan\n1. do it")),
            ""
        );
    }

    #[test]
    fn keeps_user_messages_with_text_oldest_first() {
        let prompts: Vec<String> = entries().into_iter().map(|e| e.prompt).collect();
        assert_eq!(prompts, ["first", "second", "third"]);
    }

    #[test]
    fn collapses_consecutive_duplicates_onto_the_newest_message_id() {
        let collapsed = build_composer_prompt_history_entries(&[
            message("m1", "user", "same"),
            message("m2", "user", "same"),
            message("m3", "user", "other"),
            message("m4", "user", "same"),
        ]);
        let pairs: Vec<(&str, &str)> = collapsed.iter().map(|e| (e.id.as_str(), e.prompt.as_str())).collect();
        assert_eq!(pairs, [("m2", "same"), ("m3", "other"), ("m4", "same")]);
    }

    #[test]
    fn does_not_start_browsing_from_a_non_empty_draft() {
        assert_eq!(backward(None, "typing"), None);
    }

    #[test]
    fn walks_back_from_the_newest_entry() {
        let first = backward(None, "").unwrap();
        assert_eq!(first.prompt, "third");
        assert_eq!(first.position, Some(pos("m3", "third")));
        assert_eq!(backward(first.position.as_ref(), "third").unwrap().prompt, "second");
    }

    #[test]
    fn is_a_no_op_at_the_oldest_entry() {
        assert_eq!(backward(Some(&pos("m1", "first")), "first"), None);
    }

    #[test]
    fn walks_forward_and_empties_the_composer_past_the_newest_entry() {
        let newer = forward(Some(&pos("m2", "second")), "second").unwrap();
        assert_eq!(newer.prompt, "third");
        assert_eq!(
            forward(newer.position.as_ref(), "third"),
            Some(ComposerPromptHistoryStep { position: None, prompt: String::new() })
        );
    }

    #[test]
    fn treats_an_edited_recall_as_a_fresh_draft() {
        let position = pos("m3", "third");
        assert_eq!(backward(Some(&position), "third edited"), None);
        assert_eq!(forward(Some(&position), "third edited"), None);
        assert_eq!(backward(Some(&position), "").unwrap().position, Some(pos("m3", "third")));
    }

    #[test]
    fn does_nothing_on_forward_when_not_browsing() {
        assert_eq!(forward(None, ""), None);
    }

    #[test]
    fn follows_the_entry_by_id_when_the_list_changes_under_it() {
        let grown = build_composer_prompt_history_entries(&[
            message("m0", "user", "zeroth"),
            message("m1", "user", "A"),
            message("m2", "user", "B"),
            message("m3", "user", "A"),
        ]);
        let older = step_composer_prompt_history(HistoryDirection::Backward, &grown, Some(&pos("m1", "A")), "A");
        assert_eq!(older.unwrap().prompt, "zeroth");
        let missing = step_composer_prompt_history(
            HistoryDirection::Forward,
            &grown,
            Some(&pos("gone", "not sent")),
            "not sent",
        );
        assert_eq!(missing, None);
    }

    #[test]
    fn falls_back_to_matching_text_when_a_duplicate_collapse_retires_the_id() {
        let collapsed = build_composer_prompt_history_entries(&[message("m1", "user", "first"), message("m3", "user", "A")]);
        let step = step_composer_prompt_history(HistoryDirection::Backward, &collapsed, Some(&pos("m2", "A")), "A");
        assert_eq!(step.unwrap().prompt, "first");
    }
}
