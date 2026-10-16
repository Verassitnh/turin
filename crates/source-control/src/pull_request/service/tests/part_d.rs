//! `PullRequestService.test.ts` lines 2941-3247: review verdicts and conversations, reactions,
//! replies, and merge strategies the host does not offer. Every case here runs on the GitHub
//! fake, so none is left out for a cut host.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use futures::StreamExt;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::{counter, summary_of};
use crate::pull_request::service::test_support::*;
use turin_contracts::pull_request::PullRequestState;

#[tokio::test(start_paused = true)]
async fn refuses_a_review_with_neither_a_summary_nor_a_comment_but_lets_an_approval_through() {
    let approved = counter();
    let counted = approved.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_submit_review(move |_| {
            counted.fetch_add(1, SeqCst);
            Ok(())
        })],
    )
    .await;

    let error = h
        .service
        .submit_review(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "verdict": "comment",
            "body": "   ",
            "comments": [],
        })))
        .await
        .unwrap_err();
    assert!(is_operation_error(&error));

    // An approval is a verdict in itself, so it needs no words.
    h.service
        .submit_review(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "verdict": "approve",
            "body": "",
            "comments": [],
        })))
        .await
        .unwrap();
    assert_eq!(approved.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn refuses_to_resolve_a_conversation_on_a_host_that_cannot() {
    let sent = counter();
    let (resolved, replied) = (sent.clone(), sent.clone());
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["actions"] = json!(["merge"]);
                    capabilities["mergeMethods"] = json!(["merge"]);
                    capabilities["review"] = json!({
                        "inlineComment": true,
                        "reply": false,
                        "resolve": false,
                        "verdicts": ["comment"],
                    });
                })
                .on_set_thread_resolution(move |_| {
                    resolved.fetch_add(1, SeqCst);
                    Ok(())
                })
                .on_reply_to_thread(move |_| {
                    replied.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;
    let resolve_error = h
        .service
        .set_thread_resolution(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "threadId": "t1",
            "resolved": true,
        })))
        .await
        .unwrap_err();
    let reply_error = h
        .service
        .reply_to_thread(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "threadId": "t1",
            "body": "hi",
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&resolve_error));
    assert!(is_operation_error(&reply_error));
    assert_eq!(sent.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_to_react_on_a_host_with_no_reactions() {
    let reacted = counter();
    let counted = reacted.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["reactions"] = json!(false);
                })
                .on_set_reaction(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let error = h
        .service
        .set_reaction(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "content": "heart",
            "reacted": true,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(reacted.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_to_react_on_a_host_whose_capabilities_omit_reactions_entirely() {
    let reacted = counter();
    let counted = reacted.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities.as_object_mut().unwrap().remove("reactions");
                })
                .on_set_reaction(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let error = h
        .service
        .set_reaction(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "content": "heart",
            "reacted": true,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(reacted.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn passes_a_reaction_through_with_its_subject_id_on_a_host_that_has_them() {
    let received = Arc::new(Mutex::new(None::<Value>));
    let recorded = received.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_set_reaction(move |input| {
            *recorded.lock() = Some(json!({
                "subjectId": input.subject_id,
                "content": input.content,
                "reacted": input.reacted,
            }));
            Ok(())
        })],
    )
    .await;

    h.service
        .set_reaction(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "subjectId": "IC_1",
            "content": "heart",
            "reacted": true,
        })))
        .await
        .unwrap();

    assert_eq!(
        *received.lock(),
        Some(json!({ "subjectId": "IC_1", "content": "heart", "reacted": true }))
    );
}

#[tokio::test(start_paused = true)]
async fn invalidates_the_cached_activity_after_reacting_like_the_other_mutations() {
    let activity_calls = counter();
    let counted = activity_calls.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_activity(move |_| {
                counted.fetch_add(1, SeqCst);
                Ok(empty_activity())
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service.refresh_after_turn(&reference.project_id).await;
    let mut refreshes = h.service.subscribe_refreshes();
    let previous_refresh = refreshes.next().await.unwrap();
    h.service.activity(&reference).await.unwrap();
    assert_eq!(activity_calls.load(SeqCst), 1);

    h.service
        .set_reaction(&input(with_ref(
            &reference,
            json!({ "content": "heart", "reacted": true }),
        )))
        .await
        .unwrap();
    assert!(refreshes.next().await.unwrap() > previous_refresh);
    h.service.activity(&reference).await.unwrap();

    assert_eq!(activity_calls.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn refuses_an_empty_reply_before_it_reaches_the_host() {
    let replied = counter();
    let counted = replied.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_reply_to_thread(move |_| {
            counted.fetch_add(1, SeqCst);
            Ok(())
        })],
    )
    .await;

    let error = h
        .service
        .reply_to_thread(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "threadId": "t1",
            "body": "   ",
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(replied.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_a_merge_strategy_the_host_does_not_offer() {
    let ran_with = Arc::new(Mutex::new(None::<Value>));
    let recorded = ran_with.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["actions"] = json!(["merge"]);
                    // Azure DevOps's shape: it squashes as a completion option and has no rebase.
                    capabilities["mergeMethods"] = json!(["merge", "squash"]);
                })
                .on_get_change_request_summary(|_| {
                    Ok(summary_of(
                        &change_request(1, "2026-07-02T00:00:00Z"),
                        PullRequestState::Open,
                    ))
                })
                .on_run_action(move |input| {
                    *recorded.lock() = Some(match input.merge_method {
                        Some(method) => json!(method),
                        None => json!("merge"),
                    });
                    Ok(())
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "pingdotgg/t3code", 1);

    // Every provider maps an unrecognised strategy to its own default, so letting this through
    // would merge with the wrong one rather than fail.
    let error = h
        .service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "merge", "mergeMethod": "rebase" }),
        )))
        .await
        .unwrap_err();
    assert!(is_operation_error(&error));
    assert_eq!(*ran_with.lock(), None);

    h.service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "merge", "mergeMethod": "squash" }),
        )))
        .await
        .unwrap();
    assert_eq!(*ran_with.lock(), Some(json!("squash")));
}
