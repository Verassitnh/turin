//! `PullRequestService.test.ts` lines 1-1480: preview and detail caching, host discovery and
//! refinement, listing continuations, and viewer resolution.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::Notify;
use turin_contracts::pull_request::PullRequestState;

use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

fn log() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

/// `reads.splice(0).toSorted()`
fn drain_sorted(reads: &Mutex<Vec<String>>) -> Vec<String> {
    let mut drained = std::mem::take(&mut *reads.lock());
    drained.sort();
    drained
}

fn hosted_with(number: u64, state: PullRequestState) -> ProviderChangeRequestDetail {
    let hosted = hosted_change_request("Description", 1);
    ProviderChangeRequestDetail {
        base: ProviderChangeRequest {
            number,
            state,
            ..hosted.base.clone()
        },
        ..hosted
    }
}

#[tokio::test(start_paused = true)]
async fn caches_narrow_previews_and_invalidates_them_after_refresh_or_mutation() {
    let reads = counter();
    let counted = reads.clone();
    let h = make_service(
        vec![project("p1", "web", "/w", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_viewer(|_, _| panic!("preview must not read the viewer"))
                .on_get_change_request_preview(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(preview_of(&change_request(1, "2026-07-02T00:00:00Z")))
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    let (first, second) =
        tokio::join!(h.service.preview(&reference), h.service.preview(&reference));
    second.unwrap();
    assert_eq!(
        serde_json::to_value(first.unwrap()).unwrap(),
        json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "title": "Change request 1",
            "url": "https://host/pull/1",
            "author": { "login": "octocat", "name": null, "avatarUrl": null },
            "state": "open",
            "isDraft": false,
            "createdAt": "2026-07-01T00:00:00Z",
        })
    );
    assert_eq!(reads.load(SeqCst), 1);
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    h.service.preview(&reference).await.unwrap();
    assert_eq!(reads.load(SeqCst), 2);
    h.service
        .run_action(&input(with_ref(&reference, json!({ "action": "close" }))))
        .await
        .unwrap();
    h.service.preview(&reference).await.unwrap();
    assert_eq!(reads.load(SeqCst), 3);
    h.service.refresh_after_turn(&reference.project_id).await;
    h.service.preview(&reference).await.unwrap();
    assert_eq!(reads.load(SeqCst), 4);
    let error = h
        .service
        .preview(&input(with_ref(
            &reference,
            json!({ "repository": "another/repo" }),
        )))
        .await
        .unwrap_err();
    assert!(is_operation_error(&error));
    assert_eq!(reads.load(SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn keeps_cached_previews_available_and_pauses_uncached_previews_until_quota_resets() {
    let reads = counter();
    let retry_at = chrono::DateTime::parse_from_rfc3339("2099-08-13T14:00:00Z")
        .unwrap()
        .timestamp_millis();
    let counted = reads.clone();
    let h = make_service(
        vec![project("p1", "web", "/w", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_preview(move |reference| {
                if counted.fetch_add(1, SeqCst) + 1 == 2 {
                    return Err(PullRequestProviderError {
                        retry_at: Some(retry_at),
                        ..provider_error(
                            "github",
                            "getChangeRequestPreview",
                            PullRequestProviderErrorReason::RateLimited,
                            "GitHub requests are paused until the rate limit resets.",
                        )
                    });
                }
                Ok(preview_of(&change_request(
                    reference.number,
                    "2026-07-02T00:00:00Z",
                )))
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    h.service.preview(&reference).await.unwrap();
    let limited = h
        .service
        .preview(&pr_ref("p1", "acme/web", 2))
        .await
        .unwrap_err();
    assert!(operation_of(&limited).1.contains("paused"));
    assert_eq!(h.service.preview(&reference).await.unwrap().number.get(), 1);
    for number in [2, 3, 4] {
        let paused = h
            .service
            .preview(&pr_ref("p1", "acme/web", number))
            .await
            .unwrap_err();
        assert!(operation_of(&paused).1.contains("paused"));
    }
    assert_eq!(reads.load(SeqCst), 2);
    h.clock.set_time(retry_at).await;
    assert_eq!(
        h.service
            .preview(&pr_ref("p1", "acme/web", 2))
            .await
            .unwrap()
            .number
            .get(),
        2
    );
    assert_eq!(reads.load(SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn reuses_only_unexpired_detail_for_previews() {
    let preview_reads = counter();
    let counted = preview_reads.clone();
    let h = make_service(
        vec![project("p1", "web", "/w", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request(|_| Ok(hosted_change_request("Description", 1)))
                .on_get_change_request_preview(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(ProviderChangeRequestPreview {
                        title: "Updated title".to_owned(),
                        state: PullRequestState::Closed,
                        ..preview_of(&change_request(1, "2026-07-02T00:00:00Z"))
                    })
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    h.service.detail(&reference).await.unwrap();
    assert_eq!(
        h.service.preview(&reference).await.unwrap().title.as_str(),
        "Change request 1"
    );
    assert_eq!(preview_reads.load(SeqCst), 0);
    h.clock.adjust(61_000).await;
    let preview = h.service.preview(&reference).await.unwrap();
    assert_eq!(preview.title.as_str(), "Updated title");
    assert_eq!(preview.state, PullRequestState::Closed);
    assert_eq!(preview_reads.load(SeqCst), 1);
}

fn reading_provider(
    reads: &Arc<Mutex<Vec<String>>>,
    state_of: fn(u64) -> PullRequestState,
) -> FakeProvider {
    let details = reads.clone();
    let activities = reads.clone();
    FakeProvider::new("github")
        .on_get_change_request(move |reference| {
            details.lock().push(format!("detail #{}", reference.number));
            Ok(hosted_with(reference.number, state_of(reference.number)))
        })
        .on_get_change_request_activity(move |reference| {
            activities
                .lock()
                .push(format!("activity #{}", reference.number));
            Ok(empty_activity())
        })
}

#[tokio::test(start_paused = true)]
async fn shares_detail_and_activity_for_a_minute_and_for_ten_once_merged() {
    let reads = log();
    let h = make_service(
        vec![project("p1", "web", "/w", "acme/web")],
        vec![reading_provider(&reads, |number| match number {
            1 => PullRequestState::Open,
            2 => PullRequestState::Merged,
            _ => PullRequestState::Closed,
        })],
    )
    .await;
    // What every client does on focus or a refresh signal for each panel it shows.
    // #1 is open, #2 merged, and #3 closed, which can reopen and so is held no longer than #1.
    let reread = || async {
        for number in [1, 2, 3] {
            let reference = pr_ref("p1", "acme/web", number);
            h.service.detail(&reference).await.unwrap();
            h.service.activity(&reference).await.unwrap();
        }
        tokio::task::yield_now().await;
        drain_sorted(&reads)
    };
    let everything = vec![
        "activity #1",
        "activity #2",
        "activity #3",
        "detail #1",
        "detail #2",
        "detail #3",
    ];
    assert_eq!(reread().await, everything);
    h.clock.adjust(59_000).await;
    assert_eq!(reread().await, Vec::<String>::new());
    h.clock.adjust(2_000).await;
    assert_eq!(
        reread().await,
        vec!["activity #1", "activity #3", "detail #1", "detail #3"]
    );
    h.clock.adjust(9 * 60_000).await;
    assert_eq!(reread().await, everything);
}

#[tokio::test(start_paused = true)]
async fn re_reads_an_open_pull_request_after_a_turn_but_not_a_merged_one() {
    let reads = log();
    let h = make_service(
        vec![project("p1", "web", "/w", "acme/web")],
        vec![reading_provider(&reads, |number| {
            if number == 1 {
                PullRequestState::Open
            } else {
                PullRequestState::Merged
            }
        })],
    )
    .await;
    // What every client showing #1 (open) and #2 (merged) does on each refresh signal.
    let merged = pr_ref("p1", "acme/web", 2);
    let reread = || async {
        for number in [1, 2] {
            let reference = pr_ref("p1", "acme/web", number);
            h.service.detail(&reference).await.unwrap();
            h.service.activity(&reference).await.unwrap();
        }
        tokio::task::yield_now().await;
        drain_sorted(&reads)
    };

    assert_eq!(
        reread().await,
        vec!["activity #1", "activity #2", "detail #1", "detail #2"]
    );
    for _turn in 0..3 {
        h.service.refresh_after_turn(&merged.project_id).await;
        assert_eq!(reread().await, vec!["activity #1", "detail #1"]);
    }
    // The refresh button still reads a merged pull request again.
    h.service.invalidate(&invalidate_ref(&merged), false).await;
    assert_eq!(reread().await, vec!["activity #2", "detail #2"]);
}

#[tokio::test(start_paused = true)]
async fn does_not_wait_for_an_in_flight_detail_read_to_display_a_preview() {
    let detail_started = Arc::new(Notify::new());
    let release_detail = Arc::new(Notify::new());
    let (started, release) = (detail_started.clone(), release_detail.clone());
    let h = make_service(
        vec![project("p1", "web", "/w", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request_async(move |_| {
                    let (started, release) = (started.clone(), release.clone());
                    async move {
                        started.notify_one();
                        release.notified().await;
                        Ok(hosted_change_request("Description", 1))
                    }
                })
                .on_get_change_request_preview(|_| {
                    Ok(preview_of(&change_request(1, "2026-07-02T00:00:00Z")))
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    let service = h.service.clone();
    let detail_ref = reference.clone();
    let detail = tokio::spawn(async move { service.detail(&detail_ref).await });
    detail_started.notified().await;
    assert_eq!(
        h.service.preview(&reference).await.unwrap().title.as_str(),
        "Change request 1"
    );
    release_detail.notify_one();
    detail.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn keeps_previews_warm_when_another_project_finishes_a_turn() {
    let reads = log();
    let pushed = reads.clone();
    let h = make_service(
        vec![
            project("p1", "web", "/w", "acme/web"),
            project("p2", "docs", "/d", "acme/docs"),
        ],
        vec![
            FakeProvider::new("github").on_get_change_request_preview(move |reference| {
                pushed.lock().push(reference.repo.repository.clone());
                Ok(preview_of(&change_request(1, "2026-07-02T00:00:00Z")))
            }),
        ],
    )
    .await;
    let web = pr_ref("p1", "acme/web", 1);
    let docs = pr_ref("p2", "acme/docs", 1);
    h.service.preview(&web).await.unwrap();
    h.service.preview(&docs).await.unwrap();
    h.service
        .preview(&input(with_ref(&web, json!({ "host": "github.com" }))))
        .await
        .unwrap();
    assert_eq!(*reads.lock(), vec!["acme/web", "acme/docs"]);
    h.service.refresh_after_turn(&web.project_id).await;
    h.service.preview(&web).await.unwrap();
    h.service.preview(&docs).await.unwrap();
    assert_eq!(*reads.lock(), vec!["acme/web", "acme/docs", "acme/web"]);
}

#[tokio::test(start_paused = true)]
async fn uses_full_detail_for_hosts_without_a_narrow_preview() {
    let reads = counter();
    let counted = reads.clone();
    let h = make_service(
        vec![project("p1", "web", "/w", "acme/web").provider("gitlab")],
        vec![FakeProvider::new("gitlab").on_get_change_request(move |_| {
            counted.fetch_add(1, SeqCst);
            Ok(hosted_change_request("Description", 1))
        })],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    let preview = h.service.preview(&reference).await.unwrap();
    assert_eq!(preview.title.as_str(), "Change request 1");
    assert_eq!(reads.load(SeqCst), 1);
    assert!(
        serde_json::to_value(&preview)
            .unwrap()
            .get("body")
            .is_none()
    );
}
