//! `PullRequestService.test.ts` lines 4362-4543: carrying a slice on across repositories, the
//! chunks a large workspace is searched in, and the fallback to asking each repository on its
//! own. The search case over two hosts, at 4299, is left out because its second host is GitLab;
//! see the Deviations table in PORTING.md.

use std::sync::Arc;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::{list_json, log, rows};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn carries_every_repository_of_a_slice_on_from_the_oldest_row_in_it() {
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "web", "/b", "acme/web"),
            project("p3", "docs", "/c", "acme/docs"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests_across(|_| {
                Ok(ProviderBatchedChangeRequestPage {
                    items: vec![
                        batched_change_request(1, "acme/web", "2026-07-03T00:00:00Z"),
                        batched_change_request(2, "pingdotgg/t3code", "2026-07-02T00:00:00Z"),
                        batched_change_request(3, "acme/web", "2026-07-02T00:00:00Z"),
                    ],
                    truncated: true,
                })
            }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // The boundary is the oldest row of the whole slice, not of each repository: `acme/web` has
    // been read past its newest row, so only the rows sent at the boundary are named for it.
    // `acme/docs`, which the slice holds nothing of, is not believed on silence alone — it is read
    // on its own, and that read is what says whether it has anything at all.
    assert_eq!(result["truncated"], json!(true));
    assert_eq!(
        result["nextCursors"],
        json!({
            "github.com pingdotgg/t3code": "2026-07-02T00:00:00Z|1|2",
            "github.com acme/web": "2026-07-02T00:00:00Z|2|3",
        })
    );
}

#[tokio::test(start_paused = true)]
async fn carries_a_slice_on_without_sending_the_rows_it_already_sent() {
    let cursors = Arc::new(Mutex::new(Vec::<Option<ProviderListCursor>>::new()));
    let recorded = cursors.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_list_change_requests_across(move |input| {
                recorded.lock().push(input.cursor.clone());
                Ok(ProviderBatchedChangeRequestPage {
                    items: vec![
                        batched_change_request(3, "acme/web", "2026-07-02T00:00:00Z"),
                        batched_change_request(4, "acme/web", "2026-07-02T00:00:00Z"),
                    ],
                    truncated: true,
                })
            }),
        ],
    )
    .await;

    let result = list_json(
        &h.service,
        json!({
            "state": "open",
            "cursors": { "github.com acme/web": "2026-07-02T00:00:00Z|1|3" },
        }),
    )
    .await;

    // The boundary instant is asked for inclusively, so the row already sent at it comes back and
    // is dropped here, and stays named in the next cursor, which has not moved off that instant.
    assert_eq!(
        *cursors.lock(),
        vec![Some(ProviderListCursor {
            updated_before: "2026-07-02T00:00:00Z".to_owned(),
            delivered: 1,
        })]
    );
    assert_eq!(rows(&result, "entries", &["number"]), json!([[4]]));
    assert_eq!(
        result["nextCursors"],
        json!({ "github.com acme/web": "2026-07-02T00:00:00Z|2|3,3,4" })
    );
}

#[tokio::test(start_paused = true)]
async fn reads_a_workspace_larger_than_one_search_in_chunks_and_merges_them() {
    let asked = Arc::new(Mutex::new(Vec::<usize>::new()));
    let recorded = asked.clone();
    let projects = (0..101)
        .map(|index| {
            project(
                format!("p{index}"),
                format!("repo {index}"),
                format!("/w{index}"),
                format!("acme/repo{index}"),
            )
        })
        .collect();
    let h = make_service(
        projects,
        vec![
            FakeProvider::new("github").on_list_change_requests_across(move |input| {
                recorded.lock().push(input.repositories.len());
                Ok(ProviderBatchedChangeRequestPage {
                    items: input
                        .repositories
                        .iter()
                        .enumerate()
                        .map(|(index, repository)| {
                            batched_change_request(
                                index as u64 + 1,
                                repository,
                                "2026-07-02T00:00:00Z",
                            )
                        })
                        .collect(),
                    truncated: false,
                })
            }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(*asked.lock(), vec![100, 1]);
    assert_eq!(result["entries"].as_array().unwrap().len(), 101);
}

#[tokio::test(start_paused = true)]
async fn asks_on_its_own_for_a_repository_a_search_answered_nothing_for() {
    let separately = log();
    let pushed = separately.clone();
    let h = make_service(
        vec![
            project("p1", "web", "/a", "acme/web"),
            project("p2", "docs", "/b", "acme/docs"),
        ],
        vec![
            FakeProvider::new("github")
                .on_list_change_requests(move |input| {
                    pushed.lock().push(input.repo.repository.clone());
                    if input.repo.repository == "acme/docs" {
                        return Err(request_failed());
                    }
                    Ok(page(Vec::new(), false))
                })
                .on_list_change_requests_across(|_| {
                    Ok(ProviderBatchedChangeRequestPage {
                        items: vec![batched_change_request(
                            1,
                            "acme/web",
                            "2026-07-03T00:00:00Z",
                        )],
                        truncated: false,
                    })
                }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // The slice had room and still held nothing of `acme/docs`, which is what a repository GitHub
    // will not search looks like, so it is read the old way, and its failure is still reported
    // against its own project.
    assert_eq!(*separately.lock(), vec!["acme/docs"]);
    assert_eq!(
        result["errors"],
        json!([{
            "projectId": "p2",
            "projectTitle": "docs",
            "message": "acme/docs could not be read.",
        }])
    );
    assert_eq!(rows(&result, "entries", &["number"]), json!([[1]]));
}

#[tokio::test(start_paused = true)]
async fn reads_the_repositories_one_at_a_time_when_the_search_itself_fails() {
    let separately = log();
    let pushed = separately.clone();
    let h = make_service(
        vec![
            project("p1", "web", "/a", "acme/web"),
            project("p2", "docs", "/b", "acme/docs"),
        ],
        vec![
            FakeProvider::new("github")
                .on_list_change_requests(move |input| {
                    pushed.lock().push(input.repo.repository.clone());
                    Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
                })
                .on_list_change_requests_across(|_| Err(request_failed())),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // One failed question about two repositories is not two unreadable repositories.
    let mut read = std::mem::take(&mut *separately.lock());
    read.sort();
    assert_eq!(read, vec!["acme/docs", "acme/web"]);
    assert_eq!(result["errors"], json!([]));
    assert_eq!(result["entries"].as_array().unwrap().len(), 2);
}
