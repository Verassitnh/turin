//! Port of upstream/apps/web/src/pullRequestReference.ts

use std::sync::LazyLock;

use regex::Regex;

use crate::state::helpers::source_control_clients::SOURCE_CONTROL_CLIENTS;

static PULL_REQUEST_NUMBER_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#?(\d+)$").expect("valid regex"));

/// The change request a pasted reference names: a host's change request URL,
/// returned as is, or a bare number. A host's checkout command, such as
/// `gh pr checkout 42`, is read for its argument first. `None` for anything
/// else, such as a branch name.
pub fn parse_pull_request_reference(input: &str) -> Option<String> {
    let trimmed = crate::state::contracts::js_trim(input);
    if trimmed.is_empty() {
        return None;
    }

    let normalized = SOURCE_CONTROL_CLIENTS
        .definitions()
        .iter()
        .find_map(|definition| (definition.checkout_command_argument)(trimmed))
        .unwrap_or_else(|| trimmed.to_owned());
    if normalized.is_empty() {
        return None;
    }

    if SOURCE_CONTROL_CLIENTS
        .definitions()
        .iter()
        .any(|definition| (definition.is_change_request_reference)(&normalized))
    {
        return Some(normalized);
    }

    PULL_REQUEST_NUMBER_PATTERN
        .captures(&normalized)
        .and_then(|captures| captures.get(1))
        .map(|number| number.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Option<String> {
        parse_pull_request_reference(input)
    }

    #[test]
    fn accepts_github_pull_request_urls() {
        assert_eq!(
            parse("https://github.com/pingdotgg/t3code/pull/42").as_deref(),
            Some("https://github.com/pingdotgg/t3code/pull/42")
        );
    }

    #[test]
    fn accepts_raw_numbers_and_hash_references() {
        assert_eq!(parse("42").as_deref(), Some("42"));
        assert_eq!(parse("#42").as_deref(), Some("42"));
    }

    #[test]
    fn accepts_gh_pr_checkout_commands() {
        assert_eq!(parse("gh pr checkout 42").as_deref(), Some("42"));
        assert_eq!(parse("gh pr checkout #42").as_deref(), Some("42"));
        assert_eq!(
            parse("gh pr checkout https://github.com/pingdotgg/t3code/pull/42").as_deref(),
            Some("https://github.com/pingdotgg/t3code/pull/42")
        );
    }

    #[test]
    fn rejects_non_pull_request_input() {
        assert_eq!(parse("feature/my-branch"), None);
        assert_eq!(parse("   "), None);
    }
}
