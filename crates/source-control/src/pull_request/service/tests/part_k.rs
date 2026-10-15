//! `PullRequestService.test.ts` lines 4810-5241: linked summaries held and reused for display,
//! the detail a summary enriches, and which state changes get announced to subscribers. The
//! routed-credential cases are not ported yet; see the handoff note in docs/handoff/T07.md.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use futures::StreamExt;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::Notify;
use turin_contracts::pull_request::{
    PullRequestRef, PullRequestReportStateInput, PullRequestState,
};

use super::{counter, log, summary_of};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

/// Lets spawned collectors and background refreshes run, as the upstream `Effect.yieldNow`s do.
async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

/// A detail in `state`, updated at `updated_at`, for the reads that change what they see.
fn detail_in(state: PullRequestState, updated_at: &str) -> ProviderChangeRequestDetail {
    let mut detail = hosted_change_request("body", 1);
    detail.base.state = state;
    detail.base.updated_at = updated_at.to_owned();
    detail
}

/// Every key a state-change subscription has been handed, as `host/repository#number`.
fn announcements(
    service: &crate::pull_request::service::PullRequestService,
) -> Arc<Mutex<Vec<String>>> {
    let announced = log();
    let recorded = announced.clone();
    let mut changes = service.subscribe_state_changes();
    tokio::spawn(async move {
        while let Some(key) = changes.next().await {
            recorded.lock().push(format!(
                "{}/{}#{}",
                key.host.as_str(),
                key.repository.as_str(),
                key.number.get()
            ));
        }
    });
    announced
}

#[tokio::test(start_paused = true)]
async fn shares_linked_summaries_and_reuses_them_for_display_without_asking_the_host_again() {
    let calls = counter();
    let failing = Arc::new(Mutex::new(false));
    let (counted, is_failing) = (calls.clone(), failing.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_summary_async(move |_| {
                let (counted, is_failing) = (counted.clone(), is_failing.clone());
                async move {
                    counted.fetch_add(1, SeqCst);
                    tokio::task::yield_now().await;
                    if *is_failing.lock() {
                        return Err(provider_error(
                            "github",
                            "getChangeRequestSummary",
                            PullRequestProviderErrorReason::Failed,
                            "HTTP 504",
                        ));
                    }
                    Ok(summary_of(
                        &change_request(1, "2026-07-02T00:00:00Z"),
                        PullRequestState::Open,
                    ))
                }
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let (first, second) = tokio::join!(
        h.service.summary(&reference, Some(false)),
        h.service.summary(&reference, Some(false)),
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(calls.load(SeqCst), 1);

    h.clock.adjust(61_000).await;
    *failing.lock() = true;
    let strict = PullRequestRef {
        allow_stale: Some(false),
        ..reference.clone()
    };
    assert!(is_operation_error(
        &h.service.summary(&strict, None).await.unwrap_err()
    ));

    let stale = h.service.summary(&reference, None).await.unwrap();
    assert_eq!(
        serde_json::to_value(&stale).unwrap()["updatedAt"],
        json!("2026-07-02T00:00:00Z")
    );
    // Display reads keep the last title and state rather than asking the host again.
    assert_eq!(calls.load(SeqCst), 2);

    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    assert!(is_operation_error(
        &h.service.summary(&reference, None).await.unwrap_err()
    ));
}

#[tokio::test(start_paused = true)]
async fn answers_a_known_pull_request_immediately_while_the_host_refreshes() {
    let gate = Arc::new(Notify::new());
    let calls = counter();
    let (counted, released) = (calls.clone(), gate.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_async(move |_| {
                let (counted, released) = (counted.clone(), released.clone());
                async move {
                    if counted.fetch_add(1, SeqCst) > 0 {
                        released.notified().await;
                    }
                    Ok(hosted_change_request("cached body", 4))
                }
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let first = h.service.detail(&reference).await.unwrap();
    let first = serde_json::to_value(&first).unwrap();
    assert_eq!(first["body"], json!("cached body"));
    assert_eq!(first["additions"], json!(4));

    h.clock.adjust(61_000).await;
    let second = serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap();
    assert_eq!(second["body"], json!("cached body"));
    assert_eq!(second["additions"], json!(4));
    settle().await;
    assert_eq!(calls.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn does_not_ask_the_host_again_for_a_linked_summary_it_already_holds() {
    let calls = counter();
    let counted = calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_summary(move |_| {
                counted.fetch_add(1, SeqCst);
                Ok(summary_of(
                    &change_request(1, "2026-07-02T00:00:00Z"),
                    PullRequestState::Open,
                ))
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let first = h.service.summary(&reference, None).await.unwrap();
    assert_eq!(first.title.as_str(), "Change request 1");
    h.clock.adjust(61_000).await;
    let second = h.service.summary(&reference, None).await.unwrap();
    assert_eq!(second.title.as_str(), "Change request 1");
    assert_eq!(calls.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn opening_detail_preserves_enriched_linked_summaries_and_updates_draft_and_diff_fields() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request_summary(|_| {
                    let mut summary = summary_of(
                        &change_request(1, "2026-07-02T00:00:00Z"),
                        PullRequestState::Open,
                    );
                    summary.is_draft = Some(true);
                    summary.review_decision =
                        Some(Some(serde_json::from_value(json!("approved")).unwrap()));
                    summary.checks_state =
                        Some(Some(serde_json::from_value(json!("passing")).unwrap()));
                    Ok(summary)
                })
                .on_get_change_request(|_| {
                    let mut detail = hosted_change_request("body", 14);
                    detail.base.deletions = 3;
                    detail.base.mergeability =
                        serde_json::from_value(json!("conflicting")).unwrap();
                    detail.changed_files = 5;
                    Ok(detail)
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service.summary(&reference, None).await.unwrap();
    let detail = serde_json::to_value(h.service.detail(&reference).await.unwrap()).unwrap();
    let summary = serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap();
    assert_eq!(summary["isDraft"], json!(false));
    assert_eq!(summary["author"], detail["author"]);
    assert_eq!(summary["additions"], json!(14));
    assert_eq!(summary["deletions"], json!(3));
    assert_eq!(summary["changedFiles"], json!(5));
    assert_eq!(summary["mergeability"], json!("conflicting"));
    assert_eq!(summary["reviewDecision"], json!("approved"));
    assert_eq!(summary["checksState"], json!("passing"));
}

#[tokio::test(start_paused = true)]
async fn reuses_an_observed_merged_state_for_strict_settlement_reads() {
    let summary_calls = counter();
    let counted = summary_calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request(|_| {
                    Ok(detail_in(PullRequestState::Merged, "2026-07-03T00:00:00Z"))
                })
                .on_get_change_request_summary(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Err(request_failed())
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service.detail(&reference).await.unwrap();

    let summary =
        serde_json::to_value(h.service.summary(&reference, Some(false)).await.unwrap()).unwrap();
    assert_eq!(summary["state"], json!("merged"));
    assert_eq!(summary["updatedAt"], json!("2026-07-03T00:00:00Z"));
    // A strict merged state is the one the host already confirmed, so nothing refreshes it.
    assert_eq!(summary_calls.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn announces_state_a_detail_read_sees_first_or_newly() {
    let detail = Arc::new(Mutex::new(detail_in(
        PullRequestState::Open,
        "2026-07-02T00:00:00Z",
    )));
    let summary_state = Arc::new(Mutex::new(PullRequestState::Open));
    let (current, current_summary) = (detail.clone(), summary_state.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request(move |_| Ok(current.lock().clone()))
                .on_get_change_request_summary(move |_| {
                    Ok(summary_of(
                        &change_request(1, "2026-07-04T00:00:00Z"),
                        *current_summary.lock(),
                    ))
                }),
        ],
    )
    .await;
    let announced = announcements(&h.service);
    let reference = pr_ref("p1", "acme/web", 1);
    let read_detail = || async {
        h.service
            .invalidate(&invalidate_ref(&reference), false)
            .await;
        h.service.detail(&reference).await.unwrap();
        settle().await;
    };

    // First sight announces; the same state again, even from a fresh read, does not.
    read_detail().await;
    read_detail().await;
    assert_eq!(*announced.lock(), vec!["github.com/acme/web#1"]);

    *detail.lock() = detail_in(PullRequestState::Closed, "2026-07-03T00:00:00Z");
    read_detail().await;
    assert_eq!(announced.lock().len(), 2);

    // A summary seeing the merge first does not hide it from the detail read after.
    *summary_state.lock() = PullRequestState::Merged;
    h.service.summary(&reference, Some(false)).await.unwrap();
    settle().await;
    assert_eq!(announced.lock().len(), 2);
    *detail.lock() = detail_in(PullRequestState::Merged, "2026-07-04T00:00:00Z");
    read_detail().await;
    assert_eq!(announced.lock().len(), 3);

    // A detail read older than the merge cannot announce a reopen.
    *detail.lock() = detail_in(PullRequestState::Open, "2026-07-03T00:00:00Z");
    read_detail().await;
    assert_eq!(announced.lock().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn announces_a_reported_state_without_trusting_it() {
    let detail = Arc::new(Mutex::new(detail_in(
        PullRequestState::Open,
        "2026-07-02T00:00:00Z",
    )));
    let current = detail.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request(move |_| Ok(current.lock().clone())),
        ],
    )
    .await;
    let announced = announcements(&h.service);
    let reference = pr_ref("p1", "acme/web", 1);
    let note = |state: PullRequestState| {
        let service = h.service.clone();
        let reference = reference.clone();
        async move {
            service
                .report_state(&PullRequestReportStateInput { reference, state })
                .await
                .unwrap();
            settle().await;
        }
    };
    let read_detail = || async {
        h.service
            .invalidate(&invalidate_ref(&reference), false)
            .await;
        h.service.detail(&reference).await.unwrap();
        settle().await;
    };

    read_detail().await;
    note(PullRequestState::Closed).await;
    note(PullRequestState::Closed).await;
    assert_eq!(announced.lock().len(), 2);
    h.clock.adjust(5 * 60_000).await;
    note(PullRequestState::Closed).await;
    assert_eq!(announced.lock().len(), 3);

    note(PullRequestState::Open).await;
    assert_eq!(announced.lock().len(), 4);

    note(PullRequestState::Merged).await;
    *detail.lock() = detail_in(PullRequestState::Closed, "2026-07-03T00:00:00Z");
    read_detail().await;
    assert_eq!(announced.lock().len(), 6);
}
