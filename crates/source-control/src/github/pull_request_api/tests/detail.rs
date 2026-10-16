use serde_json::json;
use turin_contracts::pull_request::PullRequestChecksState;

use super::super::detail::ListWorkflowRunsRequiringApprovalInput;
use super::super::errors::{GitHubPullRequestApiError, GitHubWorkflowApprovalRefusedReason};
use super::super::test_support::*;
use crate::github::conditional_checks::{
    KnownWorkflowRun, KnownWorkflowRuns, with_known_workflow_runs,
};
use crate::source_control_core::ambient;

fn approval_input(harness: &Harness) -> ListWorkflowRunsRequiringApprovalInput {
    ListWorkflowRunsRequiringApprovalInput {
        target: harness.target(7),
        head_sha: "abc123".into(),
        head_branch: "feat/page".into(),
        head_repository_owner: "octocat".into(),
    }
}

#[tokio::test]
async fn fails_the_read_when_gh_returns_something_unreadable() {
    let harness = Harness::new();
    harness.once_body(r#"{"message":"not found"}"#);
    let error = harness
        .cli
        .get_pull_request_detail(&harness.target(7))
        .await
        .unwrap_err();
    assert!(matches!(error, GitHubPullRequestApiError::Read(_)));
}

#[tokio::test]
async fn keeps_the_core_detail_read_separate_from_conversation_activity() {
    let harness = Harness::new();
    harness.once_body(
        core_response(json!({
            "title": "Progressive detail",
            "author": { "login": "octocat" },
            "body": "Core body",
            "changedFiles": 2,
        }))
        .to_string(),
    );
    harness.once_body(
        json!({ "data": { "repository": { "pullRequest": {
            "author": { "__typename": "User", "login": "octocat", "avatarUrl": "https://a/o" },
            "commits": { "nodes": [] },
            "comments": { "pageInfo": { "hasNextPage": false, "endCursor": null }, "nodes": [] },
            "reviews": { "pageInfo": { "hasNextPage": false, "endCursor": null }, "nodes": [] },
        } } } })
        .to_string(),
    );
    let target = harness.target(7);
    let detail = harness.cli.get_pull_request_detail(&target).await.unwrap();
    let activity = harness
        .cli
        .get_pull_request_activity(&target)
        .await
        .unwrap();

    assert_eq!(detail.body, "Core body");
    assert_eq!(activity.author.unwrap().login.as_str(), "octocat");
    let vars = harness.vars_at(0);
    assert_eq!(vars["number"], 7);
    assert_eq!(vars["headRef"], "refs/pull/7/head");
    assert!(harness.query_at(0).contains("viewerCanUpdateBranch"));
    assert!(!harness.query_at(0).contains("reviews("));
    let merge = detail.viewer_access.merge_capabilities;
    assert_eq!(
        (merge.merge, merge.squash, merge.rebase),
        (true, false, true)
    );
    let comparison = detail.comparison.unwrap();
    assert_eq!(comparison.behind_by, Some(2));
    assert!(comparison.viewer_can_update);
    // Conversation activity is its own read, and asks for the head of the conversation once.
    assert!(harness.query_at(1).contains("reviews("));
    let vars = harness.vars_at(1);
    assert_eq!(vars["head"], true);
    assert_eq!(vars["withComments"], true);
    assert_eq!(vars["withReviews"], true);
    assert_eq!(harness.call_count(), 2);
}

#[tokio::test]
async fn decodes_reviewers_labels_and_workflow_checks_without_another_detail_read() {
    let harness = Harness::new();
    harness.once_body(
        core_response(json!({
            "baseRef": null,
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "reviewer" } },
                { "requestedReviewer": {
                    "slug": "maintainers",
                    "combinedSlug": "octo/maintainers",
                    "name": "Maintainers",
                    "avatarUrl": "https://avatars.githubusercontent.com/t/1",
                } },
            ] },
            "labels": { "nodes": [{ "name": "bug", "color": "ff0000" }] },
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
                "nodes": [
                    {
                        "__typename": "CheckRun",
                        "name": "build",
                        "status": "COMPLETED",
                        "conclusion": "SUCCESS",
                        "checkSuite": { "workflowRun": { "workflow": { "name": "linux" } } },
                    },
                    {
                        "__typename": "CheckRun",
                        "name": "build",
                        "status": "COMPLETED",
                        "conclusion": "FAILURE",
                        "checkSuite": { "workflowRun": { "workflow": { "name": "windows" } } },
                    },
                ],
                "pageInfo": { "hasNextPage": false },
            } } } }] },
        }))
        .to_string(),
    );
    let detail = harness
        .cli
        .get_pull_request_detail(&harness.target(7))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 1);
    assert_eq!(detail.comparison, None);
    assert_eq!(detail.review_request_logins, vec!["reviewer".to_owned()]);
    assert!(detail.has_team_review_request);
    assert_eq!(detail.review_request_teams.len(), 1);
    let team = &detail.review_request_teams[0];
    assert_eq!(team.login.as_str(), "octo/maintainers");
    assert_eq!(team.name.as_deref(), Some("Maintainers"));
    assert_eq!(
        team.avatar_url.as_deref(),
        Some("https://avatars.githubusercontent.com/t/1")
    );
    assert_eq!(detail.labels.len(), 1);
    assert_eq!(detail.labels[0].name.as_str(), "bug");
    assert_eq!(detail.labels[0].color.as_deref(), Some("ff0000"));
    assert_eq!(detail.checks.len(), 2);
    assert_eq!(detail.checks_state, Some(PullRequestChecksState::Failing));
}

#[tokio::test]
async fn reads_every_check_when_the_combined_response_has_another_page() {
    let harness = Harness::new();
    harness.once_body(
        core_response(json!({
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
                "nodes": [{ "name": "first", "status": "COMPLETED", "conclusion": "SUCCESS" }],
                "pageInfo": { "hasNextPage": true, "endCursor": "c1" },
            } } } }] },
        }))
        .to_string(),
    );
    harness.once_body(check_contexts_page(
        vec![
            json!({ "name": "first", "status": "COMPLETED", "conclusion": "SUCCESS" }),
            json!({ "name": "last", "status": "COMPLETED", "conclusion": "FAILURE" }),
        ],
        None,
        "abc123",
    ));
    let detail = harness
        .cli
        .get_pull_request_detail(&harness.target(7))
        .await
        .unwrap();
    assert_eq!(detail.checks.len(), 2);
    assert_eq!(detail.checks_state, Some(PullRequestChecksState::Failing));
    assert!(!detail.checks_truncated);
    // The whole rollup is walked from its start, so no check is counted twice or skipped.
    let vars = harness.vars_at(1);
    assert_eq!(vars["number"], 7);
    assert_eq!(vars["after"], serde_json::Value::Null);
    assert_eq!(harness.call_count(), 2);
}

#[tokio::test]
async fn refuses_to_combine_checks_from_different_head_revisions() {
    let response = core_response(json!({
        "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
            "nodes": [],
            "pageInfo": { "hasNextPage": true },
        } } } }] },
    }));
    let harness = Harness::new();
    harness.once_body(response.to_string());
    let mut other = response["data"]["repository"]["pullRequest"].clone();
    other["headRefOid"] = json!("new-head");
    other["reviewRequests"] = json!([]);
    other["labels"] = json!([]);
    other["statusCheckRollup"] = json!([]);
    harness.once_body(other.to_string());
    let error = harness
        .cli
        .get_pull_request_detail(&harness.target(7))
        .await
        .unwrap_err();
    assert!(matches!(error, GitHubPullRequestApiError::Read(_)));
}

#[tokio::test]
async fn refuses_a_check_walk_whose_head_moved_between_pages() {
    // The page names a different head than the detail did: a push landed mid-walk.
    let harness = Harness::new();
    harness.once_body(
        core_response(json!({
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
                "nodes": [],
                "pageInfo": { "hasNextPage": true, "endCursor": "c1" },
            } } } }] },
        }))
        .to_string(),
    );
    harness.once_body(check_contexts_page(vec![], None, "new-head"));
    let error = harness
        .cli
        .get_pull_request_detail(&harness.target(7))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::Read(read) => {
            assert_eq!(read.operation, "getPullRequestDetail");
            assert!(read.cause.to_string().contains("head changed"));
        }
        other => panic!("expected a read error, got {other:?}"),
    }
}

#[tokio::test]
async fn preserves_the_reserve_for_automatic_detail_reads_and_allows_manual_checks() {
    let harness = Harness::new();
    harness.always(Ok(output_with(
        core_response(json!({})).to_string(),
        false,
        false,
        graphql_quota(499, RESET_2099),
    )));
    let target = GitHubPullRequestTarget::new("/w", "acme/web", "github.core-reserve.test", 7);
    harness.cli.get_pull_request_detail(&target).await.unwrap();
    let error = harness
        .cli
        .get_pull_request_detail(&target)
        .await
        .unwrap_err();
    assert!(error.is_rate_limit_paused());
    assert_eq!(harness.call_count(), 1);
    ambient::with_interactive(true, harness.cli.get_pull_request_detail(&target))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 2);
}

use super::super::GitHubPullRequestTarget;

#[tokio::test]
async fn counts_revalidated_runs_that_wait_on_a_maintainer_as_github_reports_them() {
    let harness = Harness::new();
    workflow_approval_routes(
        &harness,
        || cross_repository_detail("abc123"),
        heads(&[7]),
        workflow_runs(&[]),
    );
    let known = KnownWorkflowRuns {
        head_sha: "abc123".into(),
        runs: vec![
            // Live shape: waiting on approval is completed + action_required.
            KnownWorkflowRun {
                id: 10,
                name: None,
                html_url: None,
                status: Some("completed".into()),
                conclusion: Some("action_required".into()),
                head_branch: Some("feat/page".into()),
            },
            KnownWorkflowRun {
                id: 11,
                name: None,
                html_url: None,
                status: Some("completed".into()),
                conclusion: Some("success".into()),
                head_branch: Some("feat/page".into()),
            },
        ],
    };
    let input = approval_input(&harness);
    let runs = with_known_workflow_runs(
        Some(known),
        harness.cli.list_workflow_runs_requiring_approval(&input),
    )
    .await
    .unwrap();
    assert_eq!(runs.iter().map(|run| run.id).collect::<Vec<_>>(), vec![10]);
    assert_eq!(harness.rest_calls_to("actions/runs?").len(), 0);
}

#[tokio::test]
async fn reads_workflow_runs_and_their_pull_request_scope_concurrently() {
    let harness = Harness::new();
    let heads_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let runs_started = std::sync::Arc::new(tokio::sync::Notify::new());
    // Each read only answers once the other has started: serial reads would deadlock.
    {
        let (heads_started, runs_started) = (heads_started.clone(), runs_started.clone());
        harness.on_async(move |call| {
            let (heads_started, runs_started) = (heads_started.clone(), runs_started.clone());
            let graphql = call.is_graphql();
            Box::pin(async move {
                if graphql {
                    heads_started.notify_one();
                    runs_started.notified().await;
                    ok_json(heads(&[7]))
                } else {
                    runs_started.notify_one();
                    heads_started.notified().await;
                    ok_json(workflow_runs(&[10]))
                }
            })
        });
    }
    let runs = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        harness
            .cli
            .list_workflow_runs_requiring_approval(&approval_input(&harness)),
    )
    .await
    .expect("the reads ran concurrently")
    .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, 10);
    assert_eq!(runs[0].name, "build");
    assert_eq!(runs[0].url.as_deref(), Some("https://example.com/10"));
}

#[tokio::test]
async fn refuses_workflow_approval_when_one_head_belongs_to_several_pull_requests() {
    let harness = Harness::new();
    workflow_approval_routes(
        &harness,
        || cross_repository_detail("abc123"),
        heads(&[7, 8]),
        workflow_runs(&[]),
    );
    let error = harness
        .cli
        .list_workflow_runs_requiring_approval(&approval_input(&harness))
        .await
        .unwrap_err();
    match &error {
        GitHubPullRequestApiError::WorkflowApprovalRefused(refused) => {
            assert_eq!(
                refused.reason,
                GitHubWorkflowApprovalRefusedReason::HeadNotUnique
            );
            assert_eq!(refused.number, 7);
            assert_eq!(refused.observed_count, 2);
            assert_eq!(refused.limit, 1_000);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        error
            .to_string()
            .contains("instead of uniquely matching #7")
    );
}

#[tokio::test]
async fn refuses_workflow_approval_when_the_head_list_stops_before_its_end() {
    // GitHub hands the same cursor back, so the second page is never read past. #7 looks unique
    // on what was read, and an unread page could still hold another head like it.
    let mut repeating = heads(&[7]);
    repeating["data"]["repository"]["pullRequests"]["pageInfo"] =
        json!({ "hasNextPage": true, "endCursor": "same" });
    let harness = Harness::new();
    workflow_approval_routes(
        &harness,
        || cross_repository_detail("abc123"),
        repeating,
        workflow_runs(&[]),
    );
    let error = harness
        .cli
        .list_workflow_runs_requiring_approval(&approval_input(&harness))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::WorkflowApprovalRefused(refused) => {
            assert_eq!(
                refused.reason,
                GitHubWorkflowApprovalRefusedReason::HeadListTruncated
            )
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn surfaces_a_workflow_run_list_beyond_the_safe_approval_bound() {
    let harness = Harness::new();
    let run_pages = std::sync::Arc::new(parking_lot::Mutex::new(0i64));
    {
        let run_pages = run_pages.clone();
        harness.on(move |call| {
            if call.is_graphql() {
                return ok_json(heads(&[7]));
            }
            let mut pages = run_pages.lock();
            let page = *pages;
            *pages += 1;
            let ids: Vec<i64> = (0..100).map(|index| page * 100 + index + 1).collect();
            ok_json(workflow_runs(&ids))
        });
    }
    let error = harness
        .cli
        .list_workflow_runs_requiring_approval(&approval_input(&harness))
        .await
        .unwrap_err();
    // Paged a hundred at a time, it stops at the first page past the bound.
    match &error {
        GitHubPullRequestApiError::WorkflowApprovalRefused(refused) => {
            assert_eq!(
                refused.reason,
                GitHubWorkflowApprovalRefusedReason::RunListTruncated
            );
            assert_eq!(refused.number, 7);
            assert_eq!(refused.observed_count, 1_100);
            assert_eq!(refused.limit, 1_000);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(error.to_string().contains("more than 1000 workflow runs"));
    assert_eq!(*run_pages.lock(), 11);
}
