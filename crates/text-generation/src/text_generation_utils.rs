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

/// Normalize a spawn or process error into a typed `TextGenerationError`.
///
/// Upstream takes an unknown error; Rust callers pass the error's message. An
/// `error` that already is a `TextGenerationError` is returned as-is by the
/// caller, so this only sees plain failures.
pub fn normalize_cli_error(
    cli_name: &str,
    operation: Operation,
    error: &str,
    fallback: &str,
) -> TextGenerationError {
    let lower = error.to_lowercase();
    let not_found = error.contains(&format!("Command not found: {cli_name}"))
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
        cause: Some(serde_json::Value::String(error.to_owned())),
    }
}
