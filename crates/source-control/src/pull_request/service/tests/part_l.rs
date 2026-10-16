//! `PullRequestService.test.ts` lines 5282-5501: which detail and summary reads may overwrite a
//! fresher linked summary, the not-found and transient-failure answers a detail gives, and the
//! auto-merge state a detail carries.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::Notify;
use turin_contracts::pull_request::{PullRequestOperationError, PullRequestRef, PullRequestState};

use super::{counter, settle, summary_of};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

/// The linked summary a host hands back for `changeRequest(1, ...)` with `title` and `state`.
fn titled_summary(title: &str, state: PullRequestState) -> ProviderChangeRequestSummary {
    let mut summary = summary_of(&change_request(1, "2026-07-02T00:00:00Z"), state);
    summary.title = title.to_owned();
    summary
}

#[tokio::test(start_paused = true)]
async fn does_not_let_a_stale_detail_reopen_overwrite_a_fresher_linked_summary() {
    let gate = Arc::new(Notify::new());
    let detail_calls = counter();
    let summary = Arc::new(Mutex::new(("old title".to_owned(), PullRequestState::Open)));
    let (counted, released, current) = (detail_calls.clone(), gate.clone(), summary.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request_async(move |_| {
                    let (counted, released) = (counted.clone(), released.clone());
                    async move {
                        if counted.fetch_add(1, SeqCst) > 0 {
                            released.notified().await;
                        }
                        Ok(hosted_change_request("old body", 4))
                    }
                })
                .on_get_change_request_summary(move |_| {
                    let (title, state) = current.lock().clone();
                    Ok(titled_summary(&title, state))
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let first = serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap();
    assert_eq!(first["title"], json!("Change request 1"));

    *summary.lock() = ("merged title".to_owned(), PullRequestState::Merged);
    h.clock.adjust(61_000).await;
    let settled =
        serde_json::to_value(h.service.summary(&reference, Some(false)).await.unwrap()).unwrap();
    assert_eq!(settled["title"], json!("merged title"));
    assert_eq!(settled["state"], json!("merged"));

    h.clock.adjust(16_000).await;
    let stale = serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap();
    assert_eq!(stale["title"], json!("Change request 1"));
    settle().await;

    let display = serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap();
    assert_eq!(display["title"], json!("merged title"));
    assert_eq!(display["state"], json!("merged"));
    assert_eq!(detail_calls.load(SeqCst), 2);

    *summary.lock() = ("updated after merge".to_owned(), PullRequestState::Merged);
    h.clock.adjust(61_000).await;
    assert_eq!(
        serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap()["title"],
        json!("merged title")
    );
    let refreshed = serde_json::to_value(
        h.service
            .summary(
                &PullRequestRef {
                    allow_stale: Some(false),
                    ..reference.clone()
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(refreshed["title"], json!("updated after merge"));
    assert_eq!(refreshed["state"], json!("merged"));
}

#[tokio::test(start_paused = true)]
async fn does_not_let_a_still_cached_detail_overwrite_a_fresher_linked_summary() {
    let summary = Arc::new(Mutex::new(("old title".to_owned(), PullRequestState::Open)));
    let current = summary.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request(|_| Ok(hosted_change_request("old body", 4)))
                .on_get_change_request_summary(move |_| {
                    let (title, state) = current.lock().clone();
                    Ok(titled_summary(&title, state))
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let first = serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap();
    assert_eq!(first["title"], json!("Change request 1"));

    *summary.lock() = ("merged title".to_owned(), PullRequestState::Merged);
    let settled =
        serde_json::to_value(h.service.summary(&reference, Some(false)).await.unwrap()).unwrap();
    assert_eq!(settled["state"], json!("merged"));

    let cached = serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap();
    assert_eq!(cached["title"], json!("Change request 1"));
    settle().await;

    let display = serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap();
    assert_eq!(display["title"], json!("merged title"));
    assert_eq!(display["state"], json!("merged"));
}

#[tokio::test(start_paused = true)]
async fn tells_the_client_when_the_host_has_no_pull_request_under_that_number() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_get_change_request(|_| {
            Err(provider_error(
                "github",
                "getChangeRequest",
                PullRequestProviderErrorReason::NotFound,
                "Pull request not found. Check the PR number or URL and try again.",
            ))
        })],
    )
    .await;

    let error = h
        .service
        .detail(&pr_ref("p1", "acme/web", 121))
        .await
        .unwrap_err();

    match error {
        crate::pull_request::read_cache::PullRequestReadError::Operation(
            PullRequestOperationError::Operation { reason, .. },
        ) => assert_eq!(serde_json::to_value(reason).unwrap(), json!("not-found")),
        other => panic!("expected an operation error, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn keeps_recent_detail_on_a_transient_refresh_failure_but_not_after_invalidation() {
    let failing = Arc::new(Mutex::new(false));
    let is_failing = failing.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_get_change_request(move |_| {
            if *is_failing.lock() {
                return Err(provider_error(
                    "github",
                    "getChangeRequest",
                    PullRequestProviderErrorReason::Failed,
                    "spawn gh EAGAIN",
                ));
            }
            Ok(hosted_change_request("last good body", 1))
        })],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service.detail(&reference).await.unwrap();
    h.clock.adjust(61_000).await;
    *failing.lock() = true;
    let strict = PullRequestRef {
        allow_stale: Some(false),
        ..reference.clone()
    };
    assert!(is_operation_error(
        &h.service.detail(&strict).await.unwrap_err()
    ));
    let stale = serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap();
    assert_eq!(stale["body"], json!("last good body"));

    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    assert!(is_operation_error(
        &h.service.detail(&reference).await.unwrap_err()
    ));
}

#[tokio::test(start_paused = true)]
async fn carries_an_armed_auto_merge_through_to_the_detail_and_silence_as_silence() {
    async fn detail_with(auto_merge_enabled: Option<bool>) -> serde_json::Value {
        let h = make_service(
            vec![project("p1", "web", "/a", "acme/web")],
            vec![FakeProvider::new("github").on_get_change_request(move |_| {
                let mut detail = hosted_change_request("", 1);
                detail.changed_files = 0;
                detail.auto_merge_enabled = auto_merge_enabled;
                Ok(detail)
            })],
        )
        .await;
        serde_json::to_value(
            h.service
                .detail(&pr_ref("p1", "acme/web", 1))
                .await
                .unwrap(),
        )
        .unwrap()
    }

    assert_eq!(
        detail_with(Some(true)).await["autoMergeEnabled"],
        json!(true)
    );
    assert_eq!(
        detail_with(Some(false)).await["autoMergeEnabled"],
        json!(false)
    );
    // A host that says nothing leaves the field absent rather than claiming the merge is unarmed.
    assert!(detail_with(None).await.get("autoMergeEnabled").is_none());
}
