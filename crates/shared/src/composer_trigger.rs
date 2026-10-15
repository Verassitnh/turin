//! Port of upstream/packages/shared/src/composerTrigger.ts

use crate::js_string::{last_index_of_utf16, slice_utf16, utf16_len};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerTriggerKind {
    Path,
    PullRequest,
    SlashCommand,
    SlashModel,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerTrigger {
    pub kind: ComposerTriggerKind,
    pub query: String,
    pub range_start: usize,
    pub range_end: usize,
}

fn composer_file_link_basename(path: &str) -> &str {
    let separator_index = path.rfind('/').max(path.rfind('\\'));
    match separator_index {
        Some(sep) => &path[sep + 1..],
        None => path,
    }
}

fn escape_markdown_link_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn encode_markdown_link_destination(path: &str) -> String {
    crate::js_string::encode_uri(path)
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('#', "%23")
        .replace('?', "%3F")
        .replace('\\', "%5C")
}

pub fn serialize_composer_file_link(path: &str) -> String {
    let label = escape_markdown_link_label(composer_file_link_basename(path));
    format!("[{label}]({})", encode_markdown_link_destination(path))
}

/// Clamps a cursor to `[0, text.len()]` in UTF-16 units, flooring fractional
/// input like upstream's `Math.floor`.
fn clamp_cursor(text: &str, cursor: f64) -> usize {
    if !cursor.is_finite() {
        return utf16_len(text);
    }
    let floored = cursor.floor();
    if floored <= 0.0 {
        return 0;
    }
    if floored >= utf16_len(text) as f64 {
        return utf16_len(text);
    }
    floored as usize
}

fn is_whitespace_char(ch: char) -> bool {
    ch == ' ' || ch == '\n' || ch == '\t' || ch == '\r'
}

/// Detect an active trigger (@path, $skill, /command) at the cursor position.
///
/// Accepts an optional `is_whitespace_char` override so callers with inline
/// placeholder characters (e.g. terminal context chips on web) can treat those
/// as token boundaries. `cursor` is in UTF-16 units.
pub fn detect_composer_trigger(
    text: &str,
    cursor_input: f64,
    is_whitespace_char_override: Option<fn(char) -> bool>,
) -> Option<ComposerTrigger> {
    let cursor = clamp_cursor(text, cursor_input);
    let line_start = last_index_of_utf16(text, "\n", cursor.saturating_sub(1))
        .map(|index| index + 1)
        .unwrap_or(0);
    let line_prefix = slice_utf16(text, line_start, cursor);

    if line_prefix.starts_with('/') {
        // /^\/(\S*)$/
        let command_query = strip_slash_command_query(line_prefix);
        if let Some(command_query) = command_query {
            if command_query.to_lowercase() == "model" {
                return Some(ComposerTrigger {
                    kind: ComposerTriggerKind::SlashModel,
                    query: String::new(),
                    range_start: line_start,
                    range_end: cursor,
                });
            }
            return Some(ComposerTrigger {
                kind: ComposerTriggerKind::SlashCommand,
                query: command_query,
                range_start: line_start,
                range_end: cursor,
            });
        }

        // /^\/model(?:\s+(.*))?$/
        if let Some(query) = slash_model_query(line_prefix) {
            return Some(ComposerTrigger {
                kind: ComposerTriggerKind::SlashModel,
                query: turin_contracts::base_schemas::js_trim(&query).to_owned(),
                range_start: line_start,
                range_end: cursor,
            });
        }
    }

    let ws_check = is_whitespace_char_override.unwrap_or(is_whitespace_char);
    let units: Vec<u16> = text.encode_utf16().collect();
    let unit_at = |index: usize| -> char {
        // Upstream compares single UTF-16 units against ASCII delimiters.
        char::from_u32(units[index] as u32).unwrap_or(' ')
    };
    let mut token_idx = cursor as i64 - 1;
    while token_idx >= 0 && !ws_check(unit_at(token_idx as usize)) {
        token_idx -= 1;
    }
    let token_start = (token_idx + 1) as usize;

    let token = slice_utf16(text, token_start, cursor);
    if token.starts_with('/') {
        return Some(ComposerTrigger {
            kind: ComposerTriggerKind::SlashCommand,
            query: token[1..].to_owned(),
            range_start: token_start,
            range_end: cursor,
        });
    }
    if let Some(query) = pull_request_query(token) {
        return Some(ComposerTrigger {
            kind: ComposerTriggerKind::PullRequest,
            query,
            range_start: token_start,
            range_end: cursor,
        });
    }
    if let Some(prefix_len) = skill_prefix_len(token) {
        return Some(ComposerTrigger {
            kind: ComposerTriggerKind::Skill,
            query: slice_utf16(token, prefix_len, utf16_len(token)).to_owned(),
            range_start: token_start,
            range_end: cursor,
        });
    }
    if !token.starts_with('@') {
        return None;
    }

    Some(ComposerTrigger {
        kind: ComposerTriggerKind::Path,
        query: token[1..].to_owned(),
        range_start: token_start,
        range_end: cursor,
    })
}

/// `^\/(\S*)$` on the line prefix (already known to start with `/`); `\S` is
/// the full JS whitespace complement.
fn strip_slash_command_query(line_prefix: &str) -> Option<String> {
    let rest = &line_prefix[1..];
    if rest.chars().any(crate::js_string::is_js_whitespace_char) {
        return None;
    }
    Some(rest.to_owned())
}

/// `^\/model(?:\s+(.*))?$` — the captured query is trimmed by the caller.
fn slash_model_query(line_prefix: &str) -> Option<String> {
    let rest = &line_prefix[1..];
    let after = rest.strip_prefix("model")?;
    if after.is_empty() {
        return Some(String::new());
    }
    if after
        .chars()
        .next()
        .is_some_and(crate::js_string::is_js_whitespace_char)
    {
        return Some(after.to_owned());
    }
    None
}

/// `^#([\p{L}\p{N}][\p{L}\p{N}_-]*)?$`
fn pull_request_query(token: &str) -> Option<String> {
    static PULL_REQUEST: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PULL_REQUEST.get_or_init(|| {
        regex::Regex::new(r"^#([\p{L}\p{N}][\p{L}\p{N}_-]*)?$")
            .expect("valid regex")
    });
    pattern
        .captures(token)
        .map(|captures| captures.get(1).map(|m| m.as_str().to_owned()).unwrap_or_default())
}

/// `^\p{Sc}` — returns the sigil's UTF-16 length.
fn skill_prefix_len(token: &str) -> Option<usize> {
    let first = token.chars().next()?;
    static SC: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = SC.get_or_init(|| regex::Regex::new(r"\p{Sc}").expect("valid regex"));
    if pattern.is_match(&first.to_string()) {
        Some(first.len_utf16())
    } else {
        None
    }
}

pub fn replace_text_range(
    text: &str,
    range_start: usize,
    range_end: usize,
    replacement: &str,
) -> (String, usize) {
    let length = utf16_len(text);
    let safe_start = range_start.min(length);
    let safe_end = range_end.clamp(safe_start, length);
    let next_text = format!(
        "{}{replacement}{}",
        slice_utf16(text, 0, safe_start),
        slice_utf16(text, safe_end, length)
    );
    let cursor = safe_start + utf16_len(replacement);
    (next_text, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_inline_slash_skills_after_whitespace_prefixes() {
        for prefix in ["Use ", "Use\t", "Use\n  "] {
            let text = format!("{prefix}/skill:review later");
            let cursor = utf16_len(&format!("{prefix}/skill:review"));
            assert_eq!(
                detect_composer_trigger(&text, cursor as f64, None),
                Some(ComposerTrigger {
                    kind: ComposerTriggerKind::SlashCommand,
                    query: "skill:review".to_owned(),
                    range_start: prefix.chars().map(|c| c.len_utf16()).sum::<usize>(),
                    range_end: cursor,
                }),
                "{prefix:?}"
            );
        }
    }

    #[test]
    fn detects_the_inline_slash_token() {
        for text in ["Use /", "Use /review"] {
            assert_eq!(
                detect_composer_trigger(text, utf16_len(text) as f64, None),
                Some(ComposerTrigger {
                    kind: ComposerTriggerKind::SlashCommand,
                    query: text[5..].to_owned(),
                    range_start: 4,
                    range_end: utf16_len(text),
                }),
                "{text}"
            );
        }
    }

    #[test]
    fn does_not_trigger_for_non_commands() {
        for text in ["Use/review", "https://example.com/review", "Use /review "] {
            assert_eq!(detect_composer_trigger(text, utf16_len(text) as f64, None), None, "{text}");
        }
    }

    #[test]
    fn detects_skill_prefixes_and_their_source_range() {
        for prefix in ["$", "€", "£", "¥", "₹", "₩", "₿", "𑿝"] {
            let text = format!("Use {prefix}review");
            assert_eq!(
                detect_composer_trigger(&text, utf16_len(&text) as f64, None),
                Some(ComposerTrigger {
                    kind: ComposerTriggerKind::Skill,
                    query: "review".to_owned(),
                    range_start: 4,
                    range_end: utf16_len(&text),
                }),
                "{prefix}"
            );
        }
    }

    #[test]
    fn uses_the_basename_as_the_markdown_label() {
        assert_eq!(
            serialize_composer_file_link("path/to/package.json"),
            "[package.json](path/to/package.json)"
        );
    }

    #[test]
    fn encodes_markdown_sensitive_destination_characters() {
        assert_eq!(
            serialize_composer_file_link("docs/My File (draft).md"),
            "[My File (draft).md](docs/My%20File%20%28draft%29.md)"
        );
    }

    #[test]
    fn supports_windows_paths() {
        assert_eq!(
            serialize_composer_file_link("C:\\repo\\src\\index.ts"),
            "[index.ts](C:%5Crepo%5Csrc%5Cindex.ts)"
        );
    }

    #[test]
    fn preserves_paths_that_legitimately_start_with_an_at_sign() {
        assert_eq!(
            serialize_composer_file_link("@scope/package.json"),
            "[package.json](@scope/package.json)"
        );
    }
}
