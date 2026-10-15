//! Port of upstream/packages/shared/src/changeRequestUrl.ts
//!
//! GitHub only (PORTING cut list): a URL upstream would hand to GitLab,
//! Bitbucket, Azure DevOps, Forgejo or GitCafe is `None` here. Of the other
//! exports only `parseChangeRequestUrl` and the URL builder for GitHub are
//! kept.

use std::sync::LazyLock;

use regex::Regex;
use url::Url;

/// A change request named the way a thread link names one: the host below which the repository
/// is addressed, the repository path as that host writes it, and the number.
///
/// The two strings are what `pullRequestHostOf` and the project's `repositoryIdentity` produce
/// from a git remote — lower case, no port, the full path below the host — because links are
/// matched against those. Anything else matches nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    /// Forgejo's HTTP host and port, separate from the portless repository identity.
    /// Forgejo is cut, so this is always `None`; kept for upstream's shape.
    pub authority: Option<String>,
}

/// The host itself, one of its subdomains, or an install named after the provider.
fn is_host_of(hostname: &str, apex: &str, label: Option<&str>) -> bool {
    if hostname == apex
        || hostname
            .strip_suffix(apex)
            .is_some_and(|rest| rest.ends_with('.'))
    {
        return true;
    }
    label.is_some_and(|label| hostname.split('.').any(|part| part == label))
}

// The `u` flag keeps `\d` ASCII in JS; Rust's `\d` is Unicode, so spell it out.
static GITHUB_PULL_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/pull/([0-9]+)(?:/|$)").expect("static regex"));

/// The repository and number behind a change request URL on a host this can read, or `None` for
/// anything else — an issue, a commit, a repository root, a host this cannot tell apart from an
/// ordinary link. A doubtful match is worse than no match, so nothing here guesses.
pub fn parse_change_request_url(target_url: &str) -> Option<ChangeRequestLink> {
    let url = Url::parse(target_url).ok()?;
    // `javascript:`, `mailto:` and friends have no host to speak of and nothing to open.
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let host = url.host_str()?.to_lowercase();

    // GitHub, and any Enterprise install: /{owner}/{repo}/pull/{n}
    if is_host_of(&host, "github.com", Some("github")) {
        let captures = GITHUB_PULL_PATH.captures(url.path())?;
        return claim(&host, &captures);
    }
    None
}

fn claim(host: &str, captures: &regex::Captures<'_>) -> Option<ChangeRequestLink> {
    let repository = captures.get(1)?.as_str();
    // `Number(digits)` then `Number.isSafeInteger(number) && number > 0`.
    let number = captures.get(2)?.as_str().parse::<u64>().ok()?;
    const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
    if repository.is_empty() || number == 0 || number > MAX_SAFE_INTEGER {
        return None;
    }
    Some(ChangeRequestLink {
        host: host.to_owned(),
        repository: repository.to_lowercase(),
        number,
        authority: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_github_pull_urls() {
        assert_eq!(
            parse_change_request_url("https://github.com/Owner/Repo/pull/42/files"),
            Some(ChangeRequestLink {
                host: "github.com".into(),
                repository: "owner/repo".into(),
                number: 42,
                authority: None,
            })
        );
        assert!(parse_change_request_url("https://ghe.github.example.com/o/r/pull/7").is_some());
    }

    #[test]
    fn refuses_everything_else() {
        assert_eq!(
            parse_change_request_url("https://github.com/o/r/issues/4"),
            None
        );
        assert_eq!(
            parse_change_request_url("https://github.com/o/r/pull/0"),
            None
        );
        assert_eq!(parse_change_request_url("mailto:a@github.com"), None);
        assert_eq!(
            parse_change_request_url("https://example.com/o/r/pull/3"),
            None
        );
        assert_eq!(parse_change_request_url("not a url"), None);
    }
}
