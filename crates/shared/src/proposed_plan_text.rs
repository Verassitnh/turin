//! Port of upstream/packages/shared/src/proposedPlanText.ts

use crate::js_string::JS_WS;
use turin_contracts::base_schemas::js_trim;

/// JS `.` (no `s` flag) and `$`/`^` under `/m` all treat these as line terminators.
const LINE_TERMINATORS: &str = r"\n\r\x{2028}\x{2029}";

/// `/^\s{0,3}#{1,6}\s+(.+)$/m`, with JS `\s`, `.`, `^`, `$` semantics spelled out.
fn heading_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(&format!(
            r"(?m)(?:^|[\r\x{{2028}}\x{{2029}}])[{ws}]{{0,3}}#{{1,6}}[{ws}]+([^{lt}]+)(?:[\r\x{{2028}}\x{{2029}}]|$)",
            ws = JS_WS,
            lt = LINE_TERMINATORS
        ))
        .expect("static regex is valid")
    })
}

/// `/^\s{0,3}#{1,6}\s+(.+)$/` (single line, no `m`).
fn heading_text_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(&format!(
            r"^[{ws}]{{0,3}}#{{1,6}}[{ws}]+([^{lt}]+)$",
            ws = JS_WS,
            lt = LINE_TERMINATORS
        ))
        .expect("static regex is valid")
    })
}

pub fn proposed_plan_title(plan_markdown: &str) -> Option<String> {
    let heading = heading_pattern().captures(plan_markdown)?.get(1)?.as_str();
    let heading = js_trim(heading);
    (!heading.is_empty()).then(|| heading.to_owned())
}

/// JS `String.prototype.trimEnd` (same whitespace set as `trim`).
fn js_trim_end(value: &str) -> &str {
    (value.trim_end_matches(crate::js_string::is_js_whitespace_char)) as _
}

pub fn strip_displayed_plan_markdown(plan_markdown: &str) -> String {
    let trimmed = js_trim_end(plan_markdown);
    let lines: Vec<&str> = split_crlf(trimmed);
    let mut source_lines: Vec<&str> = lines;
    if source_lines
        .first()
        .is_some_and(|first| regex_first_line_heading().is_match(first))
    {
        source_lines.remove(0);
    }
    while source_lines
        .first()
        .is_some_and(|first| js_trim(first).is_empty())
    {
        source_lines.remove(0);
    }
    let first_heading_match = source_lines
        .first()
        .and_then(|first| heading_text_pattern().captures(first));
    if first_heading_match
        .as_ref()
        .and_then(|captures| captures.get(1))
        .map(|m| js_trim(m.as_str()).to_lowercase())
        .is_some_and(|heading| heading == "summary")
    {
        source_lines.remove(0);
        while source_lines
            .first()
            .is_some_and(|first| js_trim(first).is_empty())
        {
            source_lines.remove(0);
        }
    }
    source_lines.join("\n")
}

// /^\s{0,3}#{1,6}\s+/
fn regex_first_line_heading() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(&format!(r"^[{ws}]{{0,3}}#{{1,6}}[{ws}]+", ws = JS_WS))
            .expect("static regex is valid")
    })
}

/// `value.split(/\r?\n/)`
fn split_crlf(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut cursor = 0usize;
    let bytes = value.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            parts.push(&value[cursor..index]);
            index += 2;
            cursor = index;
        } else if bytes[index] == b'\n' {
            parts.push(&value[cursor..index]);
            index += 1;
            cursor = index;
        } else {
            index += 1;
        }
    }
    parts.push(&value[cursor..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_first_markdown_heading_as_the_plan_title() {
        assert_eq!(
            proposed_plan_title("# Integrate RPC\n\nBody"),
            Some("Integrate RPC".to_owned())
        );
    }

    #[test]
    fn returns_null_when_the_plan_has_no_heading() {
        assert_eq!(proposed_plan_title("- step 1"), None);
    }

    #[test]
    fn drops_the_leading_title_heading_from_displayed_plan_markdown() {
        assert_eq!(
            strip_displayed_plan_markdown("# Integrate RPC\n\n## Summary\n\n- step 1\n"),
            "- step 1"
        );
    }

    #[test]
    fn preserves_non_summary_headings_after_dropping_the_title_heading() {
        assert_eq!(
            strip_displayed_plan_markdown("# Integrate RPC\n\n## Scope\n\n- step 1\n"),
            "## Scope\n\n- step 1"
        );
    }
}
