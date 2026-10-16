//! `PullRequestService.test.ts` lines 5725-5773: a way of updating a branch that the host or the
//! viewer does not allow is refused before the provider is asked to update it.

use std::sync::Arc;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::pull_request::service::test_support::*;

/// A GitHub fake whose host can update a branch by `host_methods` and whose viewer may use
/// `viewer_methods`, recording what each update was handed.
fn update_host(
    host_methods: Value,
    viewer_methods: Value,
) -> (FakeProvider, Arc<Mutex<Option<Value>>>) {
    let taken: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let recorded = taken.clone();
    let provider = FakeProvider::new("github")
        .capabilities_with(move |capabilities| {
            capabilities["actions"] = json!(["merge", "close", "update-branch"]);
            capabilities["mergeMethods"] = json!(["merge"]);
            capabilities["updateMethods"] = host_methods.clone();
        })
        .on_get_viewer_permissions(move |_| {
            Ok(serde_json::from_value(json!({
                "actions": ["close", "update-branch"],
                "comment": true,
                "resolve": true,
                "verdicts": ["comment"],
                "requestReviewers": false,
                "updateMethods": viewer_methods.clone(),
            }))
            .unwrap())
        })
        .on_run_action(move |input| {
            *recorded.lock() = Some(match input.update_method {
                Some(method) => serde_json::to_value(method).unwrap(),
                None => json!("default"),
            });
            Ok(())
        });
    (provider, taken)
}

#[tokio::test(start_paused = true)]
async fn refuses_a_way_of_updating_a_branch_that_the_host_does_not_allow() {
    // This host brings a stale branch up to date with a merge commit and nothing else.
    let (provider, taken) = update_host(json!(["merge"]), json!(["merge"]));
    let h = make_service(vec![project("p1", "web", "/a", "acme/web")], vec![provider]).await;
    let reference = pr_ref("p1", "acme/web", 1);

    // Asking for a rebase a host does not offer must fail rather than quietly merge instead.
    let error = h
        .service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "update-branch", "updateMethod": "rebase" }),
        )))
        .await
        .unwrap_err();
    assert!(is_operation_error(&error));
    assert_eq!(
        operation_of(&error).1,
        "This host cannot update a branch by rebase."
    );
    assert_eq!(*taken.lock(), None);

    h.service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "update-branch", "updateMethod": "merge" }),
        )))
        .await
        .unwrap();
    assert_eq!(*taken.lock(), Some(json!("merge")));
}

#[tokio::test(start_paused = true)]
async fn refuses_a_way_of_updating_a_branch_that_the_viewer_does_not_allow() {
    // The host can rebase, but this viewer may only merge the base in: the viewer's own gate.
    let (provider, taken) = update_host(json!(["merge", "rebase"]), json!(["merge"]));
    let h = make_service(vec![project("p1", "web", "/a", "acme/web")], vec![provider]).await;
    let reference = pr_ref("p1", "acme/web", 1);

    let error = h
        .service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "update-branch", "updateMethod": "rebase" }),
        )))
        .await
        .unwrap_err();
    assert!(is_operation_error(&error));
    assert_eq!(
        operation_of(&error).1,
        "You need write access on this repository, or to have opened this change request, to update its branch."
    );
    assert_eq!(*taken.lock(), None);
}
