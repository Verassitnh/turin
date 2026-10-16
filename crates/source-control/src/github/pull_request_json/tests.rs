//! Port of upstream/packages/source-control-github/src/server/gitHubPullRequestJson.test.ts
//!
//! Fixtures are the upstream objects as `json!` values. Cases the Rust types cannot express (a
//! non-integer pull request number in `buildPullRequestStackMembershipsGraphQlQuery`) are
//! noted where they would have been.

use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCheckStatus, PullRequestChecksState, PullRequestCommit,
    PullRequestDiffSide, PullRequestFileViewedState, PullRequestLabelCandidate,
    PullRequestMergeCapabilities, PullRequestMergeMethod, PullRequestMergeability,
    PullRequestReaction, PullRequestReactionContent, PullRequestReviewCommentDraft,
    PullRequestReviewDecision, PullRequestReviewPosition, PullRequestReviewVerdict,
    PullRequestReviewerCandidate, PullRequestReviewerKind, PullRequestStackMembership,
    PullRequestState,
};

use super::*;

fn tne(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).unwrap()
}

fn positive(value: u64) -> PositiveInt {
    PositiveInt::new(value).unwrap()
}

fn actor(login: &str, name: Option<&str>, avatar_url: Option<&str>) -> PullRequestActor {
    PullRequestActor {
        is_bot: None,
        login: tne(login),
        name: name.map(str::to_owned),
        avatar_url: avatar_url.map(str::to_owned),
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected an object, got {other}"),
    }
}

/// `{...base, ...overrides}`.
fn merged(base: &Value, overrides: Value) -> Value {
    let mut map = object(base.clone());
    map.extend(object(overrides));
    Value::Object(map)
}

/// Rows as a repository's own GraphQL list answers them. Entries may use flat shapes for brevity:
/// `reviewRequests` and `latestReviews` as arrays, and `checks` as the rollup state.
fn list_json(entries: Vec<Value>) -> String {
    let nodes: Vec<Value> = entries
        .into_iter()
        .map(|entry| {
            let mut entry = object(entry);
            let review_requests = entry.remove("reviewRequests");
            let latest_reviews = entry.remove("latestReviews");
            let checks = entry.remove("checks");
            let mut node = object(json!({
                "number": 1,
                "title": "Add the pull requests page",
                "url": "https://github.com/pingdotgg/t3code/pull/1",
                "headRefName": "feat/page",
                "baseRefName": "main",
                "createdAt": "2026-07-01T00:00:00Z",
                "updatedAt": "2026-07-02T00:00:00Z",
            }));
            if let Some(Value::Array(requests)) = review_requests {
                let nodes: Vec<Value> = requests
                    .into_iter()
                    .map(|requested_reviewer| json!({ "requestedReviewer": requested_reviewer }))
                    .collect();
                node.insert("reviewRequests".into(), json!({ "nodes": nodes }));
            }
            if let Some(latest_reviews) = latest_reviews {
                node.insert("latestReviews".into(), json!({ "nodes": latest_reviews }));
            }
            if let Some(checks) = checks {
                node.insert(
                    "commits".into(),
                    json!({ "nodes": [{ "commit": { "statusCheckRollup": { "state": checks } } }] }),
                );
            }
            node.extend(entry);
            Value::Object(node)
        })
        .collect();
    json!({
        "data": { "repository": { "pullRequests": {
            "pageInfo": { "hasNextPage": false },
            "nodes": nodes,
        } } }
    })
    .to_string()
}

// ---------------------------------------------------------------------------------------------
// pull request list decoding
// ---------------------------------------------------------------------------------------------

#[test]
fn list_treats_a_merge_timestamp_as_merged_even_when_the_state_still_says_closed() {
    let batch = decode_pull_request_list_json(&list_json(vec![
        json!({ "state": "CLOSED", "mergedAt": "2026-07-03T00:00:00Z" }),
    ]))
    .unwrap();
    assert_eq!(batch.items[0].state, PullRequestState::Merged);
}

#[test]
fn list_normalizes_mergeability_and_defaults_unknown_values() {
    let batch = decode_pull_request_list_json(&list_json(vec![
        json!({ "mergeable": "CONFLICTING" }),
        json!({ "mergeable": "SOMETHING_NEW" }),
        json!({}),
    ]))
    .unwrap();
    assert_eq!(
        batch
            .items
            .iter()
            .map(|entry| entry.mergeability)
            .collect::<Vec<_>>(),
        vec![
            PullRequestMergeability::Conflicting,
            PullRequestMergeability::Unknown,
            PullRequestMergeability::Unknown,
        ]
    );
}

#[test]
fn list_keeps_user_review_requests_and_drops_team_ones_which_are_not_logins() {
    let batch = decode_pull_request_list_json(&list_json(vec![
        json!({ "reviewRequests": [{ "login": "octocat" }, { "slug": "web-platform" }] }),
    ]))
    .unwrap();
    assert_eq!(
        batch.items[0].review_request_logins,
        vec!["octocat".to_owned()]
    );
    assert!(batch.items[0].has_team_review_request);
}

#[test]
fn list_normalizes_the_review_decision_and_reports_nothing_for_one_github_does_not_summarize() {
    let batch = decode_pull_request_list_json(&list_json(vec![
        json!({ "reviewDecision": "APPROVED" }),
        json!({ "reviewDecision": "CHANGES_REQUESTED" }),
        json!({ "reviewDecision": "REVIEW_REQUIRED" }),
        json!({ "reviewDecision": null }),
    ]))
    .unwrap();
    assert_eq!(
        batch
            .items
            .iter()
            .map(|entry| entry.review_decision)
            .collect::<Vec<_>>(),
        vec![
            Some(PullRequestReviewDecision::Approved),
            Some(PullRequestReviewDecision::ChangesRequested),
            Some(PullRequestReviewDecision::ReviewRequired),
            None,
        ]
    );
}

#[test]
fn list_takes_the_verdict_from_the_latest_reviews_when_github_summarizes_none_as_for_a_bots_approval()
 {
    let batch = decode_pull_request_list_json(&list_json(vec![
        json!({
            "reviewDecision": null,
            "latestReviews": [{ "author": { "login": "macroscopeapp" }, "state": "APPROVED" }],
        }),
        json!({
            "reviewDecision": "REVIEW_REQUIRED",
            "latestReviews": [
                { "author": { "login": "octocat" }, "state": "APPROVED" },
                { "author": { "login": "hubot" }, "state": "CHANGES_REQUESTED" },
            ],
        }),
        json!({
            "reviewDecision": "APPROVED",
            "latestReviews": [{ "author": { "login": "hubot" }, "state": "CHANGES_REQUESTED" }],
        }),
        json!({
            "reviewDecision": null,
            "latestReviews": [{ "author": { "login": "octocat" }, "state": "COMMENTED" }],
        }),
    ]))
    .unwrap();
    assert_eq!(
        batch
            .items
            .iter()
            .map(|entry| entry.review_decision)
            .collect::<Vec<_>>(),
        vec![
            Some(PullRequestReviewDecision::Approved),
            Some(PullRequestReviewDecision::ChangesRequested),
            Some(PullRequestReviewDecision::Approved),
            None,
        ]
    );
}

#[test]
fn list_rolls_the_head_commits_rollup_up_to_the_one_word_a_row_has_space_for() {
    let batch = decode_pull_request_list_json(&list_json(vec![
        json!({ "checks": "FAILURE" }),
        json!({ "checks": "PENDING" }),
        json!({ "checks": "SUCCESS" }),
        json!({ "checks": "ERROR" }),
        json!({ "checks": "EXPECTED" }),
        json!({}),
    ]))
    .unwrap();
    assert_eq!(
        batch
            .items
            .iter()
            .map(|entry| entry.checks_state)
            .collect::<Vec<_>>(),
        vec![
            Some(PullRequestChecksState::Failing),
            Some(PullRequestChecksState::Pending),
            Some(PullRequestChecksState::Passing),
            Some(PullRequestChecksState::Failing),
            Some(PullRequestChecksState::Pending),
            None,
        ]
    );
}

#[test]
fn list_skips_malformed_entries_but_still_counts_them_so_paging_does_not_stop_early() {
    let batch = decode_pull_request_list_json(&list_json(vec![
        json!({}),
        json!({ "number": "not-a-number" }),
    ]))
    .unwrap();
    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.raw_count, 2);
}

#[test]
fn list_reports_the_next_cursor_only_while_github_says_there_is_a_next_page() {
    let page = |has_next: bool| {
        json!({ "data": { "repository": { "pullRequests": {
            "pageInfo": { "hasNextPage": has_next, "endCursor": " cursor-1 " },
            "nodes": [],
        } } } })
        .to_string()
    };
    assert_eq!(
        decode_pull_request_list_json(&page(true))
            .unwrap()
            .end_cursor
            .as_deref(),
        Some("cursor-1")
    );
    assert_eq!(
        decode_pull_request_list_json(&page(false))
            .unwrap()
            .end_cursor,
        None
    );
    // A repository the viewer cannot see answers as an empty page.
    let empty = decode_pull_request_list_json(r#"{"data":{"repository":null}}"#).unwrap();
    assert_eq!(
        (empty.items.len(), empty.raw_count, empty.end_cursor),
        (0, 0, None)
    );
}

// ---------------------------------------------------------------------------------------------
// pull request search decoding
// ---------------------------------------------------------------------------------------------

fn search_json(rollup_states: &[Option<&str>]) -> Value {
    let nodes: Vec<Value> = rollup_states
        .iter()
        .enumerate()
        .map(|(index, state)| {
            json!({
                "number": index + 1,
                "title": "Add the pull requests page",
                "url": "https://github.com/pingdotgg/t3code/pull/1",
                "headRefName": "feat/page",
                "baseRefName": "main",
                "createdAt": "2026-07-01T00:00:00Z",
                "updatedAt": "2026-07-02T00:00:00Z",
                "repository": { "nameWithOwner": "pingdotgg/t3code" },
                "commits": {
                    "nodes": [{ "commit": { "statusCheckRollup": state.map(|state| json!({ "state": state })) } }],
                },
            })
        })
        .collect();
    json!({ "data": { "search": { "pageInfo": { "hasNextPage": false }, "nodes": nodes } } })
}

#[test]
fn search_keeps_stack_membership_beside_search_results_without_extra_per_pr_reads() {
    let mut raw = search_json(&[Some("SUCCESS"), None]);
    raw["data"]["search"]["nodes"][0]["stack"] =
        json!({ "number": 3, "size": 2, "baseRefName": "main" });
    raw["data"]["search"]["nodes"][0]["stackEntry"] = json!({ "position": 1 });
    let batch = decode_pull_request_search_json(&raw.to_string()).unwrap();
    assert_eq!(
        batch.items[0].stack,
        Some(PullRequestStackMembership {
            number: positive(3),
            size: positive(2),
            position: positive(1),
            base: tne("main"),
        })
    );
    assert_eq!(batch.items[1].stack, None);
    assert!(pull_request_search_graphql_query(20, true, false).contains("stackEntry"));
    assert!(!pull_request_search_graphql_query(20, false, false).contains("stackEntry"));
}

#[test]
fn search_maps_the_rollup_enum_the_search_answers_with_onto_the_same_three_words() {
    // The search asks GitHub for the verdict rather than the checks behind it, so this path sees
    // one enum where the listing sees an array.
    let batch = decode_pull_request_search_json(
        &search_json(&[
            Some("SUCCESS"),
            Some("FAILURE"),
            Some("ERROR"),
            Some("PENDING"),
            Some("EXPECTED"),
            None,
        ])
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        batch
            .items
            .iter()
            .map(|entry| entry.checks_state)
            .collect::<Vec<_>>(),
        vec![
            Some(PullRequestChecksState::Passing),
            Some(PullRequestChecksState::Failing),
            Some(PullRequestChecksState::Failing),
            Some(PullRequestChecksState::Pending),
            Some(PullRequestChecksState::Pending),
            None,
        ]
    );
    assert_eq!(batch.items[0].repository, "pingdotgg/t3code");
    assert!(!batch.has_next_page);
}

#[test]
fn search_skips_rows_naming_no_repository_but_counts_them() {
    let mut raw = search_json(&[Some("SUCCESS"), Some("SUCCESS")]);
    raw["data"]["search"]["nodes"][1] = json!({});
    raw["data"]["search"]["pageInfo"] = json!({ "hasNextPage": true, "endCursor": "next" });
    let batch = decode_pull_request_search_json(&raw.to_string()).unwrap();
    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.raw_count, 2);
    assert!(batch.has_next_page);
    assert_eq!(batch.end_cursor.as_deref(), Some("next"));
}

// ---------------------------------------------------------------------------------------------
// pull request detail decoding
// ---------------------------------------------------------------------------------------------

fn detail_json() -> Value {
    json!({
        "number": 7,
        "title": "Detail",
        "url": "https://github.com/pingdotgg/t3code/pull/7",
        "headRefName": "feat/detail",
        "baseRefName": "main",
        "createdAt": "2026-07-01T00:00:00Z",
        "updatedAt": "2026-07-05T00:00:00Z",
        "body": "Body",
        "statusCheckRollup": [
            { "__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS" },
            { "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE" },
            { "__typename": "StatusContext", "context": "ci/legacy", "state": "SUCCESS" },
        ],
        "comments": [{ "id": "c1", "body": "second", "createdAt": "2026-07-04T00:00:00Z" }],
        "reviews": [
            { "id": "r1", "body": "first", "state": "CHANGES_REQUESTED", "submittedAt": "2026-07-03T00:00:00Z" },
            { "id": "r2", "body": "   ", "state": "APPROVED", "submittedAt": "2026-07-06T00:00:00Z" },
        ],
        "commits": [
            {
                "oid": "abc1234",
                "messageHeadline": "Ship the timeline",
                "committedDate": "2026-07-05T00:00:00Z",
                "authors": [
                    { "login": "octocat", "name": "Octo Cat", "email": "octo@example.com" },
                    { "name": "Pair Author", "email": "pair@example.com" },
                ],
            },
        ],
    })
}

/// The same conversation as the GraphQL activity query answers it.
fn activity_json(raw: &Value) -> String {
    let empty = Vec::new();
    let commits: Vec<Value> = raw["commits"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|commit| {
            let mut commit = object(commit.clone());
            let authors = commit.remove("authors").unwrap_or_else(|| json!([]));
            let nodes: Vec<Value> = authors
                .as_array()
                .unwrap()
                .iter()
                .map(|author| {
                    let mut author = object(author.clone());
                    let login = author.remove("login");
                    if let Some(login) = login {
                        author.insert("user".into(), json!({ "login": login }));
                    }
                    Value::Object(author)
                })
                .collect();
            commit.insert("authors".into(), json!({ "nodes": nodes }));
            json!({ "commit": commit })
        })
        .collect();
    json!({
        "data": { "repository": { "pullRequest": {
            "comments": { "nodes": raw.get("comments").cloned().unwrap_or(json!([])) },
            "reviews": { "nodes": raw.get("reviews").cloned().unwrap_or(json!([])) },
            "commits": { "nodes": commits },
        } } }
    })
    .to_string()
}

struct Activity {
    comments: Vec<turin_contracts::pull_request::PullRequestComment>,
    commits: Vec<PullRequestCommit>,
}

fn activity(raw: &Value) -> Activity {
    let page = decode_pull_request_activity_json(&activity_json(raw)).unwrap();
    let mut comments = page.remarks;
    comments.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Activity {
        comments,
        commits: page.commits.unwrap_or_default(),
    }
}

fn decode_detail(raw: &Value) -> GitHubPullRequestDetail {
    decode_pull_request_detail_json(&raw.to_string()).unwrap()
}

fn check_pairs(detail: &GitHubPullRequestDetail) -> Vec<(String, PullRequestCheckStatus)> {
    detail
        .checks
        .iter()
        .map(|check| (check.name.as_str().to_owned(), check.status))
        .collect()
}

#[test]
fn detail_maps_check_run_status_and_commit_status_state_onto_one_vocabulary() {
    let detail = decode_detail(&detail_json());
    assert_eq!(
        check_pairs(&detail),
        vec![
            ("build".to_owned(), PullRequestCheckStatus::Pending),
            ("test".to_owned(), PullRequestCheckStatus::Failure),
            ("ci/legacy".to_owned(), PullRequestCheckStatus::Success),
        ]
    );
}

#[test]
fn detail_keeps_what_branch_protection_requires_and_asks_for_it_on_github_com_only() {
    let detail = decode_detail(&merged(
        &detail_json(),
        json!({ "statusCheckRollup": [
            { "__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "isRequired": true },
            { "__typename": "StatusContext", "context": "bot", "state": "PENDING", "isRequired": false },
            { "__typename": "StatusContext", "context": "legacy", "state": "SUCCESS" },
        ] }),
    ));
    assert_eq!(
        detail
            .checks
            .iter()
            .map(|check| check.required)
            .collect::<Vec<_>>(),
        vec![Some(true), Some(false), None]
    );
    assert!(pull_request_core_graphql_query("github.com").contains("isRequired"));
    assert!(!pull_request_core_graphql_query("github.example.com").contains("isRequired"));
}

#[test]
fn detail_keeps_a_workflow_waiting_for_approval_out_of_the_passing_state() {
    let detail = decode_detail(&merged(
        &detail_json(),
        json!({ "statusCheckRollup": [
            { "__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS" },
            { "__typename": "CheckRun", "name": "contributor tests", "status": "COMPLETED", "conclusion": "ACTION_REQUIRED" },
        ] }),
    ));
    assert_eq!(
        detail
            .checks
            .iter()
            .map(|check| check.status)
            .collect::<Vec<_>>(),
        vec![
            PullRequestCheckStatus::Success,
            PullRequestCheckStatus::ActionRequired
        ]
    );
    assert_eq!(detail.checks_state, Some(PullRequestChecksState::Pending));
}

#[test]
fn workflow_runs_that_can_be_approved_are_decoded() {
    let page = decode_workflow_runs_json(
        &json!({ "workflow_runs": [
            { "id": 10, "name": "contributor tests", "html_url": "https://example.com/10" },
            { "id": 11, "name": null, "html_url": null },
        ] })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        page.runs,
        vec![
            GitHubWorkflowRunApproval {
                id: 10,
                name: "contributor tests".to_owned(),
                url: Some("https://example.com/10".to_owned()),
            },
            GitHubWorkflowRunApproval {
                id: 11,
                name: "Workflow run 11".to_owned(),
                url: None,
            },
        ]
    );
    assert_eq!(page.raw_count, 2);
}

#[test]
fn detail_reads_an_auto_merge_request_and_strategy_its_null_as_off_and_its_absence_as_neither() {
    let armed = |entry: Value| decode_detail(&merged(&detail_json(), entry));

    let on = armed(
        json!({ "autoMergeRequest": { "enabledBy": { "login": "octocat" }, "mergeMethod": "SQUASH" } }),
    );
    assert_eq!(on.auto_merge_enabled, Some(true));
    assert_eq!(on.auto_merge_method, Some(PullRequestMergeMethod::Squash));
    assert_eq!(
        armed(json!({ "autoMergeRequest": null })).auto_merge_enabled,
        Some(false)
    );
    // `gh` not answering for the field at all is not GitHub saying the merge is unarmed.
    assert_eq!(armed(json!({})).auto_merge_enabled, None);
}

#[test]
fn detail_shows_a_re_running_check_once_as_the_run_that_is_happening_now() {
    // What `statusCheckRollup` reports while a workflow is being re-run: the same check twice,
    // the finished run and the one that replaced it, with no id to tell them apart.
    let detail = decode_detail(&merged(
        &detail_json(),
        json!({ "statusCheckRollup": [
            {
                "__typename": "CheckRun",
                "name": "Prepare PR size config",
                "workflowName": "PR Size",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-08-11T16:06:20Z",
                "completedAt": "2026-08-11T16:06:25Z",
            },
            {
                "__typename": "CheckRun",
                "name": "Prepare PR size config",
                "workflowName": "PR Size",
                "status": "IN_PROGRESS",
                "conclusion": "",
                "startedAt": "2026-08-11T17:01:04Z",
                "completedAt": "0001-01-01T00:00:00Z",
            },
        ] }),
    ));
    assert_eq!(
        check_pairs(&detail),
        vec![(
            "Prepare PR size config".to_owned(),
            PullRequestCheckStatus::Pending
        )]
    );
    assert_eq!(detail.checks_state, Some(PullRequestChecksState::Pending));
}

#[test]
fn detail_keeps_a_check_pending_while_another_run_of_it_on_the_same_commit_is_still_going() {
    // A workflow on both `push` and `pull_request` runs the same check twice on one commit. The
    // `pull_request` run finished after the `push` run started, and the `push` run is still going.
    let finished = json!({
        "__typename": "CheckRun",
        "name": "check",
        "workflowName": "Checks",
        "status": "COMPLETED",
        "conclusion": "SUCCESS",
        "startedAt": "2026-10-08T04:06:10Z",
        "completedAt": "2026-10-08T04:18:58Z",
    });
    let running = merged(
        &finished,
        json!({
            "status": "IN_PROGRESS",
            "conclusion": "",
            "startedAt": "2026-10-08T04:06:13Z",
            "completedAt": "0001-01-01T00:00:00Z",
        }),
    );
    // A run still queued has not started either, so it carries no time at all.
    let queued = merged(
        &running,
        json!({ "status": "QUEUED", "startedAt": "0001-01-01T00:00:00Z" }),
    );
    // Both listing orders, so the run still going wins on its own rather than by coming last.
    for rollup in [
        vec![finished.clone(), running.clone()],
        vec![running.clone(), finished.clone()],
        vec![finished.clone(), queued.clone()],
        vec![queued.clone(), finished.clone()],
    ] {
        let detail = decode_detail(&merged(
            &detail_json(),
            json!({ "statusCheckRollup": rollup }),
        ));
        assert_eq!(
            check_pairs(&detail),
            vec![("check".to_owned(), PullRequestCheckStatus::Pending)]
        );
        assert_eq!(detail.checks_state, Some(PullRequestChecksState::Pending));
    }
}

#[test]
fn activity_merges_reviews_with_comments_in_time_order_and_keeps_a_bodyless_approval() {
    let detail = activity(&detail_json());
    // r2 approved without writing anything, which is still the event worth seeing.
    assert_eq!(
        detail
            .comments
            .iter()
            .map(|comment| comment.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r1", "c1", "r2"]
    );
    assert_eq!(
        detail.comments.last().unwrap().review_state.as_deref(),
        Some("APPROVED")
    );
}

#[test]
fn activity_keeps_every_attributed_commit_author_including_an_unlinked_signature() {
    let detail = activity(&detail_json());
    assert_eq!(
        detail.commits[0].authors,
        Some(vec![
            actor("octocat", Some("Octo Cat"), None),
            actor("Pair Author", Some("Pair Author"), None),
        ])
    );
}

#[test]
fn activity_drops_the_bodyless_review_github_opens_to_hold_line_comments() {
    let detail = activity(&merged(
        &detail_json(),
        json!({ "reviews": [
            // What a reviewer leaving inline comments produces: a container with a state but
            // nothing to read. Its comments come from the review threads instead.
            { "id": "r4", "body": "", "state": "COMMENTED", "submittedAt": "2026-07-07T00:00:00Z" },
            { "id": "r5", "body": "Looks good.", "state": "COMMENTED", "submittedAt": "2026-07-08T00:00:00Z" },
        ] }),
    ));
    assert_eq!(
        detail
            .comments
            .iter()
            .map(|comment| comment.id.as_str())
            .collect::<Vec<_>>(),
        vec!["c1", "r5"]
    );
}

#[test]
fn activity_keeps_a_bodyless_verdict_review_which_is_the_event_itself() {
    for state in ["APPROVED", "CHANGES_REQUESTED", "DISMISSED"] {
        let detail = activity(&merged(
            &detail_json(),
            json!({ "reviews": [
                { "id": "r6", "body": "", "state": state, "submittedAt": "2026-07-07T00:00:00Z" },
            ] }),
        ));
        assert!(
            detail
                .comments
                .iter()
                .any(|comment| comment.id.as_str() == "r6"),
            "{state}"
        );
    }
}

#[test]
fn activity_drops_a_review_that_carries_neither_a_body_nor_a_state() {
    let detail = activity(&merged(
        &detail_json(),
        json!({ "reviews": [{ "id": "r3", "body": "  ", "submittedAt": "2026-07-07T00:00:00Z" }] }),
    ));
    assert_eq!(
        detail
            .comments
            .iter()
            .map(|comment| comment.id.as_str())
            .collect::<Vec<_>>(),
        vec!["c1"]
    );
}

// ---------------------------------------------------------------------------------------------
// review thread decoding (reviewers, commits, cursors)
// ---------------------------------------------------------------------------------------------

fn threads_json_with_page(nodes: Value, total_count: u64, page_info: Value) -> String {
    json!({
        "data": { "repository": { "pullRequest": { "reviewThreads": {
            "totalCount": total_count,
            "pageInfo": page_info,
            "nodes": nodes,
        } } } }
    })
    .to_string()
}

fn threads_json(nodes: Value) -> String {
    let total = nodes.as_array().map_or(0, |nodes| nodes.len() as u64);
    threads_json_with_page(
        nodes,
        total,
        json!({ "hasNextPage": false, "endCursor": null }),
    )
}

/// The same query carries the review roster, so it is built alongside the threads.
fn review_json(requested: Vec<Value>, reviewed: Vec<Value>) -> String {
    json!({
        "data": { "repository": { "pullRequest": {
            "reviewThreads": { "totalCount": 0, "nodes": [] },
            "reviewRequests": {
                "nodes": requested.into_iter().map(|r| json!({ "requestedReviewer": r })).collect::<Vec<_>>(),
            },
            "latestReviews": {
                "nodes": reviewed.into_iter().map(|a| json!({ "author": a })).collect::<Vec<_>>(),
            },
        } } }
    })
    .to_string()
}

#[test]
fn threads_keep_a_reviewer_who_has_already_reviewed_app_or_person_with_their_avatar() {
    let result = decode_review_threads_json(&review_json(
        vec![json!({ "login": "julius", "name": "Julius", "avatarUrl": "https://avatars/j.png" })],
        // An app that has reviewed is no longer an outstanding request, which is why asking only
        // for requests reported nobody on a pull request a bot had reviewed.
        vec![json!({
            "__typename": "Bot",
            "login": "macroscopeapp",
            "avatarUrl": "https://avatars/in/900172.png",
        })],
    ))
    .unwrap();

    assert_eq!(
        result.bot_logins.iter().cloned().collect::<Vec<_>>(),
        vec!["macroscopeapp".to_owned()]
    );
    assert_eq!(
        result.reviewers,
        vec![
            actor("julius", Some("Julius"), Some("https://avatars/j.png")),
            PullRequestActor {
                is_bot: Some(true),
                login: tne("macroscopeapp"),
                name: None,
                avatar_url: Some("https://avatars/in/900172.png".to_owned()),
            },
        ]
    );
}

#[test]
fn threads_carry_per_commit_line_counts_from_the_pull_request_connection() {
    let result = decode_review_threads_json(
        &json!({ "data": { "repository": { "pullRequest": {
            "reviewThreads": { "totalCount": 0, "nodes": [] },
            "commits": { "nodes": [
                { "commit": { "oid": "abc123", "additions": 18, "deletions": 7 } },
                { "commit": { "oid": "def456", "additions": 3, "deletions": 0 } },
            ] },
        } } } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        result.commit_stats.into_iter().collect::<Vec<_>>(),
        vec![
            (
                "abc123".to_owned(),
                GitHubDiffCounts {
                    additions: 18,
                    deletions: 7
                }
            ),
            (
                "def456".to_owned(),
                GitHubDiffCounts {
                    additions: 3,
                    deletions: 0
                }
            ),
        ]
    );
}

#[test]
fn threads_omit_misleading_line_counts_from_merge_commits() {
    let result = decode_review_threads_json(
        &json!({ "data": { "repository": { "pullRequest": {
            "reviewThreads": { "totalCount": 0, "nodes": [] },
            "commits": { "nodes": [{ "commit": {
                "oid": "merge123",
                "additions": 36_858,
                "deletions": 12_928,
                "parents": { "totalCount": 2 },
            } }] },
        } } } })
        .to_string(),
    )
    .unwrap();
    assert!(result.commit_stats.is_empty());
}

#[test]
fn threads_decode_the_newest_commits_off_the_same_connection_oldest_to_newest() {
    let result = decode_review_threads_json(
        &json!({ "data": { "repository": { "pullRequest": {
            "reviewThreads": { "totalCount": 0, "nodes": [] },
            "commits": { "nodes": [
                { "commit": {
                    "oid": "abc123",
                    "messageHeadline": "Ship the timeline",
                    "committedDate": "2026-07-05T00:00:00Z",
                    "additions": 18,
                    "deletions": 7,
                    "authors": { "nodes": [{ "name": "Julius", "user": { "login": "julius" } }] },
                } },
                { "commit": {
                    "oid": "def456",
                    "messageHeadline": "Fix the flaky test",
                    "committedDate": "2026-07-06T00:00:00Z",
                } },
            ] },
        } } } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        result.commits,
        vec![
            PullRequestCommit {
                oid: tne("abc123"),
                message_headline: "Ship the timeline".to_owned(),
                committed_date: "2026-07-05T00:00:00Z".to_owned(),
                additions: None,
                deletions: None,
                authors: Some(vec![actor("julius", Some("Julius"), None)]),
            },
            PullRequestCommit {
                oid: tne("def456"),
                message_headline: "Fix the flaky test".to_owned(),
                committed_date: "2026-07-06T00:00:00Z".to_owned(),
                additions: None,
                deletions: None,
                authors: Some(vec![]),
            },
        ]
    );
}

#[test]
fn threads_list_someone_who_was_asked_and_then_answered_only_once() {
    let result = decode_review_threads_json(&review_json(
        vec![json!({ "login": "julius", "avatarUrl": "https://avatars/j.png" })],
        vec![json!({ "login": "julius", "avatarUrl": "https://avatars/j.png" })],
    ))
    .unwrap();
    assert_eq!(result.reviewers.len(), 1);
}

#[test]
fn threads_list_a_requested_team_by_org_slug_apart_from_a_user_who_shares_its_slug() {
    let result = decode_review_threads_json(&review_json(
        // Recorded from pingdotgg/t3code#12329, where CODEOWNERS asked the team.
        vec![json!({
            "combinedSlug": "pingdotgg/ping-devs",
            "name": "Ping Devs",
            "avatarUrl": "https://avatars.githubusercontent.com/t/6692390?s=400&v=4",
        })],
        vec![json!({ "__typename": "User", "login": "ping-devs", "avatarUrl": "https://avatars/u.png" })],
    ))
    .unwrap();

    assert_eq!(
        result.reviewers,
        vec![
            actor(
                "pingdotgg/ping-devs",
                Some("Ping Devs"),
                Some("https://avatars.githubusercontent.com/t/6692390?s=400&v=4"),
            ),
            actor("ping-devs", None, Some("https://avatars/u.png")),
        ]
    );
}

#[test]
fn threads_skip_a_request_whose_reviewer_github_does_not_show() {
    let result = decode_review_threads_json(&review_json(vec![Value::Null], vec![])).unwrap();
    assert_eq!(result.reviewers, vec![]);
}

#[test]
fn threads_keep_the_conversation_when_a_reviewer_has_neither_a_login_nor_a_team_slug() {
    // GraphQL answers with an empty object for a union member the query has no fragment for, such
    // as a mannequin. Failing on it would take the whole response down, comments included.
    let result = decode_review_threads_json(&review_json(
        vec![
            json!({}),
            json!({ "login": "julius", "avatarUrl": "https://avatars/j.png" }),
        ],
        vec![],
    ))
    .unwrap();
    assert_eq!(
        result.reviewers,
        vec![actor("julius", None, Some("https://avatars/j.png"))]
    );
}

#[test]
fn threads_carry_a_resolved_thread_into_the_conversation_which_was_still_said() {
    let result = decode_review_threads_json(&threads_json(json!([
        {
            "id": "PRRT_a",
            "isResolved": false,
            "path": "apps/server/src/ws.ts",
            "comments": { "nodes": [{ "id": "t1", "body": "fix this", "createdAt": "2026-07-01T00:00:00Z" }] },
        },
        {
            "id": "PRRT_b",
            "isResolved": true,
            "path": "apps/web/src/main.tsx",
            "comments": { "nodes": [{ "id": "t2", "body": "done", "createdAt": "2026-07-01T00:00:00Z" }] },
        },
    ])))
    .unwrap();
    let threads: Vec<_> = result
        .threads
        .iter()
        .map(|entry| entry.thread.clone())
        .collect();
    let comments = review_thread_conversation(&threads);
    assert_eq!(
        comments
            .iter()
            .map(|comment| comment.id.as_str())
            .collect::<Vec<_>>(),
        vec!["t1", "t2"]
    );
    assert_eq!(comments[0].id.as_str(), "t1");
    assert_eq!(
        comments[0].kind,
        turin_contracts::pull_request::PullRequestCommentKind::ReviewComment
    );
    assert_eq!(comments[0].path.as_deref(), Some("apps/server/src/ws.ts"));
}

#[test]
fn threads_carry_every_reply_not_only_the_remark_each_thread_opened_with() {
    let result = decode_review_threads_json(&threads_json(json!([{
        "id": "PRRT_c",
        "isResolved": false,
        "path": "apps/server/src/ws.ts",
        "comments": { "nodes": [
            { "id": "t1", "body": "fix this", "createdAt": "2026-07-01T00:00:00Z" },
            { "id": "t2", "body": "fixed", "createdAt": "2026-07-01T01:00:00Z" },
        ] },
    }])))
    .unwrap();
    let threads: Vec<_> = result
        .threads
        .iter()
        .map(|entry| entry.thread.clone())
        .collect();
    assert_eq!(
        review_thread_conversation(&threads)
            .iter()
            .map(|comment| comment.id.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["t1", "t2"]
    );
}

#[test]
fn threads_hand_back_the_cursor_the_next_page_of_threads_carries_on_from() {
    let result = decode_review_threads_json(&threads_json_with_page(
        json!([{
            "id": "PRRT_d",
            "path": "apps/server/src/ws.ts",
            "isResolved": false,
            "comments": { "nodes": [{ "id": "t1", "createdAt": "2026-07-01T00:00:00Z" }] },
        }]),
        80,
        json!({ "hasNextPage": true, "endCursor": "Y3Vyc29yOjE" }),
    ))
    .unwrap();
    assert_eq!(result.next_cursor.as_deref(), Some("Y3Vyc29yOjE"));
}

#[test]
fn threads_keep_githubs_own_count_of_a_thread_whose_comments_were_not_all_read() {
    let result = decode_review_threads_json(&threads_json(json!([{
        "id": "PRRT_e",
        "path": "apps/server/src/ws.ts",
        "isResolved": false,
        "comments": {
            "totalCount": 140,
            "pageInfo": { "hasNextPage": true, "endCursor": "Y3Vyc29yOjI" },
            "nodes": [{ "id": "t1", "createdAt": "2026-07-01T00:00:00Z" }],
        },
    }])))
    .unwrap();
    assert_eq!(result.threads[0].comment_count, 140);
    assert_eq!(
        result.threads[0].next_comment_cursor.as_deref(),
        Some("Y3Vyc29yOjI")
    );
}

#[test]
fn thread_comments_end_a_threads_walk_on_the_last_page_which_still_names_a_cursor() {
    let decoded = decode_review_thread_comments_json(
        &json!({ "data": {
            "repository": { "pullRequest": { "id": "PR_1" } },
            "node": {
                "pullRequest": { "id": "PR_1" },
                "comments": {
                    "pageInfo": { "hasNextPage": false, "endCursor": "Y3Vyc29yOjk" },
                    "nodes": [{ "id": "t9", "body": "last", "createdAt": "2026-07-01T00:00:00Z" }],
                },
            },
        } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        decoded
            .comments
            .iter()
            .map(|comment| comment.id.as_str())
            .collect::<Vec<_>>(),
        vec!["t9"]
    );
    assert_eq!(decoded.next_cursor, None);
    assert!(decoded.belongs_to_pull_request);
}

#[test]
fn thread_comments_refuse_a_thread_of_another_pull_request_or_of_no_pull_request() {
    let other = decode_review_thread_comments_json(
        &json!({ "data": {
            "repository": { "pullRequest": { "id": "PR_1" } },
            "node": { "pullRequest": { "id": "PR_2" }, "comments": { "nodes": [] } },
        } })
        .to_string(),
    )
    .unwrap();
    assert!(!other.belongs_to_pull_request);
    let missing = decode_review_thread_comments_json(
        &json!({ "data": { "repository": null, "node": null } }).to_string(),
    )
    .unwrap();
    assert!(!missing.belongs_to_pull_request);
    assert!(missing.comments.is_empty());
}

// ---------------------------------------------------------------------------------------------
// reaction decoding
// ---------------------------------------------------------------------------------------------

fn comment_with_groups(reaction_groups: Value) -> String {
    json!({ "data": {
        "repository": { "pullRequest": { "id": "PR_1" } },
        "node": {
            "pullRequest": { "id": "PR_1" },
            "comments": {
                "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": [{
                    "id": "t1",
                    "body": "nice",
                    "createdAt": "2026-07-01T00:00:00Z",
                    "reactionGroups": reaction_groups,
                }],
            },
        },
    } })
    .to_string()
}

fn reaction(
    content: PullRequestReactionContent,
    count: u64,
    actors: &[&str],
    viewer_has_reacted: bool,
) -> PullRequestReaction {
    PullRequestReaction {
        content,
        count: positive(count),
        actors: actors.iter().map(|actor| tne(actor)).collect(),
        viewer_has_reacted,
    }
}

#[test]
fn reactions_keep_a_named_group_widen_one_cut_short_and_drop_an_unknown_content_and_an_empty_group()
{
    let decoded = decode_review_thread_comments_json(&comment_with_groups(json!([
        {
            "content": "THUMBS_UP",
            "viewerHasReacted": true,
            "reactors": { "totalCount": 2, "nodes": [{ "login": "julius" }, { "login": "bilal" }] },
        },
        // Not one of the eight the contract carries.
        { "content": "PARTY_PARROT", "reactors": { "totalCount": 1, "nodes": [{ "login": "hubot" }] } },
        // Nobody behind it, which GitHub still answers a group for.
        { "content": "HEART", "reactors": { "totalCount": 0, "nodes": [] } },
        // More reactors than the bounded read named, and no `viewerHasReacted` at all.
        {
            "content": "ROCKET",
            "reactors": { "totalCount": 140, "nodes": [{ "login": "a" }, { "login": "b" }, { "login": "c" }] },
        },
    ])))
    .unwrap();

    assert_eq!(
        decoded.comments[0].reactions,
        Some(vec![
            reaction(
                PullRequestReactionContent::ThumbsUp,
                2,
                &["julius", "bilal"],
                true
            ),
            reaction(
                PullRequestReactionContent::Rocket,
                140,
                &["a", "b", "c"],
                false
            ),
        ])
    );
}

#[test]
fn reactions_leave_the_viewers_own_login_out_of_actors_matched_case_insensitively_while_count_still_counts_them()
 {
    let decoded = decode_review_thread_comments_json(
        &json!({ "data": {
            "viewer": { "login": "Bilal" },
            "repository": { "pullRequest": { "id": "PR_1" } },
            "node": {
                "pullRequest": { "id": "PR_1" },
                "comments": {
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": [{
                        "id": "t1",
                        "body": "nice",
                        "createdAt": "2026-07-01T00:00:00Z",
                        "reactionGroups": [{
                            "content": "HEART",
                            "viewerHasReacted": true,
                            "reactors": { "totalCount": 2, "nodes": [{ "login": "bilal" }, { "login": "julius" }] },
                        }],
                    }],
                },
            },
        } })
        .to_string(),
    )
    .unwrap();

    assert_eq!(
        decoded.comments[0].reactions,
        Some(vec![reaction(
            PullRequestReactionContent::Heart,
            2,
            &["julius"],
            true
        )])
    );
}

#[test]
fn reaction_names_round_trip_through_githubs_spelling() {
    use PullRequestReactionContent::*;
    for (content, name) in [
        (ThumbsUp, "THUMBS_UP"),
        (ThumbsDown, "THUMBS_DOWN"),
        (Laugh, "LAUGH"),
        (Hooray, "HOORAY"),
        (Confused, "CONFUSED"),
        (Heart, "HEART"),
        (Rocket, "ROCKET"),
        (Eyes, "EYES"),
    ] {
        assert_eq!(github_reaction_content(content), name);
    }
}

// ---------------------------------------------------------------------------------------------
// repository access / viewer permission decoding
// ---------------------------------------------------------------------------------------------

fn repository_json(viewer_permission: Option<Value>) -> String {
    let mut repository = object(json!({
        "pullRequest": null,
        "mergeCommitAllowed": true,
        "squashMergeAllowed": false,
        "rebaseMergeAllowed": true,
    }));
    if let Some(permission) = viewer_permission {
        repository.insert("viewerPermission".into(), permission);
    }
    json!({ "data": { "repository": repository } }).to_string()
}

#[test]
fn access_reads_merge_settings_with_viewer_permissions() {
    assert_eq!(
        decode_viewer_permissions_json(&repository_json(Some(json!("ADMIN"))))
            .unwrap()
            .merge_capabilities,
        PullRequestMergeCapabilities {
            merge: true,
            squash: false,
            rebase: true
        }
    );
}

#[test]
fn access_fails_rather_than_defaulting_open_when_a_setting_is_missing() {
    let decoded = decode_viewer_permissions_json(
        &json!({ "data": { "repository": { "pullRequest": null, "mergeCommitAllowed": true } } })
            .to_string(),
    );
    assert!(decoded.is_err());
}

#[test]
fn access_counts_the_roles_that_can_push_as_write_and_the_ones_that_cannot_as_read() {
    for permission in ["ADMIN", "MAINTAIN", "WRITE"] {
        assert!(
            decode_viewer_permissions_json(&repository_json(Some(json!(permission))))
                .unwrap()
                .can_write,
            "{permission}"
        );
    }
    for permission in ["TRIAGE", "READ", "NONE"] {
        assert!(
            !decode_viewer_permissions_json(&repository_json(Some(json!(permission))))
                .unwrap()
                .can_write,
            "{permission}"
        );
    }
}

#[test]
fn access_withholds_write_where_gh_names_no_permission_which_is_not_a_standing_it_gave() {
    // The one place an unknown answer is not granted: a Merge button a reader cannot use wastes
    // the press, where a missing one still leaves the pull request open on its host.
    assert!(
        !decode_viewer_permissions_json(&repository_json(None))
            .unwrap()
            .can_write
    );
}

#[test]
fn access_grants_write_and_triage_to_a_github_app_which_github_answers_with_a_null_permission() {
    let access = decode_viewer_permissions_json(&repository_json(Some(Value::Null))).unwrap();
    assert!(access.can_write);
    assert!(access.can_triage);
}

fn viewer_json(repository: Value) -> String {
    json!({ "data": { "repository": merged(
        &json!({
            "mergeCommitAllowed": true,
            "squashMergeAllowed": false,
            "rebaseMergeAllowed": true,
        }),
        repository,
    ) } })
    .to_string()
}

fn access(
    can_write: bool,
    can_triage: bool,
    can_update: bool,
    did_author: bool,
) -> GitHubViewerRepositoryAccess {
    GitHubViewerRepositoryAccess {
        access: GitHubViewerAccess {
            can_write,
            can_triage,
            can_update,
            did_author,
            can_update_branch: None,
        },
        merge_capabilities: PullRequestMergeCapabilities {
            merge: true,
            squash: false,
            rebase: true,
        },
    }
}

#[test]
fn viewer_reads_the_repositorys_role_and_the_pull_requests_own_viewer_fields_together() {
    assert_eq!(
        decode_viewer_permissions_json(&viewer_json(json!({
            "viewerPermission": "READ",
            "pullRequest": { "viewerCanUpdate": true, "viewerDidAuthor": true },
        })))
        .unwrap(),
        access(false, false, true, true)
    );
}

#[test]
fn viewer_says_no_to_a_passer_by_on_a_repository_they_can_only_read() {
    assert_eq!(
        decode_viewer_permissions_json(&viewer_json(json!({
            "viewerPermission": "READ",
            "pullRequest": { "viewerCanUpdate": false, "viewerDidAuthor": false },
        })))
        .unwrap(),
        access(false, false, false, false)
    );
}

#[test]
fn viewer_reads_silence_as_permission_but_not_as_authorship() {
    // A node the viewer cannot see comes back null. Updating is a permission, so an unknown
    // answer grants it and lets the host refuse; authorship is a fact about who wrote the change,
    // and claiming it for someone who did not is how an author's own rules get handed out.
    assert_eq!(
        decode_viewer_permissions_json(&viewer_json(json!({ "pullRequest": null }))).unwrap(),
        access(false, false, true, false)
    );
}

#[test]
fn viewer_reads_triage_as_enough_to_label_and_not_enough_to_write() {
    let access = decode_viewer_permissions_json(&viewer_json(json!({
        "viewerPermission": "TRIAGE",
        "pullRequest": { "viewerCanUpdate": false, "viewerDidAuthor": false },
    })))
    .unwrap();
    assert!(access.can_triage);
    assert!(!access.can_write);
}

#[test]
fn to_can_write_tells_an_absent_permission_from_a_null_one() {
    assert!(!to_can_write(None));
    assert!(to_can_write(Some(None)));
    assert!(to_can_write(Some(Some(" write "))));
    assert!(!to_can_write(Some(Some("TRIAGE"))));
}

// ---------------------------------------------------------------------------------------------
// label candidate decoding
// ---------------------------------------------------------------------------------------------

fn labels_json(defined: Value, applied: &[&str], has_next_page: bool) -> String {
    json!({ "data": { "repository": {
        "labels": { "pageInfo": { "hasNextPage": has_next_page }, "nodes": defined },
        "pullRequest": { "labels": { "nodes": applied.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>() } },
    } } })
    .to_string()
}

#[test]
fn labels_mark_the_ones_the_pull_request_already_wears() {
    let list = decode_label_candidates_json(&labels_json(
        json!([
            { "name": "bug", "color": "d73a4a", "description": "Something is broken" },
            { "name": "size:XL", "color": "e4572e", "description": null },
        ]),
        &["size:XL"],
        false,
    ))
    .unwrap();
    assert_eq!(
        list.candidates,
        vec![
            PullRequestLabelCandidate {
                name: tne("bug"),
                color: Some("d73a4a".to_owned()),
                description: Some("Something is broken".to_owned()),
                is_applied: false,
            },
            PullRequestLabelCandidate {
                name: tne("size:XL"),
                color: Some("e4572e".to_owned()),
                description: None,
                is_applied: true,
            },
        ]
    );
    assert!(!list.truncated);
}

#[test]
fn labels_keep_a_worn_label_the_repository_no_longer_defines_so_it_can_be_taken_off() {
    let list =
        decode_label_candidates_json(&labels_json(json!([{ "name": "bug" }]), &["legacy"], false))
            .unwrap();
    assert_eq!(
        list.candidates
            .iter()
            .map(|label| (label.name.as_str().to_owned(), label.is_applied))
            .collect::<Vec<_>>(),
        vec![("legacy".to_owned(), true), ("bug".to_owned(), false)]
    );
}

#[test]
fn labels_say_so_when_the_repository_defines_more_labels_than_the_read_asked_for() {
    assert!(
        decode_label_candidates_json(&labels_json(json!([]), &[], true))
            .unwrap()
            .truncated
    );
}

// ---------------------------------------------------------------------------------------------
// review thread decoding (threads themselves)
// ---------------------------------------------------------------------------------------------

fn pr_threads_json(nodes: Value, pull_request: Value) -> String {
    let total = nodes.as_array().map_or(0, |nodes| nodes.len());
    json!({ "data": { "repository": { "pullRequest": merged(
        &json!({
            "reviewThreads": { "totalCount": total, "nodes": nodes },
            "author": null,
            "comments": { "nodes": [] },
            "reviewRequests": { "nodes": [] },
            "latestReviews": { "nodes": [] },
        }),
        pull_request,
    ) } } })
    .to_string()
}

#[test]
fn thread_page_carries_what_the_reader_may_do_with_the_pull_request_off_the_conversation_read() {
    // The same response the threads arrive in, so knowing this costs no request of its own.
    assert_eq!(
        decode_review_threads_json(&pr_threads_json(
            json!([]),
            json!({ "viewerCanUpdate": false, "viewerDidAuthor": false })
        ))
        .unwrap()
        .viewer,
        GitHubViewerFlags {
            can_update: false,
            did_author: false
        }
    );
    assert_eq!(
        decode_review_threads_json(&pr_threads_json(json!([]), json!({})))
            .unwrap()
            .viewer,
        GitHubViewerFlags {
            can_update: true,
            did_author: false
        }
    );
}

fn thread_comment(id: &str, body: &str) -> Value {
    json!({
        "id": id,
        "author": { "login": "bilal", "avatarUrl": "https://avatars/b.png" },
        "body": body,
        "createdAt": "2026-07-01T00:00:00Z",
        "url": format!("https://github.com/acme/web/pull/1#discussion_r{id}"),
    })
}

#[test]
fn thread_page_anchors_a_thread_to_its_line_and_side_keeping_the_whole_conversation() {
    let review_threads = decode_review_threads_json(&pr_threads_json(
        json!([{
            "id": "PRRT_1",
            "isResolved": false,
            "isOutdated": false,
            "path": "src/a.ts",
            "line": 42,
            "diffSide": "LEFT",
            "comments": {
                "totalCount": 2,
                "nodes": [
                    merged(&thread_comment("c1", "first"), json!({ "lastEditedAt": "2026-07-02T00:00:00Z" })),
                    thread_comment("c2", "second"),
                ],
            },
        }]),
        json!({}),
    ))
    .unwrap();
    let threads: Vec<_> = review_threads
        .threads
        .iter()
        .map(|entry| entry.thread.clone())
        .collect();
    let bilal = actor("bilal", None, Some("https://avatars/b.png"));
    assert_eq!(
        threads,
        vec![turin_contracts::pull_request::PullRequestReviewThread {
            id: tne("PRRT_1"),
            path: tne("src/a.ts"),
            line: Some(positive(42)),
            side: PullRequestDiffSide::Left,
            is_resolved: false,
            is_outdated: false,
            comments: vec![
                turin_contracts::pull_request::PullRequestThreadComment {
                    id: tne("c1"),
                    author: Some(bilal.clone()),
                    body: "first".to_owned(),
                    created_at: "2026-07-01T00:00:00Z".to_owned(),
                    edited_at: Some(Some("2026-07-02T00:00:00Z".to_owned())),
                    url: Some("https://github.com/acme/web/pull/1#discussion_rc1".to_owned()),
                    reactions: Some(vec![]),
                    can_edit: None,
                },
                turin_contracts::pull_request::PullRequestThreadComment {
                    id: tne("c2"),
                    author: Some(bilal),
                    body: "second".to_owned(),
                    created_at: "2026-07-01T00:00:00Z".to_owned(),
                    edited_at: Some(None),
                    url: Some("https://github.com/acme/web/pull/1#discussion_rc2".to_owned()),
                    reactions: Some(vec![]),
                    can_edit: None,
                },
            ],
            comment_count: None,
            next_comments_cursor: None,
            can_resolve: None,
        }]
    );
    assert_eq!(
        review_thread_conversation(&threads)[0].edited_at,
        Some(Some("2026-07-02T00:00:00Z".to_owned()))
    );
}

#[test]
fn thread_page_leaves_an_outdated_thread_without_a_line_rather_than_pinning_it_to_a_stale_one() {
    let review_threads = decode_review_threads_json(&pr_threads_json(
        json!([{
            "id": "PRRT_2",
            "isResolved": true,
            "isOutdated": true,
            "path": "src/a.ts",
            // GitHub reports no current line once the thread has fallen off the diff.
            "line": null,
            "diffSide": "RIGHT",
            "comments": { "totalCount": 1, "nodes": [thread_comment("c3", "stale")] },
        }]),
        json!({}),
    ))
    .unwrap();
    let thread = &review_threads.threads[0].thread;
    assert_eq!(thread.line, None);
    assert!(thread.is_outdated);
    assert!(thread.is_resolved);
    assert_eq!(thread.side, PullRequestDiffSide::Right);
}

#[test]
fn thread_page_skips_threads_without_an_id_a_path_or_comments() {
    let page = decode_review_threads_json(&pr_threads_json(
        json!([
            { "path": "a.ts", "comments": { "nodes": [thread_comment("c1", "x")] } },
            { "id": "PRRT_x", "comments": { "nodes": [thread_comment("c1", "x")] } },
            { "id": "PRRT_y", "path": "a.ts", "comments": { "nodes": [] } },
            { "id": "PRRT_z", "path": "a.ts", "line": 0, "comments": { "nodes": [thread_comment("c1", "x")] } },
        ]),
        json!({}),
    ))
    .unwrap();
    assert_eq!(page.threads.len(), 1);
    assert_eq!(page.threads[0].thread.line, None);
    assert_eq!(page.threads[0].comment_count, 1);
}

#[test]
fn thread_page_keeps_a_resolved_thread_in_the_conversation_as_well_as_against_its_line() {
    let decoded = decode_review_threads_json(&pr_threads_json(
        json!([{
            "id": "PRRT_3",
            "isResolved": true,
            "path": "src/a.ts",
            "line": 7,
            "diffSide": "RIGHT",
            "comments": { "totalCount": 1, "nodes": [thread_comment("c4", "done")] },
        }]),
        json!({}),
    ))
    .unwrap();
    // A resolved conversation is finished work, not unsaid work: the timeline reads it and the
    // diff pins it to its line, the same as any other.
    let threads: Vec<_> = decoded
        .threads
        .iter()
        .map(|entry| entry.thread.clone())
        .collect();
    assert_eq!(
        review_thread_conversation(&threads)
            .iter()
            .map(|comment| comment.id.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["c4"]
    );
    assert_eq!(threads.len(), 1);
}

#[test]
fn thread_page_reads_edit_times_for_issue_comments_and_reviews_without_using_reaction_updates() {
    let edited_at = "2026-10-02T12:06:00Z";
    let result = decode_review_threads_json(&pr_threads_json(
        json!([]),
        json!({
            "comments": { "nodes": [
                { "id": "edited", "lastEditedAt": edited_at },
                { "id": "reaction-only", "lastEditedAt": null, "updatedAt": edited_at },
            ] },
            "reviews": { "nodes": [{ "id": "review", "lastEditedAt": edited_at }] },
        }),
    ))
    .unwrap();
    assert_eq!(
        result.edited_at_by_id.into_iter().collect::<Vec<_>>(),
        vec![
            ("edited".to_owned(), edited_at.to_owned()),
            ("review".to_owned(), edited_at.to_owned()),
        ]
    );
}

#[test]
fn thread_page_puts_remark_reactions_in_reactions_by_id_and_the_pull_requests_own_in_reactions() {
    let result = decode_review_threads_json(&pr_threads_json(
        json!([]),
        json!({
            "reactionGroups": [{
                "content": "HEART",
                "viewerHasReacted": true,
                "reactors": { "totalCount": 1, "nodes": [{ "login": "bilal" }] },
            }],
            "comments": { "nodes": [{
                "id": "c1",
                "reactionGroups": [{ "content": "THUMBS_UP", "reactors": { "totalCount": 1, "nodes": [{ "login": "julius" }] } }],
            }] },
            "reviews": { "nodes": [{
                "id": "r1",
                "reactionGroups": [{ "content": "EYES", "reactors": { "totalCount": 1, "nodes": [{ "login": "hubot" }] } }],
            }] },
        }),
    ))
    .unwrap();

    assert_eq!(
        result.reactions,
        vec![reaction(
            PullRequestReactionContent::Heart,
            1,
            &["bilal"],
            true
        )]
    );
    assert_eq!(
        result.reactions_by_id.into_iter().collect::<Vec<_>>(),
        vec![
            (
                "c1".to_owned(),
                vec![reaction(
                    PullRequestReactionContent::ThumbsUp,
                    1,
                    &["julius"],
                    false
                )]
            ),
            (
                "r1".to_owned(),
                vec![reaction(
                    PullRequestReactionContent::Eyes,
                    1,
                    &["hubot"],
                    false
                )]
            ),
        ]
    );
}

#[test]
fn thread_page_leaves_the_viewers_own_login_out_of_the_pull_requests_own_reactions() {
    let result = decode_review_threads_json(
        &json!({ "data": {
            "viewer": { "login": "Bilal" },
            "repository": { "pullRequest": {
                "reviewThreads": { "totalCount": 0, "nodes": [] },
                "reactionGroups": [{
                    "content": "HEART",
                    "viewerHasReacted": true,
                    "reactors": { "totalCount": 2, "nodes": [{ "login": "bilal" }, { "login": "julius" }] },
                }],
            } },
        } })
        .to_string(),
    )
    .unwrap();

    assert_eq!(
        result.reactions,
        vec![reaction(
            PullRequestReactionContent::Heart,
            2,
            &["julius"],
            true
        )]
    );
}

#[test]
fn thread_page_collects_avatars_and_dismissals_and_follows_their_cursors() {
    let result = decode_review_threads_json(&pr_threads_json(
        json!([]),
        json!({
            "author": { "__typename": "User", "login": "author", "avatarUrl": "https://a/a.png" },
            "reviewDismissals": {
                "pageInfo": { "hasNextPage": true, "endCursor": "dismissed-2" },
                "nodes": [
                    { "dismissalMessage": " stale ", "review": { "id": "PRR_1" } },
                    { "dismissalMessage": null, "review": { "id": "PRR_2" } },
                    { "dismissalMessage": "no id", "review": null },
                ],
            },
        }),
    ))
    .unwrap();
    assert_eq!(
        result.avatars_by_login.into_iter().collect::<Vec<_>>(),
        vec![("author".to_owned(), "https://a/a.png".to_owned())]
    );
    assert_eq!(
        result
            .dismissals_by_review_id
            .into_iter()
            .collect::<Vec<_>>(),
        vec![("PRR_1".to_owned(), "stale".to_owned())]
    );
    assert_eq!(result.next_dismissal_cursor.as_deref(), Some("dismissed-2"));
}

#[test]
fn review_dismissals_pages_decode_in_the_shape_the_thread_read_carries() {
    let page = decode_review_dismissals_json(
        &json!({ "data": { "repository": { "pullRequest": { "timelineItems": {
            "pageInfo": { "hasNextPage": false, "endCursor": "x" },
            "nodes": [{ "dismissalMessage": "reason", "review": { "id": "PRR_9" } }],
        } } } } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        page.dismissals_by_review_id
            .get("PRR_9")
            .map(String::as_str),
        Some("reason")
    );
    assert_eq!(page.next_cursor, None);
}

// ---------------------------------------------------------------------------------------------
// node ids, subject scope, queries
// ---------------------------------------------------------------------------------------------

#[test]
fn node_id_reads_the_pull_requests_own_node_id_which_a_reaction_on_its_description_is_addressed_by()
{
    assert_eq!(
        decode_pull_request_node_id_json(
            &json!({ "data": { "repository": { "pullRequest": { "id": "PR_kwDOA" } } } })
                .to_string()
        )
        .unwrap(),
        "PR_kwDOA"
    );
}

#[test]
fn reaction_subject_scope_accepts_only_the_pull_request_or_what_hangs_off_it() {
    let scope = |repository: Value, node: Value| {
        decode_reaction_subject_scope_json(
            &json!({ "data": { "repository": repository, "node": node } }).to_string(),
        )
        .unwrap()
    };
    let pr = json!({ "pullRequest": { "id": "PR_1" } });
    assert!(scope(pr.clone(), json!({ "id": "PR_1" })));
    assert!(scope(
        pr.clone(),
        json!({ "id": "IC_1", "pullRequest": { "id": "PR_1" } })
    ));
    assert!(!scope(
        pr.clone(),
        json!({ "id": "IC_1", "pullRequest": { "id": "PR_2" } })
    ));
    assert!(!scope(pr.clone(), Value::Null));
    assert!(!scope(Value::Null, json!({ "id": "PR_1" })));
}

#[test]
fn review_threads_query_caps_the_initial_query_after_the_104_point_rate_limit_regression() {
    let pattern =
        regex::Regex::new(r"reviewThreads\(first: (\d+)[\s\S]*?comments\(first: (\d+)\)").unwrap();
    let captures = pattern
        .captures(REVIEW_THREADS_GRAPHQL_QUERY)
        .expect("review-thread connections");
    let threads: u64 = captures[1].parse().unwrap();
    let comments: u64 = captures[2].parse().unwrap();
    assert!(threads * comments <= 1_000);
}

#[test]
fn review_threads_query_asks_for_reaction_groups_on_the_pull_request_its_comments_its_reviews_and_each_threads_comments()
 {
    assert_eq!(
        REVIEW_THREADS_GRAPHQL_QUERY
            .matches("reactionGroups")
            .count(),
        4
    );
    // The reviews connection is new: only reactions were ever wanted off it.
    assert!(REVIEW_THREADS_GRAPHQL_QUERY.contains("reviews(first:"));
}

#[test]
fn search_and_list_documents_clamp_the_row_count_the_way_github_demands() {
    assert!(
        pull_request_search_graphql_query(500, false, false).contains("first: 100, after: $after")
    );
    assert!(pull_request_search_graphql_query(0, false, false).contains("first: 1, after: $after"));
    assert!(
        pull_request_list_graphql_query(30, false)
            .contains("pullRequests(first: 30, after: $after")
    );
    assert!(pull_request_search_graphql_query(20, false, true).contains("additions deletions"));
    assert!(!pull_request_search_graphql_query(20, false, false).contains("additions deletions"));
    assert!(pull_request_list_graphql_query(20, false).contains("additions deletions"));
}

// ---------------------------------------------------------------------------------------------
// reviewer candidates and payloads
// ---------------------------------------------------------------------------------------------

fn candidates_json(
    assignable: Vec<Value>,
    requested: Vec<Value>,
    author: Option<&str>,
    has_next_page: bool,
) -> String {
    json!({ "data": { "repository": {
        "assignableUsers": {
            "pageInfo": { "hasNextPage": has_next_page },
            "nodes": assignable,
        },
        "pullRequest": {
            "author": author.map(|login| json!({ "login": login })),
            "reviewRequests": {
                "nodes": requested.into_iter().map(|r| json!({ "requestedReviewer": r })).collect::<Vec<_>>(),
            },
        },
    } } })
    .to_string()
}

fn candidate(
    id: &str,
    kind: PullRequestReviewerKind,
    login: &str,
    name: Option<&str>,
    is_requested: bool,
) -> PullRequestReviewerCandidate {
    PullRequestReviewerCandidate {
        is_bot: None,
        login: tne(login),
        name: name.map(str::to_owned),
        avatar_url: None,
        id: tne(id),
        kind,
        is_requested,
    }
}

#[test]
fn reviewer_candidates_leave_the_author_out_of_the_people_their_own_pull_request_can_be_sent_to() {
    let list = decode_reviewer_candidates_json(&candidates_json(
        vec![
            json!({ "login": "bilal" }),
            json!({ "login": "octocat", "name": "The Octocat" }),
        ],
        vec![],
        Some("bilal"),
        false,
    ))
    .unwrap();
    assert_eq!(
        list.candidates,
        vec![candidate(
            "octocat",
            PullRequestReviewerKind::User,
            "octocat",
            Some("The Octocat"),
            false
        )]
    );
    assert!(!list.truncated);
}

#[test]
fn reviewer_candidates_mark_whoever_has_already_been_asked_and_leave_the_rest_to_be_asked() {
    let list = decode_reviewer_candidates_json(&candidates_json(
        vec![json!({ "login": "octocat" }), json!({ "login": "hubot" })],
        vec![json!({ "login": "octocat" })],
        None,
        false,
    ))
    .unwrap();
    assert_eq!(
        list.candidates
            .iter()
            .map(|candidate| (candidate.login.as_str().to_owned(), candidate.is_requested))
            .collect::<Vec<_>>(),
        vec![("octocat".to_owned(), true), ("hubot".to_owned(), false)]
    );
}

#[test]
fn reviewer_candidates_keep_a_requested_team_apart_from_the_people_so_the_request_can_be_taken_back()
 {
    // A team is never among the assignable users, and a request that cannot be seen cannot be
    // undone, so the ones GitHub reports are carried, marked as the teams they are.
    let list = decode_reviewer_candidates_json(&candidates_json(
        vec![json!({ "login": "octocat" })],
        vec![json!({ "slug": "reviewers", "combinedSlug": "acme/reviewers", "name": "Reviewers" })],
        None,
        false,
    ))
    .unwrap();
    assert_eq!(
        list.candidates,
        vec![
            // Asked by slug, shown by the same `org/slug` as its reviewer row.
            candidate(
                "reviewers",
                PullRequestReviewerKind::Team,
                "acme/reviewers",
                Some("Reviewers"),
                true
            ),
            candidate(
                "octocat",
                PullRequestReviewerKind::User,
                "octocat",
                None,
                false
            ),
        ]
    );
}

#[test]
fn reviewer_candidates_say_so_when_the_repository_has_more_people_than_the_read_asked_for() {
    assert!(
        decode_reviewer_candidates_json(&candidates_json(
            vec![json!({ "login": "octocat" })],
            vec![],
            None,
            true
        ))
        .unwrap()
        .truncated
    );
}

#[test]
fn reviewer_request_sends_people_and_teams_in_the_two_lists_github_keeps_them_in() {
    let request = build_reviewer_request(&[
        ("octocat".to_owned(), PullRequestReviewerKind::User),
        ("reviewers".to_owned(), PullRequestReviewerKind::Team),
        ("hubot".to_owned(), PullRequestReviewerKind::User),
    ]);
    assert_eq!(
        request.to_value(),
        json!({ "reviewers": ["octocat", "hubot"], "team_reviewers": ["reviewers"] })
    );
}

#[test]
fn reviewer_request_sends_both_lists_even_where_one_of_them_is_empty_which_is_what_github_reads() {
    assert_eq!(
        build_reviewer_request(&[("octocat".to_owned(), PullRequestReviewerKind::User)]).to_value(),
        json!({ "reviewers": ["octocat"], "team_reviewers": [] })
    );
}

#[test]
fn label_request_adds_to_what_is_there() {
    assert_eq!(
        build_label_request(&["bug".to_owned(), "size:XL".to_owned()]).to_value(),
        json!({ "labels": ["bug", "size:XL"] })
    );
}

#[test]
fn review_submission_sends_the_verdict_the_summary_and_every_line_comment_in_one_body() {
    let payload = build_review_submission(
        PullRequestReviewVerdict::RequestChanges,
        "Two things.",
        &[
            PullRequestReviewCommentDraft {
                path: tne("src/a.ts"),
                old_path: None,
                position: PullRequestReviewPosition::Added {
                    new_line: positive(12),
                },
                body: "rename this".to_owned(),
            },
            PullRequestReviewCommentDraft {
                path: tne("src/b.ts"),
                old_path: None,
                position: PullRequestReviewPosition::Deleted {
                    old_line: positive(3),
                },
                body: "why remove?".to_owned(),
            },
            PullRequestReviewCommentDraft {
                path: tne("src/c.ts"),
                old_path: None,
                position: PullRequestReviewPosition::Context {
                    old_line: positive(5),
                    new_line: positive(6),
                    side: PullRequestDiffSide::Left,
                },
                body: "left copy".to_owned(),
            },
            PullRequestReviewCommentDraft {
                path: tne("src/c.ts"),
                old_path: None,
                position: PullRequestReviewPosition::Context {
                    old_line: positive(5),
                    new_line: positive(6),
                    side: PullRequestDiffSide::Right,
                },
                body: "right copy".to_owned(),
            },
        ],
    );
    assert_eq!(
        payload.to_value(),
        json!({
            "event": "REQUEST_CHANGES",
            "body": "Two things.",
            "comments": [
                { "path": "src/a.ts", "line": 12, "side": "RIGHT", "body": "rename this" },
                { "path": "src/b.ts", "line": 3, "side": "LEFT", "body": "why remove?" },
                { "path": "src/c.ts", "line": 5, "side": "LEFT", "body": "left copy" },
                { "path": "src/c.ts", "line": 6, "side": "RIGHT", "body": "right copy" },
            ],
        })
    );
}

#[test]
fn review_submission_sends_an_approval_with_no_words_and_no_comments() {
    assert_eq!(
        build_review_submission(PullRequestReviewVerdict::Approve, "", &[]).to_value(),
        json!({ "event": "APPROVE", "body": "", "comments": [] })
    );
}

// ---------------------------------------------------------------------------------------------
// files
// ---------------------------------------------------------------------------------------------

fn files_patch(entries: Value) -> GitHubPullRequestFilesPatch {
    decode_pull_request_files_json(&entries.to_string()).unwrap()
}

#[test]
fn files_quote_literal_backslashes_without_interpreting_them_as_escapes() {
    let result = files_patch(json!([{
        "filename": r"src\notes.ts",
        "status": "modified",
        "patch": "@@ -1 +1 @@\n-old\n+new",
    }]));
    assert_eq!(
        result.patch,
        [
            r#"diff --git "a/src\\notes.ts" "b/src\\notes.ts""#,
            r#"--- "a/src\\notes.ts""#,
            r#"+++ "b/src\\notes.ts""#,
            "@@ -1 +1 @@",
            "-old",
            "+new",
            "",
        ]
        .join("\n")
    );
}

#[test]
fn files_preserve_spaces_and_literal_backslashes_in_both_rename_paths() {
    let result = files_patch(json!([{
        "previous_filename": r" old\name.ts ",
        "filename": r" new\name.ts ",
        "status": "renamed",
    }]));
    assert_eq!(
        result.patch,
        [
            r#"diff --git "a/ old\\name.ts " "b/ new\\name.ts ""#,
            r#"rename from " old\\name.ts ""#,
            r#"rename to " new\\name.ts ""#,
            r#"--- "a/ old\\name.ts ""#,
            r#"+++ "b/ new\\name.ts ""#,
            "",
        ]
        .join("\n")
    );
}

#[test]
fn files_assemble_a_unified_patch_the_files_api_does_not_return() {
    let result = files_patch(json!([
        { "filename": "src/app.ts", "status": "modified", "patch": "@@ -1 +1 @@\n-old\n+new" },
    ]));
    assert_eq!(
        result.patch,
        [
            "diff --git a/src/app.ts b/src/app.ts",
            "--- a/src/app.ts",
            "+++ b/src/app.ts",
            "@@ -1 +1 @@",
            "-old",
            "+new",
            "",
        ]
        .join("\n")
    );
    assert!(!result.truncated);
    assert_eq!(result.raw_count, 1);
}

#[test]
fn files_point_an_added_file_at_dev_null_on_the_left_and_a_removed_one_on_the_right() {
    let result = files_patch(json!([
        { "filename": "src/new.ts", "status": "added", "patch": "@@ -0,0 +1 @@\n+hello" },
        { "filename": "src/gone.ts", "status": "removed", "patch": "@@ -1 +0,0 @@\n-bye" },
    ]));
    assert_eq!(
        result.patch,
        [
            "diff --git a/src/new.ts b/src/new.ts",
            "new file mode 100644",
            "--- /dev/null",
            "+++ b/src/new.ts",
            "@@ -0,0 +1 @@",
            "+hello",
            "diff --git a/src/gone.ts b/src/gone.ts",
            "deleted file mode 100644",
            "--- a/src/gone.ts",
            "+++ /dev/null",
            "@@ -1 +0,0 @@",
            "-bye",
            "",
        ]
        .join("\n")
    );
}

#[test]
fn files_name_both_paths_of_a_rename_counting_its_hunks_against_the_old_one() {
    let result = files_patch(json!([{
        "filename": "src/new.ts",
        "status": "renamed",
        "previous_filename": "src/old.ts",
        "patch": "@@ -1 +1 @@\n-old\n+new",
    }]));
    assert_eq!(
        result.patch,
        [
            "diff --git a/src/old.ts b/src/new.ts",
            "rename from src/old.ts",
            "rename to src/new.ts",
            "--- a/src/old.ts",
            "+++ b/src/new.ts",
            "@@ -1 +1 @@",
            "-old",
            "+new",
            "",
        ]
        .join("\n")
    );
}

#[test]
fn files_still_list_a_file_github_sent_no_hunks_for_and_say_what_was_withheld() {
    let result = files_patch(json!([
        // Binary: it changed, and none of it can be shown.
        { "filename": "logo.png", "status": "modified", "additions": 4, "deletions": 2 },
        {
            "filename": "src/app.ts",
            "status": "modified",
            "additions": 1,
            "deletions": 1,
            "patch": "@@ -1 +1 @@\n-old\n+new",
        },
    ]));

    // Dropping it would take the file out of the change altogether, not just its contents.
    assert!(result.patch.contains("diff --git a/logo.png b/logo.png"));
    assert!(
        result
            .patch
            .contains("diff --git a/src/app.ts b/src/app.ts")
    );
    assert!(result.truncated);
    assert_eq!(result.raw_count, 2);
    assert_eq!(
        result.omitted_file_stats,
        vec![turin_contracts::pull_request::PullRequestOmittedFileStat {
            path: tne("logo.png"),
            additions: 4.0,
            deletions: 2.0,
        }]
    );
}

#[test]
fn files_do_not_call_a_pure_rename_incomplete_since_it_has_no_hunks_to_withhold() {
    let result = files_patch(json!([{
        "filename": "src/new.ts",
        "previous_filename": "src/old.ts",
        "status": "renamed",
        "additions": 0,
        "deletions": 0,
    }]));
    assert!(result.patch.contains("rename from src/old.ts"));
    assert!(!result.truncated);
}

#[test]
fn files_skip_entries_that_do_not_decode_but_still_count_them() {
    let result = files_patch(json!([
        { "status": "modified" },
        { "filename": "a.ts", "patch": "@@ -1 +1 @@\n-a\n+b\n" },
    ]));
    assert_eq!(result.raw_count, 2);
    assert_eq!(
        result.patch,
        "diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-a\n+b\n"
    );
    assert!(decode_pull_request_files_json("{}").is_err());
}

#[test]
fn commit_files_read_a_commit_with_no_files_as_an_empty_patch() {
    let empty = decode_commit_files_json("{}").unwrap();
    assert_eq!(
        (empty.patch.as_str(), empty.raw_count, empty.truncated),
        ("", 0, false)
    );
    let commit = decode_commit_files_json(
        &json!({ "files": [{ "filename": "a.ts", "patch": "@@ -1 +1 @@\n-a\n+b" }] }).to_string(),
    )
    .unwrap();
    assert_eq!(commit.raw_count, 1);
    assert!(commit.patch.starts_with("diff --git a/a.ts b/a.ts\n"));
}

#[test]
fn quoting_follows_gits_c_style_escapes() {
    assert_eq!(quote_git_patch_path("a/plain.ts"), "a/plain.ts");
    assert_eq!(quote_git_patch_path("a/tab\there"), "\"a/tab\\there\"");
    assert_eq!(quote_git_patch_path("a/new\nline"), "\"a/new\\nline\"");
    assert_eq!(quote_git_patch_path("a/q\"uote"), "\"a/q\\\"uote\"");
    assert_eq!(quote_git_patch_path("a/bell\u{7}"), "\"a/bell\\a\"");
    assert_eq!(quote_git_patch_path("a/esc\u{1b}"), "\"a/esc\\033\"");
    assert_eq!(quote_git_patch_path("a/del\u{7f}"), "\"a/del\\177\"");
    // Non-ASCII stays as it is: `core.quotePath` is a terminal setting.
    assert_eq!(quote_git_patch_path("a/caf\u{e9}.ts"), "a/caf\u{e9}.ts");
}

// ---------------------------------------------------------------------------------------------
// files viewed
// ---------------------------------------------------------------------------------------------

fn viewed_page(nodes: Value, page_info: Value) -> String {
    json!({ "data": { "repository": { "pullRequest": { "files": {
        "pageInfo": page_info,
        "nodes": nodes,
    } } } } })
    .to_string()
}

fn viewed(path: &str, state: PullRequestFileViewedState) -> GitHubViewedFile {
    GitHubViewedFile {
        path: path.to_owned(),
        state,
    }
}

#[test]
fn files_viewed_reads_each_files_state_and_where_the_next_page_carries_on() {
    let decoded = decode_pull_request_files_viewed_json(&viewed_page(
        json!([
            { "path": "src/a.ts", "viewerViewedState": "VIEWED" },
            { "path": "src/b.ts", "viewerViewedState": "UNVIEWED" },
            { "path": "src/c.ts", "viewerViewedState": "DISMISSED" },
        ]),
        json!({ "hasNextPage": true, "endCursor": "cursor-2" }),
    ))
    .unwrap();
    assert_eq!(
        decoded,
        GitHubPullRequestFilesViewedPage {
            files: vec![
                viewed("src/a.ts", PullRequestFileViewedState::Viewed),
                viewed("src/b.ts", PullRequestFileViewedState::Unviewed),
                viewed("src/c.ts", PullRequestFileViewedState::Dismissed),
            ],
            next_cursor: Some("cursor-2".to_owned()),
        }
    );
}

#[test]
fn files_viewed_treats_a_state_it_has_never_heard_of_as_unread_rather_than_failing_the_page() {
    let decoded = decode_pull_request_files_viewed_json(&viewed_page(
        json!([{ "path": "src/a.ts", "viewerViewedState": "SOMETHING_NEW" }]),
        json!({ "hasNextPage": false, "endCursor": null }),
    ))
    .unwrap();
    assert_eq!(
        decoded,
        GitHubPullRequestFilesViewedPage {
            files: vec![viewed("src/a.ts", PullRequestFileViewedState::Unviewed)],
            next_cursor: None,
        }
    );
}

#[test]
fn files_viewed_answers_empty_for_a_pull_request_the_host_has_nothing_to_say_about() {
    let decoded = decode_pull_request_files_viewed_json(
        &json!({ "data": { "repository": { "pullRequest": null } } }).to_string(),
    )
    .unwrap();
    assert_eq!(
        decoded,
        GitHubPullRequestFilesViewedPage {
            files: vec![],
            next_cursor: None
        }
    );
}

#[test]
fn set_files_viewed_asks_for_nothing_when_nothing_was_pressed() {
    assert_eq!(build_set_files_viewed_graphql_mutation("PR_1", &[]), None);
}

#[test]
fn set_files_viewed_clears_and_restores_in_one_document_each_file_under_its_own_alias() {
    let mutation = build_set_files_viewed_graphql_mutation(
        "PR_1",
        &[
            ("src/a.ts".to_owned(), true),
            ("src/b.ts".to_owned(), false),
        ],
    )
    .expect("a document");
    assert!(
        mutation
            .query
            .contains("mutation($pullRequestId: ID!, $f0_path: String!, $f1_path: String!)")
    );
    assert!(mutation.query.contains(
        "f0: markFileAsViewed(input: { pullRequestId: $pullRequestId, path: $f0_path })"
    ));
    assert!(mutation.query.contains(
        "f1: unmarkFileAsViewed(input: { pullRequestId: $pullRequestId, path: $f1_path })"
    ));
    assert_eq!(
        Value::Object(mutation.variables),
        json!({ "pullRequestId": "PR_1", "f0_path": "src/a.ts", "f1_path": "src/b.ts" })
    );
}

#[test]
fn set_files_viewed_keeps_a_path_out_of_the_document_so_one_cannot_be_read_as_part_of_it() {
    let hostile = r#"") { __typename } evil: markFileAsViewed(input: { path: "x"#;
    let mutation = build_set_files_viewed_graphql_mutation("PR_1", &[(hostile.to_owned(), true)])
        .expect("a document");
    assert!(!mutation.query.contains("evil"));
    assert_eq!(mutation.variables["f0_path"], json!(hostile));
}

// ---------------------------------------------------------------------------------------------
// host-native stacks
// ---------------------------------------------------------------------------------------------

/// A stack as the preview lists it, bottom to top, with the fields it answers today.
fn stack(overrides: Value) -> Value {
    merged(
        &json!({
            "id": 42,
            "number": 3,
            "node_id": "STK_kwDO",
            "url": "https://api.github.com/repos/acme/web/stacks/3",
            "base": { "ref": "main", "sha": "abc" },
            "open": true,
            "created_at": "2026-09-01T00:00:00Z",
            "pull_requests": [
                { "number": 10, "head": { "ref": "feat/one" }, "state": "closed", "merged_at": "2026-09-02T00:00:00Z" },
                { "number": 11, "head": { "ref": "feat/two" }, "state": "open", "merged_at": null },
                { "number": 12, "head": { "ref": "feat/three" }, "state": "closed", "merged_at": null },
            ],
        }),
        overrides,
    )
}

/// The one stack a listing answered with, which these reads all expect to find.
fn expect_stack(overrides: Value) -> GitHubPullRequestStack {
    decode_pull_request_stacks_json(&json!([stack(overrides)]).to_string())
        .unwrap()
        .expect("a stack")
}

fn layer(number: u64, head_branch: &str, state: PullRequestState) -> GitHubPullRequestStackLayer {
    GitHubPullRequestStackLayer {
        title: None,
        is_draft: None,
        head_sha: None,
        number,
        head_branch: head_branch.to_owned(),
        state,
    }
}

#[test]
fn stacks_read_the_first_stack_bottom_to_top_with_merged_at_outranking_state() {
    assert_eq!(
        expect_stack(json!({})),
        GitHubPullRequestStack {
            id: "42".to_owned(),
            number: 3,
            url: "https://api.github.com/repos/acme/web/stacks/3".to_owned(),
            base: "main".to_owned(),
            layers: vec![
                layer(10, "feat/one", PullRequestState::Merged),
                layer(11, "feat/two", PullRequestState::Open),
                layer(12, "feat/three", PullRequestState::Closed),
            ],
        }
    );
}

#[test]
fn stacks_retain_the_detailed_layer_titles_draft_state_and_expected_revision() {
    assert_eq!(
        expect_stack(json!({ "pull_requests": [{
            "number": 11,
            "title": "Second layer",
            "draft": true,
            "head": { "ref": "feat/two", "sha": "abc123" },
            "state": "open",
            "merged_at": null,
        }] }))
        .layers,
        vec![GitHubPullRequestStackLayer {
            title: Some("Second layer".to_owned()),
            is_draft: Some(true),
            head_sha: Some("abc123".to_owned()),
            number: 11,
            head_branch: "feat/two".to_owned(),
            state: PullRequestState::Open,
        }]
    );
}

#[test]
fn stacks_accept_a_base_named_as_a_bare_branch_which_is_what_the_preview_started_out_sending() {
    assert_eq!(expect_stack(json!({ "base": "develop" })).base, "develop");
}

#[test]
fn stacks_prefer_the_page_a_person_opens_over_the_api_url_where_the_host_reports_one() {
    assert_eq!(
        expect_stack(json!({ "html_url": "https://github.com/acme/web/stacks/3" })).url,
        "https://github.com/acme/web/stacks/3"
    );
}

#[test]
fn stacks_fall_back_to_the_node_id_then_the_number_for_a_stack_without_an_id() {
    let without_id = {
        let mut raw = object(stack(json!({})));
        raw.remove("id");
        Value::Object(raw)
    };
    let decoded = decode_pull_request_stacks_json(&json!([without_id]).to_string())
        .unwrap()
        .unwrap();
    assert_eq!(decoded.id, "STK_kwDO");
    assert_eq!(expect_stack(json!({ "id": null, "node_id": null })).id, "3");
    assert_eq!(expect_stack(json!({ "id": "opaque" })).id, "opaque");
}

#[test]
fn stacks_read_an_empty_listing_as_not_stacked() {
    assert_eq!(decode_pull_request_stacks_json("[]").unwrap(), None);
}

#[test]
fn stacks_refuse_a_stack_without_a_number_or_without_its_pull_requests() {
    let without = |key: &str| {
        let mut raw = object(stack(json!({})));
        raw.remove(key);
        json!([raw]).to_string()
    };
    assert!(decode_pull_request_stacks_json(&without("number")).is_err());
    assert!(decode_pull_request_stacks_json(&without("pull_requests")).is_err());
    assert!(decode_pull_request_stacks_json("{").is_err());
}

// ---------------------------------------------------------------------------------------------
// stack membership batches
// ---------------------------------------------------------------------------------------------

#[test]
fn memberships_map_aliases_while_skipping_missing_pull_requests_and_incomplete_memberships() {
    let memberships = decode_pull_request_stack_memberships_json(
        &json!({ "data": {
            "s0": { "pullRequest": {
                "stack": { "number": 3, "size": 2, "baseRefName": "main" },
                "stackEntry": { "position": 1 },
            } },
            "s1": null,
            "s2": { "pullRequest": null },
            "s3": { "pullRequest": { "stack": null, "stackEntry": null } },
            "s4": { "pullRequest": { "stack": { "number": 3, "size": 2, "baseRefName": "main" } } },
        } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        memberships.into_iter().collect::<Vec<_>>(),
        vec![(
            0,
            PullRequestStackMembership {
                number: positive(3),
                size: positive(2),
                base: tne("main"),
                position: positive(1),
            }
        )]
    );
}

#[test]
fn memberships_refuse_malformed_responses_and_unsafe_query_selectors() {
    assert!(decode_pull_request_stack_memberships_json(r#"{"errors":[]}"#).is_err());
    assert_eq!(
        build_pull_request_stack_memberships_graphql_query(r#"acme/web") { x } #"#, &[1]),
        None
    );
    assert_eq!(
        build_pull_request_stack_memberships_graphql_query("acme/web", &[0]),
        None
    );
    // The upstream case of a fractional number (`[1.5]`) cannot be written: numbers are `u64`.
    assert_eq!(
        build_pull_request_stack_memberships_graphql_query("acme/web", &[]),
        None
    );
    let document = build_pull_request_stack_memberships_graphql_query("acme/web", &[7, 8])
        .expect("a document");
    assert_eq!(
        Value::Object(document.variables),
        json!({
            "s0_owner": "acme", "s0_name": "web", "s0_number": 7,
            "s1_owner": "acme", "s1_name": "web", "s1_number": 8,
        })
    );
}

#[test]
fn stats_and_summaries_documents_are_named_and_aliased_per_row() {
    let selectors = [ChangeRequestSelector {
        repository: "acme/web".to_owned(),
        number: 7,
    }];
    let stats = build_pull_request_stats_graphql_query(&selectors).unwrap();
    assert_eq!(
        stats.query,
        "query PullRequestStats($s0_owner: String!, $s0_name: String!, $s0_number: Int!) {\n  s0: repository(owner: $s0_owner, name: $s0_name) { pullRequest(number: $s0_number) { additions deletions } }\n}"
    );
    let decoded = decode_pull_request_stats_json(
        &json!({ "data": {
            "s0": { "pullRequest": { "additions": 5, "deletions": null } },
            "s1": { "pullRequest": null },
            "s2": null,
            "rateLimit": { "cost": 1 },
        } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        decoded.into_iter().collect::<Vec<_>>(),
        vec![(
            0,
            GitHubDiffCounts {
                additions: 5,
                deletions: 0
            }
        )]
    );
    assert_eq!(
        decode_pull_request_stats_json(r#"{"data":null}"#)
            .unwrap()
            .len(),
        0
    );
}

// ---------------------------------------------------------------------------------------------
// batched summaries
// ---------------------------------------------------------------------------------------------

fn selector(repository: &str, number: u64) -> ChangeRequestSelector {
    ChangeRequestSelector {
        repository: repository.to_owned(),
        number,
    }
}

#[test]
fn summaries_refuse_a_repository_graphql_cannot_address_rather_than_writing_it_into_the_document() {
    assert_eq!(
        build_pull_request_summaries_graphql_query(&[selector(r#"acme/web") { x } #"#, 1)], false),
        None
    );
    assert_eq!(
        build_pull_request_summaries_graphql_query(&[selector("acme/web", 0)], false),
        None
    );
    assert_eq!(build_pull_request_summaries_graphql_query(&[], false), None);
    assert_eq!(
        build_pull_request_summaries_graphql_query(&[selector("a/b/c", 1)], false),
        None
    );
}

#[test]
fn summaries_file_each_answer_by_its_alias_and_skip_what_github_or_the_decoder_could_not_give() {
    let decoded = decode_pull_request_summaries_json(
        &json!({ "data": {
            "s0": { "pullRequest": {
                "number": 7,
                "title": "Merged",
                "url": "https://github.com/acme/web/pull/7",
                "author": { "__typename": "Bot", "login": "renovate", "avatarUrl": "https://a/r.png" },
                "headRefName": "feat/seven",
                "baseRefName": "main",
                "state": "MERGED",
                "mergedAt": "2026-08-24T00:00:00Z",
                "closedAt": "2026-08-24T00:00:00Z",
                "updatedAt": "2026-08-24T00:00:00Z",
                "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": "FAILURE" } } }] },
            } },
            "s1": { "pullRequest": null },
            "s2": { "pullRequest": { "number": 9 } },
            "rateLimit": { "cost": 1 },
        } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(decoded.keys().copied().collect::<Vec<_>>(), vec![0]);
    let summary = &decoded[&0];
    assert_eq!(summary.number, 7);
    assert_eq!(summary.state, PullRequestState::Merged);
    assert_eq!(summary.merged_at.as_deref(), Some("2026-08-24T00:00:00Z"));
    assert_eq!(
        summary.author,
        Some(PullRequestActor {
            is_bot: Some(true),
            login: tne("renovate"),
            name: None,
            avatar_url: Some("https://a/r.png".to_owned()),
        })
    );
    assert_eq!(summary.checks_state, Some(PullRequestChecksState::Failing));
    assert_eq!(summary.mergeability, PullRequestMergeability::Unknown);
    assert_eq!(summary.additions, 0);
    // The document did not ask about stacks, so the summary does not claim an answer.
    assert_eq!(summary.stack, None);
}

#[test]
fn summaries_read_stack_membership_only_where_the_document_asked_for_it() {
    assert!(
        build_pull_request_summaries_graphql_query(&[selector("acme/web", 7)], true)
            .unwrap()
            .query
            .contains("stack { number size baseRefName } stackEntry { position }")
    );
    assert!(
        !build_pull_request_summaries_graphql_query(&[selector("acme/web", 7)], false)
            .unwrap()
            .query
            .contains("stack {")
    );
    let pull_request = |number: u64, stack: Value| {
        json!({ "pullRequest": merged(
            &json!({
                "number": number,
                "title": "Stacked",
                "url": format!("https://github.com/acme/web/pull/{number}"),
                "headRefName": format!("feat/{number}"),
                "baseRefName": "main",
                "state": "OPEN",
                "updatedAt": "2026-08-24T00:00:00Z",
            }),
            stack,
        ) })
    };
    let decoded = decode_pull_request_summaries_json(
        &json!({ "data": {
            "s0": pull_request(7, json!({ "stack": null, "stackEntry": null })),
            "s1": pull_request(8, json!({
                "stack": { "number": 3, "size": 2, "baseRefName": "main" },
                "stackEntry": { "position": 2 },
            })),
        } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(decoded[&0].stack, Some(None));
    assert_eq!(
        decoded[&1].stack,
        Some(Some(PullRequestStackMembership {
            number: positive(3),
            size: positive(2),
            base: tne("main"),
            position: positive(2),
        }))
    );
}

#[test]
fn the_single_summary_query_is_aliased_the_way_the_batch_is() {
    assert_eq!(
        pull_request_summary_graphql_query(false),
        format!(
            "query($owner: String!, $name: String!, $number: Int!) {{\n  s0: repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{ {PULL_REQUEST_SUMMARY_SELECTION} }} }}\n}}"
        )
    );
    assert!(pull_request_summary_graphql_query(true).contains(&format!(
        "{PULL_REQUEST_SUMMARY_SELECTION} {STACK_MEMBERSHIP_SELECTION} }}"
    )));
}

// ---------------------------------------------------------------------------------------------
// pull request watch fingerprints
// ---------------------------------------------------------------------------------------------

#[test]
fn watch_fingerprints_ask_for_every_pull_request_in_one_aliased_read_and_refuse_an_unsafe_selector()
{
    let document = build_pull_request_watch_fingerprints_graphql_query(&[
        selector("pingdotgg/t3code", 7),
        selector("pingdotgg/lakebed", 8),
    ])
    .expect("a document");
    assert!(document.query.contains(
        "w0: repository(owner: $w0_owner, name: $w0_name) { pullRequest(number: $w0_number)"
    ));
    assert!(
        document
            .query
            .contains("w1: repository(owner: $w1_owner, name: $w1_name)")
    );
    assert_eq!(
        Value::Object(document.variables),
        json!({
            "w0_owner": "pingdotgg", "w0_name": "t3code", "w0_number": 7,
            "w1_owner": "pingdotgg", "w1_name": "lakebed", "w1_number": 8,
        })
    );
    assert_eq!(
        build_pull_request_watch_fingerprints_graphql_query(&[selector(r#"evil") { x"#, 1)]),
        None
    );
}

#[test]
fn watch_fingerprints_move_status_with_checks_and_remarks_with_comments_each_without_the_other() {
    struct Input {
        running: u64,
        failed: u64,
        comment_edited_at: Option<&'static str>,
        reviews: u64,
    }
    let pull_request = |input: &Input| {
        json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "headRefOid": "abc123",
            "comments": { "totalCount": 1, "nodes": [{ "lastEditedAt": input.comment_edited_at }] },
            "reviews": { "totalCount": input.reviews, "nodes": [] },
            "reviewThreads": { "totalCount": 0 },
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
                "checkRunCountsByState": [
                    { "state": "IN_PROGRESS", "count": input.running },
                    { "state": "FAILURE", "count": input.failed },
                    { "state": "SUCCESS", "count": 0 },
                ],
                "statusContextCountsByState": null,
            } } } }] },
        })
    };
    let decode = |pull_requests: Vec<Value>| {
        let data: Map<String, Value> = pull_requests
            .into_iter()
            .enumerate()
            .map(|(index, value)| (format!("w{index}"), json!({ "pullRequest": value })))
            .collect();
        decode_pull_request_watch_fingerprints_json(&json!({ "data": data }).to_string()).unwrap()
    };
    let base = || Input {
        running: 2,
        failed: 0,
        comment_edited_at: None,
        reviews: 1,
    };
    let fingerprint = |input: Input| decode(vec![pull_request(&input)]).swap_remove(&0).unwrap();
    let before = fingerprint(base());
    let check_finished = fingerprint(Input {
        running: 1,
        failed: 1,
        ..base()
    });
    let summary_edited = fingerprint(Input {
        comment_edited_at: Some("2026-10-05T23:47:43Z"),
        ..base()
    });
    let replied = fingerprint(Input {
        reviews: 2,
        ..base()
    });

    assert_ne!(check_finished.status, before.status);
    assert_eq!(check_finished.remarks, before.remarks);
    assert_ne!(summary_edited.remarks, before.remarks);
    assert_eq!(summary_edited.status, before.status);
    assert_ne!(replied.remarks, before.remarks);
    // A pull request GitHub had no answer for is left for a full read.
    assert!(!decode(vec![Value::Null, pull_request(&base())]).contains_key(&0));
    assert_eq!(before.status, "OPEN MERGEABLE abc123 IN_PROGRESS:2 ");
    assert_eq!(before.remarks, "1  1  0");
}

// ---------------------------------------------------------------------------------------------
// core
// ---------------------------------------------------------------------------------------------

const CORE_PULL_REQUEST: &str = r#"{
    "number": 7,
    "title": "Core",
    "url": "https://github.com/acme/web/pull/7",
    "body": "Body",
    "state": "OPEN",
    "isDraft": false,
    "mergeable": "MERGEABLE",
    "reviewDecision": null,
    "additions": 3,
    "deletions": 1,
    "changedFiles": 2,
    "createdAt": "2026-07-01T00:00:00Z",
    "updatedAt": "2026-07-02T00:00:00Z",
    "mergedAt": null,
    "closedAt": null,
    "headRefName": "feat/core",
    "baseRefName": "main",
    "headRefOid": "abc",
    "isCrossRepository": false,
    "headRepositoryOwner": { "login": "acme" },
    "author": { "login": "octocat", "avatarUrl": "https://a/o.png", "id": "U_1", "name": "Octo" },
    "autoMergeRequest": null,
    "viewerCanUpdate": true,
    "viewerDidAuthor": false,
    "viewerCanUpdateBranch": true,
    "baseRef": { "compare": { "behindBy": 4 } },
    "reviewRequests": { "nodes": [
        { "requestedReviewer": { "login": "hubot", "name": "Hubot" } },
        { "requestedReviewer": { "slug": "web", "combinedSlug": "acme/web", "name": "Web", "avatarUrl": "https://a/t.png" } },
        { "requestedReviewer": null }
    ] },
    "labels": { "nodes": [{ "name": "bug", "color": "d73a4a" }] },
    "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
        "nodes": [
            {
                "__typename": "CheckRun",
                "name": "build",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "checkSuite": { "workflowRun": { "workflow": { "name": "CI" } } }
            },
            { "__typename": "StatusContext", "context": "ci/legacy", "state": "PENDING" }
        ],
        "pageInfo": { "hasNextPage": true }
    } } } }] }
}"#;

fn core_json(overrides: Value) -> Value {
    let mut repository = object(json!({
        "mergeCommitAllowed": true,
        "squashMergeAllowed": true,
        "rebaseMergeAllowed": false,
        "viewerPermission": "WRITE",
    }));
    repository.insert(
        "pullRequest".into(),
        serde_json::from_str(CORE_PULL_REQUEST).unwrap(),
    );
    repository.extend(object(overrides));
    json!({ "data": { "repository": repository } })
}

#[test]
fn core_decodes_the_detail_the_viewer_the_comparison_and_the_teams_from_one_read() {
    let core = decode_pull_request_core_json(&core_json(json!({})).to_string()).unwrap();
    assert_eq!(core.number, 7);
    assert_eq!(core.state, PullRequestState::Open);
    assert_eq!(core.review_request_logins, vec!["hubot".to_owned()]);
    assert!(core.has_team_review_request);
    assert_eq!(
        core.review_request_teams,
        vec![actor("acme/web", Some("Web"), Some("https://a/t.png"))]
    );
    assert_eq!(core.labels.len(), 1);
    assert_eq!(core.head_sha.as_deref(), Some("abc"));
    assert_eq!(core.head_repository_owner.as_deref(), Some("acme"));
    assert_eq!(core.auto_merge_enabled, Some(false));
    assert_eq!(
        check_pairs(&core.base),
        vec![
            ("build".to_owned(), PullRequestCheckStatus::Success),
            ("ci/legacy".to_owned(), PullRequestCheckStatus::Pending),
        ]
    );
    assert_eq!(core.checks_state, Some(PullRequestChecksState::Pending));
    assert!(core.checks_truncated);
    assert_eq!(
        core.comparison,
        Some(GitHubBaseComparison {
            behind_by: Some(4),
            viewer_can_update: true
        })
    );
    assert_eq!(core.viewer_access, access_with(true, true, true, false));
}

fn access_with(
    can_write: bool,
    can_triage: bool,
    can_update: bool,
    did_author: bool,
) -> GitHubViewerRepositoryAccess {
    GitHubViewerRepositoryAccess {
        access: GitHubViewerAccess {
            can_write,
            can_triage,
            can_update,
            did_author,
            can_update_branch: None,
        },
        merge_capabilities: PullRequestMergeCapabilities {
            merge: true,
            squash: true,
            rebase: false,
        },
    }
}

#[test]
fn core_leaves_the_comparison_out_of_a_closed_pull_request_and_of_a_head_that_cannot_be_compared() {
    let closed = core_json(json!({}));
    let mut closed = closed;
    closed["data"]["repository"]["pullRequest"]["state"] = json!("CLOSED");
    assert_eq!(
        decode_pull_request_core_json(&closed.to_string())
            .unwrap()
            .comparison,
        None
    );
    let mut uncomparable = core_json(json!({}));
    uncomparable["data"]["repository"]["pullRequest"]["baseRef"] = json!({ "compare": null });
    assert_eq!(
        decode_pull_request_core_json(&uncomparable.to_string())
            .unwrap()
            .comparison,
        None
    );
}

#[test]
fn core_fails_on_a_response_missing_a_required_field() {
    let mut missing = core_json(json!({}));
    missing["data"]["repository"]["pullRequest"]
        .as_object_mut()
        .unwrap()
        .remove("viewerCanUpdateBranch");
    assert!(decode_pull_request_core_json(&missing.to_string()).is_err());
}

#[test]
fn check_contexts_page_and_roll_up() {
    let page = decode_pull_request_check_contexts_json(
        &json!({ "data": { "repository": { "pullRequest": {
            "headRefOid": "abc",
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
                "nodes": [
                    { "__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "FAILURE",
                      "checkSuite": { "workflowRun": { "workflow": { "name": "CI" } } } },
                    { "__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS" },
                ],
                "pageInfo": { "hasNextPage": true, "endCursor": "next" },
            } } } }] },
        } } } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(page.head_sha, "abc");
    assert_eq!(page.next_cursor.as_deref(), Some("next"));
    assert_eq!(page.contexts[0].workflow_name.as_deref(), Some("CI"));
    let rolled = pull_request_checks_from_contexts(&page.contexts);
    assert_eq!(rolled.checks_state, Some(PullRequestChecksState::Failing));
    assert_eq!(rolled.checks.len(), 2);
}

#[test]
fn heads_actors_and_previews_decode() {
    let heads = decode_pull_request_heads_json(
        &json!({ "data": { "repository": { "pullRequests": {
            "pageInfo": { "hasNextPage": true, "endCursor": "h2" },
            "nodes": [
                { "number": 4, "headRefOid": "aaa", "isCrossRepository": true, "headRepositoryOwner": { "login": " fork " } },
                { "number": 5, "headRefOid": "bbb" },
            ],
        } } } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(heads.next_cursor.as_deref(), Some("h2"));
    assert_eq!(
        heads.heads,
        vec![
            GitHubPullRequestHead {
                number: 4,
                head_sha: "aaa".to_owned(),
                is_cross_repository: Some(true),
                head_repository_owner: Some("fork".to_owned()),
            },
            GitHubPullRequestHead {
                number: 5,
                head_sha: "bbb".to_owned(),
                is_cross_repository: None,
                head_repository_owner: None,
            },
        ]
    );
    let avatars = decode_actor_avatars_json(
        &json!({ "data": { "nodes": [
            { "login": "octocat", "avatarUrl": "https://a/o.png" },
            null,
            { "login": "nobody" },
        ] } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        avatars.into_iter().collect::<Vec<_>>(),
        vec![("octocat".to_owned(), "https://a/o.png".to_owned())]
    );
    let preview = decode_pull_request_preview_json(
        &json!({ "data": { "repository": { "pullRequest": {
            "number": 9,
            "title": "Preview",
            "url": "https://github.com/acme/web/pull/9",
            "state": "OPEN",
            "isDraft": true,
            "createdAt": "2026-07-01T00:00:00Z",
            "author": { "login": "octocat" },
        } } } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(preview.number, 9);
    assert!(preview.is_draft);
    assert_eq!(preview.state, PullRequestState::Open);
    assert_eq!(preview.author, Some(actor("octocat", None, None)));
}

// ---------------------------------------------------------------------------------------------
// GraphQL document text
// ---------------------------------------------------------------------------------------------

/// Every document, evaluated from the upstream TypeScript (the template literals run under
/// Node with the schema imports stubbed), so the text is compared byte for byte.
#[test]
fn graphql_documents_match_the_upstream_text_byte_for_byte() {
    let upstream: Map<String, Value> = object(
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/pull_request_json/graphql_documents.json"
        ))
        .unwrap(),
    );
    let expected = |key: &str| {
        upstream[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key}"))
            .to_owned()
    };
    let constants: [(&str, &str); 23] = [
        ("ACTOR_AVATARS_GRAPHQL_QUERY", ACTOR_AVATARS_GRAPHQL_QUERY),
        (
            "PULL_REQUEST_PREVIEW_GRAPHQL_QUERY",
            PULL_REQUEST_PREVIEW_GRAPHQL_QUERY,
        ),
        (
            "PULL_REQUEST_ACTIVITY_GRAPHQL_QUERY",
            PULL_REQUEST_ACTIVITY_GRAPHQL_QUERY,
        ),
        ("REVIEW_THREADS_GRAPHQL_QUERY", REVIEW_THREADS_GRAPHQL_QUERY),
        (
            "REVIEW_THREAD_COMMENTS_GRAPHQL_QUERY",
            REVIEW_THREAD_COMMENTS_GRAPHQL_QUERY,
        ),
        (
            "REVIEW_THREAD_REPLY_GRAPHQL_MUTATION",
            REVIEW_THREAD_REPLY_GRAPHQL_MUTATION,
        ),
        (
            "PULL_REQUEST_NODE_ID_GRAPHQL_QUERY",
            PULL_REQUEST_NODE_ID_GRAPHQL_QUERY,
        ),
        (
            "REACTION_SUBJECT_PULL_REQUEST_GRAPHQL_QUERY",
            REACTION_SUBJECT_PULL_REQUEST_GRAPHQL_QUERY,
        ),
        (
            "ADD_REACTION_GRAPHQL_MUTATION",
            ADD_REACTION_GRAPHQL_MUTATION,
        ),
        (
            "REMOVE_REACTION_GRAPHQL_MUTATION",
            REMOVE_REACTION_GRAPHQL_MUTATION,
        ),
        (
            "RESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION",
            RESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION,
        ),
        (
            "UNRESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION",
            UNRESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION,
        ),
        (
            "UPDATE_PULL_REQUEST_GRAPHQL_MUTATION",
            UPDATE_PULL_REQUEST_GRAPHQL_MUTATION,
        ),
        (
            "REVERT_PULL_REQUEST_GRAPHQL_MUTATION",
            REVERT_PULL_REQUEST_GRAPHQL_MUTATION,
        ),
        (
            "UPDATE_ISSUE_COMMENT_GRAPHQL_MUTATION",
            UPDATE_ISSUE_COMMENT_GRAPHQL_MUTATION,
        ),
        (
            "UPDATE_REVIEW_COMMENT_GRAPHQL_MUTATION",
            UPDATE_REVIEW_COMMENT_GRAPHQL_MUTATION,
        ),
        (
            "REVIEW_DISMISSALS_GRAPHQL_QUERY",
            REVIEW_DISMISSALS_GRAPHQL_QUERY,
        ),
        (
            "PULL_REQUEST_HEADS_GRAPHQL_QUERY",
            PULL_REQUEST_HEADS_GRAPHQL_QUERY,
        ),
        (
            "REVIEWER_CANDIDATES_GRAPHQL_QUERY",
            REVIEWER_CANDIDATES_GRAPHQL_QUERY,
        ),
        (
            "LABEL_CANDIDATES_GRAPHQL_QUERY",
            LABEL_CANDIDATES_GRAPHQL_QUERY,
        ),
        (
            "VIEWER_PERMISSIONS_GRAPHQL_QUERY",
            VIEWER_PERMISSIONS_GRAPHQL_QUERY,
        ),
        (
            "PULL_REQUEST_FILES_VIEWED_GRAPHQL_QUERY",
            PULL_REQUEST_FILES_VIEWED_GRAPHQL_QUERY,
        ),
        ("REACTION_GROUPS_FIELDS", REACTION_GROUPS_FIELDS),
    ];
    for (key, actual) in constants {
        assert_eq!(actual, expected(key), "{key}");
    }
    assert_eq!(
        pull_request_core_graphql_query("github.com"),
        expected("core_gh")
    );
    assert_eq!(
        pull_request_core_graphql_query("ghe.example.com"),
        expected("core_ghe")
    );
    assert_eq!(
        pull_request_search_graphql_query(20, true, true),
        expected("search_a")
    );
    assert_eq!(
        pull_request_search_graphql_query(500, false, false),
        expected("search_b")
    );
    assert_eq!(
        pull_request_search_graphql_query(0, false, false),
        expected("search_c")
    );
    assert_eq!(
        pull_request_list_graphql_query(30, true),
        expected("list_a")
    );
    assert_eq!(
        pull_request_list_graphql_query(30, false),
        expected("list_b")
    );
    assert_eq!(
        pull_request_summary_graphql_query(true),
        expected("summary_a")
    );
    assert_eq!(
        pull_request_summary_graphql_query(false),
        expected("summary_b")
    );
    assert_eq!(
        pull_request_check_contexts_graphql_query("github.com"),
        expected("ctx")
    );
    assert_eq!(
        pull_request_check_contexts_graphql_query("x.com"),
        expected("ctx2")
    );
    assert_eq!(
        pull_request_watch_fingerprint_selection(),
        expected("watch")
    );
    assert_eq!(PULL_REQUEST_SUMMARY_SELECTION, expected("summarySel"));
}
