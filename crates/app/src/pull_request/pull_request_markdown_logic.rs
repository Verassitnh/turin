//! Port of the pure text matching in upstream/apps/web/src/components/pullRequest/
//! pullRequestMarkdown.logic.ts. The remark/mdast tree traversal and authored-link/code
//! exclusion are skipped; callers pass plain text from eligible nodes.
use regex::Regex;
use std::sync::LazyLock;
use turin_contracts::js_trim;
static CANDIDATES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)#[1-9][0-9]*|[0-9a-f]{40}").unwrap());
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestAutolinkKind {
    Reference,
    Commit,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestAutolink {
    pub start: usize,
    pub end: usize,
    pub kind: PullRequestAutolinkKind,
    pub url: String,
    pub label: String,
}
fn word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
/// GitHub-style same-repository candidates with the exact text boundary rules.
pub fn find_pull_request_autolinks(input: &str, repository_url: &str) -> Vec<PullRequestAutolink> {
    let repository_url = repository_url.trim_end_matches('/');
    CANDIDATES
        .find_iter(input)
        .filter_map(|found| {
            let matched = found.as_str();
            let reference = matched.starts_with('#');
            let before = input[..found.start()].chars().next_back();
            let after = input[found.end()..].chars().next();
            if before.is_some_and(|ch| {
                if reference {
                    word(ch)
                } else {
                    !js_trim(&ch.to_string()).is_empty() && !"([{".contains(ch)
                }
            }) || after.is_some_and(word)
            {
                return None;
            }
            let (kind, url, label) = if reference {
                (
                    PullRequestAutolinkKind::Reference,
                    format!("{repository_url}/issues/{}", &matched[1..]),
                    matched.to_owned(),
                )
            } else {
                (
                    PullRequestAutolinkKind::Commit,
                    format!("{repository_url}/commit/{matched}"),
                    matched[..7].to_owned(),
                )
            };
            Some(PullRequestAutolink {
                start: input[..found.start()].encode_utf16().count(),
                end: input[..found.end()].encode_utf16().count(),
                kind,
                url,
                label,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[test]
    fn references_need_word_boundaries() {
        let links = find_pull_request_autolinks("see #42, x#43 and #1z", "https://github.com/a/b/");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "#42");
        assert_eq!(links[0].url, "https://github.com/a/b/issues/42");
    }
    #[test]
    fn commits_need_whitespace_or_open_delimiter() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let links = find_pull_request_autolinks(
            &format!("{sha} ({sha}) x{sha} {sha}x"),
            "https://github.com/a/b",
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].label, "0123456");
        assert_eq!(links[0].url, format!("https://github.com/a/b/commit/{sha}"));
    }
    #[test]
    fn zero_reference_is_not_candidate() {
        assert!(find_pull_request_autolinks("#0", "https://github.com/a/b").is_empty());
    }
}
