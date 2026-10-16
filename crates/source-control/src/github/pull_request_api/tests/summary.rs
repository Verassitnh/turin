use serde_json::json;
use turin_contracts::pull_request::{
    PullRequestChecksState, PullRequestMergeability, PullRequestReviewDecision, PullRequestState,
};

use super::super::test_support::*;

fn node(number: u64) -> serde_json::Value {
    json!({
        "number": number,
        "title": format!("Pull request {number}"),
        "url": format!("https://github.com/acme/web/pull/{number}"),
        "author": { "__typename": "User", "login": "octocat", "name": "Octo Cat", "avatarUrl": null },
        "baseRefName": "main",
        "headRefName": format!("feat/{number}"),
        "state": "OPEN",
        "isDraft": false,
        "mergeable": "MERGEABLE",
        "reviewDecision": null,
        "latestReviews": { "nodes": [{ "state": "APPROVED", "author": { "login": "reviewer" } }] },
        "additions": 12,
        "deletions": 3,
        "changedFiles": 2,
        "updatedAt": "2026-08-24T12:34:56.000Z",
        "mergedAt": null,
        "closedAt": null,
        "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": "SUCCESS" } } }] },
    })
}

#[tokio::test(start_paused = true)]
async fn reads_linked_pull_requests_on_one_host_together_filed_back_by_position() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": { "s0": { "pullRequest": node(7) }, "s1": { "pullRequest": node(8) } } })
            .to_string(),
    );
    let (target_seven, target_eight) = (harness.target(7), harness.target(8));
    let (seven, eight) = tokio::join!(
        harness.cli.get_pull_request_summary(&target_seven),
        harness.cli.get_pull_request_summary(&target_eight),
    );
    let (seven, eight) = (seven.unwrap(), eight.unwrap());
    assert_eq!(seven.number, 7);
    assert_eq!(seven.state, PullRequestState::Open);
    assert_eq!(seven.head_branch, "feat/7");
    assert_eq!(
        seven
            .author
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .login
            .as_str(),
        "octocat"
    );
    assert_eq!(seven.changed_files, Some(2));
    assert_eq!(
        seven.review_decision,
        Some(Some(PullRequestReviewDecision::Approved))
    );
    assert_eq!(
        seven.checks_state,
        Some(Some(PullRequestChecksState::Passing))
    );
    assert_eq!(seven.mergeability, Some(PullRequestMergeability::Mergeable));
    assert_eq!(eight.head_branch, "feat/8");
    assert_eq!(harness.call_count(), 1);
    assert!(harness.query_at(0).contains(
        "s0: repository(owner: $s0_owner, name: $s0_name) { pullRequest(number: $s0_number)"
    ));
    let vars = harness.vars_at(0);
    assert_eq!(vars["s0_owner"], "acme");
    assert_eq!(vars["s0_name"], "web");
    assert_eq!(vars["s0_number"], 7);
    assert_eq!(vars["s1_number"], 8);
}

#[tokio::test(start_paused = true)]
async fn fingerprints_watched_pull_requests_on_one_host_in_one_read() {
    let node = |comments: u64| {
        json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "headRefOid": "abc123",
            "comments": { "totalCount": comments, "nodes": [] },
            "reviews": { "totalCount": 0, "nodes": [] },
            "reviewThreads": { "totalCount": 0 },
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": null } }] },
        })
    };
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": { "w0": { "pullRequest": node(1) }, "w1": { "pullRequest": null } } })
            .to_string(),
    );
    let (target_seven, target_eight) = (harness.target(7), harness.target(8));
    let (seven, eight) = tokio::join!(
        harness
            .cli
            .get_pull_request_watch_fingerprint(&target_seven),
        harness
            .cli
            .get_pull_request_watch_fingerprint(&target_eight),
    );
    assert!(seven.unwrap().unwrap().remarks.starts_with("1 "));
    // GitHub had no answer for #8, so its watch reads it in full.
    assert_eq!(eight.unwrap(), None);
    assert_eq!(harness.call_count(), 1);
    assert!(harness.query_at(0).contains(
        "w1: repository(owner: $w1_owner, name: $w1_name) { pullRequest(number: $w1_number)"
    ));
    let vars = harness.vars_at(0);
    assert_eq!(vars["w1_owner"], "acme");
    assert_eq!(vars["w1_name"], "web");
    assert_eq!(vars["w1_number"], 8);
}

#[tokio::test(start_paused = true)]
async fn reads_a_pull_request_the_batch_said_nothing_about_on_its_own() {
    let harness = Harness::new();
    harness.once_body(r#"{"data":{"s0":{"pullRequest":null}}}"#);
    harness.once_body(
        json!({ "data": { "s0": { "pullRequest": {
            "number": 7,
            "title": "Reuse the summary",
            "url": "https://github.com/acme/web/pull/7",
            "author": { "login": "octocat", "name": "Octo Cat" },
            "baseRefName": "main",
            "headRefName": "feat/summary",
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "additions": 12,
            "deletions": 3,
            "changedFiles": 2,
            "updatedAt": "2026-08-24T12:34:56.000Z",
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": "SUCCESS" } } }] },
        } } } })
        .to_string(),
    );
    let summary = harness
        .cli
        .get_pull_request_summary(&harness.target(7))
        .await
        .unwrap();
    assert_eq!(summary.head_branch, "feat/summary");
    assert_eq!(
        summary.checks_state,
        Some(Some(PullRequestChecksState::Passing))
    );
    assert_eq!(harness.call_count(), 2);
    assert_eq!(
        serde_json::Value::Object(harness.vars_at(1)),
        json!({ "owner": "acme", "name": "web", "number": 7 })
    );
}

#[tokio::test(start_paused = true)]
async fn a_paused_budget_fails_every_batched_summary_without_reading_one_at_a_time() {
    // Not an upstream test: pins the `catchCauseIf` exception for a paused rate limit.
    let harness = Harness::new();
    harness.once(Err(crate::github::api::GitHubApiError::Paused(
        crate::source_control_core::rate_limit::SourceControlRateLimitPausedError {
            provider: turin_contracts::source_control::SourceControlProviderKind::new("github")
                .unwrap(),
            host: "github.com".into(),
            retry_at: 1,
        },
    )));
    let (target_seven, target_eight) = (harness.target(7), harness.target(8));
    let (seven, eight) = tokio::join!(
        harness.cli.get_pull_request_summary(&target_seven),
        harness.cli.get_pull_request_summary(&target_eight),
    );
    assert!(seven.unwrap_err().is_rate_limit_paused());
    assert!(eight.unwrap_err().is_rate_limit_paused());
    assert_eq!(harness.call_count(), 1);
}
