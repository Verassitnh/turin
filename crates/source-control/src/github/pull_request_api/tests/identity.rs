use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::Notify;

use super::super::errors::GitHubPullRequestApiError;
use super::super::test_support::*;
use super::super::*;
use crate::github::api::{
    GitHubApiClient, GitHubApiError, GitHubApiResponseError, GitHubHttp, GitHubHttpRequest,
    GitHubHttpResponse, GitHubRequestCredential,
};
use crate::github::credentials::{
    GitHubCredential, GitHubCredentialSource, GitHubCredentialUnavailableError, GitHubCredentials,
};
use crate::github::quota::GitHubQuota;
use crate::source_control_core::Cause;
use crate::source_control_core::clock::ManualClock;
use crate::source_control_core::pull_request_provider::VerifiedCredentialInput;
use crate::source_control_core::rate_limit::SourceControlRateLimit;
use crate::source_control_core::redacted::Redacted;

struct SwitchingCredentials {
    active: Arc<Mutex<String>>,
}

#[async_trait]
impl GitHubCredentials for SwitchingCredentials {
    async fn get(&self, host: &str) -> Result<GitHubCredential, GitHubCredentialUnavailableError> {
        let token = self.active.lock().clone();
        Ok(GitHubCredential {
            host: host.to_owned(),
            fingerprint: format!("{host}:{}{}", token.len(), token.chars().last().unwrap()),
            token: Redacted::new(token),
            source: GitHubCredentialSource::Gh,
        })
    }
    async fn invalidate(&self, _host: &str) {}
}

struct RecordingHttp {
    /// `(url, authorization)` of every request.
    requests: Mutex<Vec<(String, Option<String>)>>,
}

#[async_trait]
impl GitHubHttp for RecordingHttp {
    async fn execute(&self, request: GitHubHttpRequest) -> Result<GitHubHttpResponse, Cause> {
        self.requests.lock().push((
            request.url.clone(),
            request.header("authorization").map(str::to_owned),
        ));
        let body = if request.url.ends_with("/user") {
            json!({ "id": 123, "login": "same-account" })
        } else {
            json!({ "data": { "repository": { "pullRequest": { "id": "PR_1" } }, "addComment": {} } })
        };
        Ok(GitHubHttpResponse {
            status: 200,
            body: body.to_string(),
            ..Default::default()
        })
    }
}

fn input(host: &str) -> VerifiedCredentialInput {
    VerifiedCredentialInput {
        cwd: "/w".into(),
        host: host.into(),
    }
}

#[tokio::test]
async fn keeps_a_verified_credential_through_an_auth_switch_and_separates_token_fingerprints() {
    let active = Arc::new(Mutex::new("broad-credential".to_owned()));
    let http = Arc::new(RecordingHttp {
        requests: Mutex::new(Vec::new()),
    });
    let clock = ManualClock::new(0);
    let api = Arc::new(GitHubApiClient::new(
        http.clone(),
        Arc::new(SwitchingCredentials {
            active: active.clone(),
        }),
        Arc::new(GitHubQuota::new(clock.clone())),
        SourceControlRateLimit::new(clock.clone()),
        clock.clone(),
    ));
    let cli = GitHubPullRequestApi::new(api, Arc::new(ScriptedHost::default()), clock);
    let input = input("github.com");

    let first = cli
        .with_verified_credential(&input, |identity| {
            let cli = cli.clone();
            let active = active.clone();
            let input = input.clone();
            async move {
                *active.lock() = "restricted-credential".into();
                assert_eq!(cli.get_viewer_login(&input).await?, "same-account");
                cli.comment_on_pull_request(&actions::CommentOnPullRequestInput {
                    target: GitHubPullRequestTarget::new("/repo", "owner/repo", "github.com", 1),
                    body: "comment".into(),
                })
                .await?;
                Ok::<_, GitHubPullRequestApiError>(identity)
            }
        })
        .await
        .unwrap();
    let second = cli
        .with_verified_credential(&input, |identity| async move {
            Ok::<_, GitHubPullRequestApiError>(identity)
        })
        .await
        .unwrap();
    assert_eq!(first.account_id, second.account_id);
    assert_ne!(first.credential_fingerprint, second.credential_fingerprint);
    let printed = format!("{first:?}{second:?}");
    assert!(!printed.contains("broad-credential"));
    assert!(!printed.contains("restricted-credential"));

    let requests = http.requests.lock().clone();
    // The comment was written under the credential the page verified, not the switched one.
    let graphql: Vec<_> = requests
        .iter()
        .filter(|(url, _)| url.ends_with("/graphql"))
        .map(|(_, authorization)| authorization.clone())
        .collect();
    assert_eq!(
        graphql,
        vec![
            Some("Bearer broad-credential".to_owned()),
            Some("Bearer broad-credential".to_owned())
        ]
    );
    let user: Vec<_> = requests
        .iter()
        .filter(|(url, _)| url.ends_with("/user"))
        .map(|(_, authorization)| authorization.clone())
        .collect();
    assert_eq!(
        user,
        vec![
            Some("Bearer broad-credential".to_owned()),
            Some("Bearer restricted-credential".to_owned())
        ]
    );
    let identity = cli.get_routing_identity(&input).await.unwrap();
    assert_eq!(
        (identity.account_id.as_str(), identity.viewer.as_str()),
        ("123", "same-account")
    );
}

#[tokio::test]
async fn holds_background_previews_once_an_answer_reports_the_reserve_until_it_resets() {
    let harness = Harness::new();
    let host = "preview-budget.example";
    harness.quota.observe(host, &graphql_quota(501, RESET_2099));
    // GitHub's count, not ours: the answer below reports the reserve has been reached.
    harness.always(Ok(output_with(
        json!({ "data": { "repository": { "pullRequest": {
            "number": 7,
            "title": "Fast previews",
            "url": "https://preview-budget.example/acme/web/pull/7",
            "state": "OPEN",
            "isDraft": false,
            "createdAt": "2026-07-01T00:00:00Z",
            "author": null,
        } } } })
        .to_string(),
        false,
        false,
        graphql_quota(499, RESET_2099),
    )));
    let target = GitHubPullRequestTarget::new("/w", "acme/web", host, 7);
    harness.cli.get_pull_request_preview(&target).await.unwrap();
    let held = harness
        .cli
        .get_pull_request_preview(&GitHubPullRequestTarget {
            number: 8,
            ..target.clone()
        })
        .await
        .unwrap_err();
    assert!(held.is_rate_limit_paused());
    assert_eq!(harness.call_count(), 1);
    harness.clock.set(RESET_2099);
    harness.cli.get_pull_request_preview(&target).await.unwrap();
    assert_eq!(harness.call_count(), 2);
}

#[tokio::test]
async fn loads_the_complete_hover_card_with_one_graphql_request() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": { "repository": { "pullRequest": {
            "number": 7,
            "title": "Fast previews",
            "url": "https://github.example/acme/web/pull/7",
            "state": "MERGED",
            "isDraft": false,
            "createdAt": "2026-07-01T00:00:00Z",
            "author": {
                "login": "octocat",
                "name": "Octo Cat",
                "avatarUrl": "https://github.example/avatar.png",
            },
        } } } })
        .to_string(),
    );
    let preview = harness
        .cli
        .get_pull_request_preview(&GitHubPullRequestTarget::new(
            "/w",
            "acme/web",
            "github.example",
            7,
        ))
        .await
        .unwrap();
    assert_eq!(preview.number, 7);
    assert_eq!(preview.title, "Fast previews");
    assert_eq!(preview.url, "https://github.example/acme/web/pull/7");
    assert_eq!(
        preview.state,
        turin_contracts::pull_request::PullRequestState::Merged
    );
    assert!(!preview.is_draft);
    assert_eq!(preview.created_at, "2026-07-01T00:00:00Z");
    let author = preview.author.unwrap();
    assert_eq!(author.login.as_str(), "octocat");
    assert_eq!(author.name.as_deref(), Some("Octo Cat"));
    assert_eq!(
        author.avatar_url.as_deref(),
        Some("https://github.example/avatar.png")
    );
    assert_eq!(harness.call_count(), 1);
    let call = harness.call_at(0);
    assert!(call.is_graphql());
    assert_eq!(call.host(), "github.example");
}

#[tokio::test]
async fn coalesces_concurrent_identity_verification_for_the_same_host_and_credential() {
    let harness = Harness::new();
    harness.on_async(|_| {
        Box::pin(async {
            tokio::task::yield_now().await;
            ok(r#"{"id":123,"login":"viewer"}"#)
        })
    });
    let input = input("github.identity-flight.test");
    let results =
        futures::future::join_all((0..4).map(|_| harness.cli.get_routing_identity(&input))).await;
    for result in results {
        let identity = result.unwrap();
        assert_eq!(
            (identity.account_id.as_str(), identity.viewer.as_str()),
            ("123", "viewer")
        );
    }
    assert_eq!(harness.call_count(), 1);
    assert_eq!(harness.path_at(0), "user");
}

#[tokio::test]
async fn lets_another_identity_reader_continue_when_the_first_verification_is_interrupted() {
    let harness = Harness::new();
    let started = Arc::new(Notify::new());
    let verifications = Arc::new(Mutex::new(0));
    {
        let started = started.clone();
        let verifications = verifications.clone();
        harness.on_async(move |_| {
            let started = started.clone();
            let n = {
                let mut count = verifications.lock();
                *count += 1;
                *count
            };
            Box::pin(async move {
                if n == 1 {
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
                ok(r#"{"id":123,"login":"viewer"}"#)
            })
        });
    }
    let input = input("github.identity-cancel.test");
    let first = {
        let cli = harness.cli.clone();
        let input = input.clone();
        tokio::spawn(async move { cli.get_routing_identity(&input).await })
    };
    started.notified().await;
    // The second reader is queued on the same credential's lock behind the first.
    let second = {
        let cli = harness.cli.clone();
        let input = input.clone();
        tokio::spawn(async move { cli.get_routing_identity(&input).await })
    };
    tokio::task::yield_now().await;
    first.abort();
    let identity = second.await.unwrap().unwrap();
    assert_eq!(
        (identity.account_id.as_str(), identity.viewer.as_str()),
        ("123", "viewer")
    );
    assert_eq!(*verifications.lock(), 2);
}

#[tokio::test]
async fn fails_when_the_authenticated_account_has_no_login() {
    let harness = Harness::new();
    harness.once_body("  ");
    let error = harness
        .cli
        .get_viewer_login(&input("github.com"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        GitHubPullRequestApiError::ViewerLoginUnavailable(_)
    ));
}

#[tokio::test]
async fn looks_up_the_authenticated_account_on_the_requested_enterprise_host() {
    let harness = Harness::new();
    harness.once_body(r#"{"id":456,"login":"enterprise-user"}"#);
    let login = harness
        .cli
        .get_viewer_login(&input("github.acme.com"))
        .await
        .unwrap();
    assert_eq!(login, "enterprise-user");
    assert!(
        harness
            .api
            .credential_calls
            .lock()
            .contains(&"github.acme.com".to_owned())
    );
    let call = harness.call_at(0);
    assert!(call.is_rest());
    assert_eq!((call.host(), call.path()), ("github.acme.com", "user"));
}

#[tokio::test]
async fn reuses_verified_credentials_offline_and_refuses_an_unverified_replacement() {
    let harness = Harness::new();
    let input = input("github.identity-cache.test");
    let credential = |suffix: &'static str, token: &'static str| {
        move |host: &str| {
            Ok(GitHubRequestCredential {
                token: Redacted::new(token.to_owned()),
                fingerprint: format!("{host}:{suffix}"),
            })
        }
    };
    harness.set_credential(credential("a", "test-credential-a"));
    harness.once_body(r#"{"id":123,"login":"maria-rcks"}"#);
    let identity = harness.cli.get_routing_identity(&input).await.unwrap();
    assert_eq!(
        (identity.account_id.as_str(), identity.viewer.as_str()),
        ("123", "maria-rcks")
    );

    // The same credential again is answered from what was verified, without asking GitHub.
    let identity = harness.cli.get_routing_identity(&input).await.unwrap();
    assert_eq!(
        (identity.account_id.as_str(), identity.viewer.as_str()),
        ("123", "maria-rcks")
    );
    assert_eq!(harness.call_count(), 1);

    // A switched credential is not trusted on the strength of the old one's answer.
    harness.set_credential(credential("b", "test-credential-b"));
    harness.once(Err(GitHubApiError::Response(GitHubApiResponseError {
        host: "github.com".into(),
        operation: "getRoutingIdentity".into(),
        status: 502,
        github_errors: None,
    })));
    // GitHub's own refusal is reported as itself, so the page can say what went wrong.
    let failure = harness.cli.get_routing_identity(&input).await.unwrap_err();
    assert!(matches!(
        failure,
        GitHubPullRequestApiError::Api(GitHubApiError::Response(_))
    ));
    assert!(!format!("{failure} {failure:?}").contains("test-credential-b"));
    assert_eq!(harness.call_count(), 2);

    harness.once_body(r#"{"id":456,"login":"maria-rcks"}"#);
    let identity = harness.cli.get_routing_identity(&input).await.unwrap();
    assert_eq!(
        (identity.account_id.as_str(), identity.viewer.as_str()),
        ("456", "maria-rcks")
    );
}
