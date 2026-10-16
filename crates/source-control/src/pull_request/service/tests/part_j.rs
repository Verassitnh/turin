//! `PullRequestService.test.ts` lines 4299-4809: line counts for listed rows, the fresh diff
//! after a revision changes under a cold read, and detail that serves without its activity.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::Notify;
use turin_contracts::pull_request::PullRequestListStatsInput;

use super::{counter, list_json, log, rows, summary_of};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn reads_a_host_s_repositories_in_one_search_and_files_the_rows_back_under_each() {
    let asked: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let separately = log();
    let (searched, individually, gitlab_separately) =
        (asked.clone(), separately.clone(), separately.clone());
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "web", "/b", "acme/web"),
            project("p3", "on gitlab", "/c", "group/project").provider("gitlab"),
        ],
        vec![
            FakeProvider::new("github")
                .on_list_change_requests(move |input| {
                    individually.lock().push(input.repo.repository.clone());
                    Ok(page(Vec::new(), false))
                })
                .on_list_change_requests_across(move |input| {
                    searched.lock().push(input.repositories.clone());
                    Ok(ProviderBatchedChangeRequestPage {
                        items: vec![
                            batched_change_request(1, "acme/web", "2026-07-03T00:00:00Z"),
                            batched_change_request(2, "pingdotgg/t3code", "2026-07-02T00:00:00Z"),
                        ],
                        truncated: false,
                    })
                }),
            // A host with no search across repositories keeps being asked one at a time.
            FakeProvider::new("gitlab").on_list_change_requests(move |input| {
                gitlab_separately.lock().push(input.repo.repository.clone());
                Ok(page(vec![change_request(3, "2026-07-01T00:00:00Z")], false))
            }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(
        *asked.lock(),
        vec![vec!["pingdotgg/t3code".to_owned(), "acme/web".to_owned()]]
    );
    assert_eq!(*separately.lock(), vec!["group/project"]);
    // Ordered by update across every host, and each row under the project whose repository it
    // came from.
    assert_eq!(
        rows(&result, "entries", &["projectId", "number"]),
        json!([["p2", 1], ["p1", 2], ["p3", 3]])
    );
}

#[tokio::test(start_paused = true)]
async fn fills_in_the_line_counts_for_the_rows_it_is_given() {
    let asked = Arc::new(Mutex::new(Vec::<Vec<(String, u64)>>::new()));
    let recorded = asked.clone();
    let h = make_service(
        vec![
            project("p1", "web", "/a", "acme/web"),
            project("p2", "on gitlab", "/b", "group/project").provider("gitlab"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_request_stats(move |input| {
                recorded.lock().push(input.change_requests.clone());
                Ok(vec![ProviderChangeRequestStat {
                    repository: "acme/web".to_owned(),
                    number: 1,
                    additions: 12,
                    deletions: 3,
                }])
            }),
            // Its listing carries the counts already, so it has nothing to be asked.
            FakeProvider::new("gitlab"),
        ],
    )
    .await;

    let result = h
        .service
        .list_stats(&input(json!({
            "refs": [
                { "projectId": "p1", "repository": "acme/web", "number": 1 },
                { "projectId": "p1", "repository": "acme/web", "number": 2 },
                { "projectId": "p2", "repository": "group/project", "number": 3 },
                // Not the repository this project's remote points at, so it is dropped rather than asked.
                { "projectId": "p1", "repository": "evil/repo", "number": 4 },
            ],
        })))
        .await
        .unwrap();

    assert_eq!(
        *asked.lock(),
        vec![vec![("acme/web".to_owned(), 1), ("acme/web".to_owned(), 2)]]
    );
    // Only the rows the host answered for; the other is left with whatever the listing had.
    assert_eq!(
        serde_json::to_value(&result).unwrap()["stats"],
        json!([{
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "additions": 12,
            "deletions": 3,
        }])
    );
}

#[tokio::test(start_paused = true)]
async fn reuses_counts_across_overlapping_pages_until_expiry_explicit_invalidation_or_a_reference_changes()
 {
    let asked = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
    let recorded = asked.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_list_change_request_stats(move |input| {
                recorded.lock().push(
                    input
                        .change_requests
                        .iter()
                        .map(|(_, number)| *number)
                        .collect(),
                );
                Ok(input
                    .change_requests
                    .iter()
                    .map(|(repository, number)| ProviderChangeRequestStat {
                        repository: repository.clone(),
                        number: *number,
                        additions: 12,
                        deletions: 3,
                    })
                    .collect())
            }),
        ],
    )
    .await;
    let stats = |numbers: &[u64]| {
        let refs: Vec<Value> = numbers
            .iter()
            .map(|number| json!({ "projectId": "p1", "repository": "acme/web", "number": number }))
            .collect();
        input::<PullRequestListStatsInput>(json!({ "refs": refs }))
    };

    h.service.list_stats(&stats(&[1, 2])).await.unwrap();
    let overlapping = h.service.list_stats(&stats(&[2, 3])).await.unwrap();
    assert_eq!(
        rows(
            &serde_json::to_value(&overlapping).unwrap(),
            "stats",
            &["number"]
        ),
        json!([[2], [3]])
    );
    h.service.list_stats(&stats(&[1, 2, 3])).await.unwrap();
    assert_eq!(*asked.lock(), vec![vec![1, 2], vec![3]]);

    h.service
        .invalidate(&invalidate_ref(&pr_ref("p1", "acme/web", 2)), false)
        .await;
    h.service.list_stats(&stats(&[1, 2, 3])).await.unwrap();
    assert_eq!(*asked.lock(), vec![vec![1, 2], vec![3], vec![2]]);

    h.clock.adjust(61_000).await;
    h.service.list_stats(&stats(&[1, 2, 3])).await.unwrap();
    assert_eq!(
        *asked.lock(),
        vec![vec![1, 2], vec![3], vec![2], vec![1, 2, 3]]
    );

    h.service
        .refresh_after_turn(&turin_contracts::base_schemas::ProjectId::new("p1").unwrap())
        .await;
    h.service.list_stats(&stats(&[1])).await.unwrap();
    assert_eq!(
        *asked.lock(),
        vec![vec![1, 2], vec![3], vec![2], vec![1, 2, 3], vec![1]]
    );

    h.service.invalidate(&input(json!({})), false).await;
    h.service.list_stats(&stats(&[1])).await.unwrap();
    assert_eq!(
        *asked.lock(),
        vec![
            vec![1, 2],
            vec![3],
            vec![2],
            vec![1, 2, 3],
            vec![1],
            vec![1]
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn reads_the_fresh_diff_when_detail_or_summary_discovers_a_changed_revision() {
    let summary_started = Arc::new(Notify::new());
    let release_summary = Arc::new(Notify::new());
    let revision = Arc::new(Mutex::new("2026-07-02T00:00:00Z".to_owned()));
    let patch = Arc::new(Mutex::new("old patch".to_owned()));
    let diff_calls = counter();
    let (started, release) = (summary_started.clone(), release_summary.clone());
    let counted_summaries = counter();
    let (detail_revision, summary_revision) = (revision.clone(), revision.clone());
    let diff_patch = patch.clone();
    let counted = diff_calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request(move |_| {
                    let mut detail = hosted_change_request("body", 1);
                    detail.base.updated_at = detail_revision.lock().clone();
                    Ok(detail)
                })
                .on_get_change_request_summary_async(move |_| {
                    let (started, release) = (started.clone(), release.clone());
                    let result = change_request(1, &summary_revision.lock().clone());
                    // Only the cold read waits; upstream's release deferred stays resolved after.
                    let cold = counted_summaries.fetch_add(1, SeqCst) == 0;
                    async move {
                        if cold {
                            started.notify_one();
                            release.notified().await;
                        }
                        Ok(summary_of(
                            &result,
                            turin_contracts::pull_request::PullRequestState::Open,
                        ))
                    }
                })
                .on_get_diff(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(ProviderDiffSlice {
                        patch: diff_patch.lock().clone(),
                        truncated: false,
                        next_cursor: None,
                        omitted_file_stats: None,
                    })
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    let diff_of = |service: &crate::pull_request::service::PullRequestService| {
        let service = service.clone();
        async move {
            let result = service
                .diff(&input(
                    json!({ "projectId": "p1", "repository": "acme/web", "number": 1 }),
                ))
                .await
                .unwrap();
            serde_json::to_value(result).unwrap()["patch"].clone()
        }
    };

    let service = h.service.clone();
    let cold_reference = reference.clone();
    let cold_summary = tokio::spawn(async move { service.summary(&cold_reference, None).await });
    summary_started.notified().await;
    h.service.detail(&reference).await.unwrap();
    assert_eq!(diff_of(&h.service).await, json!("old patch"));
    *revision.lock() = "2026-07-02T00:01:00Z".to_owned();
    *patch.lock() = "new patch".to_owned();
    h.clock.adjust(61_000).await;
    h.service.detail(&reference).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap()["updatedAt"],
        json!("2026-07-02T00:01:00Z")
    );
    release_summary.notify_one();
    cold_summary.await.unwrap().unwrap();
    h.service.summary(&reference, Some(false)).await.unwrap();
    assert_eq!(diff_of(&h.service).await, json!("new patch"));
    assert_eq!(diff_calls.load(SeqCst), 2);

    *revision.lock() = "2026-07-02T00:02:00Z".to_owned();
    *patch.lock() = "summary-discovered patch".to_owned();
    h.clock.adjust(61_000).await;
    h.service.summary(&reference, Some(false)).await.unwrap();
    assert_eq!(diff_of(&h.service).await, json!("summary-discovered patch"));
    assert_eq!(diff_calls.load(SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn keeps_the_rows_when_the_line_counts_cannot_be_read() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_list_change_request_stats(|_| Err(request_failed()))],
    )
    .await;

    let result = h
        .service
        .list_stats(&input(json!({
            "refs": [{ "projectId": "p1", "repository": "acme/web", "number": 1 }],
        })))
        .await
        .unwrap();

    assert_eq!(serde_json::to_value(&result).unwrap()["stats"], json!([]));
}

#[tokio::test(start_paused = true)]
async fn serves_core_detail_without_waiting_for_activity_and_shares_activity_between_clients() {
    let core_calls = counter();
    let activity_calls = counter();
    let stats_calls = counter();
    let (core, activity, stats) = (
        core_calls.clone(),
        activity_calls.clone(),
        stats_calls.clone(),
    );
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_list_change_request_stats(move |_| {
                    stats.fetch_add(1, SeqCst);
                    Ok(Vec::new())
                })
                .on_get_change_request(move |_| {
                    core.fetch_add(1, SeqCst);
                    let mut detail = hosted_change_request("Ready before the conversation", 1);
                    detail.changed_files = 2;
                    Ok(detail)
                })
                .on_get_change_request_activity(move |_| {
                    activity.fetch_add(1, SeqCst);
                    Ok(ProviderChangeRequestActivity {
                        comments_truncated: true,
                        review_threads_truncated: Some(true),
                        ..empty_activity()
                    })
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let core_detail = h.service.detail(&reference).await.unwrap();
    let core_json = serde_json::to_value(&core_detail).unwrap();
    assert_eq!(core_json["body"], json!("Ready before the conversation"));
    assert_eq!(core_calls.load(SeqCst), 1);
    assert_eq!(activity_calls.load(SeqCst), 0);

    let counts = h
        .service
        .list_stats(&input(json!({ "refs": [reference.clone()] })))
        .await
        .unwrap();
    assert_eq!(stats_calls.load(SeqCst), 0);
    assert_eq!(
        serde_json::to_value(&counts).unwrap()["stats"],
        json!([{
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "additions": core_json["additions"],
            "deletions": core_json["deletions"],
        }])
    );

    let (first, second) = tokio::join!(
        h.service.activity(&reference),
        h.service.activity(&reference)
    );
    assert_eq!(activity_calls.load(SeqCst), 1);
    for activity in [first.unwrap(), second.unwrap()] {
        assert_eq!(
            serde_json::to_value(activity).unwrap()["reviewThreadsTruncated"],
            json!(true)
        );
    }

    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    h.service.activity(&reference).await.unwrap();
    assert_eq!(activity_calls.load(SeqCst), 2);
}
