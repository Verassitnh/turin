//! Port of upstream/apps/server/src/pullRequest/pullRequestSyncKey.ts
//!
//! GitHub only: upstream's Azure DevOps branch (hostless checkout-scoped repositories resolved
//! through the identity's canonical key) is cut with that host. Hosted aliases are still
//! normalized by `canonical_repository_key`.

use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::pull_request::PullRequestRef;
use turin_contracts::thread_pull_request::ThreadPullRequestKey;

use crate::shared::thread_pull_requests::{
    ThreadPullRequestKeySource, normalize_thread_pull_request_key, pull_request_host_of_identity,
};

/// Convert checkout-scoped references to the host-level identity used by linked threads.
pub fn pull_request_sync_key(
    reference: &PullRequestRef,
    identity: Option<&RepositoryIdentity>,
) -> Option<ThreadPullRequestKey> {
    let host = match reference.host.as_ref() {
        Some(host) => host.to_string(),
        None => pull_request_host_of_identity(identity?)?,
    };
    Some(normalize_thread_pull_request_key(
        ThreadPullRequestKeySource {
            host: &host,
            repository: reference.repository.as_str(),
            number: reference.number.get(),
            authority: None,
            url: None,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_contracts::base_schemas::{PositiveInt, ProjectId, TrimmedNonEmptyString};

    fn reference(host: Option<&str>, repository: &str) -> PullRequestRef {
        PullRequestRef {
            project_id: ProjectId::new("project").unwrap(),
            host: host.map(|h| TrimmedNonEmptyString::new(h).unwrap()),
            expected_account_id: None,
            allow_stale: None,
            repository: TrimmedNonEmptyString::new(repository).unwrap(),
            number: PositiveInt::new(7).unwrap(),
        }
    }

    fn key(host: &str, repository: &str, number: u64) -> ThreadPullRequestKey {
        ThreadPullRequestKey {
            host: TrimmedNonEmptyString::new(host).unwrap(),
            repository: TrimmedNonEmptyString::new(repository).unwrap(),
            number: PositiveInt::new(number).unwrap(),
        }
    }

    // The hostless/checkout-host Azure DevOps cases are dropped with that host; the remainder
    // of upstream's alias test is kept, plus the identity-derived host this port still supports.
    #[test]
    fn normalizes_complete_hosted_aliases_without_requiring_a_checkout() {
        assert_eq!(
            pull_request_sync_key(
                &reference(Some("org.visualstudio.com"), "project/_git/web"),
                None
            ),
            Some(key("dev.azure.com", "org/project/_git/web", 7))
        );
        assert_eq!(
            pull_request_sync_key(&reference(Some("github.com"), "acme/web"), None),
            Some(key("github.com", "acme/web", 7))
        );
        assert_eq!(pull_request_sync_key(&reference(None, "web"), None), None);
    }

    #[test]
    fn reads_the_host_from_the_checkout_identity_when_the_reference_has_none() {
        let identity: RepositoryIdentity = serde_json::from_value(serde_json::json!({
            "canonicalKey": "GitHub.com/acme/web",
            "provider": "github",
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": "https://github.com/acme/web.git",
            },
        }))
        .unwrap();
        assert_eq!(
            pull_request_sync_key(&reference(None, "Acme/Web"), Some(&identity)),
            Some(key("github.com", "acme/web", 7))
        );
        // A reference's own host wins over the identity's.
        assert_eq!(
            pull_request_sync_key(&reference(Some("ghe.test"), "acme/web"), Some(&identity)),
            Some(key("ghe.test", "acme/web", 7))
        );
    }

    #[test]
    fn a_local_path_identity_has_no_host() {
        let identity: RepositoryIdentity = serde_json::from_value(serde_json::json!({
            "canonicalKey": "/tmp/r/remote",
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": "/tmp/r/remote.git",
            },
        }))
        .unwrap();
        assert_eq!(
            pull_request_sync_key(&reference(None, "web"), Some(&identity)),
            None
        );
    }
}
