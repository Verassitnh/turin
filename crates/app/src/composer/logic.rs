//! Port of upstream/apps/web/src/composer-logic.ts
//!
//! Cursor spaces: *expanded* offsets index the stored prompt string;
//! *collapsed* offsets count each inline chip (mention, skill) as one
//! position. Offsets are UTF-16 code units, like upstream.
//!
//! `formatAssistantCitationForComposer` is not ported (citations: see
//! `editor_mentions`). `composerSubmissionIntentForKey` takes the already
//! resolved keybinding command instead of resolving it, because the shortcut
//! resolver lives with the keybindings task.

use std::sync::OnceLock;

use regex::Regex;

use crate::composer::editor_mentions::{ComposerPromptSegment, split_prompt_into_composer_segments};
use crate::composer::js_string::utf16_len;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerTriggerKind {
    Path,
    PullRequest,
    SlashCommand,
    Skill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSlashCommand {
    Model,
    Plan,
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSubmissionIntent {
    Foreground,
    Background,
    Alternate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerTrigger {
    pub kind: ComposerTriggerKind,
    pub query: String,
    pub range_start: usize,
    pub range_end: usize,
}

/// `ClientSettings["sendShortcut"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendShortcut {
    Enter,
    ModEnter,
    ModEnterMultiline,
}

fn composer_requires_modifier(send_shortcut: Option<SendShortcut>, prompt: &str) -> bool {
    send_shortcut == Some(SendShortcut::ModEnter)
        || (send_shortcut == Some(SendShortcut::ModEnterMultiline)
            && prompt.chars().any(|c| c == '\r' || c == '\n'))
}

/// The subset of a key event the submission rules read.
#[derive(Debug, Clone, Default)]
pub struct ComposerKeyEvent {
    pub key: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub is_composing: bool,
    pub repeat: bool,
}

pub struct SubmissionIntentInput<'a> {
    pub event: &'a ComposerKeyEvent,
    /// The keybinding command the event resolved to, if any.
    pub command: Option<&'a str>,
    pub is_mobile_viewport: bool,
    pub is_draft_thread: bool,
    pub is_running: bool,
    pub send_shortcut: Option<SendShortcut>,
    pub prompt: &'a str,
}

/// `composerSubmissionIntentForKey`.
pub fn composer_submission_intent_for_key(
    input: &SubmissionIntentInput<'_>,
) -> Option<ComposerSubmissionIntent> {
    let event = input.event;
    if input.is_mobile_viewport || event.is_composing || event.repeat {
        return None;
    }
    match input.command {
        Some("composer.sendAlternate") if input.is_running => {
            return Some(ComposerSubmissionIntent::Alternate);
        }
        Some("composer.sendBackground") if input.is_draft_thread => {
            return Some(ComposerSubmissionIntent::Background);
        }
        Some("composer.sendAndNewThread") if !input.is_draft_thread => {
            return Some(ComposerSubmissionIntent::Background);
        }
        _ => {}
    }
    if input.command.is_some() || event.key != "Enter" || event.shift_key || event.alt_key {
        return None;
    }
    if composer_requires_modifier(input.send_shortcut, input.prompt)
        && !event.meta_key
        && !event.ctrl_key
    {
        return None;
    }
    Some(ComposerSubmissionIntent::Foreground)
}

fn is_inline_token_segment(segment: &ComposerPromptSegment) -> bool {
    segment.is_inline_token()
}

fn clamp_cursor(len: usize, cursor: usize) -> usize {
    cursor.min(len)
}

fn is_whitespace(unit: u16) -> bool {
    matches!(unit, 0x20 | 0x0a | 0x09 | 0x0d)
}

fn token_start_for_cursor(units: &[u16], cursor: usize) -> usize {
    let mut index = cursor;
    while index > 0 && !is_whitespace(units[index - 1]) {
        index -= 1;
    }
    index
}

/// `expandCollapsedComposerCursor`.
pub fn expand_collapsed_composer_cursor(text: &str, cursor_input: usize, literal_text: bool) -> usize {
    let len = utf16_len(text);
    if literal_text {
        return clamp_cursor(len, cursor_input);
    }
    let collapsed_cursor = clamp_cursor(len, cursor_input);
    let segments = split_prompt_into_composer_segments(text);
    if segments.is_empty() {
        return collapsed_cursor;
    }
    let mut remaining = collapsed_cursor;
    let mut expanded_cursor = 0usize;
    for segment in &segments {
        if segment.is_inline_token() {
            let expanded_length = segment.source_len();
            if remaining <= 1 {
                return expanded_cursor + if remaining == 0 { 0 } else { expanded_length };
            }
            remaining -= 1;
            expanded_cursor += expanded_length;
            continue;
        }
        let segment_length = segment.source_len();
        if remaining <= segment_length {
            return expanded_cursor + remaining;
        }
        remaining -= segment_length;
        expanded_cursor += segment_length;
    }
    expanded_cursor
}

fn collapsed_segment_length(segment: &ComposerPromptSegment) -> usize {
    match segment {
        ComposerPromptSegment::Text { .. } => segment.source_len(),
        _ => 1,
    }
}

fn clamp_collapsed_cursor_for_segments(segments: &[ComposerPromptSegment], cursor_input: usize) -> usize {
    let collapsed_length: usize = segments.iter().map(collapsed_segment_length).sum();
    cursor_input.min(collapsed_length)
}

/// `clampCollapsedComposerCursor`.
pub fn clamp_collapsed_composer_cursor(text: &str, cursor_input: usize, literal_text: bool) -> usize {
    if literal_text {
        return clamp_cursor(utf16_len(text), cursor_input);
    }
    clamp_collapsed_cursor_for_segments(&split_prompt_into_composer_segments(text), cursor_input)
}

/// `collapseExpandedComposerCursor`.
pub fn collapse_expanded_composer_cursor(text: &str, cursor_input: usize, literal_text: bool) -> usize {
    let len = utf16_len(text);
    if literal_text {
        return clamp_cursor(len, cursor_input);
    }
    let expanded_cursor = clamp_cursor(len, cursor_input);
    let segments = split_prompt_into_composer_segments(text);
    if segments.is_empty() {
        return expanded_cursor;
    }
    let mut remaining = expanded_cursor;
    let mut collapsed_cursor = 0usize;
    for segment in &segments {
        let segment_length = segment.source_len();
        if segment.is_inline_token() {
            if remaining == 0 {
                return collapsed_cursor;
            }
            if remaining <= segment_length {
                return collapsed_cursor + 1;
            }
            remaining -= segment_length;
            collapsed_cursor += 1;
            continue;
        }
        if remaining <= segment_length {
            return collapsed_cursor + remaining;
        }
        remaining -= segment_length;
        collapsed_cursor += segment_length;
    }
    collapsed_cursor
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDirection {
    Left,
    Right,
}

/// `isCollapsedCursorAdjacentToInlineToken`.
pub fn is_collapsed_cursor_adjacent_to_inline_token(
    text: &str,
    cursor_input: usize,
    direction: CursorDirection,
) -> bool {
    let segments = split_prompt_into_composer_segments(text);
    if !segments.iter().any(is_inline_token_segment) {
        return false;
    }
    let cursor = clamp_collapsed_cursor_for_segments(&segments, cursor_input);
    let mut collapsed_offset = 0usize;
    for segment in &segments {
        if is_inline_token_segment(segment) {
            if direction == CursorDirection::Left && cursor == collapsed_offset + 1 {
                return true;
            }
            if direction == CursorDirection::Right && cursor == collapsed_offset {
                return true;
            }
        }
        collapsed_offset += collapsed_segment_length(segment);
    }
    false
}

fn pull_request_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^#([\p{L}\p{N}][\p{L}\p{N}_-]*)?$").expect("valid regex"))
}

fn currency_prefix_len(token: &str) -> Option<usize> {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\p{Sc}").expect("valid regex"))
        .find(token)
        .map(|m| m.end())
}

/// `detectComposerTrigger`: `text` is the expanded prompt, `cursor_input` an
/// expanded offset.
pub fn detect_composer_trigger(text: &str, cursor_input: usize) -> Option<ComposerTrigger> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let cursor = clamp_cursor(units.len(), cursor_input);
    let token_start = token_start_for_cursor(&units, cursor);
    let token = String::from_utf16_lossy(&units[token_start..cursor]);
    let trigger = |kind, query: &str| ComposerTrigger {
        kind,
        query: query.to_owned(),
        range_start: token_start,
        range_end: cursor,
    };
    if let Some(query) = token.strip_prefix('/') {
        return Some(trigger(ComposerTriggerKind::SlashCommand, query));
    }
    if let Some(captures) = pull_request_pattern().captures(&token) {
        let query = captures.get(1).map_or("", |m| m.as_str());
        return Some(trigger(ComposerTriggerKind::PullRequest, query));
    }
    if let Some(prefix_len) = currency_prefix_len(&token) {
        return Some(trigger(ComposerTriggerKind::Skill, &token[prefix_len..]));
    }
    token
        .strip_prefix('@')
        .map(|query| trigger(ComposerTriggerKind::Path, query))
}

/// `pastedPathQueryLength`: length of the leading part of `pasted` that
/// continues an `@` path query typed at the end of `line_before`.
pub fn pasted_path_query_length(line_before: &str, pasted: &str, path_query_active: bool) -> usize {
    if !path_query_active {
        return 0;
    }
    let before_len = utf16_len(line_before);
    if detect_composer_trigger(line_before, before_len).map(|t| t.kind)
        != Some(ComposerTriggerKind::Path)
    {
        return 0;
    }
    let leading: String = pasted
        .chars()
        .take_while(|c| !crate::composer::js_string::is_js_whitespace_char(*c))
        .collect();
    utf16_len(&leading)
}

/// Caret and trigger after replacing composer text and continuing at the end.
pub struct ComposerStateAtPromptEnd {
    pub cursor: usize,
    pub trigger: Option<ComposerTrigger>,
}

/// `composerStateAtPromptEnd`.
pub fn composer_state_at_prompt_end(text: &str, literal_text: bool) -> ComposerStateAtPromptEnd {
    let cursor = collapse_expanded_composer_cursor(text, utf16_len(text), literal_text);
    ComposerStateAtPromptEnd {
        cursor,
        trigger: if literal_text {
            None
        } else {
            detect_composer_trigger(text, expand_collapsed_composer_cursor(text, cursor, false))
        },
    }
}

/// `parseStandaloneComposerSlashCommand`.
pub fn parse_standalone_composer_slash_command(text: &str) -> Option<ComposerSlashCommand> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)^/(plan|default)\s*$").expect("valid regex"));
    let trimmed = turin_contracts::base_schemas::js_trim(text);
    let captures = re.captures(trimmed)?;
    if captures[1].eq_ignore_ascii_case("plan") {
        Some(ComposerSlashCommand::Plan)
    } else {
        Some(ComposerSlashCommand::Default)
    }
}

pub struct ReplacedRange {
    pub text: String,
    pub cursor: usize,
}

/// `replaceTextRange`.
pub fn replace_text_range(text: &str, range_start: usize, range_end: usize, replacement: &str) -> ReplacedRange {
    let units: Vec<u16> = text.encode_utf16().collect();
    let safe_start = range_start.min(units.len());
    let safe_end = range_end.clamp(safe_start, units.len());
    let next = format!(
        "{}{}{}",
        String::from_utf16_lossy(&units[..safe_start]),
        replacement,
        String::from_utf16_lossy(&units[safe_end..])
    );
    ReplacedRange {
        text: next,
        cursor: safe_start + utf16_len(replacement),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trig(kind: ComposerTriggerKind, query: &str, start: usize, end: usize) -> ComposerTrigger {
        ComposerTrigger {
            kind,
            query: query.to_owned(),
            range_start: start,
            range_end: end,
        }
    }

    fn key(key: &str) -> ComposerKeyEvent {
        ComposerKeyEvent {
            key: key.to_owned(),
            ..Default::default()
        }
    }

    fn intent(event: &ComposerKeyEvent, command: Option<&str>, running: bool, draft: bool) -> Option<ComposerSubmissionIntent> {
        composer_submission_intent_for_key(&SubmissionIntentInput {
            event,
            command,
            is_mobile_viewport: false,
            is_draft_thread: draft,
            is_running: running,
            send_shortcut: None,
            prompt: "",
        })
    }

    #[test]
    fn submission_intent_for_plain_enter_and_modified_enter() {
        assert_eq!(intent(&key("Enter"), None, false, false), Some(ComposerSubmissionIntent::Foreground));
        let mut shifted = key("Enter");
        shifted.shift_key = true;
        assert_eq!(intent(&shifted, None, false, false), None);
        assert_eq!(
            intent(&key("Enter"), Some("composer.sendAlternate"), true, false),
            Some(ComposerSubmissionIntent::Alternate)
        );
        assert_eq!(intent(&key("Enter"), Some("composer.sendAlternate"), false, false), None);
        assert_eq!(
            intent(&key("Enter"), Some("composer.sendBackground"), false, true),
            Some(ComposerSubmissionIntent::Background)
        );
        assert_eq!(
            intent(&key("Enter"), Some("composer.sendAndNewThread"), false, false),
            Some(ComposerSubmissionIntent::Background)
        );
    }

    #[test]
    fn mod_enter_send_shortcut_requires_a_modifier() {
        let mut input = SubmissionIntentInput {
            event: &key("Enter"),
            command: None,
            is_mobile_viewport: false,
            is_draft_thread: false,
            is_running: false,
            send_shortcut: Some(SendShortcut::ModEnter),
            prompt: "hi",
        };
        assert_eq!(composer_submission_intent_for_key(&input), None);
        let mut with_meta = key("Enter");
        with_meta.meta_key = true;
        input.event = &with_meta;
        assert_eq!(composer_submission_intent_for_key(&input), Some(ComposerSubmissionIntent::Foreground));
        let plain = key("Enter");
        input.event = &plain;
        input.send_shortcut = Some(SendShortcut::ModEnterMultiline);
        assert_eq!(composer_submission_intent_for_key(&input), Some(ComposerSubmissionIntent::Foreground));
        input.prompt = "a\nb";
        assert_eq!(composer_submission_intent_for_key(&input), None);
    }

    #[test]
    fn detects_path_trigger_at_cursor() {
        let text = "Please inspect @";
        assert_eq!(
            detect_composer_trigger(text, utf16_len(text)),
            Some(trig(ComposerTriggerKind::Path, "", "Please inspect ".len(), text.len()))
        );
        let text = "Please inspect @buildi";
        assert_eq!(
            detect_composer_trigger(text, utf16_len(text)),
            Some(trig(ComposerTriggerKind::Path, "buildi", "Please inspect ".len(), text.len()))
        );
    }

    #[test]
    fn detects_slash_skill_and_pull_request_tokens() {
        assert_eq!(
            detect_composer_trigger("/mo", 3),
            Some(trig(ComposerTriggerKind::SlashCommand, "mo", 0, 3))
        );
        assert_eq!(
            detect_composer_trigger("run $rev", 8),
            Some(trig(ComposerTriggerKind::Skill, "rev", 4, 8))
        );
        assert_eq!(
            detect_composer_trigger("see #12", 7),
            Some(trig(ComposerTriggerKind::PullRequest, "12", 4, 7))
        );
        assert_eq!(
            detect_composer_trigger("see #", 5),
            Some(trig(ComposerTriggerKind::PullRequest, "", 4, 5))
        );
        assert_eq!(
            detect_composer_trigger("see #fix-bug", 12),
            Some(trig(ComposerTriggerKind::PullRequest, "fix-bug", 4, 12))
        );
        // headings and embedded hashes never open the picker
        assert_eq!(detect_composer_trigger("# ", 2), None);
        assert_eq!(detect_composer_trigger("a#12", 4), None);
        assert_eq!(detect_composer_trigger("plain", 5), None);
    }

    #[test]
    fn detects_path_trigger_in_the_middle_of_existing_text() {
        let text = "please open @src and then";
        let cursor = "please open @src".len();
        assert_eq!(
            detect_composer_trigger(text, cursor),
            Some(trig(ComposerTriggerKind::Path, "src", 12, cursor))
        );
    }

    #[test]
    fn replace_text_range_returns_the_new_cursor() {
        let replaced = replace_text_range("hello @src", 6, 10, "");
        assert_eq!(replaced.text, "hello ");
        assert_eq!(replaced.cursor, 6);
    }

    #[test]
    fn expand_collapsed_cursor_maps_chips_to_source_offsets() {
        assert_eq!(expand_collapsed_composer_cursor("plain text", 5, false), 5);
        let text = "what's in my @AGENTS.md fsfdas";
        assert_eq!(
            expand_collapsed_composer_cursor(text, "what's in my ".len() + 2, false),
            "what's in my @AGENTS.md ".len()
        );
        let text = "what is in @\"My File.md\" please";
        assert_eq!(
            expand_collapsed_composer_cursor(text, "what is in ".len() + 2, false),
            "what is in @\"My File.md\" ".len()
        );
        let text = "what's in [AGENTS.md](AGENTS.md) please";
        assert_eq!(
            expand_collapsed_composer_cursor(text, "what's in ".len() + 2, false),
            "what's in [AGENTS.md](AGENTS.md) ".len()
        );
    }

    #[test]
    fn path_trigger_closes_after_selecting_a_mention() {
        let text = "what's in my @AGENTS.md ";
        let expanded = expand_collapsed_composer_cursor(text, "what's in my ".len() + 2, false);
        assert_eq!(detect_composer_trigger(text, expanded), None);
    }

    #[test]
    fn skill_cursor_maps_for_any_currency_sigil() {
        for prefix in ["$", "€", "\u{11FDD}"] {
            let text = format!("run {prefix}review-follow-up then");
            let collapsed = 4 + 2;
            let expanded = utf16_len(&format!("run {prefix}review-follow-up "));
            assert_eq!(expand_collapsed_composer_cursor(&text, collapsed, false), expanded);
            assert_eq!(collapse_expanded_composer_cursor(&text, expanded, false), collapsed);
        }
    }

    #[test]
    fn collapse_expanded_cursor_maps_back() {
        assert_eq!(collapse_expanded_composer_cursor("plain text", 5, false), 5);
        let text = "what's in my @AGENTS.md fsfdas";
        assert_eq!(
            collapse_expanded_composer_cursor(text, "what's in my @AGENTS.md ".len(), false),
            "what's in my ".len() + 2
        );
        let text = "open @AGENTS.md then @src/index.ts ";
        let collapsed = collapse_expanded_composer_cursor(text, text.len(), false);
        assert_eq!(collapsed, "open ".len() + 1 + " then @src/index.ts ".len());
        assert_eq!(expand_collapsed_composer_cursor(text, collapsed, false), text.len());
        let text = "install @scope/pkg then @README.md ";
        let collapsed = collapse_expanded_composer_cursor(text, text.len(), false);
        assert_eq!(collapsed, "install @scope/pkg then ".len() + 1 + 1);
        assert_eq!(expand_collapsed_composer_cursor(text, collapsed, false), text.len());
    }

    #[test]
    fn preserves_the_caret_before_trailing_text_after_mixed_width_skill_chips() {
        let text = "\u{11FDD}ui a $review tail";
        let expanded = utf16_len("\u{11FDD}ui a $review ");
        assert_eq!(collapse_expanded_composer_cursor(text, expanded, false), 6);
        assert_eq!(expand_collapsed_composer_cursor(text, 6, false), expanded);
    }

    #[test]
    fn composer_state_at_prompt_end_collapses_chips() {
        let prompt = "see @AGENTS.md please";
        assert_eq!(
            composer_state_at_prompt_end(prompt, false).cursor,
            "see ".len() + 1 + " please".len()
        );
        let prompt = "look at @";
        let state = composer_state_at_prompt_end(prompt, false);
        assert_eq!(state.cursor, prompt.len());
        assert_eq!(
            state.trigger,
            Some(trig(ComposerTriggerKind::Path, "", "look at ".len(), prompt.len()))
        );
        let literal = composer_state_at_prompt_end("see @AGENTS.md ", true);
        assert_eq!(literal.cursor, "see @AGENTS.md ".len());
        assert_eq!(literal.trigger, None);
    }

    #[test]
    fn clamp_collapsed_cursor_uses_the_collapsed_length() {
        let text = "open @AGENTS.md then ";
        let collapsed = "open ".len() + 1 + " then ".len();
        assert_eq!(clamp_collapsed_composer_cursor(text, text.len(), false), collapsed);
        assert_eq!(clamp_collapsed_composer_cursor(text, usize::MAX, false), collapsed);
        assert_eq!(clamp_collapsed_composer_cursor(text, usize::MAX, true), text.len());
    }

    #[test]
    fn cursor_adjacency_to_inline_tokens() {
        use CursorDirection::*;
        assert!(!is_collapsed_cursor_adjacent_to_inline_token("plain", 3, Left));
        assert!(!is_collapsed_cursor_adjacent_to_inline_token("a @b", 4, Left));
        let text = "open @AGENTS.md then";
        assert!(is_collapsed_cursor_adjacent_to_inline_token(text, 6, Left));
        assert!(!is_collapsed_cursor_adjacent_to_inline_token(text, 5, Left));
        assert!(is_collapsed_cursor_adjacent_to_inline_token(text, 5, Right));
        assert!(!is_collapsed_cursor_adjacent_to_inline_token(text, 6, Right));
        let skill = "run $review now";
        assert!(is_collapsed_cursor_adjacent_to_inline_token(skill, 5, Left));
        assert!(is_collapsed_cursor_adjacent_to_inline_token(skill, 4, Right));
    }

    #[test]
    fn parses_standalone_slash_commands() {
        assert_eq!(parse_standalone_composer_slash_command("/plan"), Some(ComposerSlashCommand::Plan));
        assert_eq!(parse_standalone_composer_slash_command("  /DEFAULT "), Some(ComposerSlashCommand::Default));
        assert_eq!(parse_standalone_composer_slash_command("/plan do it"), None);
    }

    #[test]
    fn pasted_path_query_continues_only_an_active_path_query() {
        assert_eq!(pasted_path_query_length("open @src/", "app.rs and more", true), 6);
        assert_eq!(pasted_path_query_length("open @src/", "app.rs", false), 0);
        assert_eq!(pasted_path_query_length("open ", "app.rs", true), 0);
    }
}
