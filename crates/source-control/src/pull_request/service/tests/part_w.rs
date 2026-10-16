//! `PullRequestService.test.ts` lines 3732-3797: a replacement read after a canceled lookup gets its
//! own answer, for the listing and for the line counts. Upstream's case joins the canceled entry
//! while its lookup is still finishing cancellation; the Rust coalescer holds only weak references,
//! so a canceled lookup is dropped before a replacement can find it. The case therefore pins the
//! outcome (one fresh answer, asked for once more), not the interleaving.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::Notify;

use super::counter;
use crate::pull_request::service::PullRequestService;
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

/// One read of the operation under test, answered with the number of the one row it names.
async fn read(service: &PullRequestService, operation: &str) -> Vec<u64> {
    match operation {
        "list" => {
            let listed = service
                .list(&input(json!({ "state": "open" })))
                .await
                .unwrap();
            serde_json::to_value(&listed).unwrap()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["number"].as_u64().unwrap())
                .collect()
        }
        "listStats" => {
            let stats = service
                .list_stats(&input(json!({
                    "refs": [{ "projectId": "p1", "repository": "acme/web", "number": 1 }],
                })))
                .await
                .unwrap();
            serde_json::to_value(&stats).unwrap()["stats"]
                .as_array()
                .unwrap()
                .iter()
                .map(|stat| stat["number"].as_u64().unwrap())
                .collect()
        }
        other => panic!("unknown read {other}"),
    }
}

/// Runs the replacement scenario for one read: the first host call never answers and is canceled,
/// and the read after it must be answered by a new host call.
async fn replacement_after_a_canceled_lookup(operation: &'static str) {
    let host_calls = counter();
    let started = Arc::new(Notify::new());
    let lookup = {
        let (host_calls, started) = (host_calls.clone(), started.clone());
        move || {
            let call = host_calls.fetch_add(1, SeqCst) + 1;
            let started = started.clone();
            async move {
                if call == 1 {
                    started.notify_one();
                    // The first lookup never answers; the test cancels it.
                    std::future::pending::<()>().await;
                }
            }
        }
    };
    let (listed_lookup, stats_lookup) = (lookup.clone(), lookup);
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_list_change_requests_async(move |_| {
                    let lookup = listed_lookup.clone();
                    async move {
                        lookup().await;
                        Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
                    }
                })
                .on_list_change_request_stats_async(move |_| {
                    let lookup = stats_lookup.clone();
                    async move {
                        lookup().await;
                        Ok(vec![ProviderChangeRequestStat {
                            repository: "acme/web".to_owned(),
                            number: 1,
                            additions: 3,
                            deletions: 1,
                        }])
                    }
                }),
        ],
    )
    .await;
    let service = h.service.clone();
    let original = tokio::spawn(async move { read(&service, operation).await });
    started.notified().await;

    original.abort();
    assert!(original.await.unwrap_err().is_cancelled());

    let service = h.service.clone();
    let replacement = tokio::spawn(async move { read(&service, operation).await })
        .await
        .unwrap();
    assert_eq!(replacement, vec![1]);
    assert_eq!(host_calls.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn retries_a_replacement_list_read_that_follows_a_canceled_lookup() {
    replacement_after_a_canceled_lookup("list").await;
}

#[tokio::test(start_paused = true)]
async fn retries_a_replacement_list_stats_read_that_follows_a_canceled_lookup() {
    replacement_after_a_canceled_lookup("listStats").await;
}
