//! Port of upstream/apps/server/src/git/linkCreatedPullRequest.ts
//! and the URL reader it uses, upstream/packages/shared/src/changeRequestUrl.ts.
//!
//! Dispatching `thread.pull-request.link` needs the orchestrator. This module
//! keeps the pure key. [`CreatedPullRequestLinker`] is the seam that dispatch
//! plugs into.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::PositiveInt;
use turin_contracts::git::{GitPrStepStatus, GitStackedPrStep};

static GITHUB_PULL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/pull/(\d+)(?:/|$)").unwrap());
static GITCAFE_PULL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/pulls/(\d+)(?:/|$)").unwrap());
static FORGEJO_PULL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+(?:/[^/]+)+)/pulls/(\d+)(?:/|$)").unwrap());
static GITLAB_MR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+(?:/[^/]+)+)/-/merge_requests/(\d+)(?:/|$)").unwrap());
static BITBUCKET_PR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/pull-requests/(\d+)(?:/|$)").unwrap());
static AZURE_PR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/((?:[^/]+/)*_git/[^/]+)/pullrequest/(\d+)(?:/|$)").unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    /// Forgejo's HTTP host and port. Absent for every other host.
    pub authority: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedPullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// Project repository identity, used only when the pull request URL is one
/// [`parse_change_request_url`] cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRepositoryIdentity {
    pub host: String,
    pub repository: String,
}

struct HttpUrl {
    hostname: String,
    /// `host` as the URL wrote it, port included.
    host: String,
    pathname: String,
}

pub fn created_pull_request_key(
    pr: &GitStackedPrStep,
    project: Option<&ProjectRepositoryIdentity>,
) -> Option<CreatedPullRequestKey> {
    if pr.status != GitPrStepStatus::Created && pr.status != GitPrStepStatus::OpenedExisting {
        return None;
    }
    let number = pr.number.map(PositiveInt::get)?;
    let url = pr.url.clone()?;
    if let Some(parsed) = parse_change_request_url(&url) {
        return Some(CreatedPullRequestKey {
            host: parsed.host,
            repository: parsed.repository,
            number: parsed.number,
            url,
        });
    }
    let project = project?;
    if project.host.is_empty() || project.repository.is_empty() {
        return None;
    }
    Some(CreatedPullRequestKey {
        host: project.host.clone(),
        repository: project.repository.to_lowercase(),
        number,
        url,
    })
}

/// Repository and number behind a change-request URL this can read.
pub fn parse_change_request_url(target_url: &str) -> Option<ChangeRequestLink> {
    let url = parse_http_url(target_url)?;
    let host = url.hostname.to_lowercase();
    if host == "git.cafe" || host == "staging.git.cafe" {
        return claim(&host, GITCAFE_PULL.captures(&url.pathname));
    }
    if is_host_of(&host, "github.com", Some("github"))
        && let Some(link) = claim(&host, GITHUB_PULL.captures(&url.pathname))
    {
        return Some(link);
    }
    if let Some(link) = claim(&host, FORGEJO_PULL.captures(&url.pathname)) {
        return Some(ChangeRequestLink {
            authority: Some(url.host.to_lowercase()),
            ..link
        });
    }
    if let Some(link) = claim(&host, GITLAB_MR.captures(&url.pathname)) {
        return Some(link);
    }
    if is_host_of(&host, "bitbucket.org", Some("bitbucket")) {
        return claim(&host, BITBUCKET_PR.captures(&url.pathname));
    }
    if is_host_of(&host, "dev.azure.com", None) || host.ends_with(".visualstudio.com") {
        return claim(&host, AZURE_PR.captures(&url.pathname));
    }
    None
}

fn is_host_of(hostname: &str, apex: &str, label: Option<&str>) -> bool {
    hostname == apex
        || hostname.ends_with(&format!(".{apex}"))
        || label.is_some_and(|label| hostname.split('.').any(|part| part == label))
}

fn claim(host: &str, caps: Option<regex::Captures<'_>>) -> Option<ChangeRequestLink> {
    let caps = caps?;
    let repository = caps.get(1)?.as_str();
    let number: u64 = caps.get(2)?.as_str().parse().ok()?;
    if number == 0 {
        return None;
    }
    Some(ChangeRequestLink {
        host: host.to_owned(),
        repository: repository.to_lowercase(),
        number,
        authority: None,
    })
}

fn parse_http_url(value: &str) -> Option<HttpUrl> {
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if authority.is_empty() {
        return None;
    }
    let hostname = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let hostname = hostname.split(':').next().unwrap_or(hostname);
    if hostname.is_empty() {
        return None;
    }
    Some(HttpUrl {
        hostname: hostname.to_owned(),
        host: authority.to_owned(),
        pathname: format!("/{path}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(status: GitPrStepStatus, number: Option<u64>, url: Option<&str>) -> GitStackedPrStep {
        GitStackedPrStep {
            status,
            url: url.map(str::to_owned),
            number: number.and_then(PositiveInt::new),
            base_branch: None,
            head_branch: None,
            title: None,
        }
    }

    #[test]
    fn created_key_reads_the_url_and_falls_back_to_the_project() {
        let key = created_pull_request_key(
            &step(
                GitPrStepStatus::Created,
                Some(42),
                Some("https://github.com/pingdotgg/codething-mvp/pull/42"),
            ),
            None,
        )
        .unwrap();
        assert_eq!(key.host, "github.com");
        assert_eq!(key.repository, "pingdotgg/codething-mvp");
        assert_eq!(key.number, 42);

        let fallback = created_pull_request_key(
            &step(GitPrStepStatus::OpenedExisting, Some(7), Some("not a url")),
            Some(&ProjectRepositoryIdentity {
                host: "gitlab.example".into(),
                repository: "Group/Repo".into(),
            }),
        )
        .unwrap();
        assert_eq!(fallback.repository, "group/repo");
        assert!(
            created_pull_request_key(
                &step(GitPrStepStatus::SkippedNotRequested, None, None),
                None
            )
            .is_none()
        );
    }

    #[test]
    fn parses_the_hosts_a_thread_link_can_read() {
        let gitlab =
            parse_change_request_url("https://gitlab.example/group/sub/repo/-/merge_requests/42")
                .unwrap();
        assert_eq!(gitlab.repository, "group/sub/repo");
        assert!(parse_change_request_url("https://github.com/team/repository/issues/42").is_none());
    }
}
