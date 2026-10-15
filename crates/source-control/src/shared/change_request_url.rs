//! Port of upstream/packages/shared/src/changeRequestUrl.ts
//!
//! GitHub only: upstream's GitCafe, Forgejo/Gitea, GitLab, Bitbucket and Azure DevOps URL shapes
//! are cut with those hosts (PORTING.md cut list). `authority` stays on [`ChangeRequestLink`]
//! as a neutral hook (it is Forgejo's HTTP host and port upstream) but is always `None` here.

use std::sync::LazyLock;

use regex::Regex;
use reqwest::Url;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::thread_pull_request::ThreadLinkedPullRequest;

/// A change request named the way a thread link names one: the host below which the repository
/// is addressed, the repository path as that host writes it, and the number.
///
/// The two strings are what `pullRequestHostOf` and the project's `repositoryIdentity` produce
/// from a git remote — lower case, no port, the full path below the host — because links are
/// matched against those. Anything else matches nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    /// Forgejo's HTTP host and port, separate from the portless repository identity. Always
    /// `None` while Forgejo is cut.
    pub authority: Option<String>,
}

/// The host itself, one of its subdomains, or an install named after the provider.
fn is_host_of(hostname: &str, apex: &str, label: Option<&str>) -> bool {
    if hostname == apex || hostname.ends_with(&format!(".{apex}")) {
        return true;
    }
    label.is_some_and(|label| hostname.split('.').any(|part| part == label))
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "https" | "http")
}

/// `/{owner}/{repo}/pull/{n}`; `\d` is ASCII-only under JS's `u` flag, so `[0-9]` here.
static GITHUB_PULL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/pull/([0-9]+)(?:/|$)").unwrap());
static GITHUB_ISSUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/issues/([0-9]+)(?:/|$)").unwrap());

/// The repository and number behind a change request URL on a host this can read, or `None` for
/// anything else — an issue, a commit, a repository root, a host this cannot tell apart from an
/// ordinary link. A doubtful match is worse than no match, so nothing here guesses.
///
/// Upstream recognises each host by the path shape it alone uses, guarded by a hostname it could
/// plausibly be served from, since self-hosted installs are named whatever their admin chose;
/// `/pull/` is generic enough that it is only believed from a GitHub-ish host. Only that GitHub
/// shape is kept.
///
/// Nothing here tries to tell a lookalike hostname from a real one — `github.com.evil.test` and
/// the rest are an open set, and blocking spellings of it costs real hosts. What a claim is
/// worth is decided where it is used.
pub fn parse_change_request_url(target_url: &str) -> Option<ChangeRequestLink> {
    let url = Url::parse(target_url).ok()?;
    // `javascript:`, `mailto:` and friends have no host to speak of and nothing to open.
    if !is_http(&url) {
        return None;
    }
    let host = url.host_str()?.to_lowercase();

    // GitHub, and any Enterprise install: /{owner}/{repo}/pull/{n}
    if is_host_of(&host, "github.com", Some("github")) {
        return claim(&host, GITHUB_PULL.captures(url.path()));
    }
    None
}

fn claim(host: &str, captures: Option<regex::Captures<'_>>) -> Option<ChangeRequestLink> {
    let captures = captures?;
    let repository = captures.get(1)?.as_str();
    // `Number.isSafeInteger(number) && number > 0`
    let number: u64 = captures.get(2)?.as_str().parse().ok()?;
    if repository.is_empty() || number == 0 || number > 9_007_199_254_740_991 {
        return None;
    }
    Some(ChangeRequestLink {
        host: host.to_owned(),
        repository: repository.to_lowercase(),
        number,
        authority: None,
    })
}

/// The web URL a host writes for a change request; `None` when the host shape is unknown.
///
/// Only `"github"` is known here. `remote_url` is Forgejo's input upstream and is unused.
pub fn change_request_url_for(
    kind: Option<&str>,
    host: &str,
    repository: &str,
    number: u64,
    _remote_url: Option<&str>,
) -> Option<String> {
    match kind {
        Some("github") => Some(format!("https://{host}/{repository}/pull/{number}")),
        _ => None,
    }
}

/// Builds a GitHub URL that remains available when the pull request API cannot be read.
pub fn github_pull_request_browser_url(
    identity: Option<&RepositoryIdentity>,
    repository: &str,
    number: u64,
) -> Option<String> {
    let identity = identity?;
    if identity.provider.as_ref().map(|p| p.as_str()) != Some("github")
        || !(1..=9_007_199_254_740_991).contains(&number)
    {
        return None;
    }
    let repository_path: Vec<&str> = repository.split('/').collect();
    if repository_path.len() != 2
        || repository_path
            .iter()
            .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return None;
    }

    // SCP-style remotes are read from their normalized identity below.
    let origin = Url::parse(crate::shared::js::js_trim(
        identity.locator.remote_url.as_str(),
    ))
    .ok()
    .filter(is_http)
    .map(|url| url.origin().ascii_serialization());
    let hostname = identity
        .canonical_key
        .as_str()
        .split('/')
        .next()
        .unwrap_or("");
    if origin.is_none() && hostname.is_empty() {
        return None;
    }

    let mut url = Url::parse(&origin.unwrap_or_else(|| format!("https://{hostname}"))).ok()?;
    url.set_path(&format!("/{}/pull/{number}", repository_path.join("/")));
    Some(url.to_string())
}

/// The pull-request URL a GitHub-style `#123` autolink might name. GitHub writes every bare
/// reference through `/issues/`, including pull requests, so this only builds a candidate: the
/// caller must successfully read it as a pull request before treating it as one.
pub fn pull_request_candidate_url_from_reference_autolink(target_url: &str) -> Option<String> {
    let mut url = Url::parse(target_url).ok()?;
    if !is_http(&url) {
        return None;
    }
    let hostname = url.host_str()?.to_lowercase();
    if !(hostname == "github.com"
        || hostname.ends_with(".github.com")
        || hostname.split('.').any(|part| part == "github"))
    {
        return None;
    }
    let (repository, number) = {
        let captures = GITHUB_ISSUE.captures(url.path())?;
        (
            captures.get(1)?.as_str().to_owned(),
            captures.get(2)?.as_str().to_owned(),
        )
    };
    url.set_path(&format!("/{repository}/pull/{number}"));
    Some(url.to_string())
}

/// Match a stored PR without requiring its project to remain available.
pub fn matches_linked_pull_request_url(
    linked_pull_request: &ThreadLinkedPullRequest,
    target_url: &str,
) -> bool {
    let linked = parse_change_request_url(linked_pull_request.url.as_str());
    let target = parse_change_request_url(target_url);
    matches!((linked, target), (Some(linked), Some(target)) if linked == target)
}

/// The repository root behind a recognised change-request URL, without PR-specific state.
pub fn change_request_repository_url(target_url: &str) -> Option<String> {
    // Upstream also lists GitLab, Bitbucket and Azure route spellings here; only GitHub's
    // `/pull/{n}` can parse while those hosts are cut.
    static REPOSITORY_PATH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(.*?)(?:/pulls?/[0-9]+)(?:/|$)").unwrap());
    parse_change_request_url(target_url)?;
    let mut url = Url::parse(target_url).ok()?;
    let repository_path = REPOSITORY_PATH
        .captures(url.path())
        .and_then(|captures| captures.get(1))
        .map(|m| m.as_str().to_owned())?;
    if repository_path.is_empty() {
        return None;
    }
    url.set_path(&repository_path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

/// The URL of pull request `number` in the same repository as `url`, on the same origin.
pub fn sibling_pull_request_url(url: &str, number: u64) -> Option<String> {
    let reference = parse_change_request_url(url)?;
    if !(1..=9_007_199_254_740_991).contains(&number) {
        return None;
    }
    let mut sibling = Url::parse(url).ok()?;
    // Upstream slices `pathname` past `reference.repository.length + 1` and matches the route
    // there; with only GitHub's route left, the parse above already proved `/{repo}/pull/{n}`,
    // so re-check it on the original path (which also sidesteps lowercasing changing lengths).
    GITHUB_PULL.captures(sibling.path())?;
    sibling.set_path(&format!("/{}/pull/{number}", reference.repository));
    sibling.set_query(None);
    sibling.set_fragment(None);
    Some(sibling.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn link(host: &str, repository: &str, number: u64) -> ChangeRequestLink {
        ChangeRequestLink {
            host: host.into(),
            repository: repository.into(),
            number,
            authority: None,
        }
    }

    #[test]
    fn reads_a_github_pull_request_lower_casing_the_repository() {
        assert_eq!(
            parse_change_request_url("https://github.com/T3Tools/T3Code/pull/123"),
            Some(link("github.com", "t3tools/t3code", 123))
        );
    }

    #[test]
    fn reads_a_pull_request_on_a_github_enterprise_host() {
        assert_eq!(
            parse_change_request_url("https://github.acme.test/platform/api/pull/7"),
            Some(link("github.acme.test", "platform/api", 7))
        );
    }

    #[test]
    fn reads_a_supported_github_host_with_a_middle_dns_label() {
        assert_eq!(
            parse_change_request_url("https://code.github.example.com/acme/web/pull/42"),
            Some(link("code.github.example.com", "acme/web", 42))
        );
        assert_eq!(
            pull_request_candidate_url_from_reference_autolink(
                "https://code.github.example.com/acme/web/issues/42"
            ),
            Some("https://code.github.example.com/acme/web/pull/42".to_owned())
        );
    }

    #[test]
    fn survives_trailing_segments_a_trailing_slash_and_a_query_string() {
        assert_eq!(
            parse_change_request_url("https://github.com/t3tools/t3code/pull/123/files?w=1"),
            Some(link("github.com", "t3tools/t3code", 123))
        );
        assert_eq!(
            parse_change_request_url("https://github.com/t3tools/t3code/pull/123/"),
            Some(link("github.com", "t3tools/t3code", 123))
        );
    }

    #[test]
    fn claims_nothing_it_cannot_be_sure_of() {
        for url in [
            "https://github.com/t3tools/t3code/issues/123",
            "https://github.com/t3tools/t3code/commit/0a1b2c3",
            "https://github.com/t3tools/t3code",
            "https://github.com/t3tools/t3code/pull/abc",
            "https://gitlab.com/t3tools/t3code/-/issues/12",
            "https://blog.example.test/2026/updates/pull/3",
            "javascript:alert(1)//github.com/t3tools/t3code/pull/1",
            "not a url",
            "https://github.com/t3tools/t3code/pull/0",
            "https://github.com/t3tools/t3code/pull/99999999999999999999",
        ] {
            assert_eq!(parse_change_request_url(url), None, "{url}");
        }
    }

    // Upstream's non-GitHub parse cases (Forgejo, GitLab, Bitbucket, Azure DevOps, GitCafe) are
    // dropped with those hosts; they must now parse as nothing rather than as a link.
    #[test]
    fn cut_hosts_are_not_recognised() {
        for url in [
            "https://github.internal/team/repo/pulls/7",
            "https://gitlab.com/t3tools/platform/t3code/-/merge_requests/42",
            "https://code.acme.test/team/project/-/merge_requests/9",
            "https://bitbucket.org/workspace/repo/pull-requests/5",
            "https://dev.azure.com/acme/platform/_git/t3code/pullrequest/17",
            "http://git.example.test:3000/team/repo/pulls/42/files",
        ] {
            assert_eq!(parse_change_request_url(url), None, "{url}");
        }
    }

    #[test]
    fn builds_a_canonical_sibling() {
        for (url, expected) in [
            (
                "https://github.com/pull/1/pull/42/files",
                "https://github.com/pull/1/pull/43",
            ),
            (
                "https://github.com/acme/web/pull/42#discussion_r123",
                "https://github.com/acme/web/pull/43",
            ),
            (
                "https://github.com/acme/web/pull/42/files?w=1",
                "https://github.com/acme/web/pull/43",
            ),
            (
                "https://github.acme.test:8443/acme/web/pull/42/",
                "https://github.acme.test:8443/acme/web/pull/43",
            ),
        ] {
            assert_eq!(
                sibling_pull_request_url(url, 43).as_deref(),
                Some(expected),
                "{url}"
            );
        }
    }

    #[test]
    fn sibling_rejects_non_pr_urls_and_invalid_numbers() {
        assert_eq!(
            sibling_pull_request_url("https://github.com/acme/web/issues/42", 43),
            None
        );
        assert_eq!(
            sibling_pull_request_url("https://github.com/acme/web/pull/42", 0),
            None
        );
    }

    #[test]
    fn change_request_url_for_github_only() {
        assert_eq!(
            change_request_url_for(Some("github"), "github.com", "acme/web", 42, None).as_deref(),
            Some("https://github.com/acme/web/pull/42")
        );
        for kind in [Some("gitlab"), Some("forgejo"), Some("azure-devops"), None] {
            assert_eq!(change_request_url_for(kind, "h", "a/b", 1, None), None);
        }
    }

    #[test]
    fn repository_url_strips_the_pull_route_query_and_hash() {
        assert_eq!(
            change_request_repository_url("https://github.com/acme/web/pull/42/files?w=1#x")
                .as_deref(),
            Some("https://github.com/acme/web")
        );
        assert_eq!(
            change_request_repository_url("https://github.com/acme/web/issues/1"),
            None
        );
    }

    #[test]
    fn matches_a_linked_pull_request_by_host_repository_and_number() {
        let linked = |url: &str| {
            serde_json::from_value::<ThreadLinkedPullRequest>(serde_json::json!({
                "projectId": "p",
                "repository": "acme/web",
                "number": 42,
                "url": url,
            }))
            .unwrap()
        };
        let linked = linked("https://github.com/Acme/Web/pull/42");
        assert!(matches_linked_pull_request_url(
            &linked,
            "https://github.com/acme/web/pull/42/files"
        ));
        assert!(!matches_linked_pull_request_url(
            &linked,
            "https://github.com/acme/web/pull/43"
        ));
        assert!(!matches_linked_pull_request_url(
            &linked,
            "https://example.com/x"
        ));
    }

    fn identity(
        provider: Option<&str>,
        canonical_key: &str,
        remote_url: &str,
    ) -> RepositoryIdentity {
        serde_json::from_value(serde_json::json!({
            "canonicalKey": canonical_key,
            "locator": { "source": "git-remote", "remoteName": "origin", "remoteUrl": remote_url },
            "provider": provider,
        }))
        .unwrap()
    }

    #[test]
    fn github_browser_url_prefers_the_http_remote_origin() {
        let id = identity(
            Some("github"),
            "ghe.acme.test/acme/web",
            "https://ghe.acme.test:8443/acme/web.git",
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&id), "acme/web", 7).as_deref(),
            Some("https://ghe.acme.test:8443/acme/web/pull/7")
        );
        let scp = identity(
            Some("github"),
            "github.com/acme/web",
            "git@github.com:acme/web.git",
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&scp), "acme/web", 7).as_deref(),
            Some("https://github.com/acme/web/pull/7")
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&scp), "acme/web/x", 7),
            None
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&scp), "acme/..", 7),
            None
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&scp), "acme/web", 0),
            None
        );
        assert_eq!(github_pull_request_browser_url(None, "acme/web", 1), None);
        let other = identity(
            Some("gitlab"),
            "gitlab.com/acme/web",
            "https://gitlab.com/acme/web.git",
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&other), "acme/web", 1),
            None
        );
    }
}
