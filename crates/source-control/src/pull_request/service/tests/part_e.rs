//! `PullRequestService.test.ts` lines 3245-3533: the host a repository is read from, search
//! and viewer fallbacks, and reviewer requests and candidates. The case that asks every host
//! the reader's search is left out, because its second project is a GitLab one; see the
//! Deviations table in PORTING.md.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::{counter, list_json, log};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn hands_the_provider_the_host_its_repository_lives_on() {
    let hosts = log();
    let pushed = hosts.clone();
    let h = make_service(
        vec![project("p1", "enterprise", "/a", "acme/web").host("github.acme.dev")],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |input| {
                pushed.lock().push(input.repo.host.clone());
                Ok(page(Vec::new(), false))
            }),
        ],
    )
    .await;

    list_json(&h.service, json!({ "state": "open" })).await;

    // The identity a project records is the path below its host, so the host has to travel
    // separately or a GitHub Enterprise repository is read off github.com instead.
    assert_eq!(*hosts.lock(), vec!["github.acme.dev"]);
}

#[tokio::test(start_paused = true)]
async fn asks_for_no_search_when_the_reader_has_typed_nothing() {
    let asked: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = asked.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |input| {
                recorded.lock().push(input.query.clone());
                Ok(page(Vec::new(), false))
            }),
        ],
    )
    .await;

    list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(*asked.lock(), vec![None]);
}

#[tokio::test(start_paused = true)]
async fn asks_another_checkout_who_is_signed_in_when_the_first_one_cannot_answer() {
    let asked = log();
    let recorded = asked.clone();
    let h = make_service(
        vec![
            // One repository, checked out twice. The listing reads it once; the viewer lookup has
            // two places to ask.
            project("p1", "t3code (stale worktree)", "/gone", "pingdotgg/t3code"),
            project("p2", "t3code", "/healthy", "pingdotgg/t3code"),
        ],
        vec![
            FakeProvider::new("github")
                .on_get_viewer(move |cwd, _| {
                    recorded.lock().push(cwd.clone());
                    if cwd == "/gone" {
                        Err(provider_error(
                            "github",
                            "getViewer",
                            PullRequestProviderErrorReason::Failed,
                            "not a git repository",
                        ))
                    } else {
                        Ok("bilal".to_owned())
                    }
                })
                .on_list_change_requests(|_| {
                    Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
                }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // De-duplicating the listing must not throw away the checkouts the fallback needs: the host
    // is readable, so it is read.
    assert_eq!(*asked.lock(), vec!["/gone", "/healthy"]);
    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    assert_eq!(result["providers"][0]["configured"], true);
}

#[tokio::test(start_paused = true)]
async fn refuses_to_ask_for_a_review_on_a_host_that_cannot_before_any_call_is_made() {
    let asked = counter();
    let counted = asked.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["actions"] = json!(["merge"]);
                    capabilities["reviewers"] =
                        json!({ "request": false, "listCandidates": false });
                })
                .on_get_viewer_permissions(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Err(provider_error(
                        "github",
                        "getViewerPermissions",
                        PullRequestProviderErrorReason::Failed,
                        "must not be called",
                    ))
                })
                .on_set_reviewer_request(|_| panic!("must not be called")),
        ],
    )
    .await;

    let error = h
        .service
        .request_reviewers(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "reviewers": [{ "id": "octocat", "kind": "user" }],
            "requested": true,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert!(
        operation_of(&error)
            .1
            .contains("cannot ask somebody for a review.")
    );
    assert_eq!(asked.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_the_candidate_list_on_a_host_that_has_no_such_list_to_give() {
    let listed = counter();
    let counted = listed.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["diff"] = json!(false);
                    capabilities["comment"] = json!(false);
                    capabilities["actions"] = json!(["merge"]);
                    capabilities["search"] = json!(false);
                    // Azure's shape: it takes a reviewer, and names nobody who could be one.
                    capabilities["reviewers"] = json!({ "request": true, "listCandidates": false });
                })
                .on_list_reviewer_candidates(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Err(provider_error(
                        "github",
                        "listReviewerCandidates",
                        PullRequestProviderErrorReason::Failed,
                        "must not be called",
                    ))
                }),
        ],
    )
    .await;

    let error = h
        .service
        .reviewer_candidates(&pr_ref("p1", "acme/web", 1))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert!(
        operation_of(&error)
            .1
            .contains("cannot say who may review a change request.")
    );
    assert_eq!(listed.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_a_review_request_this_viewer_may_not_make_and_says_what_access_it_takes() {
    let sent = counter();
    let counted = sent.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                // The host asks for reviews; this account only reads the repository.
                .on_get_viewer_permissions(|_| {
                    Ok(serde_json::from_value(json!({
                        "actions": ["ready", "draft", "close", "reopen"],
                        "comment": true,
                        "resolve": true,
                        "verdicts": ["comment", "approve", "request-changes"],
                        "requestReviewers": false,
                    }))
                    .unwrap())
                })
                .on_set_reviewer_request(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let error = h
        .service
        .request_reviewers(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "reviewers": [{ "id": "octocat", "kind": "user" }],
            "requested": true,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert!(
        operation_of(&error)
            .1
            .contains("You need write access on this repository to ask for a review.")
    );
    assert_eq!(sent.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn keeps_the_menu_from_a_viewer_who_may_not_ask_which_is_all_it_is_for() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_viewer_permissions(|_| {
                    Ok(serde_json::from_value(json!({
                        "actions": [],
                        "comment": true,
                        "resolve": false,
                        "verdicts": ["comment", "approve", "request-changes"],
                        "requestReviewers": false,
                    }))
                    .unwrap())
                })
                .on_list_reviewer_candidates(|_| panic!("must not be called")),
        ],
    )
    .await;

    let error = h
        .service
        .reviewer_candidates(&pr_ref("p1", "acme/web", 1))
        .await
        .unwrap_err();

    assert!(
        operation_of(&error)
            .1
            .contains("You need write access on this repository to ask for a review.")
    );
}
