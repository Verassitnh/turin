//! Port of upstream/packages/shared/src/changeRequestUrl.ts.
//!
//! Every host shape upstream reads is kept, because stored thread links carry them. The
//! shared copy in `helpers/shared_support.rs` is gone; this is the only one in the app state.
//! Only the GitHub Enterprise and GitHub URL builders stay on the source-control client
//! registry (`helpers/source_control_clients.rs`).

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::environment::RepositoryIdentity;
use url::Url;

/// A change request named the way a thread link names one: the host below which the repository
/// is addressed, the repository path as that host writes it, and the number.
///
/// The two strings are what `pullRequestHostOf` and the project's `repositoryIdentity` produce
/// from a git remote: lower case, no port, the full path below the host. Links are matched
/// against those, so anything else matches nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    /// Forgejo's HTTP host and port, separate from the portless repository identity.
    pub authority: Option<String>,
}

/// The host itself, one of its subdomains, or an install named after the provider.
fn is_host_of(hostname: &str, apex: &str, label: Option<&str>) -> bool {
    if hostname == apex || hostname.ends_with(&format!(".{apex}")) {
        return true;
    }
    label.is_some_and(|label| hostname.split('.').any(|part| part == label))
}

fn path_regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

static GITCAFE_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+/[^/]+)/pulls/([0-9]+)(?:/|$)"));
static GITHUB_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+/[^/]+)/pull/([0-9]+)(?:/|$)"));
static FORGEJO_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+(?:/[^/]+)+)/pulls/([0-9]+)(?:/|$)"));
static GITLAB_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+(?:/[^/]+)+)/-/merge_requests/([0-9]+)(?:/|$)"));
static BITBUCKET_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+/[^/]+)/pull-requests/([0-9]+)(?:/|$)"));
static AZURE_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/((?:[^/]+/)*_git/[^/]+)/pullrequest/([0-9]+)(?:/|$)"));

/// JS `URL.host`: the hostname plus the port when it is not the scheme's default.
pub fn url_host_with_port(url: &Url) -> String {
    let hostname = url.host_str().unwrap_or("");
    match url.port() {
        Some(port) => format!("{hostname}:{port}"),
        None => hostname.to_owned(),
    }
}

/// The repository and number behind a change request URL on a host this can read, or `None` for
/// anything else: an issue, a commit, a repository root, a host this cannot tell apart from an
/// ordinary link. A doubtful match is worse than no match, so nothing here guesses.
///
/// Each host is recognised by the path shape it alone uses, guarded by a hostname it could
/// plausibly be served from, since self-hosted installs are named whatever their admin chose:
/// GitLab's `/-/` marker is unique enough to trust on any hostname, while `/pull/` is generic
/// enough that it is only believed from a GitHub-ish host.
///
/// Nothing here tries to tell a lookalike hostname from a real one: `github.com.evil.test` and
/// the rest are an open set, and blocking spellings of it costs real hosts (`gitlab.com.br` is a
/// registrable domain). What a claim is worth is decided where it is used.
pub fn parse_change_request_url(target_url: &str) -> Option<ChangeRequestLink> {
    let url = Url::parse(target_url).ok()?;
    // `javascript:`, `mailto:` and friends have no host to speak of and nothing to open.
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let host = url.host_str().unwrap_or("").to_lowercase();
    let pathname = url.path();

    // GitCafe, only on its own two hosts: /{owner}/{repo}/pulls/{n}
    if host == "git.cafe" || host == "staging.git.cafe" {
        return claim(&host, GITCAFE_PATH.captures(pathname));
    }
    // GitHub, and any Enterprise install: /{owner}/{repo}/pull/{n}
    if is_host_of(&host, "github.com", Some("github"))
        && let Some(captures) = GITHUB_PATH.captures(pathname)
    {
        return claim(&host, Some(captures));
    }
    // Forgejo and Gitea use /pulls/ on arbitrary self-hosted domains.
    if let Some(forgejo) = FORGEJO_PATH.captures(pathname) {
        return claim(&host, Some(forgejo)).map(|link| ChangeRequestLink {
            authority: Some(url_host_with_port(&url).to_lowercase()),
            ..link
        });
    }
    // GitLab, self-hosted included: /{group}/[{subgroup}/...]{repo}/-/merge_requests/{n}. The `/-/`
    // separator is GitLab's own, so the hostname is not asked about.
    if let Some(gitlab) = GITLAB_PATH.captures(pathname) {
        return claim(&host, Some(gitlab));
    }
    // Bitbucket Cloud: /{workspace}/{repo}/pull-requests/{n}
    if is_host_of(&host, "bitbucket.org", Some("bitbucket")) {
        return claim(&host, BITBUCKET_PATH.captures(pathname));
    }
    // Azure DevOps, both the current host and the per-organisation one it replaced. `_git` is part
    // of the repository path there, as it is in the remote URL the identity is read from.
    if is_host_of(&host, "dev.azure.com", None) || host.ends_with(".visualstudio.com") {
        return claim(&host, AZURE_PATH.captures(pathname));
    }
    None
}

/// `matchesLinkedPullRequestUrl`: match a stored PR without requiring its project to remain
/// available.
pub fn matches_linked_pull_request_url(linked_url: &str, target_url: &str) -> bool {
    match (
        parse_change_request_url(linked_url),
        parse_change_request_url(target_url),
    ) {
        (Some(linked), Some(target)) => linked == target,
        _ => false,
    }
}

/// `gitHubPullRequestBrowserUrl`: builds a GitHub URL that remains available when the pull
/// request API cannot be read.
pub fn github_pull_request_browser_url(
    identity: Option<&RepositoryIdentity>,
    repository: &str,
    number: u64,
) -> Option<String> {
    let identity = identity?;
    if identity.provider.as_ref().map(|provider| provider.as_str()) != Some("github")
        || !(1..=9_007_199_254_740_991).contains(&number)
    {
        return None;
    }
    let segments: Vec<&str> = repository.split('/').collect();
    if segments.len() != 2
        || segments
            .iter()
            .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return None;
    }
    // An http(s) remote names its own origin; SCP-style remotes are read from the normalized
    // identity below.
    let origin = Url::parse(identity.locator.remote_url.as_str().trim())
        .ok()
        .filter(|url| url.scheme() == "http" || url.scheme() == "https")
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
    url.set_path(&format!("/{}/pull/{number}", segments.join("/")));
    Some(url.to_string())
}

fn claim(host: &str, captures: Option<regex::Captures<'_>>) -> Option<ChangeRequestLink> {
    let captures = captures?;
    let repository = captures
        .get(1)
        .map(|m| m.as_str())
        .filter(|s| !s.is_empty())?;
    let number: u64 = captures.get(2)?.as_str().parse().ok()?;
    // `Number.isSafeInteger(number) && number > 0`
    (number > 0 && number <= 9_007_199_254_740_991).then(|| ChangeRequestLink {
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
    fn parses_change_request_urls_by_host_shape() {
        let link = |url: &str| {
            parse_change_request_url(url).map(|l| (l.host, l.repository, l.number, l.authority))
        };
        assert_eq!(
            link("https://github.com/Acme/Web/pull/7/files"),
            Some(("github.com".into(), "acme/web".into(), 7, None))
        );
        // Forgejo keeps the HTTP authority (host and port) apart from the portless identity.
        assert_eq!(
            link("http://forge.example:3000/team/repo/pulls/7"),
            Some((
                "forge.example".into(),
                "team/repo".into(),
                7,
                Some("forge.example:3000".into())
            ))
        );
        assert_eq!(
            link("https://gitlab.com/g/s/r/-/merge_requests/3"),
            Some(("gitlab.com".into(), "g/s/r".into(), 3, None))
        );
        assert_eq!(link("https://github.com/acme/web/issues/7"), None);
        assert_eq!(link("javascript:alert(1)"), None);
        assert_eq!(link("https://github.com/acme/web/pull/0"), None);
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

    #[test]
    fn a_stored_link_matches_the_same_change_request_only() {
        assert!(matches_linked_pull_request_url(
            "https://github.com/Acme/Turin/pull/7",
            "https://github.com/acme/turin/pull/7/files"
        ));
        assert!(!matches_linked_pull_request_url(
            "https://github.com/acme/turin/pull/7",
            "https://github.com/acme/turin/pull/8"
        ));
        assert!(!matches_linked_pull_request_url(
            "https://github.com/acme/turin/pull/7",
            "not a url"
        ));
    }

    fn github_identity(remote_url: &str) -> RepositoryIdentity {
        serde_json::from_value(serde_json::json!({
            "canonicalKey": "github.com/acme/turin",
            "provider": "github",
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": remote_url,
            },
        }))
        .unwrap()
    }

    #[test]
    fn the_browser_url_names_the_remotes_own_origin() {
        let identity = github_identity("https://ghe.example.com/acme/turin.git");
        assert_eq!(
            github_pull_request_browser_url(Some(&identity), "acme/turin", 7).as_deref(),
            Some("https://ghe.example.com/acme/turin/pull/7")
        );
    }

    #[test]
    fn an_scp_remote_falls_back_to_the_canonical_host() {
        let identity = github_identity("git@github.com:acme/turin.git");
        assert_eq!(
            github_pull_request_browser_url(Some(&identity), "acme/turin", 7).as_deref(),
            Some("https://github.com/acme/turin/pull/7")
        );
    }

    #[test]
    fn the_browser_url_refuses_what_it_cannot_build() {
        let identity = github_identity("https://github.com/acme/turin.git");
        assert_eq!(github_pull_request_browser_url(None, "acme/turin", 7), None);
        assert_eq!(
            github_pull_request_browser_url(Some(&identity), "acme/turin", 0),
            None
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&identity), "acme/../turin", 7),
            None
        );
        assert_eq!(
            github_pull_request_browser_url(Some(&identity), "acme", 7),
            None
        );
    }
}
