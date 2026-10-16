use serde_json::json;
use turin_contracts::pull_request::PullRequestState;

use super::super::errors::GitHubPullRequestApiError;
use super::super::stack::GetPullRequestStackInput;
use super::super::test_support::*;
use crate::github::api::{
    GitHubApiAuthenticationError, GitHubApiError, GitHubApiNotFoundError, GitHubApiResponseError,
};
use crate::github::pull_request_json::{GitHubPullRequestStack, GitHubPullRequestStackLayer};

fn input(host: &str, include_details: bool) -> GetPullRequestStackInput {
    GetPullRequestStackInput {
        target: crate::github::pull_request_api::GitHubPullRequestTarget::new(
            "/w", "acme/web", host, 7,
        ),
        include_details,
    }
}

#[tokio::test]
async fn reads_the_stack_a_pull_request_is_in_through_the_stacks_preview_on_its_host() {
    let harness = Harness::new();
    harness.once_body(
        json!([{
            "id": 42,
            "number": 3,
            "url": "https://api.github.com/repos/acme/web/stacks/3",
            "base": { "ref": "main" },
            "pull_requests": [
                { "number": 6, "head": { "ref": "feat/one" }, "state": "closed", "merged_at": "2026-09-02" },
                { "number": 7, "head": { "ref": "feat/two" }, "state": "open", "merged_at": null },
            ],
        }])
        .to_string(),
    );
    let stack = harness
        .cli
        .get_pull_request_stack(&input("ghe.example.com", false))
        .await
        .unwrap();
    let layer = |number, head_branch: &str, state| GitHubPullRequestStackLayer {
        title: None,
        is_draft: None,
        head_sha: None,
        number,
        head_branch: head_branch.into(),
        state,
    };
    assert_eq!(
        stack,
        Some(GitHubPullRequestStack {
            id: "42".into(),
            number: 3,
            url: "https://api.github.com/repos/acme/web/stacks/3".into(),
            base: "main".into(),
            layers: vec![
                layer(6, "feat/one", PullRequestState::Merged),
                layer(7, "feat/two", PullRequestState::Open),
            ],
        })
    );
    let call = harness.call_at(0);
    assert!(call.is_rest());
    assert_eq!(call.host(), "ghe.example.com");
    assert_eq!(call.path(), "repos/acme/web/stacks?pull_request=7");
}

#[tokio::test]
async fn fetches_layer_titles_only_when_the_caller_asks_for_stack_details() {
    let minimal = json!({
        "url": "https://api.github.com/repos/acme/web/stacks/3",
        "number": 3,
        "base": { "ref": "main" },
        "pull_requests": [
            { "number": 7, "head": { "ref": "feat/two", "sha": "abc123" }, "state": "open", "merged_at": null }
        ],
    });
    let harness = Harness::new();
    harness.once_body(json!([minimal]).to_string());
    let mut detailed = minimal.clone();
    detailed["pull_requests"] = json!([{
        "number": 7,
        "head": { "ref": "feat/two", "sha": "abc123" },
        "state": "open",
        "merged_at": null,
        "title": "Second layer",
        "draft": false,
    }]);
    harness.once_body(detailed.to_string());
    let stack = harness
        .cli
        .get_pull_request_stack(&input("github.com", true))
        .await
        .unwrap()
        .unwrap();
    let layer = &stack.layers[0];
    assert_eq!(layer.title.as_deref(), Some("Second layer"));
    assert_eq!(layer.head_sha.as_deref(), Some("abc123"));
    assert_eq!(layer.is_draft, Some(false));
    assert_eq!(harness.path_at(1), "repos/acme/web/stacks/3");
}

#[tokio::test]
async fn reads_an_empty_stacks_listing_as_not_stacked() {
    let harness = Harness::new();
    harness.once_body("[]");
    assert_eq!(
        harness
            .cli
            .get_pull_request_stack(&input("github.com", false))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn reads_a_host_that_refuses_the_stacks_preview_as_not_stacked() {
    let harness = Harness::new();
    harness.once(Err(GitHubApiError::NotFound(GitHubApiNotFoundError {
        host: "github.com".into(),
        operation: "getPullRequestStack".into(),
    })));
    assert_eq!(
        harness
            .cli
            .get_pull_request_stack(&input("github.com", false))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn does_not_read_a_refused_credential_as_an_unstacked_pull_request() {
    let harness = Harness::new();
    harness.once(Err(GitHubApiError::Authentication(
        GitHubApiAuthenticationError {
            host: "github.com".into(),
            operation: "getPullRequestStack".into(),
        },
    )));
    let error = harness
        .cli
        .get_pull_request_stack(&input("github.com", false))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        GitHubPullRequestApiError::Api(GitHubApiError::Authentication(_))
    ));
}

#[tokio::test]
async fn preserves_transient_stack_failures_instead_of_reporting_no_stack() {
    let harness = Harness::new();
    harness.once(Err(GitHubApiError::Response(GitHubApiResponseError {
        host: "github.com".into(),
        operation: "getPullRequestStack".into(),
        status: 503,
        github_errors: None,
    })));
    let error = harness
        .cli
        .get_pull_request_stack(&input("github.com", false))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        GitHubPullRequestApiError::Api(GitHubApiError::Response(ref response)) if response.status == 503
    ));
}

#[tokio::test]
async fn reports_a_stacks_answer_it_cannot_read_against_the_stack_read() {
    let harness = Harness::new();
    harness.once_body(r#"[{"id":42}]"#);
    let error = harness
        .cli
        .get_pull_request_stack(&input("github.com", false))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::Read(read) => assert_eq!(read.operation, "getPullRequestStack"),
        other => panic!("expected a read error, got {other:?}"),
    }
}
