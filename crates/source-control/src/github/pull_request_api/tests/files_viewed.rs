use serde_json::{Value, json};
use turin_contracts::pull_request::PullRequestFileViewedState;

use super::super::errors::GitHubPullRequestApiError;
use super::super::files_viewed::*;
use super::super::test_support::*;
use super::super::{GitHubPullRequestTarget, NODE_ID_CACHE_CAPACITY};

fn viewed_page(index: u32, has_next_page: bool) -> Answer {
    ok_json(
        json!({ "data": { "repository": { "pullRequest": { "files": {
        "pageInfo": { "hasNextPage": has_next_page, "endCursor": format!("cursor-{index}") },
        "nodes": [
            { "path": format!("src/file{index}.ts"), "viewerViewedState": "VIEWED" },
            { "path": format!("src/other{index}.ts"), "viewerViewedState": "UNVIEWED" },
        ],
    } } } } }),
    )
}

fn press(
    target: GitHubPullRequestTarget,
    path: &str,
    viewed: bool,
) -> SetPullRequestFilesViewedInput {
    SetPullRequestFilesViewedInput {
        target,
        files: vec![ViewedChange {
            path: path.into(),
            viewed,
        }],
    }
}

#[tokio::test]
async fn reads_every_page_of_viewed_files_and_says_so_when_there_are_too_many() {
    let harness = Harness::new();
    harness.once(viewed_page(0, true));
    harness.once(viewed_page(1, true));
    harness.once(viewed_page(2, false));
    let viewed = harness
        .cli
        .get_pull_request_files_viewed(&harness.target(7))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 3);
    // The first page asks from the start; each one after it carries the cursor before it.
    assert_eq!(
        [
            harness.vars_at(0)["after"].clone(),
            harness.vars_at(1)["after"].clone(),
            harness.vars_at(2)["after"].clone()
        ],
        [Value::Null, json!("cursor-0"), json!("cursor-1")]
    );
    assert!(!viewed.truncated);
    let files: Vec<_> = viewed
        .files
        .iter()
        .map(|file| (file.path.clone(), file.state))
        .collect();
    use PullRequestFileViewedState::{Unviewed, Viewed};
    assert_eq!(
        files,
        vec![
            ("src/file0.ts".to_owned(), Viewed),
            ("src/other0.ts".to_owned(), Unviewed),
            ("src/file1.ts".to_owned(), Viewed),
            ("src/other1.ts".to_owned(), Unviewed),
            ("src/file2.ts".to_owned(), Viewed),
            ("src/other2.ts".to_owned(), Unviewed),
        ]
    );
}

#[tokio::test]
async fn stops_paging_viewed_files_rather_than_following_a_change_without_end() {
    let harness = Harness::new();
    harness.always(ok_json(
        json!({ "data": { "repository": { "pullRequest": { "files": {
        "pageInfo": { "hasNextPage": true, "endCursor": "cursor" },
        "nodes": [{ "path": "src/file.ts", "viewerViewedState": "VIEWED" }],
    } } } } }),
    ));
    let viewed = harness
        .cli
        .get_pull_request_files_viewed(&harness.target(7))
        .await
        .unwrap();
    // The same cursor twice is a page GitHub already gave, so reading stops there.
    assert_eq!(harness.call_count(), 2);
    assert!(viewed.truncated);
}

#[tokio::test]
async fn clears_and_restores_a_burst_of_files_in_one_request() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": { "repository": { "pullRequest": { "id": "PR_1" } } } }).to_string(),
    );
    harness.once_body("{}");
    harness
        .cli
        .set_pull_request_files_viewed(&SetPullRequestFilesViewedInput {
            target: harness.target(23),
            files: vec![
                ViewedChange {
                    path: "src/a.ts".into(),
                    viewed: true,
                },
                ViewedChange {
                    path: "src/b.ts".into(),
                    viewed: false,
                },
            ],
        })
        .await
        .unwrap();
    // One request to learn the pull request's node id, one for every press together.
    assert_eq!(harness.call_count(), 2);
    assert!(harness.query_at(1).contains("f0: markFileAsViewed"));
    assert!(harness.query_at(1).contains("f1: unmarkFileAsViewed"));
    assert_eq!(
        Value::Object(harness.vars_at(1)),
        json!({ "pullRequestId": "PR_1", "f0_path": "src/a.ts", "f1_path": "src/b.ts" })
    );
}

#[tokio::test]
async fn asks_the_host_nothing_when_nothing_was_pressed() {
    let harness = Harness::new();
    harness
        .cli
        .set_pull_request_files_viewed(&SetPullRequestFilesViewedInput {
            target: harness.target(7),
            files: vec![],
        })
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 0);
}

#[tokio::test]
async fn looks_a_pull_requests_node_id_up_once_however_often_it_is_written_to() {
    let harness = Harness::new();
    harness.once_body(
        json!({ "data": { "repository": { "pullRequest": { "id": "PR_24" } } } }).to_string(),
    );
    harness.always(ok("{}"));
    let target = harness.target(24);
    harness
        .cli
        .set_pull_request_files_viewed(&press(target.clone(), "src/a.ts", true))
        .await
        .unwrap();
    harness
        .cli
        .set_pull_request_files_viewed(&press(target.clone(), "src/b.ts", true))
        .await
        .unwrap();
    harness
        .cli
        .update_pull_request(
            &crate::github::pull_request_api::actions::UpdatePullRequestInput {
                target,
                title: Some("Ticked through".into()),
                body: None,
            },
        )
        .await
        .unwrap();
    // One lookup, then a mutation per write, every one of them addressed by the id it answered.
    assert_eq!(harness.call_count(), 4);
    assert_eq!(
        Value::Object(harness.vars_at(0)),
        json!({ "owner": "acme", "name": "web", "number": 24 })
    );
    let ids: Vec<_> = (1..=3)
        .map(|index| harness.vars_at(index)["pullRequestId"].clone())
        .collect();
    assert_eq!(ids, vec![json!("PR_24"), json!("PR_24"), json!("PR_24")]);
}

#[tokio::test]
async fn does_not_remember_a_node_id_lookup_that_failed() {
    let harness = Harness::new();
    harness.once_body(r#"{"message":"not found"}"#);
    harness.once_body(
        json!({ "data": { "repository": { "pullRequest": { "id": "PR_25" } } } }).to_string(),
    );
    harness.once_body("{}");
    let write = || async {
        let input = press(harness.target(25), "src/a.ts", true);
        harness.cli.set_pull_request_files_viewed(&input).await
    };
    let error = write().await.unwrap_err();
    assert!(matches!(error, GitHubPullRequestApiError::Read(_)));
    write().await.unwrap();
    assert_eq!(harness.call_count(), 3);
    assert_eq!(harness.vars_at(2)["pullRequestId"], "PR_25");
}

#[tokio::test]
async fn keeps_the_pull_request_being_ticked_through_not_the_one_looked_up_first() {
    // Ordered by insertion alone, a hit does not renew its entry, so the review the reader is
    // working down is the first thing evicted once a listing has walked a cache's worth of cold
    // pull requests, and every press after that pays a round trip again.
    const HOT: u64 = 9_000;
    let harness = Harness::new();
    let lookups = std::sync::Arc::new(parking_lot::Mutex::new(
        std::collections::HashMap::<u64, u32>::new(),
    ));
    {
        let lookups = lookups.clone();
        harness.on(move |call| {
            if !call.is_graphql() || !call.query().contains(NODE_ID_QUERY) {
                return ok("{}");
            }
            let number = call.variables()["number"].as_u64().unwrap();
            *lookups.lock().entry(number).or_default() += 1;
            ok_json(json!({ "data": { "repository": { "pullRequest": { "id": format!("PR_{number}") } } } }))
        });
    }
    let harness = &harness;
    let tick = |number: u64| async move {
        let input = press(harness.target(number), "src/a.ts", true);
        harness.cli.set_pull_request_files_viewed(&input).await
    };
    tick(HOT).await.unwrap();
    // A cache's worth of cold pull requests, with the open one pressed in between each of them.
    for filled in 0..NODE_ID_CACHE_CAPACITY as u64 {
        tick(HOT + 1 + filled).await.unwrap();
        tick(HOT).await.unwrap();
    }
    assert_eq!(lookups.lock().get(&HOT), Some(&1));
}
