//! Port of the parts of upstream/packages/shared/src/threadPullRequests.ts
//! (and `changeRequestUrl.ts` / `sourceControl.ts` helpers it calls) that the
//! projection store uses: link identity, the legacy single-link upgrade, and
//! the single-link sync patch.
//!
//! GitHub only (PORTING cut list). Upstream's other hosts only change the
//! `authority` a Forgejo URL recovers; for a link on one of those hosts the
//! key falls back to the stored host and repository, which is also what
//! upstream computes when `parseChangeRequestUrl` returns null. The UI
//! helpers in this file upstream (badges, chains) belong to T14.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::orchestration_v2::OrchestrationV2AppThread;
use turin_contracts::thread_pull_request::{
    ThreadLinkedPullRequest, ThreadPullRequestLink, ThreadPullRequestLinkSource,
};
use turin_contracts::{PositiveInt, TrimmedNonEmptyString, js_trim};
use url::Url;

/// A change request named the way a thread link names one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChangeRequestLink {
    host: String,
    repository: String,
    number: u64,
    authority: Option<String>,
}

static GITHUB_PULL_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/pull/([0-9]+)(?:/|$)").expect("static regex"));
static SSH_AZURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)/v3/([^/]+)/([^/]+)/([^/]+)$")
        .expect("static regex")
});
static LEGACY_AZURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([^.]+)\.visualstudio\.com/(?:defaultcollection/)?([^/]+)/_git/([^/]+)$")
        .expect("static regex")
});

/// `canonicalRepositoryKey` (packages/shared/src/sourceControl.ts).
fn canonical_repository_key(key: &str) -> String {
    let key = SSH_AZURE.replace(key, "dev.azure.com/$1/$2/_git/$3");
    LEGACY_AZURE
        .replace(&key, "dev.azure.com/$1/$2/_git/$3")
        .into_owned()
}

/// The host itself, one of its subdomains, or an install named after the provider.
fn is_host_of(hostname: &str, apex: &str, label: &str) -> bool {
    hostname == apex
        || hostname
            .strip_suffix(apex)
            .is_some_and(|rest| rest.ends_with('.'))
        || hostname.split('.').any(|part| part == label)
}

/// `parseChangeRequestUrl`, GitHub only.
fn parse_change_request_url(target_url: &str) -> Option<ChangeRequestLink> {
    let url = Url::parse(target_url).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    if !is_host_of(&host, "github.com", "github") {
        return None;
    }
    let captures = GITHUB_PULL_PATH.captures(url.path())?;
    let repository = captures.get(1)?.as_str();
    let number = captures.get(2)?.as_str().parse::<u64>().ok()?;
    const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
    if repository.is_empty() || number == 0 || number > MAX_SAFE_INTEGER {
        return None;
    }
    Some(ChangeRequestLink {
        host,
        repository: repository.to_lowercase(),
        number,
        authority: None,
    })
}

/// `ThreadPullRequestKey` over plain strings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NormalizedPullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// `ThreadPullRequestKey & { authority?, url? }`.
struct KeySource<'a> {
    host: &'a str,
    repository: &'a str,
    number: u64,
    authority: Option<&'a str>,
    url: Option<&'a str>,
}

impl<'a> From<&'a ThreadPullRequestLink> for KeySource<'a> {
    fn from(link: &'a ThreadPullRequestLink) -> Self {
        Self {
            host: link.host.as_str(),
            repository: link.repository.as_str(),
            number: link.number.get(),
            authority: None,
            url: Some(link.url.as_str()),
        }
    }
}

/// Normalize stored links, recovering Forgejo HTTP ports from old links' URLs.
fn normalize_key(key: &KeySource<'_>) -> NormalizedPullRequestKey {
    let parsed = key.url.and_then(parse_change_request_url);
    let authority = key.authority.map(str::to_owned).or_else(|| {
        parsed.as_ref().and_then(|parsed| {
            (parsed.repository == js_trim(key.repository).to_lowercase()
                && parsed.number == key.number)
                .then(|| parsed.authority.clone())
                .flatten()
        })
    });
    let canonical = canonical_repository_key(&format!(
        "{}/{}",
        js_trim(authority.as_deref().unwrap_or(key.host)).to_lowercase(),
        js_trim(key.repository).to_lowercase()
    ));
    let separator = canonical.find('/').unwrap_or(canonical.len());
    NormalizedPullRequestKey {
        host: canonical[..separator].to_owned(),
        repository: canonical.get(separator + 1..).unwrap_or("").to_owned(),
        number: key.number,
    }
}

/// Legacy Azure selectors omit the organization and project; recover those from the PR URL.
pub fn legacy_thread_pull_request_key(
    linked: &ThreadLinkedPullRequest,
    fallback_host: Option<&str>,
) -> NormalizedPullRequestKey {
    if let Some(parsed) = parse_change_request_url(linked.url.as_str())
        && parsed.number == linked.number.get()
    {
        let canonical = canonical_repository_key(&format!("{}/{}", parsed.host, parsed.repository));
        if parsed.authority.is_some() || canonical.starts_with("dev.azure.com/") {
            return normalize_key(&KeySource {
                host: &parsed.host,
                repository: &parsed.repository,
                number: parsed.number,
                authority: parsed.authority.as_deref(),
                url: None,
            });
        }
    }
    let host = match fallback_host {
        Some(host) => host.to_owned(),
        None => Url::parse(linked.url.as_str())
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned()),
    };
    let host = js_trim(&host).to_lowercase();
    NormalizedPullRequestKey {
        host: if host.is_empty() {
            "unknown".to_owned()
        } else {
            host
        },
        repository: js_trim(linked.repository.as_str()).to_lowercase(),
        number: linked.number.get(),
    }
}

/// `threadPullRequestKeyOf`: `host/repository#number`.
pub fn thread_pull_request_key_of(link: &ThreadPullRequestLink) -> String {
    let normalized = normalize_key(&link.into());
    format!(
        "{}/{}#{}",
        normalized.host, normalized.repository, normalized.number
    )
}

/// Identity comparison for links: host-level, case-insensitive on host and repository.
pub fn thread_pull_request_keys_equal(
    left: &ThreadPullRequestLink,
    right: &ThreadPullRequestLink,
) -> bool {
    thread_pull_request_key_of(left) == thread_pull_request_key_of(right)
}

/// Older V2 event payloads stored one link; an explicit empty array means it was unlinked.
pub fn thread_pull_requests_of(thread: &OrchestrationV2AppThread) -> Vec<ThreadPullRequestLink> {
    if let Some(links) = &thread.pull_requests {
        return links.clone();
    }
    let Some(Some(linked)) = &thread.linked_pull_request else {
        return Vec::new();
    };
    let key = legacy_thread_pull_request_key(linked, None);
    let non_blank = |value: String| {
        TrimmedNonEmptyString::new(&value)
            .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("literal"))
    };
    vec![ThreadPullRequestLink {
        host: non_blank(key.host),
        repository: non_blank(key.repository),
        number: PositiveInt::new(key.number).unwrap_or(linked.number),
        url: linked.url.clone(),
        source: ThreadPullRequestLinkSource::Manual,
        linked_at: "1970-01-01T00:00:00.000Z".to_owned(),
        snapshot: None,
        stack: None,
        watch: None,
    }]
}

/// Apply a single-link sync without copying unrelated PRs into the event history.
pub fn apply_thread_pull_request_link_update(
    thread: &OrchestrationV2AppThread,
    updated: &ThreadPullRequestLink,
) -> OrchestrationV2AppThread {
    let mut next = thread.clone();
    next.pull_requests = Some(
        thread_pull_requests_of(thread)
            .into_iter()
            .map(|link| {
                if thread_pull_request_keys_equal(&link, updated) {
                    updated.clone()
                } else {
                    link
                }
            })
            .collect(),
    );
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(host: &str, repository: &str, number: u64, url: &str) -> ThreadPullRequestLink {
        serde_json::from_value(json!({
            "host": host, "repository": repository, "number": number, "url": url,
            "source": "manual", "linkedAt": "2026-01-01T00:00:00.000Z",
            "snapshot": null, "stack": null
        }))
        .unwrap()
    }

    fn thread(extra: serde_json::Value) -> OrchestrationV2AppThread {
        let mut value = json!({
            "createdBy": "user", "creationSource": "web", "id": "thread-1",
            "projectId": "project-1", "title": "t", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "runtimeMode": "full-access", "interactionMode": "default",
            "branch": null, "worktreePath": null, "activeProviderThreadId": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1"},
            "forkedFrom": null, "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z", "archivedAt": null,
            "settledOverride": null, "settledAt": null, "lastVisitedAt": null, "deletedAt": null
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn link_identity_is_case_insensitive_on_host_and_repository() {
        let a = link(
            "GitHub.com",
            "Owner/Repo",
            7,
            "https://github.com/owner/repo/pull/7",
        );
        let b = link(
            "github.com",
            "owner/repo",
            7,
            "https://github.com/owner/repo/pull/7",
        );
        assert_eq!(thread_pull_request_key_of(&a), "github.com/owner/repo#7");
        assert!(thread_pull_request_keys_equal(&a, &b));
        let other = link(
            "github.com",
            "owner/repo",
            8,
            "https://github.com/owner/repo/pull/8",
        );
        assert!(!thread_pull_request_keys_equal(&a, &other));
    }

    #[test]
    fn azure_selectors_collapse_to_the_canonical_host() {
        assert_eq!(
            canonical_repository_key("ssh.dev.azure.com/v3/org/proj/repo"),
            "dev.azure.com/org/proj/_git/repo"
        );
        assert_eq!(
            canonical_repository_key("org.visualstudio.com/defaultcollection/proj/_git/repo"),
            "dev.azure.com/org/proj/_git/repo"
        );
    }

    #[test]
    fn explicit_pull_requests_win_over_the_legacy_link() {
        let explicit = thread(json!({
            "pullRequests": [],
            "linkedPullRequest": {"projectId": "project-1", "repository": "o/r", "number": 3,
                "url": "https://github.com/o/r/pull/3"}
        }));
        assert!(thread_pull_requests_of(&explicit).is_empty());
    }

    #[test]
    fn legacy_link_upgrades_to_a_manual_link() {
        let legacy = thread(json!({
            "linkedPullRequest": {"projectId": "project-1", "repository": "Owner/Repo", "number": 3,
                "url": "https://github.com/Owner/Repo/pull/3"}
        }));
        let links = thread_pull_requests_of(&legacy);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].host.as_str(), "github.com");
        assert_eq!(links[0].repository.as_str(), "owner/repo");
        assert_eq!(links[0].source, ThreadPullRequestLinkSource::Manual);
        assert_eq!(links[0].linked_at, "1970-01-01T00:00:00.000Z");
        let none = thread(json!({"linkedPullRequest": null}));
        assert!(thread_pull_requests_of(&none).is_empty());
    }

    #[test]
    fn link_update_replaces_only_the_matching_link() {
        let first = link("github.com", "o/r", 1, "https://github.com/o/r/pull/1");
        let second = link("github.com", "o/r", 2, "https://github.com/o/r/pull/2");
        let current = thread(json!({"pullRequests": [first.clone(), second.clone()]}));
        let mut updated = second.clone();
        updated.source = ThreadPullRequestLinkSource::Agent;
        let next = apply_thread_pull_request_link_update(&current, &updated);
        let links = next.pull_requests.unwrap();
        assert_eq!(links[0], first);
        assert_eq!(links[1].source, ThreadPullRequestLinkSource::Agent);
    }
}
