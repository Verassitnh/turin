//! `PullRequestService.test.ts` lines 3798-3971 and 3972-4032: one cold viewer lookup shared by
//! concurrent lists, the host reads a normal page takes, and listings cached per scope and
//! freshness.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{counter, list_json, rows};
use crate::pull_request::service::PullRequestService;
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn shares_one_cold_viewer_lookup_across_distinct_concurrent_lists() {
    let viewer_calls = counter();
    let list_calls = counter();
    let (viewer_counted, list_counted) = (viewer_calls.clone(), list_calls.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_viewer_async(move |_, _| {
                    let viewer_counted = viewer_counted.clone();
                    async move {
                        viewer_counted.fetch_add(1, SeqCst);
                        tokio::task::yield_now().await;
                        Ok("bilal".to_owned())
                    }
                })
                .on_list_change_requests(move |_| {
                    list_counted.fetch_add(1, SeqCst);
                    Ok(page(Vec::new(), false))
                }),
        ],
    )
    .await;

    let (all, authored, reviewing) = tokio::join!(
        list_json(&h.service, json!({ "state": "open", "involvement": "all" })),
        list_json(
            &h.service,
            json!({ "state": "open", "involvement": "authored" })
        ),
        list_json(
            &h.service,
            json!({ "state": "open", "involvement": "reviewing" })
        ),
    );
    drop((all, authored, reviewing));

    assert_eq!(viewer_calls.load(SeqCst), 1);
    assert_eq!(list_calls.load(SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn uses_five_host_reads_for_the_normal_indexed_repository_page_workflow() {
    let viewer_calls = counter();
    let search_calls = counter();
    let fallback_calls = counter();
    let stats_calls = counter();
    let (viewed, searched, fallback, stats) = (
        viewer_calls.clone(),
        search_calls.clone(),
        fallback_calls.clone(),
        stats_calls.clone(),
    );
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_viewer(move |_, _| {
                    viewed.fetch_add(1, SeqCst);
                    Ok("bilal".to_owned())
                })
                .on_list_change_requests_across(move |input| {
                    searched.fetch_add(1, SeqCst);
                    let all = serde_json::to_value(input.involvement).unwrap() == json!("all");
                    Ok(ProviderBatchedChangeRequestPage {
                        items: if all {
                            vec![batched_change_request(
                                1,
                                "acme/web",
                                "2026-07-02T00:00:00Z",
                            )]
                        } else {
                            Vec::new()
                        },
                        truncated: false,
                    })
                })
                .on_list_change_requests(move |_| {
                    fallback.fetch_add(1, SeqCst);
                    Ok(page(Vec::new(), false))
                })
                .on_list_change_request_stats(move |_| {
                    stats.fetch_add(1, SeqCst);
                    Ok(vec![ProviderChangeRequestStat {
                        repository: "acme/web".to_owned(),
                        number: 1,
                        additions: 3,
                        deletions: 1,
                    }])
                }),
        ],
    )
    .await;

    let baseline = list_json(&h.service, json!({ "state": "open", "involvement": "all" })).await;
    tokio::join!(
        list_json(
            &h.service,
            json!({ "state": "open", "involvement": "authored" })
        ),
        list_json(
            &h.service,
            json!({ "state": "open", "involvement": "reviewing" })
        ),
    );
    let refs: Vec<Value> = baseline["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            json!({
                "projectId": entry["projectId"],
                "repository": entry["repository"],
                "number": entry["number"],
            })
        })
        .collect();
    h.service
        .list_stats(&input(json!({ "refs": refs })))
        .await
        .unwrap();

    assert_eq!(
        (
            viewer_calls.load(SeqCst),
            search_calls.load(SeqCst),
            fallback_calls.load(SeqCst),
            stats_calls.load(SeqCst),
        ),
        (1, 3, 0, 1)
    );
}

#[tokio::test(start_paused = true)]
async fn returns_the_refreshed_listing_on_the_first_read_after_its_cache_expires() {
    let host_calls = counter();
    let counted = host_calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |_| {
                let call = counted.fetch_add(1, SeqCst) + 1;
                Ok(ProviderChangeRequestPage {
                    items: vec![change_request(call as u64, "2026-07-02T00:00:00Z")],
                    truncated: false,
                    cursor_advance: None,
                    continues: false,
                })
            }),
        ],
    )
    .await;

    let first = list_json(&h.service, json!({ "state": "open" })).await;
    assert_eq!(
        first["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["number"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1)]
    );

    h.clock.adjust(31_000).await;
    let refreshed = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(host_calls.load(SeqCst), 2);
    assert_eq!(
        refreshed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["number"].clone())
            .collect::<Vec<_>>(),
        vec![json!(2)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_listing_narrowed_to_some_projects_is_its_own_cache_entry() {
    let asked: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = asked.clone();
    let h = make_service(
        vec![
            project("p1", "web", "/a", "acme/web"),
            project("p2", "docs", "/b", "acme/docs"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests_across(move |input| {
                recorded.lock().push(input.repositories.clone());
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

    list_json(&h.service, json!({ "state": "open" })).await;
    let narrowed = list_json(&h.service, json!({ "state": "open", "projectIds": ["p2"] })).await;

    // The narrowing is part of the key, so it reads its own scope instead of the wider answer.
    assert_eq!(
        *asked.lock(),
        vec![
            vec!["acme/web".to_owned(), "acme/docs".to_owned()],
            vec!["acme/docs".to_owned()],
        ]
    );
    assert_eq!(
        rows(&narrowed, "entries", &["repository"]),
        json!([["acme/docs"]])
    );

    // Asking again with the same narrowing is still the same answer.
    list_json(&h.service, json!({ "state": "open", "projectIds": ["p2"] })).await;
    assert_eq!(asked.lock().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn keeps_listing_freshness_tied_to_read_start_when_filtered_reads_finish_out_of_order() {
    let older_started = Arc::new(Notify::new());
    let release_older = Arc::new(Notify::new());
    let reads = counter();
    let counted = reads.clone();
    let (started, release) = (older_started.clone(), release_older.clone());
    let updated_at = "2026-07-02T00:00:00Z";
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_list_change_requests_async(move |input| {
                counted.fetch_add(1, SeqCst);
                let (started, release) = (started.clone(), release.clone());
                let older =
                    serde_json::to_value(&input.filters).unwrap()["checks"] == json!("failing");
                async move {
                    if older {
                        started.notify_one();
                        release.notified().await;
                    }
                    let (checks_state, mergeability) = if older {
                        ("failing", "mergeable")
                    } else {
                        ("passing", "conflicting")
                    };
                    Ok(ProviderChangeRequestPage {
                        items: vec![ProviderChangeRequest {
                            checks_state: Some(
                                serde_json::from_value(json!(checks_state)).unwrap(),
                            ),
                            mergeability: serde_json::from_value(json!(mergeability)).unwrap(),
                            ..change_request(1, updated_at)
                        }],
                        truncated: false,
                        cursor_advance: None,
                        continues: false,
                    })
                }
            }),
        ],
    )
    .await;
    let older_input = json!({ "state": "open", "filters": { "checks": "failing" } });
    let newer_input = json!({ "state": "open", "filters": { "checks": "passing" } });

    let service: PullRequestService = h.service.clone();
    let older_request = older_input.clone();
    let older_read = tokio::spawn(async move { list_json(&service, older_request).await });
    older_started.notified().await;
    h.clock.adjust(1_000).await;
    let newer = list_json(&h.service, newer_input).await;
    release_older.notify_one();
    let older = older_read.await.unwrap();

    let older_row = &older["entries"][0];
    let newer_row = &newer["entries"][0];
    assert_eq!(older_row["checksState"], json!("failing"));
    assert_eq!(older_row["mergeability"], json!("mergeable"));
    assert_eq!(newer_row["checksState"], json!("passing"));
    assert_eq!(newer_row["mergeability"], json!("conflicting"));
    let older_observed = older_row["observedAt"].as_f64().unwrap();
    let newer_observed = newer_row["observedAt"].as_f64().unwrap();
    assert!(older_observed < newer_observed);

    let cached_older = list_json(&h.service, older_input).await;
    assert_eq!(
        cached_older["entries"][0]["observedAt"],
        older_row["observedAt"]
    );
    assert_eq!(reads.load(SeqCst), 2);
}
