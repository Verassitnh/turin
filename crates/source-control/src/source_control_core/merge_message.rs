//! Port of upstream/packages/source-control-core/src/server/mergeMessage.ts

use regex::Regex;
use std::sync::LazyLock;

const AGENT_EMAILS: &[&str] = &[
    "noreply@anthropic.com",
    "claude@anthropic.com",
    "noreply@openai.com",
    "codex@openai.com",
    "cursoragent@cursor.com",
    "copilot@github.com",
    "noreply@github.com",
];

static GENERATED_BY_AGENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:🤖\s*)?generated (?:with|by)\s+(?:claude(?: code)?|codex|cursor|github copilot|copilot|opencode|grok|antigravity)[.!]?$",
    )
    .expect("valid regex")
});
static INDENTED_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?: {4}|\t)").unwrap());
static FENCE_DELIMITER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ {0,3}(`{3,}|~{3,})(.*)$").unwrap());
static BLANK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]*$").unwrap());
static LIST_BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[-*]\s+)").unwrap());
static CO_AUTHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^co-authored-by:\s*(.*?)\s*<([^<>]+)>\s*$").unwrap());
static COPILOT_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^github copilot$|^copilot$").unwrap());
static CREDIT_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\[([^\]]+)\]\(https://(?:claude\.ai|www\.anthropic\.com|chatgpt\.com|openai\.com|cursor\.com|github\.com|opencode\.ai|x\.ai|antigravity\.google)(?:/[^\s)]*)?\)",
    )
    .unwrap()
});
static TRAILING_BLANKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\r?\n[ \t]*)+$").unwrap());

/// JS `String.prototype.trim` whitespace set (also strips U+FEFF).
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// Remove standalone agent credits while keeping human signatures and quoted examples.
pub fn remove_agent_credits(message: &str) -> String {
    let mut fence: Option<String> = None;
    let mut removed = false;
    let newline = if message.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut kept: Vec<&str> = Vec::new();
    // JS: message.split(/\r?\n/)
    for line in message
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
    {
        if INDENTED_CODE.is_match(line) {
            kept.push(line);
            continue;
        }
        if let Some(caps) = FENCE_DELIMITER.captures(line) {
            let marker = caps.get(1).map_or("", |m| m.as_str());
            let remainder = caps.get(2).map_or("", |m| m.as_str());
            match &fence {
                None => {
                    if marker.starts_with('~') || !remainder.contains('`') {
                        fence = Some(marker.to_owned());
                    }
                }
                Some(open) => {
                    if marker.as_bytes()[0] == open.as_bytes()[0]
                        && marker.len() >= open.len()
                        && BLANK.is_match(remainder)
                    {
                        fence = None;
                    }
                }
            }
            kept.push(line);
            continue;
        }
        if fence.is_some() {
            kept.push(line);
            continue;
        }
        let trimmed = js_trim(line);
        let text = LIST_BULLET.replace(trimmed, "");
        let author = CO_AUTHOR.captures(&text);
        // GitHub's generic noreply address is also used by people; require Copilot's name.
        let agent_author = author.as_ref().is_some_and(|caps| {
            let email = caps.get(2).map_or("", |m| m.as_str()).to_lowercase();
            AGENT_EMAILS.contains(&email.as_str())
                && (email != "noreply@github.com"
                    || COPILOT_NAME.is_match(caps.get(1).map_or("", |m| m.as_str())))
        });
        let footer = CREDIT_LINK.replace_all(&text, "$1");
        if !agent_author && !GENERATED_BY_AGENT.is_match(&footer) {
            kept.push(line);
            continue;
        }
        removed = true;
    }
    if removed {
        TRAILING_BLANKS
            .replace(&kept.join(newline), "")
            .into_owned()
    } else {
        message.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const EXAMPLE: &str = "Co-authored-by: Claude <noreply@anthropic.com>";

    #[test]
    fn removes_recognized_agent_trailers_and_generated_footers_keeping_human_co_authors() {
        let message = [
            "Details about Claude support.",
            "",
            "",
            "Co-authored-by: Alice <alice@example.com>",
            "Co-authored-by: Claude <noreply@anthropic.com>",
            "* Co-authored-by: Codex <noreply@openai.com>",
            "co-authored-by: Cursor <cursoragent@cursor.com>",
            "Co-authored-by: Copilot <copilot@github.com>",
            "",
            "🤖 Generated with [Claude Code](https://claude.ai/code)",
        ]
        .join("\n");
        assert_eq!(
            remove_agent_credits(&message),
            "Details about Claude support.\n\n\nCo-authored-by: Alice <alice@example.com>"
        );
    }

    #[test]
    fn preserves_unknown_identities_humans_named_after_agents_and_ordinary_prose() {
        let message = [
            "Co-authored-by: Claude <claude@example.com>",
            "Co-authored-by: Alice <noreply@github.com>",
            "Co-authored-by: Release Bot <release@example.com>",
            "Generated by our new Claude integration.",
            "Generated with [Claude Code](https://example.com)",
            "",
        ]
        .join("\n");
        assert_eq!(remove_agent_credits(&message), message);
    }

    #[test]
    fn preserves_code_fences_and_block_quotes_containing_attribution_examples() {
        let message = format!("Example:\n\n```text\n{EXAMPLE}\n```\n\n> {EXAMPLE}\n\n{EXAMPLE}");
        assert_eq!(
            remove_agent_credits(&message),
            format!("Example:\n\n```text\n{EXAMPLE}\n```\n\n> {EXAMPLE}")
        );
    }

    #[test]
    fn preserves_indented_examples_and_whitespace_on_surviving_lines() {
        assert_eq!(
            remove_agent_credits(&format!(
                "Example:\n\n    {EXAMPLE}\n\n{EXAMPLE}\nDetails  \n"
            )),
            format!("Example:\n\n    {EXAMPLE}\n\nDetails  ")
        );
    }

    #[test]
    fn removes_trailers_after_a_line_that_cannot_open_a_fence() {
        for line in ["``` text ```", "    ```", "\t~~~"] {
            assert_eq!(
                remove_agent_credits(&format!("{line}\n\n{EXAMPLE}")),
                line,
                "{line:?}"
            );
        }
    }

    #[test]
    fn keeps_examples_after_a_false_closing_fence() {
        for fence in ["```", "~~~"] {
            let first = &fence[..1];
            let content = format!(
                "{fence}text\n{fence} trailing text\n{EXAMPLE}\n    {fence}\n{EXAMPLE}\n{}\n{EXAMPLE}\n   {fence}{first} \t",
                &fence[..2]
            );
            assert_eq!(
                remove_agent_credits(&format!("{content}\n\n{EXAMPLE}")),
                content
            );
        }
    }

    #[test]
    fn preserves_crlf_line_endings() {
        assert_eq!(
            remove_agent_credits(
                "Details\r\n\r\nCo-authored-by: Claude <noreply@anthropic.com>\r\nCo-authored-by: Alice <alice@example.com>"
            ),
            "Details\r\n\r\nCo-authored-by: Alice <alice@example.com>"
        );
    }

    #[test]
    fn returns_an_empty_body_when_only_agent_credits_remain() {
        assert_eq!(
            remove_agent_credits(
                "Co-authored-by: Claude <noreply@anthropic.com>\n\nGenerated by Codex\n"
            ),
            ""
        );
    }
}
