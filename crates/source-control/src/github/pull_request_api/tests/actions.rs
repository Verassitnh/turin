use serde_json::{Value, json};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestMergeMethod, PullRequestReactionContent,
    PullRequestReviewCommentDraft, PullRequestReviewVerdict, PullRequestReviewerKind,
    PullRequestUpdateMethod,
};

use super::super::actions::*;
use super::super::errors::GitHubPullRequestApiError;
use super::super::test_support::*;
use crate::github::api::RestMethod;

fn action(harness: &Harness, action: PullRequestAction) -> RunPullRequestActionInput {
    RunPullRequestActionInput::new(harness.target(7), action)
}

/// The state a merge or branch update reads first: behind its base and blocked on checks.
fn action_state(overrides: Value) -> Value {
    let mut pull_request = json!({
        "id": "PR_7",
        "headRefOid": "abc123",
        "isMergeQueueEnabled": false,
        "mergeStateStatus": "BLOCKED",
        "baseRef": { "compare": { "behindBy": 2 } },
    });
    merge_into(&mut pull_request, overrides);
    json!({ "data": { "repository": { "pullRequest": pull_request } } })
}

fn merge_into(base: &mut Value, overrides: Value) {
    if let (Some(base), Value::Object(overrides)) = (base.as_object_mut(), overrides) {
        base.extend(overrides);
    }
}

fn merge_message(body: &str, is_merge_queue_enabled: bool) -> Value {
    json!({ "data": { "repository": { "pullRequest": {
        "isMergeQueueEnabled": is_merge_queue_enabled,
        "headRefOid": "abc123",
        "viewerMergeBodyText": body,
    } } } })
}

#[tokio::test]
async fn updates_a_stale_branch_with_a_merge_commit_unless_asked_to_rebase() {
    let harness = Harness::new();
    harness.route(vec![(
        "query PullRequestActionState",
        action_state(json!({})),
    )]);
    harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::UpdateBranch))
        .await
        .unwrap();
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            update_method: Some(PullRequestUpdateMethod::Rebase),
            ..action(&harness, PullRequestAction::UpdateBranch)
        })
        .await
        .unwrap();
    // GitHub's own default: a merge commit unless asked to rebase, pinned to the head it read.
    assert_eq!(
        harness.variables_of("updatePullRequestBranch("),
        vec![
            json!({ "pullRequestId": "PR_7", "expectedHeadOid": "abc123", "updateMethod": "MERGE" }),
            json!({ "pullRequestId": "PR_7", "expectedHeadOid": "abc123", "updateMethod": "REBASE" }),
        ]
    );
}

#[tokio::test]
async fn treats_an_already_current_branch_as_updated() {
    let harness = Harness::new();
    harness.route(vec![(
        "query PullRequestActionState",
        action_state(json!({ "baseRef": { "compare": { "behindBy": 0 } } })),
    )]);
    harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::UpdateBranch))
        .await
        .unwrap();
    assert!(harness.variables_of("updatePullRequestBranch(").is_empty());
}

#[tokio::test]
async fn merges_with_the_strategy_it_was_asked_for() {
    let harness = Harness::new();
    harness.route(vec![(
        "query PullRequestActionState",
        action_state(json!({})),
    )]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            merge_method: Some(PullRequestMergeMethod::Squash),
            ..action(&harness, PullRequestAction::Merge)
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("mergePullRequest("),
        vec![json!({ "input": { "pullRequestId": "PR_7", "mergeMethod": "SQUASH" } })]
    );
    assert!(
        harness
            .variables_of("enablePullRequestAutoMerge(")
            .is_empty()
    );
}

async fn removes_agent_credits_for(which: PullRequestAction) {
    let harness = Harness::new();
    harness.route(vec![
        (
            "query PullRequestMergeMessage",
            merge_message(
                "Details\n\nCo-authored-by: Alice <alice@example.com>\nCo-authored-by: Claude <noreply@anthropic.com>",
                false,
            ),
        ),
        ("query PullRequestActionState", action_state(json!({}))),
    ]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            merge_method: Some(PullRequestMergeMethod::Squash),
            remove_agent_credits_on_merge: true,
            ..action(&harness, which)
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("query PullRequestMergeMessage")[0]["method"],
        "SQUASH"
    );
    let needle = if which == PullRequestAction::Merge {
        "mergePullRequest("
    } else {
        "enablePullRequestAutoMerge("
    };
    assert_eq!(
        harness.variables_of(needle),
        vec![json!({ "input": {
            "pullRequestId": "PR_7",
            "mergeMethod": "SQUASH",
            // Pinned to the head the message was read from, so it cannot describe other commits.
            "expectedHeadOid": "abc123",
            "commitBody": "Details\n\nCo-authored-by: Alice <alice@example.com>",
        } })]
    );
}

#[tokio::test]
async fn removes_agent_credits_from_the_proposed_message_for_merge() {
    removes_agent_credits_for(PullRequestAction::Merge).await;
}

#[tokio::test]
async fn removes_agent_credits_from_the_proposed_message_for_enable_auto_merge() {
    removes_agent_credits_for(PullRequestAction::EnableAutoMerge).await;
}

async fn keeps_githubs_default_message(body: &str, queued: bool) {
    let harness = Harness::new();
    harness.route(vec![
        ("query PullRequestMergeMessage", merge_message(body, queued)),
        (
            "query PullRequestActionState",
            action_state(json!({ "isMergeQueueEnabled": queued })),
        ),
    ]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            remove_agent_credits_on_merge: true,
            ..action(&harness, PullRequestAction::Merge)
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("query PullRequestMergeMessage")[0]["method"],
        "MERGE"
    );
    // A merge queue takes the pull request through auto-merge, with its own message.
    let needle = if queued {
        "enablePullRequestAutoMerge("
    } else {
        "mergePullRequest("
    };
    assert_eq!(
        harness.variables_of(needle),
        vec![json!({ "input": { "pullRequestId": "PR_7", "mergeMethod": "MERGE" } })]
    );
}

#[tokio::test]
async fn keeps_githubs_default_message_for_an_unchanged_message() {
    keeps_githubs_default_message(
        "Details\n\nCo-authored-by: Alice <alice@example.com>",
        false,
    )
    .await;
}

#[tokio::test]
async fn keeps_githubs_default_message_for_a_merge_queue() {
    keeps_githubs_default_message("Co-authored-by: Claude <noreply@anthropic.com>", true).await;
}

#[tokio::test]
async fn passes_an_explicitly_empty_body_when_the_proposed_message_only_credits_an_agent() {
    let harness = Harness::new();
    harness.route(vec![
        (
            "query PullRequestMergeMessage",
            merge_message("Co-authored-by: Claude <noreply@anthropic.com>", false),
        ),
        ("query PullRequestActionState", action_state(json!({}))),
    ]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            remove_agent_credits_on_merge: true,
            ..action(&harness, PullRequestAction::Merge)
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("mergePullRequest("),
        vec![json!({ "input": {
            "pullRequestId": "PR_7",
            "mergeMethod": "MERGE",
            "expectedHeadOid": "abc123",
            "commitBody": "",
        } })]
    );
}

#[tokio::test]
async fn does_not_fetch_a_message_for_rebase_merges() {
    let harness = Harness::new();
    harness.route(vec![(
        "query PullRequestActionState",
        action_state(json!({})),
    )]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            merge_method: Some(PullRequestMergeMethod::Rebase),
            remove_agent_credits_on_merge: true,
            ..action(&harness, PullRequestAction::Merge)
        })
        .await
        .unwrap();
    assert!(
        harness
            .variables_of("query PullRequestMergeMessage")
            .is_empty()
    );
    assert_eq!(
        harness.variables_of("mergePullRequest("),
        vec![json!({ "input": { "pullRequestId": "PR_7", "mergeMethod": "REBASE" } })]
    );
}

#[tokio::test]
async fn refuses_to_merge_when_the_proposed_message_cannot_be_read() {
    let harness = Harness::new();
    harness.route(vec![
        (
            "query PullRequestMergeMessage",
            json!({ "data": { "repository": { "pullRequest": null } } }),
        ),
        ("query PullRequestActionState", action_state(json!({}))),
    ]);
    let result = harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            remove_agent_credits_on_merge: true,
            ..action(&harness, PullRequestAction::Merge)
        })
        .await;
    assert!(result.is_err());
    assert!(harness.variables_of("mergePullRequest(").is_empty());
}

#[tokio::test]
async fn arms_auto_merge_with_the_same_strategy_a_merge_would_have_used() {
    let harness = Harness::new();
    harness.route(vec![(
        "query PullRequestActionState",
        action_state(json!({})),
    )]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            merge_method: Some(PullRequestMergeMethod::Squash),
            ..action(&harness, PullRequestAction::EnableAutoMerge)
        })
        .await
        .unwrap();
    // No strategy asked for is GitHub's own default, exactly as it is for a merge now.
    harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::EnableAutoMerge))
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("enablePullRequestAutoMerge("),
        vec![
            json!({ "input": { "pullRequestId": "PR_7", "mergeMethod": "SQUASH" } }),
            json!({ "input": { "pullRequestId": "PR_7", "mergeMethod": "MERGE" } }),
        ]
    );
    assert!(harness.variables_of("mergePullRequest(").is_empty());
}

#[tokio::test]
async fn merges_at_once_when_auto_merge_is_asked_of_a_pull_request_that_is_ready_now() {
    let harness = Harness::new();
    harness.route(vec![(
        "query PullRequestActionState",
        action_state(json!({ "mergeStateStatus": "CLEAN" })),
    )]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            merge_method: Some(PullRequestMergeMethod::Squash),
            ..action(&harness, PullRequestAction::EnableAutoMerge)
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("mergePullRequest("),
        vec![json!({ "input": { "pullRequestId": "PR_7", "mergeMethod": "SQUASH" } })]
    );
    assert!(
        harness
            .variables_of("enablePullRequestAutoMerge(")
            .is_empty()
    );
}

#[tokio::test]
async fn takes_auto_merge_back_off_without_naming_a_strategy() {
    let harness = Harness::new();
    harness.route(vec![(NODE_ID_QUERY, node_id_answer("PR_7"))]);
    harness
        .cli
        .run_pull_request_action(&RunPullRequestActionInput {
            merge_method: Some(PullRequestMergeMethod::Squash),
            ..action(&harness, PullRequestAction::DisableAutoMerge)
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("disablePullRequestAutoMerge("),
        vec![json!({ "pullRequestId": "PR_7" })]
    );
}

#[tokio::test]
async fn opens_a_pull_request_that_reverts_a_merged_pull_request() {
    let harness = Harness::new();
    harness.route(vec![(NODE_ID_QUERY, node_id_answer("PR_7"))]);
    harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::Revert))
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("revertPullRequest("),
        vec![json!({ "pullRequestId": "PR_7" })]
    );
}

#[tokio::test]
async fn does_not_approve_action_required_runs_for_a_same_repository_pull_request() {
    let harness = Harness::new();
    harness.once_body(
        core_response(json!({
            "number": 7,
            "title": "Pull request 7",
            "url": "https://github.com/acme/web/pull/7",
            "headRefName": "feat/page",
            "headRefOid": "abc123",
            "isCrossRepository": false,
            "headRepositoryOwner": { "login": "acme" },
            "baseRefName": "main",
            "createdAt": "2026-07-01T00:00:00Z",
            "updatedAt": "2026-07-02T00:00:00Z",
        }))
        .to_string(),
    );
    harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::ApproveWorkflows))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 1);
}

#[tokio::test]
async fn finds_and_approves_every_workflow_waiting_on_a_maintainer() {
    let harness = Harness::new();
    workflow_approval_routes(
        &harness,
        || cross_repository_detail("abc123"),
        heads(&[7]),
        workflow_runs(&[10, 11]),
    );
    harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::ApproveWorkflows))
        .await
        .unwrap();
    // Open pull requests on this head branch, and runs scoped to this exact head.
    assert_eq!(
        harness.variables_of("headRefName: $head")[0],
        json!({ "owner": "acme", "name": "web", "head": "feat/page", "after": null })
    );
    assert_eq!(
        harness.rest_calls_to("actions/runs?")[0].path,
        "repos/acme/web/actions/runs?head_sha=abc123&branch=feat%2Fpage&event=pull_request&status=action_required&per_page=100&page=1"
    );
    // Each run is approved only after the head is read again and still lists it.
    let approvals: Vec<_> = harness
        .rest_calls_to("/approve")
        .into_iter()
        .map(|call| (call.method, call.path))
        .collect();
    assert_eq!(
        approvals,
        vec![
            (
                RestMethod::Post,
                "repos/acme/web/actions/runs/10/approve".to_owned()
            ),
            (
                RestMethod::Post,
                "repos/acme/web/actions/runs/11/approve".to_owned()
            ),
        ]
    );
    assert_eq!(harness.rest_calls_to("actions/runs?").len(), 3);
}

#[tokio::test]
async fn refuses_a_stale_workflow_approval_after_the_pull_request_head_changes() {
    let harness = Harness::new();
    let detail_reads = std::sync::Arc::new(parking_lot::Mutex::new(0));
    {
        let detail_reads = detail_reads.clone();
        workflow_approval_routes(
            &harness,
            move || {
                let mut reads = detail_reads.lock();
                *reads += 1;
                cross_repository_detail(if *reads == 1 { "abc123" } else { "def456" })
            },
            heads(&[7]),
            workflow_runs(&[10]),
        );
    }
    let error = harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::ApproveWorkflows))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::WorkflowApprovalHeadChanged(changed) => {
            assert_eq!(changed.number, 7)
        }
        other => panic!("expected a head-changed error, got {other:?}"),
    }
    assert_eq!(*detail_reads.lock(), 2);
    assert!(harness.rest_calls_to("/approve").is_empty());
}

#[tokio::test]
async fn refuses_workflow_approval_when_github_omits_the_head_repository() {
    let harness = Harness::new();
    harness.once_body(
        core_response(json!({
            "number": 7,
            "title": "Pull request 7",
            "url": "https://github.com/acme/web/pull/7",
            "headRefName": "feat/page",
            "headRefOid": "abc123",
            "isCrossRepository": true,
            "headRepositoryOwner": null,
            "baseRefName": "main",
            "createdAt": "2026-07-01T00:00:00Z",
            "updatedAt": "2026-07-02T00:00:00Z",
        }))
        .to_string(),
    );
    let error = harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::ApproveWorkflows))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::WorkflowApprovalHeadUnavailable(unavailable) => {
            assert_eq!(unavailable.number, 7)
        }
        other => panic!("expected a head-unavailable error, got {other:?}"),
    }
    assert_eq!(harness.call_count(), 1);
}

#[tokio::test]
async fn returns_a_pull_request_to_draft_by_converting_it() {
    let harness = Harness::new();
    harness.route(vec![(NODE_ID_QUERY, node_id_answer("PR_7"))]);
    harness
        .cli
        .run_pull_request_action(&action(&harness, PullRequestAction::Draft))
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("convertPullRequestToDraft("),
        vec![json!({ "pullRequestId": "PR_7" })]
    );
    assert!(
        harness
            .variables_of("markPullRequestReadyForReview(")
            .is_empty()
    );
}

#[tokio::test]
async fn sends_a_comment_body_as_a_variable_never_inside_the_document() {
    let harness = Harness::new();
    harness.route(vec![(NODE_ID_QUERY, node_id_answer("PR_7"))]);
    harness
        .cli
        .comment_on_pull_request(&CommentOnPullRequestInput {
            target: harness.target(7),
            body: "Looks good.".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("addComment("),
        vec![json!({ "subjectId": "PR_7", "body": "Looks good." })]
    );
    assert!(
        !harness
            .query_at(harness.call_count() - 1)
            .contains("Looks good.")
    );
}

#[tokio::test]
async fn sends_a_whole_review_as_one_request() {
    let harness = Harness::new();
    harness.always(ok("{}"));
    let comment: PullRequestReviewCommentDraft = serde_json::from_value(json!({
        "path": "src/a.ts",
        "position": { "kind": "added", "newLine": 4 },
        "body": "nit",
    }))
    .unwrap();
    harness
        .cli
        .submit_review(&SubmitReviewInput {
            target: harness.target(7),
            verdict: PullRequestReviewVerdict::Approve,
            body: "Looks right.".into(),
            comments: vec![comment],
        })
        .await
        .unwrap();
    // One request, so nothing is on the pull request until the verdict is.
    assert_eq!(harness.call_count(), 1);
    let reviews = harness.rest_calls_to("/reviews");
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].method, RestMethod::Post);
    assert_eq!(reviews[0].path, "repos/acme/web/pulls/7/reviews");
    assert_eq!(
        reviews[0].body,
        Some(json!({
            "event": "APPROVE",
            "body": "Looks right.",
            "comments": [{ "path": "src/a.ts", "line": 4, "side": "RIGHT", "body": "nit" }],
        }))
    );
}

#[tokio::test]
async fn sends_a_reply_body_as_a_variable_never_inside_the_document() {
    let harness = Harness::new();
    harness.always(ok("{}"));
    harness
        .cli
        .reply_to_review_thread(&ReplyToReviewThreadInput {
            cwd: "/w".into(),
            repository: "acme/web".into(),
            host: "github.com".into(),
            thread_id: "PRRT_1".into(),
            body: "Fixed in 42ff8ec.".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("addPullRequestReviewThreadReply("),
        vec![json!({ "threadId": "PRRT_1", "body": "Fixed in 42ff8ec." })]
    );
    assert!(!harness.query_at(0).contains("Fixed in 42ff8ec."));
}

#[tokio::test]
async fn resolves_and_unresolves_through_the_mutation_each_one_needs() {
    let harness = Harness::new();
    harness.always(ok("{}"));
    for resolved in [true, false] {
        harness
            .cli
            .set_review_thread_resolution(&SetReviewThreadResolutionInput {
                cwd: "/w".into(),
                repository: "acme/web".into(),
                host: "github.acme.dev".into(),
                thread_id: "PRRT_1".into(),
                resolved,
            })
            .await
            .unwrap();
    }
    assert!(harness.query_at(0).contains("resolveReviewThread("));
    assert!(!harness.query_at(0).contains("unresolveReviewThread("));
    assert!(harness.query_at(1).contains("unresolveReviewThread("));
    assert_eq!(
        Value::Object(harness.vars_at(0)),
        json!({ "threadId": "PRRT_1" })
    );
    assert_eq!(
        Value::Object(harness.vars_at(1)),
        json!({ "threadId": "PRRT_1" })
    );
    // A GitHub Enterprise thread is resolved on its own host, not on github.com.
    assert_eq!(harness.call_at(0).host(), "github.acme.dev");
    assert_eq!(harness.call_at(1).host(), "github.acme.dev");
}

fn reaction(
    harness: &Harness,
    subject_id: Option<&str>,
    content: PullRequestReactionContent,
    reacted: bool,
) -> SetReactionInput {
    SetReactionInput {
        target: harness.target(7),
        subject_id: subject_id.map(str::to_owned),
        content,
        reacted,
    }
}

#[tokio::test]
async fn confirms_a_given_subject_belongs_to_the_named_pull_request_then_reacts_to_it() {
    let harness = Harness::new();
    harness.route(vec![(
        SUBJECT_SCOPE_QUERY,
        subject_scope("IC_1", "PR_kwDOA"),
    )]);
    harness
        .cli
        .set_reaction(&reaction(
            &harness,
            Some("IC_1"),
            PullRequestReactionContent::Heart,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 2);
    assert_eq!(
        harness.variables_of(SUBJECT_SCOPE_QUERY),
        vec![json!({ "owner": "acme", "name": "web", "number": 7, "subjectId": "IC_1" })]
    );
    assert_eq!(
        harness.variables_of("addReaction("),
        vec![json!({ "subjectId": "IC_1", "content": "HEART" })]
    );
}

#[tokio::test]
async fn refuses_a_given_subject_that_belongs_to_a_different_pull_request() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": {
            "repository": { "pullRequest": { "id": "PR_thisOne" } },
            // A comment on pull request #99 of a different repository, named as though it
            // belonged to #7 here.
            "node": { "id": "IC_99", "pullRequest": { "id": "PR_someOtherOne" } },
        } })
        .to_string(),
    );
    let error = harness
        .cli
        .set_reaction(&reaction(
            &harness,
            Some("IC_99"),
            PullRequestReactionContent::Heart,
            true,
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, GitHubPullRequestApiError::SubjectScope(_)));
    // Refused before any mutation was sent.
    assert_eq!(harness.call_count(), 1);
}

#[tokio::test]
async fn looks_up_the_pull_requests_own_node_id_when_no_subject_was_given() {
    let harness = Harness::new();
    harness.route(vec![(NODE_ID_QUERY, node_id_answer("PR_kwDOA"))]);
    // Its own pull request: a node id looked up once is remembered for the life of the service.
    harness
        .cli
        .set_reaction(&SetReactionInput {
            target: harness.target(21),
            subject_id: None,
            content: PullRequestReactionContent::Rocket,
            reacted: true,
        })
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 2);
    assert_eq!(
        harness.variables_of(NODE_ID_QUERY),
        vec![json!({ "owner": "acme", "name": "web", "number": 21 })]
    );
    assert_eq!(
        harness.variables_of("addReaction("),
        vec![json!({ "subjectId": "PR_kwDOA", "content": "ROCKET" })]
    );
}

#[tokio::test]
async fn takes_a_reaction_back_through_the_remove_mutation() {
    let harness = Harness::new();
    harness.route(vec![(
        SUBJECT_SCOPE_QUERY,
        subject_scope("IC_1", "PR_kwDOA"),
    )]);
    harness
        .cli
        .set_reaction(&reaction(
            &harness,
            Some("IC_1"),
            PullRequestReactionContent::Heart,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(
        harness.variables_of("removeReaction("),
        vec![json!({ "subjectId": "IC_1", "content": "HEART" })]
    );
    assert!(harness.variables_of("addReaction(").is_empty());
}

#[tokio::test]
async fn rewrites_only_the_words_a_request_named() {
    let harness = Harness::new();
    harness.route(vec![(NODE_ID_QUERY, node_id_answer("PR_kwDOA"))]);
    let rewrite = |title: Option<&str>, body: Option<&str>| UpdatePullRequestInput {
        target: harness.target(22),
        title: title.map(str::to_owned),
        body: body.map(str::to_owned),
    };
    harness
        .cli
        .update_pull_request(&rewrite(Some("A better title"), None))
        .await
        .unwrap();
    harness
        .cli
        .update_pull_request(&rewrite(None, Some("A better description.")))
        .await
        .unwrap();
    harness
        .cli
        .update_pull_request(&rewrite(Some("Both"), Some("at once.")))
        .await
        .unwrap();
    // One node id lookup for the pull request, then a mutation per rewrite.
    assert_eq!(harness.variables_of(NODE_ID_QUERY).len(), 1);
    assert_eq!(
        harness.variables_of("updatePullRequest("),
        vec![
            json!({ "pullRequestId": "PR_kwDOA", "title": "A better title" }),
            json!({ "pullRequestId": "PR_kwDOA", "body": "A better description." }),
            json!({ "pullRequestId": "PR_kwDOA", "title": "Both", "body": "at once." }),
        ]
    );
}

#[tokio::test]
async fn rewrites_a_remark_through_the_mutation_its_kind_needs() {
    let harness = Harness::new();
    harness.route(vec![(
        SUBJECT_SCOPE_QUERY,
        subject_scope("IC_1", "PR_kwDOA"),
    )]);
    for kind in [
        UpdateCommentKind::IssueComment,
        UpdateCommentKind::ReviewComment,
    ] {
        harness
            .cli
            .update_comment(&UpdateCommentInput {
                target: harness.target(7),
                comment_id: "IC_1".into(),
                kind,
                body: "Reworded.".into(),
            })
            .await
            .unwrap();
    }
    let subjects: Vec<_> = harness
        .variables_of(SUBJECT_SCOPE_QUERY)
        .into_iter()
        .map(|variables| variables["subjectId"].clone())
        .collect();
    assert_eq!(subjects, vec![json!("IC_1"), json!("IC_1")]);
    assert_eq!(
        harness.variables_of("updateIssueComment("),
        vec![json!({ "commentId": "IC_1", "body": "Reworded." })]
    );
    assert_eq!(
        harness.variables_of("updatePullRequestReviewComment("),
        vec![json!({ "commentId": "IC_1", "body": "Reworded." })]
    );
}

#[tokio::test]
async fn refuses_a_comment_that_belongs_to_a_different_pull_request() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": {
            "repository": { "pullRequest": { "id": "PR_thisOne" } },
            "node": { "id": "IC_99", "pullRequest": { "id": "PR_someOtherOne" } },
        } })
        .to_string(),
    );
    let error = harness
        .cli
        .update_comment(&UpdateCommentInput {
            target: harness.target(7),
            comment_id: "IC_99".into(),
            kind: UpdateCommentKind::IssueComment,
            body: "Reworded.".into(),
        })
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::SubjectScope(scope) => {
            assert_eq!(scope.operation, "updateComment")
        }
        other => panic!("expected a scope error, got {other:?}"),
    }
    // Refused before any mutation was sent.
    assert_eq!(harness.call_count(), 1);
}

#[tokio::test]
async fn asks_github_to_review_naming_the_collection_a_request_is_added_to() {
    let harness = Harness::new();
    harness.always(ok("{}"));
    harness
        .cli
        .set_reviewer_request(&SetReviewerRequestInput {
            target: harness.target(7),
            reviewers: vec![
                ("octocat".into(), PullRequestReviewerKind::User),
                ("reviewers".into(), PullRequestReviewerKind::Team),
            ],
            requested: true,
        })
        .await
        .unwrap();
    let call = harness.call_at(0);
    assert!(call.is_rest());
    let rest = call.rest();
    assert_eq!(rest.method, RestMethod::Post);
    assert_eq!(rest.path, "repos/acme/web/pulls/7/requested_reviewers");
    assert_eq!(
        rest.body,
        Some(json!({ "reviewers": ["octocat"], "team_reviewers": ["reviewers"] }))
    );
}

#[tokio::test]
async fn takes_a_request_back_by_deleting_from_the_same_collection_it_was_added_to() {
    let harness = Harness::new();
    harness.always(ok("{}"));
    harness
        .cli
        .set_reviewer_request(&SetReviewerRequestInput {
            target: harness.target(7),
            reviewers: vec![("octocat".into(), PullRequestReviewerKind::User)],
            requested: false,
        })
        .await
        .unwrap();
    let call = harness.call_at(0);
    let rest = call.rest();
    assert_eq!(rest.method, RestMethod::Delete);
    assert_eq!(rest.path, "repos/acme/web/pulls/7/requested_reviewers");
    assert_eq!(
        rest.body,
        Some(json!({ "reviewers": ["octocat"], "team_reviewers": [] }))
    );
}

#[tokio::test]
async fn puts_labels_on_by_posting_to_the_issues_own_collection_all_at_once() {
    let harness = Harness::new();
    harness.always(ok("[]"));
    harness
        .cli
        .set_labels(&SetLabelsInput {
            target: harness.target(7),
            labels: vec!["bug".into(), "size:XL".into()],
            applied: true,
        })
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 1);
    let call = harness.call_at(0);
    let rest = call.rest();
    assert_eq!(rest.method, RestMethod::Post);
    assert_eq!(rest.path, "repos/acme/web/issues/7/labels");
    assert_eq!(rest.body, Some(json!({ "labels": ["bug", "size:XL"] })));
}

#[tokio::test]
async fn takes_labels_off_one_at_a_time_naming_each_in_the_path_encoded() {
    let harness = Harness::new();
    harness.always(ok("[]"));
    harness
        .cli
        .set_labels(&SetLabelsInput {
            target: harness.target(7),
            labels: vec!["good first issue".into(), "area/web".into()],
            applied: false,
        })
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 2);
    let first = harness.call_at(0);
    assert_eq!(first.rest().method, RestMethod::Delete);
    assert_eq!(
        first.path(),
        "repos/acme/web/issues/7/labels/good%20first%20issue"
    );
    let second = harness.call_at(1);
    assert_eq!(second.rest().method, RestMethod::Delete);
    assert_eq!(second.path(), "repos/acme/web/issues/7/labels/area%2Fweb");
}
