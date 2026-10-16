//! `PullRequestService.test.ts` lines 5930-6117: rewrites a host never claimed, remark rewrites,
//! and what a rewrite or a terminal turn leaves cached in the detail, and the signed-in account
//! a detail names.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::counter;
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn refuses_to_rewrite_anything_on_a_host_that_never_claimed_it() {
    let rewrites = counter();
    let (changed, commented) = (rewrites.clone(), rewrites.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["actions"] = json!(["merge"]);
                    capabilities["mergeMethods"] = json!(["merge"]);
                    capabilities.as_object_mut().unwrap().remove("edit");
                })
                .on_update_change_request(move |_| {
                    changed.fetch_add(1, SeqCst);
                    Ok(())
                })
                .on_update_comment(move |_| {
                    commented.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let rewrite_error = h
        .service
        .update(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "title": "New",
        })))
        .await
        .unwrap_err();
    let comment_error = h
        .service
        .update_comment(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "commentId": "IC_1",
            "kind": "issue-comment",
            "body": "New",
        })))
        .await
        .unwrap_err();

    assert!(
        operation_of(&rewrite_error)
            .1
            .contains("cannot rewrite a change request.")
    );
    assert!(
        operation_of(&comment_error)
            .1
            .contains("cannot rewrite a comment.")
    );
    assert_eq!(rewrites.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn passes_a_rewritten_remark_through_with_the_id_and_kind_it_arrived_under() {
    let received = Arc::new(Mutex::new(None::<serde_json::Value>));
    let recorded = received.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_update_comment(move |input| {
            let kind = match input.kind {
                CommentUpdateKind::IssueComment => "issue-comment",
                CommentUpdateKind::ReviewComment => "review-comment",
            };
            *recorded.lock() = Some(json!({
                "id": input.comment_id,
                "kind": kind,
                "body": input.body,
            }));
            Ok(())
        })],
    )
    .await;

    h.service
        .update_comment(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "commentId": "PRRC_1",
            "kind": "review-comment",
            "body": "Second thoughts",
        })))
        .await
        .unwrap();

    assert_eq!(
        *received.lock(),
        Some(json!({
            "id": "PRRC_1",
            "kind": "review-comment",
            "body": "Second thoughts",
        }))
    );
}

#[tokio::test(start_paused = true)]
async fn refuses_a_remark_rewritten_into_nothing_but_whitespace() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_update_comment(|_| panic!("must not be called"))],
    )
    .await;

    let error = h
        .service
        .update_comment(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "commentId": "IC_1",
            "kind": "issue-comment",
            "body": "   \n  ",
        })))
        .await
        .unwrap_err();

    assert!(
        operation_of(&error)
            .1
            .contains("A comment cannot be empty.")
    );
}

#[tokio::test(start_paused = true)]
async fn forgets_the_cached_detail_after_a_rewrite_or_terminal_turn() {
    let core_calls = counter();
    let counted = core_calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_get_change_request(move |_| {
            counted.fetch_add(1, SeqCst);
            Ok(hosted_change_request("", 1))
        })],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service.detail(&reference).await.unwrap();
    h.service
        .update(&input(with_ref(&reference, json!({ "title": "Renamed" }))))
        .await
        .unwrap();
    h.service.detail(&reference).await.unwrap();
    assert_eq!(core_calls.load(SeqCst), 2);

    h.service
        .refresh_after_turn(&turin_contracts::base_schemas::ProjectId::new("p1").unwrap())
        .await;
    h.service.detail(&reference).await.unwrap();
    assert_eq!(core_calls.load(SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn names_the_signed_in_account_in_the_detail_and_says_nothing_where_the_host_cannot() {
    let named = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request(|_| Ok(hosted_change_request("", 1))),
        ],
    )
    .await;
    let unnamed = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_change_request(|_| Ok(hosted_change_request("", 1)))
                .on_get_viewer(|_, _| {
                    Err(unusable(
                        "github",
                        PullRequestProviderErrorReason::Unauthenticated,
                    ))
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let named_detail =
        serde_json::to_value(named.service.detail(&reference).await.unwrap()).unwrap();
    let unnamed_detail =
        serde_json::to_value(unnamed.service.detail(&reference).await.unwrap()).unwrap();

    assert_eq!(named_detail["viewer"], json!("bilal"));
    assert!(unnamed_detail.get("viewer").is_none());
}
