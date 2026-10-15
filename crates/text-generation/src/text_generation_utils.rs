//! Port of upstream/packages/provider-core/src/server/textGenerationUtils.ts
//!
//! `toJsonSchemaObject` has no counterpart: output schemas are written as
//! `serde_json::Value` next to each operation (see `text_generation_operations`).

use turin_contracts::git::TextGenerationError;
use turin_provider_core::js::{collapse_whitespace, is_js_whitespace, take_utf16, utf16_len};

use crate::text_generation_operations::Operation;

/// `limitSection`: truncate to `max_chars` UTF-16 units, appending a `[truncated]` marker.
pub fn limit_section(value: &str, max_chars: usize) -> String {
    if utf16_len(value) <= max_chars {
        return value.to_owned();
    }
    format!("{}\n\n[truncated]", take_utf16(value, max_chars))
}

/// `String.prototype.trimEnd` (JS whitespace, not `str::trim_end`).
fn js_trim_end(value: &str) -> &str {
    value.trim_end_matches(is_js_whitespace)
}

/// First line of `value`, trimmed the way `value.trim().split(/\r?\n/g)[0]?.trim()` does.
fn first_trimmed_line(value: &str) -> &str {
    let trimmed = turin_contracts::js_trim(value);
    let line = trimmed.split('\n').next().unwrap_or("");
    turin_contracts::js_trim(line)
}

/// Normalise a raw commit subject to imperative-mood, ≤72 chars, no trailing period.
pub fn sanitize_commit_subject(raw: &str) -> String {
    let single_line = first_trimmed_line(raw);
    let without_trailing_period =
        turin_contracts::js_trim(single_line.trim_end_matches('.')).to_owned();
    if without_trailing_period.is_empty() {
        return "Update project files".to_owned();
    }
    if utf16_len(&without_trailing_period) <= 72 {
        return without_trailing_period;
    }
    js_trim_end(take_utf16(&without_trailing_period, 72)).to_owned()
}

/// Normalise a raw PR title to a single line with a sensible fallback.
pub fn sanitize_pr_title(raw: &str) -> String {
    let single_line = first_trimmed_line(raw);
    if single_line.is_empty() {
        return "Update project changes".to_owned();
    }
    single_line.to_owned()
}

// Prompts ask for under 40 characters. This cap only stops a runaway model
// from pushing a paragraph into the sidebar, header, and window title.
const MAX_THREAD_TITLE_CHARS: usize = 120;

#[derive(serde::Deserialize)]
struct TitleObject {
    title: String,
}

/// Normalise a raw thread title to a single line. Clients truncate for display.
pub fn sanitize_thread_title(raw: &str) -> String {
    // Unwrap a JSON-formatted title before truncation can cut off the closing brace.
    let title = serde_json::from_str::<TitleObject>(raw)
        .map(|decoded| decoded.title)
        .unwrap_or_else(|_| raw.to_owned());
    let line = first_trimmed_line(&title);
    let unquoted = line.trim_start_matches(['\'', '"', '`']);
    let unquoted = unquoted.trim_end_matches(['\'', '"', '`']);
    let normalized = collapse_whitespace(turin_contracts::js_trim(unquoted));
    if normalized.is_empty() {
        return "New thread".to_owned();
    }
    if utf16_len(&normalized) <= MAX_THREAD_TITLE_CHARS {
        return normalized;
    }
    format!(
        "{}...",
        js_trim_end(take_utf16(&normalized, MAX_THREAD_TITLE_CHARS - 3))
    )
}

/// CLI name to human-readable label, e.g. "codex" → "Codex CLI (`codex`)".
fn cli_label(cli_name: &str) -> String {
    let mut chars = cli_name.chars();
    let capitalized = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => String::new(),
    };
    format!("{capitalized} CLI (`{cli_name}`)")
}

/// A failure from a CLI run, in the shapes upstream's `normalizeCliError` tells apart.
#[derive(Debug, Clone)]
pub enum CliFailure {
    /// Already a `TextGenerationError`: returned as-is.
    TextGeneration(TextGenerationError),
    /// An `Error`: its message is checked for a missing binary.
    Error(String),
    /// Any other value: the fallback is used.
    Other,
}

impl From<String> for CliFailure {
    fn from(message: String) -> Self {
        Self::Error(message)
    }
}

/// Normalize a CLI failure into a typed `TextGenerationError`.
///
/// Only the message of an `Error` is inspected, and it is kept as `cause` only, never in
/// `detail`, so a secret in a CLI message cannot reach a caller's error text.
pub fn normalize_cli_error(
    cli_name: &str,
    operation: Operation,
    failure: impl Into<CliFailure>,
    fallback: &str,
) -> TextGenerationError {
    let message = match failure.into() {
        CliFailure::TextGeneration(existing) => return existing,
        CliFailure::Other => {
            return TextGenerationError::TextGenerationError {
                operation: operation.as_str().to_owned(),
                detail: fallback.to_owned(),
                cause: None,
            };
        }
        CliFailure::Error(message) => message,
    };
    let lower = message.to_lowercase();
    let not_found = message.contains(&format!("Command not found: {cli_name}"))
        || lower.contains(&format!("spawn {}", cli_name.to_lowercase()))
        || lower.contains("enoent");
    let detail = if not_found {
        format!(
            "{} is required but not available on PATH.",
            cli_label(cli_name)
        )
    } else {
        fallback.to_owned()
    };
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail,
        cause: Some(serde_json::Value::String(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation() -> Operation {
        Operation::GenerateCommitMessage
    }

    #[test]
    fn sanitize_thread_title_unwraps_a_json_title_before_normalizing() {
        for raw in [
            r#"{"title": "Refresh ev-stg APP ASG instances"}"#,
            "{\n  \"title\": \"Refresh ev-stg APP ASG instances\"\n}",
        ] {
            assert_eq!(
                sanitize_thread_title(raw),
                "Refresh ev-stg APP ASG instances"
            );
        }
    }

    #[test]
    fn sanitize_thread_title_preserves_text_that_is_not_a_json_title() {
        for raw in [
            "Rolling ES Refresh ev-stg",
            "Fix {title} interpolation",
            r#"{"title": 42}"#,
            r#"{"subject": "Fix parsing"}"#,
            r#"{"title": "unfinished}"#,
        ] {
            assert_eq!(sanitize_thread_title(raw), raw);
        }
    }

    #[test]
    fn sanitize_thread_title_normalizes_the_extracted_title() {
        assert_eq!(
            sanitize_thread_title(r#"{"title": "  Fix   reconnect failures  "}"#),
            "Fix reconnect failures"
        );
        assert_eq!(sanitize_thread_title(r#"{"title": "  "}"#), "New thread");
        assert_eq!(
            sanitize_thread_title(
                r#"{"title": "Reconnect failures after restart because the session state does not recover"}"#
            ),
            "Reconnect failures after restart because the session state does not recover"
        );
    }

    #[test]
    fn sanitize_thread_title_keeps_complete_titles_for_client_display_truncation() {
        assert_eq!(
            sanitize_thread_title(
                "  \"Reconnect failures after restart because the session state does not recover\"  "
            ),
            "Reconnect failures after restart because the session state does not recover"
        );
    }

    #[test]
    fn sanitize_thread_title_caps_runaway_titles_so_a_paragraph_cannot_reach_the_sidebar() {
        let words = (0..40)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let title = sanitize_thread_title(&words);
        assert!(utf16_len(&title) <= 120);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn sanitize_commit_subject_drops_trailing_periods_and_falls_back_when_empty() {
        assert_eq!(
            sanitize_commit_subject("  Add search.\nbody line"),
            "Add search"
        );
        assert_eq!(sanitize_commit_subject("  ...  "), "Update project files");
    }

    #[test]
    fn sanitize_commit_subject_caps_at_72_units() {
        let subject = sanitize_commit_subject(&"a".repeat(100));
        assert_eq!(subject.len(), 72);
    }

    #[test]
    fn sanitize_pr_title_takes_the_first_line_or_falls_back() {
        assert_eq!(sanitize_pr_title("  Improve flow\nignored"), "Improve flow");
        assert_eq!(sanitize_pr_title("   "), "Update project changes");
    }

    #[test]
    fn limit_section_marks_truncation_only_when_it_happens() {
        assert_eq!(limit_section("short", 10), "short");
        assert_eq!(limit_section("abcdef", 3), "abc\n\n[truncated]");
    }

    #[test]
    fn normalize_cli_error_detects_command_not_found_and_names_the_cli() {
        let error = normalize_cli_error(
            "claude",
            Operation::GenerateCommitMessage,
            CliFailure::Error("Command not found: claude".to_owned()),
            "Something went wrong",
        );
        let TextGenerationError::TextGenerationError { detail, .. } = error;
        assert!(detail.contains("Claude CLI"));
        assert!(detail.contains("not available on PATH"));
    }

    #[test]
    fn normalize_cli_error_uses_the_cli_name_from_the_first_argument_for_codex() {
        let error = normalize_cli_error(
            "codex",
            Operation::GenerateBranchName,
            CliFailure::Error("Command not found: codex".to_owned()),
            "Something went wrong",
        );
        let TextGenerationError::TextGenerationError { detail, .. } = error;
        assert!(detail.contains("Codex CLI"));
        assert!(detail.contains("not available on PATH"));
    }

    #[test]
    fn normalize_cli_error_returns_an_existing_text_generation_error_as_is() {
        let existing = TextGenerationError::TextGenerationError {
            operation: "generatePrContent".to_owned(),
            detail: "Already wrapped".to_owned(),
            cause: None,
        };
        let result = normalize_cli_error(
            "claude",
            Operation::GeneratePrContent,
            CliFailure::TextGeneration(existing.clone()),
            "fallback",
        );
        assert_eq!(result, existing);
    }

    #[test]
    fn normalize_cli_error_wraps_non_error_values_with_the_fallback_message() {
        let result = normalize_cli_error("codex", operation(), CliFailure::Other, "fallback");
        let TextGenerationError::TextGenerationError { detail, .. } = result;
        assert_eq!(detail, "fallback");
    }

    #[test]
    fn normalize_cli_error_does_not_expose_cli_failure_details_in_the_public_error_message() {
        let result = normalize_cli_error(
            "codex",
            operation(),
            CliFailure::Error("request failed with access_token=secret-token".to_owned()),
            "Failed to generate a commit message",
        );
        let TextGenerationError::TextGenerationError { detail, .. } = &result;
        assert_eq!(detail, "Failed to generate a commit message");
        assert!(!result.to_string().contains("secret-token"));
    }
}
