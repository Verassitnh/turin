use serde_json::json;

use super::super::activity::*;
use super::super::errors::GitHubPullRequestApiError;
use super::super::test_support::*;

#[tokio::test]
async fn skips_the_avatar_lookup_when_a_listing_named_nobody() {
    let harness = Harness::new();
    let avatars = harness
        .cli
        .list_actor_avatars(&ListActorAvatarsInput {
            cwd: "/w".into(),
            repository: "acme/web".into(),
            host: "github.com".into(),
            ids: vec![],
        })
        .await
        .unwrap();
    assert_eq!(avatars.len(), 0);
    assert_eq!(harness.call_count(), 0);
}

#[tokio::test]
async fn looks_up_avatars_by_node_id_in_one_graphql_read() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": { "nodes": [{ "login": "octocat", "avatarUrl": "https://avatars/octocat" }] } })
            .to_string(),
    );
    let avatars = harness
        .cli
        .list_actor_avatars(&ListActorAvatarsInput {
            cwd: "/w".into(),
            repository: "acme/web".into(),
            host: "github.com".into(),
            ids: vec!["MDQ6VXNlcjE=".into()],
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::Value::Object(harness.vars_at(0)),
        json!({ "ids": ["MDQ6VXNlcjE="] })
    );
    assert_eq!(
        avatars.get("octocat").map(String::as_str),
        Some("https://avatars/octocat")
    );
}

#[tokio::test]
async fn asks_a_github_enterprise_host_for_its_own_review_threads() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": { "repository": { "pullRequest": { "reviewThreads": {
            "totalCount": 0, "nodes": []
        } } } } })
        .to_string(),
    );
    harness
        .cli
        .list_review_thread_comments(&GitHubPullRequestTarget::new(
            "/w",
            "acme/web",
            "github.acme.dev",
            7,
        ))
        .await
        .unwrap();
    assert_eq!(harness.call_at(0).host(), "github.acme.dev");
    let vars = harness.vars_at(0);
    assert_eq!(vars["owner"], "acme");
    assert_eq!(vars["name"], "web");
    assert_eq!(vars["number"], 7);
}

use super::super::GitHubPullRequestTarget;

#[tokio::test]
async fn follows_the_cursor_to_the_review_threads_the_first_page_left_behind() {
    let harness = Harness::new();
    harness.once_body(review_threads_page(
        vec![thread("PRRT_1", &["c1"])],
        Some("Y3Vyc29yOjE"),
    ));
    harness.once_body(review_threads_page(vec![thread("PRRT_2", &["c2"])], None));
    let conversation = harness
        .cli
        .list_review_thread_comments(&harness.target(7))
        .await
        .unwrap();
    // The first page asks from the beginning; the next carries on from where it stopped.
    assert_eq!(harness.vars_at(0)["cursor"], serde_json::Value::Null);
    assert_eq!(harness.vars_at(1)["cursor"], "Y3Vyc29yOjE");
    let ids: Vec<_> = conversation
        .comments
        .iter()
        .map(|comment| comment.id.as_str())
        .collect();
    assert_eq!(ids, vec!["c1", "c2"]);
    assert!(!conversation.truncated);
}

#[tokio::test]
async fn stops_at_the_thread_bound_and_says_the_conversation_was_cut_short() {
    // A host that never runs out of pages: the walk has to end itself.
    let harness = Harness::new();
    let mut looping = thread("PRRT_1", &["c1"]);
    looping["comments"] = thread_comments(&["c1"], Some("Y3Vyc29yOjI"), Some(3));
    harness.always(ok(review_threads_page(vec![looping], Some("Y3Vyc29yOjE"))));
    let conversation = harness
        .cli
        .list_review_thread_comments(&harness.target(7))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 10);
    assert!(conversation.truncated);
    assert!(conversation.review_threads_truncated);
}

#[tokio::test]
async fn leaves_a_long_thread_paged_until_the_reader_asks_for_more() {
    let harness = Harness::new();
    let mut long = thread("PRRT_1", &["c1"]);
    long["comments"] = thread_comments(&["c1"], Some("Y3Vyc29yOjI"), Some(3));
    harness.once_body(review_threads_page(vec![long], None));
    let conversation = harness
        .cli
        .list_review_thread_comments(&harness.target(7))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 1);
    let ids: Vec<_> = conversation
        .comments
        .iter()
        .map(|comment| comment.id.as_str())
        .collect();
    assert_eq!(ids, vec!["c1"]);
    assert_eq!(conversation.review_threads[0].comment_count, Some(3));
    assert_eq!(
        conversation.review_threads[0]
            .next_comments_cursor
            .as_ref()
            .map(|cursor| cursor.as_str()),
        Some("Y3Vyc29yOjI")
    );
    assert!(conversation.truncated);
    assert!(!conversation.review_threads_truncated);
}

#[tokio::test]
async fn reads_one_requested_page_from_a_review_thread_cursor() {
    let harness = Harness::new();
    harness.once_body(thread_comments_page(&["c2", "c3"], None, 3, "PR_7"));
    let page = harness
        .cli
        .get_review_thread_comments(&GetReviewThreadCommentsInput {
            target: harness.target(7),
            thread_id: "PRRT_1".into(),
            cursor: "Y3Vyc29yOjI".into(),
        })
        .await
        .unwrap();
    let vars = harness.vars_at(0);
    assert_eq!(vars["owner"], "acme");
    assert_eq!(vars["name"], "web");
    assert_eq!(vars["number"], 7);
    assert_eq!(vars["threadId"], "PRRT_1");
    assert_eq!(vars["cursor"], "Y3Vyc29yOjI");
    assert_eq!(harness.call_count(), 1);
    let ids: Vec<_> = page
        .comments
        .iter()
        .map(|comment| comment.id.as_str())
        .collect();
    assert_eq!(ids, vec!["c2", "c3"]);
    assert_eq!(page.next_cursor, None);
}

#[tokio::test]
async fn refuses_a_review_thread_from_another_pull_request() {
    let harness = Harness::new();
    harness.once_body(thread_comments_page(&["foreign"], None, 1, "PR_8"));
    let error = harness
        .cli
        .get_review_thread_comments(&GetReviewThreadCommentsInput {
            target: harness.target(7),
            thread_id: "PRRT_FOREIGN".into(),
            cursor: "Y3Vyc29yOjI".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(error, GitHubPullRequestApiError::SubjectScope(_)));
}

fn viewer_answer() -> String {
    json!({ "data": { "repository": {
        "mergeCommitAllowed": true,
        "squashMergeAllowed": false,
        "rebaseMergeAllowed": true,
        "viewerPermission": "READ",
        "pullRequest": { "viewerCanUpdate": true, "viewerDidAuthor": true },
    } } })
    .to_string()
}

fn assert_read_only_author(
    access: &crate::github::pull_request_json::GitHubViewerRepositoryAccess,
) {
    assert_eq!(
        (
            access.merge_capabilities.merge,
            access.merge_capabilities.squash,
            access.merge_capabilities.rebase
        ),
        (true, false, true)
    );
    assert!(!access.can_write);
    assert!(!access.can_triage);
    assert!(access.can_update);
    assert!(access.did_author);
}

#[tokio::test]
async fn asks_for_the_readers_standing_on_the_repository_and_on_the_pull_request_at_once() {
    let harness = Harness::new();
    harness.always(ok(viewer_answer()));
    let access = harness
        .cli
        .get_viewer_access(&GetViewerAccessInput {
            target: harness.target(7),
            allow_reserve: false,
        })
        .await
        .unwrap();
    // One request, because both answers hang off the same repository object.
    assert_eq!(harness.call_count(), 1);
    let vars = harness.vars_at(0);
    assert_eq!(
        (vars["owner"].as_str(), vars["name"].as_str()),
        (Some("acme"), Some("web"))
    );
    assert_eq!(vars["number"], 7);
    assert!(
        harness
            .query_at(0)
            .contains("mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed")
    );
    assert_read_only_author(&access);
}

#[tokio::test]
async fn stops_graphql_reads_at_the_protected_reserve_until_reset() {
    let harness = Harness::new();
    harness.always(Ok(output_with(
        viewer_answer(),
        false,
        false,
        graphql_quota(499, RESET_2099),
    )));
    let input = GetViewerAccessInput {
        target: harness.target(7),
        allow_reserve: false,
    };
    harness.cli.get_viewer_access(&input).await.unwrap();
    let error = harness.cli.get_viewer_access(&input).await.unwrap_err();
    match error {
        GitHubPullRequestApiError::Api(crate::github::api::GitHubApiError::Paused(paused)) => {
            assert_eq!(paused.host, "github.com");
            assert_eq!(paused.retry_at, RESET_2099);
        }
        other => panic!("expected a pause, got {other:?}"),
    }
    assert_eq!(harness.call_count(), 1);
}

#[tokio::test]
async fn lets_an_interactive_permission_read_use_the_protected_reserve() {
    let harness = Harness::new();
    harness.once(Ok(output_with(
        viewer_answer(),
        false,
        false,
        graphql_quota(499, RESET_2099),
    )));
    harness.once_body(viewer_answer());
    harness
        .cli
        .get_viewer_access(&GetViewerAccessInput {
            target: harness.target(7),
            allow_reserve: false,
        })
        .await
        .unwrap();
    let access = harness
        .cli
        .get_viewer_access(&GetViewerAccessInput {
            target: harness.target(7),
            allow_reserve: true,
        })
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 2);
    assert_read_only_author(&access);
}

#[tokio::test]
async fn reads_who_may_review_and_who_already_has_in_one_request() {
    let harness = Harness::new();
    harness.always(ok_json(json!({ "data": { "repository": {
        "assignableUsers": {
            "pageInfo": { "hasNextPage": false },
            "nodes": [{ "login": "bilal" }, { "login": "octocat" }, { "login": "hubot" }],
        },
        "pullRequest": {
            "author": { "login": "bilal" },
            "reviewRequests": { "nodes": [{ "requestedReviewer": { "login": "octocat" } }] },
        },
    } } })));
    let list = harness
        .cli
        .list_reviewer_candidates(&harness.target(7))
        .await
        .unwrap();
    // The people, who has been asked and who opened the pull request all hang off the same
    // repository object, so the menu costs one request.
    assert_eq!(harness.call_count(), 1);
    let vars = harness.vars_at(0);
    assert_eq!(
        (vars["owner"].as_str(), vars["name"].as_str()),
        (Some("acme"), Some("web"))
    );
    assert_eq!(vars["number"], 7);
    let candidates: Vec<_> = list
        .candidates
        .iter()
        .map(|candidate| (candidate.login.as_str().to_owned(), candidate.is_requested))
        .collect();
    assert_eq!(
        candidates,
        vec![("octocat".to_owned(), true), ("hubot".to_owned(), false)]
    );
}
