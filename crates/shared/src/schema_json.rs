//! Port of upstream/packages/shared/src/schemaJson.ts
//!
//! Effect Schema codecs (`decodeJsonResult`, `fromLenientJson`,
//! `fromJsonStringPretty`) have no counterpart here. The portable pieces are
//! object extraction, lenient JSONC parsing, and the diagnostic formatter.
//! Callers pass an explicit issue tree; the formatter never prints values.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use turin_contracts::base_schemas::js_trim;

use crate::js_string::{slice_utf16, utf16_len};

const MAX_SCHEMA_DIAGNOSTIC_ISSUES: usize = 8;
const MAX_SCHEMA_DIAGNOSTIC_PATH_SEGMENTS: usize = 16;
const MAX_SCHEMA_DIAGNOSTIC_PATH_SEGMENT_LENGTH: usize = 64;
const MAX_SCHEMA_DIAGNOSTIC_LENGTH: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaPathKey {
    Index(i64),
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaIssue {
    InvalidType,
    InvalidValue,
    MissingKey,
    UnexpectedKey,
    Forbidden,
    OneOf,
    Filter(Box<SchemaIssue>),
    Encoding(Box<SchemaIssue>),
    Pointer {
        path: Vec<SchemaPathKey>,
        issue: Box<SchemaIssue>,
    },
    Composite(Vec<SchemaIssue>),
    AnyOf(Vec<SchemaIssue>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaReason {
    Fail(SchemaIssue),
    Die,
    Interrupt,
}

struct DiagnosticIssue {
    message: String,
    path: Vec<SchemaPathKey>,
}

pub fn extract_json_object(raw: &str) -> String {
    let trimmed = js_trim(raw);
    if trimmed.is_empty() {
        return String::new();
    }
    let Some(start) = trimmed.find('{') else {
        return trimmed.to_owned();
    };
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaping = false;
    for (index, char) in trimmed[start..].char_indices() {
        if in_string {
            if escaping {
                escaping = false;
            } else if char == '\\' {
                escaping = true;
            } else if char == '"' {
                in_string = false;
            }
            continue;
        }
        if char == '"' {
            in_string = true;
            continue;
        }
        if char == '{' {
            depth += 1;
            continue;
        }
        if char == '}' {
            depth -= 1;
            if depth == 0 {
                return trimmed[start..start + index + char.len_utf8()].to_owned();
            }
        }
    }
    trimmed[start..].to_owned()
}

/// Strips `//` comments, `/* */` comments, and trailing commas, then parses JSON.
/// A parse failure is `Invalid value` and does not include the input.
pub fn parse_lenient_json(input: &str) -> Result<Value, &'static str> {
    let stripped = strip_lenient_json(input);
    serde_json::from_str(&stripped).map_err(|_| "Invalid value")
}

pub fn format_schema_error(reasons: &[SchemaReason]) -> String {
    let mut issues = Vec::new();
    let mut issue_count = 0usize;
    let mut failure_count = 0usize;
    let mut defect_count = 0usize;
    let mut interruption_count = 0usize;
    for reason in reasons {
        match reason {
            SchemaReason::Fail(issue) => {
                failure_count += 1;
                issue_count += collect(issue, &[], &mut issues);
            }
            SchemaReason::Die => defect_count += 1,
            SchemaReason::Interrupt => interruption_count += 1,
        }
    }
    if issues.is_empty() {
        return format!(
            "Schema validation failed (failureCount={failure_count}, defectCount={defect_count}, interruptionCount={interruption_count})."
        );
    }
    let omitted = issue_count - issues.len();
    let formatted = issues
        .iter()
        .map(format_diagnostic_issue)
        .collect::<Vec<_>>()
        .join("\n");
    if omitted == 0 {
        return truncate_diagnostic(&formatted, MAX_SCHEMA_DIAGNOSTIC_LENGTH);
    }
    let suffix = format!("\n... and {omitted} more issue(s)");
    let room = MAX_SCHEMA_DIAGNOSTIC_LENGTH - utf16_len(&suffix);
    truncate_diagnostic(&formatted, room) + &suffix
}

fn collect(issue: &SchemaIssue, path: &[SchemaPathKey], diagnostics: &mut Vec<DiagnosticIssue>) -> usize {
    match issue {
        SchemaIssue::Encoding(inner) => return collect(inner, path, diagnostics),
        SchemaIssue::Filter(inner) if !matches!(inner.as_ref(), SchemaIssue::InvalidValue) => {
            return collect(inner, path, diagnostics);
        }
        SchemaIssue::Pointer {
            path: pointer,
            issue,
        } => {
            let mut next = path.to_vec();
            next.extend(pointer.iter().cloned());
            return collect(issue, &next, diagnostics);
        }
        SchemaIssue::Composite(issues) => {
            return issues
                .iter()
                .map(|issue| collect(issue, path, diagnostics))
                .sum();
        }
        SchemaIssue::AnyOf(issues) if !issues.is_empty() => {
            return issues
                .iter()
                .map(|issue| collect(issue, path, diagnostics))
                .sum();
        }
        SchemaIssue::AnyOf(_) => {}
        _ => {}
    }
    if diagnostics.len() < MAX_SCHEMA_DIAGNOSTIC_ISSUES {
        diagnostics.push(DiagnosticIssue {
            message: diagnostic_message(issue).to_owned(),
            path: path.to_vec(),
        });
    }
    1
}

fn diagnostic_message(issue: &SchemaIssue) -> &'static str {
    match issue {
        SchemaIssue::InvalidType => "Invalid type",
        SchemaIssue::MissingKey => "Missing key",
        SchemaIssue::UnexpectedKey => "Unexpected key",
        SchemaIssue::Forbidden => "Forbidden operation",
        SchemaIssue::OneOf => "Expected exactly one schema member to match",
        _ => "Invalid value",
    }
}

fn format_diagnostic_issue(issue: &DiagnosticIssue) -> String {
    if issue.path.is_empty() {
        return issue.message.clone();
    }
    let path = issue
        .path
        .iter()
        .take(MAX_SCHEMA_DIAGNOSTIC_PATH_SEGMENTS)
        .map(format_path_segment)
        .collect::<String>();
    let suffix = if issue.path.len() > MAX_SCHEMA_DIAGNOSTIC_PATH_SEGMENTS {
        "[...]"
    } else {
        ""
    };
    format!("{}\n  at {path}{suffix}", issue.message)
}

fn format_path_segment(key: &SchemaPathKey) -> String {
    match key {
        SchemaPathKey::Index(index) => format!("[{index}]"),
        SchemaPathKey::Name(name) => {
            let truncated = truncate_diagnostic(name, MAX_SCHEMA_DIAGNOSTIC_PATH_SEGMENT_LENGTH);
            format!("[{}]", serde_json::to_string(&truncated).unwrap_or_else(|_| "\"\"".into()))
        }
    }
}

fn truncate_diagnostic(value: &str, max_length: usize) -> String {
    if utf16_len(value) <= max_length {
        value.to_owned()
    } else {
        format!("{}...", slice_utf16(value, 0, max_length.saturating_sub(3)))
    }
}

fn strip_lenient_json(input: &str) -> String {
    let line_comments = regex(r#"("(?:[^"\\]|\\.)*")|//[^\n]*"#);
    let stripped = line_comments
        .replace_all(input, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures.get(0).unwrap().as_str().to_owned()
            } else {
                String::new()
            }
        })
        .into_owned();
    let block_comments = regex(r#"("(?:[^"\\]|\\.)*")|/\*(?s:.*?)\*/"#);
    let stripped = block_comments
        .replace_all(&stripped, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures.get(0).unwrap().as_str().to_owned()
            } else {
                String::new()
            }
        })
        .into_owned();
    let trailing = regex(r#"("(?:[^"\\]|\\.)*")|,(\s*[}\]])"#);
    trailing
        .replace_all(&stripped, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures.get(0).unwrap().as_str().to_owned()
            } else {
                captures.get(2).map(|group| group.as_str().to_owned()).unwrap_or_default()
            }
        })
        .into_owned()
}

fn regex(pattern: &str) -> &'static Regex {
    static LINE: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static TRAILING: OnceLock<Regex> = OnceLock::new();
    match pattern {
        r#"("(?:[^"\\]|\\.)*")|//[^\n]*"# => LINE.get_or_init(|| Regex::new(pattern).unwrap()),
        r#"("(?:[^"\\]|\\.)*")|/\*(?s:.*?)\*/"# => {
            BLOCK.get_or_init(|| Regex::new(pattern).unwrap())
        }
        _ => TRAILING.get_or_init(|| Regex::new(pattern).unwrap()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_balanced_object() {
        let raw = "Sure, here is the JSON:\n\n```json\n{\n  \"subject\": \"Update README\",\n  \"body\": \"\"\n}\n```\nDone.";
        assert_eq!(
            extract_json_object(raw),
            "{\n  \"subject\": \"Update README\",\n  \"body\": \"\"\n}"
        );
    }

    #[test]
    fn ignores_braces_inside_strings() {
        assert_eq!(
            extract_json_object("prefix {\"message\":\"literal } brace\",\"nested\":{\"ok\":true}} suffix"),
            "{\"message\":\"literal } brace\",\"nested\":{\"ok\":true}}"
        );
    }

    #[test]
    fn returns_trimmed_input_when_no_object_starts() {
        assert_eq!(
            extract_json_object("  no structured output  "),
            "no structured output"
        );
    }

    #[test]
    fn decodes_json_with_comments_and_trailing_commas() {
        let value = parse_lenient_json(
            "{\n        // Comments are valid in settings files.\n        \"enabled\": true,\n        \"values\": [1, 2,],\n      }",
        )
        .unwrap();
        assert_eq!(value["enabled"], true);
        assert_eq!(value["values"][0], 1);
        assert_eq!(value["values"][1], 2);
    }

    #[test]
    fn rejects_malformed_json_without_the_secret() {
        let error = parse_lenient_json("{\"token\":\"credential=secret-value\",,}").unwrap_err();
        assert_eq!(error, "Invalid value");
        assert!(!error.contains("credential=secret-value"));
    }

    #[test]
    fn preserves_commas_inside_string_values() {
        let note = parse_lenient_json("{\"note\":\"a,]\"}").unwrap();
        assert_eq!(note["note"], "a,]");
        let list = parse_lenient_json("{\"list\":[\"x,}\"]}").unwrap();
        assert_eq!(list["list"][0], "x,}");
        let values = parse_lenient_json("{\"values\":[1, 2,],}").unwrap();
        assert_eq!(values["values"][1], 2);
    }

    #[test]
    fn formats_a_type_failure_with_its_path() {
        let diagnostic = format_schema_error(&[SchemaReason::Fail(SchemaIssue::Pointer {
            path: vec![SchemaPathKey::Name("token".into())],
            issue: Box::new(SchemaIssue::InvalidType),
        })]);
        assert_eq!(diagnostic, "Invalid type\n  at [\"token\"]");
    }

    #[test]
    fn preserves_nested_filter_paths() {
        let diagnostic = format_schema_error(&[SchemaReason::Fail(SchemaIssue::Pointer {
            path: vec![
                SchemaPathKey::Name("session".into()),
                SchemaPathKey::Name("token".into()),
            ],
            issue: Box::new(SchemaIssue::Filter(Box::new(SchemaIssue::InvalidValue))),
        })]);
        assert_eq!(diagnostic, "Invalid value\n  at [\"session\"][\"token\"]");
        assert!(!diagnostic.contains("credential=secret-value"));
    }

    #[test]
    fn summarizes_a_defect_without_its_message() {
        assert_eq!(
            format_schema_error(&[SchemaReason::Die]),
            "Schema validation failed (failureCount=0, defectCount=1, interruptionCount=0)."
        );
    }

    #[test]
    fn bounds_the_number_of_formatted_issues() {
        let reason = SchemaReason::Fail(SchemaIssue::Pointer {
            path: vec![SchemaPathKey::Name("token".into())],
            issue: Box::new(SchemaIssue::InvalidType),
        });
        let reasons = vec![reason; 10];
        let diagnostic = format_schema_error(&reasons);
        assert_eq!(diagnostic.matches("Invalid type").count(), 8);
        assert!(diagnostic.contains("... and 2 more issue(s)"));
    }

    #[test]
    fn retains_the_omitted_count_when_bounding_long_diagnostics() {
        let long_path = (0..16)
            .map(|index| SchemaPathKey::Name(format!("{index}-{}", "segment".repeat(16))))
            .collect();
        let reason = SchemaReason::Fail(SchemaIssue::Pointer {
            path: long_path,
            issue: Box::new(SchemaIssue::InvalidValue),
        });
        let reasons = vec![reason; 10];
        let diagnostic = format_schema_error(&reasons);
        assert!(utf16_len(&diagnostic) <= 2_048);
        assert!(diagnostic.ends_with("\n... and 2 more issue(s)"));
    }
}
