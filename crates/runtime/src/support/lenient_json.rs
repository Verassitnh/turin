//! Port of `fromLenientJson` / `fromJsonStringPretty` from
//! upstream/packages/shared/src/schemaJson.ts.
//!
//! The comment/trailing-comma stripper is copied from `turin-shared` (T05a)
//! until that crate lands on this branch.

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

/// Strips `//` comments, `/* */` comments, and trailing commas, then parses
/// JSON (`fromLenientJson`'s decode side).
pub fn parse_lenient_json(input: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(&strip_lenient_json(input))
}

/// `fromJsonStringPretty` encode side: `JSON.stringify(value, null, 2)`.
pub fn to_json_string_pretty<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    serde_json::to_string_pretty(value)
}

fn strip_lenient_json(input: &str) -> String {
    let stripped = line_comments()
        .replace_all(input, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures[0].to_owned()
            } else {
                String::new()
            }
        })
        .into_owned();
    let stripped = block_comments()
        .replace_all(&stripped, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures[0].to_owned()
            } else {
                String::new()
            }
        })
        .into_owned();
    trailing_commas()
        .replace_all(&stripped, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures[0].to_owned()
            } else {
                captures
                    .get(2)
                    .map(|group| group.as_str().to_owned())
                    .unwrap_or_default()
            }
        })
        .into_owned()
}

fn line_comments() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|//[^\n]*"#).unwrap())
}

fn block_comments() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|/\*(?s:.*?)\*/"#).unwrap())
}

fn trailing_commas() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|,(\s*[}\]])"#).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comments_and_trailing_commas_outside_strings() {
        let value = parse_lenient_json(
            "{\n  // note\n  \"a\": \"http://x\", /* block */\n  \"b\": [1, 2,],\n}",
        )
        .unwrap();
        assert_eq!(value, serde_json::json!({ "a": "http://x", "b": [1, 2] }));
    }

    #[test]
    fn pretty_prints_with_two_spaces() {
        assert_eq!(
            to_json_string_pretty(&serde_json::json!({ "a": [1] })).unwrap(),
            "{\n  \"a\": [\n    1\n  ]\n}"
        );
    }
}
