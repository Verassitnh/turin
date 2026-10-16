//! `PullRequestService.test.ts` lines 4033-4298: what a mutation, a refresh, or a project turn
//! leaves warm, invalidation and refresh notifications to subscribed readers, and failed reads
//! not being cached.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use futures::StreamExt;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::{counter, list_json, log, summary_of};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestInvalidateInput, PullRequestRef, PullRequestState,
};

#[tokio::test(start_paused = true)]
async fn keeps_unrelated_prs_warm_after_a_mutation_explicit_refresh_and_project_turn() {
    let calls = log();
    let pushed = calls.clone();
    let h = make_service(
        vec![
            project("p1", "web", "/a", "acme/web"),
            project("p2", "docs", "/b", "acme/docs"),
        ],
        vec![
            FakeProvider::new("github").on_get_change_request(move |input| {
                pushed
                    .lock()
                    .push(format!("{}/{}", input.repo.repository, input.number));
                Ok(ProviderChangeRequestDetail {
                    base: ProviderChangeRequest {
                        number: input.number,
                        ..hosted_change_request("body", 1).base
                    },
                    ..hosted_change_request("body", 1)
                })
            }),
        ],
    )
    .await;
    let refs = [
        pr_ref("p1", "acme/web", 1),
        pr_ref("p1", "acme/web", 2),
        pr_ref("p2", "acme/docs", 3),
    ];
    let read_all = || async {
        for reference in &refs {
            h.service
                .summary(&with_allow_stale(reference, false), None)
                .await
                .unwrap();
        }
    };
    read_all().await;
    h.service
        .invalidate(
            &PullRequestInvalidateInput {
                reference: Some(PullRequestRef {
                    host: Some(tne("github.com")),
                    ..refs[0].clone()
                }),
                files_viewed_only: None,
            },
            false,
        )
        .await;
    read_all().await;
    assert_eq!(
        *calls.lock(),
        vec!["acme/web/1", "acme/web/2", "acme/docs/3", "acme/web/1"]
    );
    h.service
        .comment(&input(with_ref(&refs[0], json!({ "body": "hello" }))))
        .await
        .unwrap();
    read_all().await;
    assert_eq!(calls.lock()[4..], ["acme/web/1".to_owned()]);
    h.service
        .refresh_after_turn(&ProjectId::new("p1").unwrap())
        .await;
    read_all().await;
    assert_eq!(
        calls.lock()[5..],
        ["acme/web/1".to_owned(), "acme/web/2".to_owned()]
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_matching_pr_numbers_on_different_hosts_separate_and_refreshes_the_serving_project() {
    let hosts = log();
    let pushed = hosts.clone();
    let h = make_service(
        vec![
            project("p1", "public", "/a", "acme/web"),
            project("p2", "enterprise", "/b", "acme/web").host("enterprise.test"),
        ],
        vec![
            FakeProvider::new("github").on_get_change_request(move |input| {
                pushed.lock().push(input.repo.host.clone());
                Ok(hosted_change_request("body", 1))
            }),
        ],
    )
    .await;
    let own = pr_ref("p1", "acme/web", 1);
    let other = PullRequestRef {
        host: Some(tne("enterprise.test")),
        ..own.clone()
    };
    let read_both = || async {
        h.service.summary(&own, None).await.unwrap();
        h.service.summary(&other, None).await.unwrap();
    };
    read_both().await;
    h.service
        .invalidate(
            &PullRequestInvalidateInput {
                reference: Some(PullRequestRef {
                    host: Some(tne("github.com")),
                    ..own.clone()
                }),
                files_viewed_only: None,
            },
            false,
        )
        .await;
    read_both().await;
    assert_eq!(
        *hosts.lock(),
        vec!["github.com", "enterprise.test", "github.com"]
    );
    h.service.invalidate(&invalidate_ref(&own), false).await;
    read_both().await;
    assert_eq!(hosts.lock()[3..], ["github.com".to_owned()]);
    h.service
        .refresh_after_turn(&ProjectId::new("p2").unwrap())
        .await;
    read_both().await;
    assert_eq!(hosts.lock()[4..], ["enterprise.test".to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn does_not_revive_old_summaries_when_project_epochs_are_evicted() {
    let title = Arc::new(Mutex::new("old".to_owned()));
    let current = title.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_get_change_request(move |_| {
            let mut detail = hosted_change_request("body", 1);
            detail.base.title = current.lock().clone();
            Ok(detail)
        })],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    let title_of = |summary: &serde_json::Value| summary["title"].as_str().unwrap().to_owned();
    assert_eq!(
        title_of(
            &serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap()
        ),
        "old"
    );
    *title.lock() = "new".to_owned();
    h.service.refresh_after_turn(&reference.project_id).await;
    assert_eq!(
        title_of(
            &serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap()
        ),
        "new"
    );
    for index in 0..2048 {
        h.service
            .refresh_after_turn(&ProjectId::new(format!("project-{index}")).unwrap())
            .await;
    }
    assert_eq!(
        title_of(
            &serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap()
        ),
        "new"
    );
}

#[tokio::test(start_paused = true)]
async fn explicit_and_turn_invalidations_make_the_next_listing_ask_the_host_again() {
    let host_calls = counter();
    let viewer_calls = counter();
    let (listed, viewed) = (host_calls.clone(), viewer_calls.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_viewer(move |_, _| {
                    viewed.fetch_add(1, SeqCst);
                    Ok("bilal".to_owned())
                })
                .on_list_change_requests(move |_| {
                    listed.fetch_add(1, SeqCst);
                    Ok(page(Vec::new(), false))
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    list_json(&h.service, json!({ "state": "open" })).await;
    h.service.invalidate(&input(json!({})), false).await;
    list_json(&h.service, json!({ "state": "open" })).await;
    assert_eq!(host_calls.load(SeqCst), 2);
    assert_eq!(viewer_calls.load(SeqCst), 2);

    // Forgetting one change request leaves the listings shared.
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    list_json(&h.service, json!({ "state": "open" })).await;
    assert_eq!(host_calls.load(SeqCst), 2);
    h.service.refresh_after_turn(&reference.project_id).await;
    let mut refreshes = h.service.subscribe_refreshes();
    let refresh = refreshes.next().await.unwrap();
    list_json(&h.service, json!({ "state": "open" })).await;
    assert!(refresh > 0);
    assert_eq!(host_calls.load(SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn close_and_reopen_notify_subscribed_readers_after_invalidating_their_cached_state() {
    let host_calls = counter();
    let state = Arc::new(Mutex::new(PullRequestState::Open));
    let (listed, current, next) = (host_calls.clone(), state.clone(), state.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request_summary(move |_| {
                    Ok(summary_of(
                        &change_request(1, "2026-09-16T00:00:00.000Z"),
                        *current.lock(),
                    ))
                })
                .on_run_action(move |input| {
                    *next.lock() = if input.action == PullRequestAction::Close {
                        PullRequestState::Closed
                    } else {
                        PullRequestState::Open
                    };
                    Ok(())
                })
                .on_list_change_requests(move |_| {
                    listed.fetch_add(1, SeqCst);
                    Ok(page(Vec::new(), false))
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service.refresh_after_turn(&reference.project_id).await;
    list_json(&h.service, json!({ "state": "open" })).await;
    assert_eq!(
        serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap()["state"],
        json!("open")
    );
    for (action, expected) in [("close", "closed"), ("reopen", "open")] {
        // The current revision is dropped; the action's own refresh is the one awaited.
        let mut refreshes = h.service.subscribe_refreshes();
        refreshes.next().await.unwrap();
        h.service
            .run_action(&input(with_ref(&reference, json!({ "action": action }))))
            .await
            .unwrap();
        refreshes.next().await.unwrap();
        list_json(&h.service, json!({ "state": "open" })).await;
        let refreshed =
            serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap();
        assert_eq!(refreshed["state"], json!(expected));
    }
    assert_eq!(host_calls.load(SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn explicit_invalidation_refreshes_origin_readers_after_a_routed_host_mutation() {
    let state = Arc::new(Mutex::new(PullRequestState::Open));
    let current = state.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_summary(move |_| {
                Ok(summary_of(
                    &change_request(1, "2026-09-16T00:00:00.000Z"),
                    *current.lock(),
                ))
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    h.service.refresh_after_turn(&reference.project_id).await;
    let mut revision = h.service.subscribe_refreshes().next().await.unwrap();
    // The sync reactor invalidates before reading; it must not notify itself again.
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    assert_eq!(
        h.service.subscribe_refreshes().next().await.unwrap(),
        revision
    );
    assert_eq!(
        serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap()["state"],
        json!("open")
    );

    for next_state in [PullRequestState::Closed, PullRequestState::Open] {
        let mut refreshes = h.service.subscribe_refreshes();
        refreshes.next().await.unwrap();
        *state.lock() = next_state;
        h.service
            .invalidate(&invalidate_ref(&reference), true)
            .await;
        let next_revision = refreshes.next().await.unwrap();
        let summary =
            serde_json::to_value(h.service.summary(&reference, None).await.unwrap()).unwrap();
        assert_eq!(summary["state"], serde_json::to_value(next_state).unwrap());
        assert!(next_revision > revision);
        revision = next_revision;
    }
}

#[tokio::test(start_paused = true)]
async fn does_not_cache_a_failed_listing() {
    let viewer_calls = counter();
    let counted = viewer_calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            // The viewer lookup is what fails the whole listing rather than one repository.
            FakeProvider::new("github").on_get_viewer(move |_, _| {
                if counted.fetch_add(1, SeqCst) == 0 {
                    Err(request_failed())
                } else {
                    Ok("bilal".to_owned())
                }
            }),
        ],
    )
    .await;

    let error = h
        .service
        .list(&input(json!({ "state": "open" })))
        .await
        .unwrap_err();
    assert!(is_operation_error(&error));
    let second = list_json(&h.service, json!({ "state": "open" })).await;
    assert_eq!(viewer_calls.load(SeqCst), 2);
    assert_eq!(second["providers"][0]["configured"], true);
}

/// `{ ...reference, allowStale }`
fn with_allow_stale(reference: &PullRequestRef, allow_stale: bool) -> PullRequestRef {
    PullRequestRef {
        allow_stale: Some(allow_stale),
        ..reference.clone()
    }
}
