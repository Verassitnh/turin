//! Port of upstream/packages/provider-acp/src/server/stderr.ts
//!
//! Bounded stderr tail for startup and exit diagnostics, and the redaction
//! applied before any stderr excerpt reaches a user-facing error.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::js_trim;

/// Last few KiB of ACP child stderr kept for startup / exit diagnostics.
pub const ACP_STDERR_TAIL_MAX_CHARS: usize = 4_096;

static PAIRING_URL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https?://\S*/pair#\S*").expect("pairing pattern"));
static BEARER_TOKEN_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._\-+=/]+").expect("bearer pattern"));
static BASIC_AUTH_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bAuthorization:\s*Basic\s+\S+").expect("basic pattern"));
static API_KEY_HEADER_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bx-api-key:\s*\S+").expect("api key pattern"));
static SECRET_TOKEN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk-[A-Za-z0-9][A-Za-z0-9-]{7,}|ghp_[A-Za-z0-9]+|xox[a-zA-Z]-[A-Za-z0-9-]+)\b")
        .expect("secret token pattern")
});

/// Appends `chunk` and keeps only the last [`ACP_STDERR_TAIL_MAX_CHARS`] UTF-16
/// code units, like `String#slice(-n)`. Rust strings cannot hold a lone
/// surrogate, so a cut that would split a character drops that character.
pub fn append_acp_stderr_tail(current: &str, chunk: &str) -> String {
    let next = format!("{current}{chunk}");
    if next.encode_utf16().count() <= ACP_STDERR_TAIL_MAX_CHARS {
        return next;
    }
    let mut budget = ACP_STDERR_TAIL_MAX_CHARS;
    let mut start = next.len();
    for (index, c) in next.char_indices().rev() {
        let units = c.len_utf16();
        if units > budget {
            break;
        }
        budget -= units;
        start = index;
    }
    next[start..].to_owned()
}

/// Bounded, redacted excerpt safe to put on user-facing adapter errors.
///
/// `environment` plays the role of `process.env`: `HOME` and `USERPROFILE`
/// are replaced by `~` alongside `home_directory`.
pub fn sanitize_acp_stderr_excerpt(
    text: &str,
    home_directory: &str,
    environment: &HashMap<String, String>,
) -> String {
    let mut result = text.replace('\0', "");
    let mut homes: Vec<&str> = Vec::new();
    let candidates = [
        environment.get("HOME").map(String::as_str),
        environment.get("USERPROFILE").map(String::as_str),
        Some(home_directory),
    ];
    for home in candidates.into_iter().flatten() {
        if home.encode_utf16().count() > 1 && !homes.contains(&home) {
            homes.push(home);
        }
    }
    for home in homes {
        result = result.replace(home, "~");
    }
    let result = PAIRING_URL_PATTERN.replace_all(&result, "[pairing-url]");
    let result = BEARER_TOKEN_PATTERN.replace_all(&result, "Bearer [redacted]");
    let result = BASIC_AUTH_PATTERN.replace_all(&result, "Authorization: Basic [redacted]");
    let result = API_KEY_HEADER_PATTERN.replace_all(&result, "x-api-key: [redacted]");
    let result = SECRET_TOKEN_PATTERN.replace_all(&result, "[redacted]");
    js_trim(&result).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_bounded_tail_of_stderr_chunks() {
        let prefix = "x".repeat(ACP_STDERR_TAIL_MAX_CHARS);
        assert_eq!(
            append_acp_stderr_tail(&prefix, "abc"),
            format!("{}abc", &prefix[3..])
        );
    }

    #[test]
    fn redacts_home_paths_pairing_urls_and_tokens_from_stderr_excerpts() {
        let text = [
            "Invalid project config at /Users/ada/.cursor/cli.json",
            "Authorization: Bearer secret-token-value",
            "Visit http://localhost:5733/pair#token=ABCDEF for pairing",
            "key=sk-abcdefghijklmnopqrstuv",
        ]
        .join("\n");
        let env = HashMap::from([("HOME".to_owned(), "/Users/ada".to_owned())]);
        let excerpt = sanitize_acp_stderr_excerpt(&text, "/home/server", &env);

        assert!(excerpt.contains("Invalid project config at ~/.cursor/cli.json"));
        assert!(excerpt.contains("Bearer [redacted]"));
        assert!(excerpt.contains("[pairing-url]"));
        assert!(excerpt.contains("[redacted]"));
        assert!(!excerpt.contains("secret-token-value"));
        assert!(!excerpt.contains("ABCDEF"));
        assert!(!excerpt.contains("sk-abcdefghijklmnopqrstuv"));
    }

    #[test]
    fn redacts_hyphenated_openai_project_keys_and_header_credentials() {
        let text = [
            "openai=sk-proj-abcdefghijklmnopqrstuvwxyz012345",
            "svc=sk-svcacct-abcdefghijklmnopqrstuvwxyz012345",
            "anthropic=sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345",
            "Authorization: Basic dXNlcjpwYXNz",
            "x-api-key: ant-api-key-value",
        ]
        .join("\n");
        let excerpt = sanitize_acp_stderr_excerpt(&text, "/Users/ada", &HashMap::new());

        assert!(excerpt.contains("[redacted]"));
        assert!(excerpt.contains("Authorization: Basic [redacted]"));
        assert!(excerpt.contains("x-api-key: [redacted]"));
        assert!(!excerpt.contains("sk-proj-"));
        assert!(!excerpt.contains("sk-svcacct-"));
        assert!(!excerpt.contains("sk-ant-api03-"));
        assert!(!excerpt.contains("abcdefghijklmnopqrstuvwxyz012345"));
        assert!(!excerpt.contains("dXNlcjpwYXNz"));
        assert!(!excerpt.contains("ant-api-key-value"));
    }
}
