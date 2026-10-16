//! `PullRequestService.test.ts` lines 3534-3731: reviewer and label candidates and changes, and
//! the listing cache a repeated or concurrent read answers from.

use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::{counter, list_json, rows};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn hands_the_hosts_own_candidate_list_back_and_asks_for_it_with_the_change_request() {
    let asked_for = std::sync::Arc::new(Mutex::new(None::<u64>));
    let recorded = asked_for.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_list_reviewer_candidates(move |input| {
                *recorded.lock() = Some(input.number);
                Ok(serde_json::from_value(json!({
                    "candidates": [{
                        "id": "octocat",
                        "kind": "user",
                        "login": "octocat",
                        "name": null,
                        "avatarUrl": null,
                        "isRequested": true,
                    }],
                    "truncated": false,
                }))
                .unwrap())
            }),
        ],
    )
    .await;

    let list = h
        .service
        .reviewer_candidates(&pr_ref("p1", "acme/web", 4))
        .await
        .unwrap();

    assert_eq!(*asked_for.lock(), Some(4));
    assert_eq!(
        rows(
            &serde_json::to_value(&list).unwrap(),
            "candidates",
            &["login"]
        ),
        json!([["octocat"]])
    );
}

#[tokio::test(start_paused = true)]
async fn refuses_a_label_change_on_a_host_that_has_not_said_it_takes_one() {
    let changed = counter();
    let counted = changed.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            // The method is there; the capability that would let it be called is not.
            FakeProvider::new("github").on_set_labels(move |_| {
                counted.fetch_add(1, SeqCst);
                Ok(())
            }),
        ],
    )
    .await;

    let error = h
        .service
        .set_labels(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "labels": ["bug"],
            "applied": true,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert!(operation_of(&error).1.contains("cannot change the labels"));
    assert_eq!(changed.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_a_label_change_this_viewer_may_not_make_and_says_what_access_it_takes() {
    let changed = counter();
    let counted = changed.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["labels"] = json!(true);
                })
                .on_get_viewer_permissions(|_| {
                    Ok(serde_json::from_value(json!({
                        "actions": [],
                        "comment": true,
                        "resolve": false,
                        "verdicts": ["comment", "approve", "request-changes"],
                        "requestReviewers": false,
                        "labels": false,
                    }))
                    .unwrap())
                })
                .on_list_label_candidates(|_| panic!("must not be called"))
                .on_set_labels(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let list_error = h
        .service
        .label_candidates(&pr_ref("p1", "acme/web", 1))
        .await
        .unwrap_err();
    assert!(
        operation_of(&list_error)
            .1
            .contains("You need triage access on this repository")
    );

    let error = h
        .service
        .set_labels(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "labels": ["bug"],
            "applied": true,
        })))
        .await
        .unwrap_err();
    assert!(
        operation_of(&error)
            .1
            .contains("You need triage access on this repository")
    );
    assert_eq!(changed.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn hands_a_label_change_to_the_host_and_reads_the_labels_back_for_the_menu() {
    let received = std::sync::Arc::new(Mutex::new(None::<serde_json::Value>));
    let recorded = received.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["labels"] = json!(true);
                })
                .on_list_label_candidates(|_| {
                    Ok(serde_json::from_value(json!({
                        "candidates": [{
                            "name": "bug",
                            "color": null,
                            "description": null,
                            "isApplied": false,
                        }],
                        "truncated": false,
                    }))
                    .unwrap())
                })
                .on_set_labels(move |input| {
                    *recorded.lock() = Some(json!({
                        "labels": input.labels,
                        "applied": input.applied,
                    }));
                    Ok(())
                }),
        ],
    )
    .await;

    let list = h
        .service
        .label_candidates(&pr_ref("p1", "acme/web", 4))
        .await
        .unwrap();
    assert_eq!(
        rows(
            &serde_json::to_value(&list).unwrap(),
            "candidates",
            &["name"]
        ),
        json!([["bug"]])
    );

    h.service
        .set_labels(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 4,
            "labels": ["bug"],
            "applied": false,
        })))
        .await
        .unwrap();
    assert_eq!(
        *received.lock(),
        Some(json!({ "labels": ["bug"], "applied": false }))
    );
}

#[tokio::test(start_paused = true)]
async fn answers_a_repeated_listing_from_cache_and_concurrent_readers_share_one_request() {
    let host_calls = counter();
    let counted = host_calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |_| {
                counted.fetch_add(1, SeqCst);
                Ok(ProviderChangeRequestPage {
                    items: vec![change_request(1, "2026-07-02T00:00:00Z")],
                    truncated: false,
                    cursor_advance: None,
                    continues: false,
                })
            }),
        ],
    )
    .await;

    tokio::join!(
        list_json(&h.service, json!({ "state": "open" })),
        list_json(&h.service, json!({ "state": "open" })),
    );
    list_json(&h.service, json!({ "state": "open" })).await;
    assert_eq!(host_calls.load(SeqCst), 1);

    // A different filter is a different answer, not a cache hit.
    list_json(&h.service, json!({ "state": "all" })).await;
    assert_eq!(host_calls.load(SeqCst), 2);
}
