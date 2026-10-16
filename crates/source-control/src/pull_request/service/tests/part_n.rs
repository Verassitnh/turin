//! `PullRequestService.test.ts` lines 5833-5929: the review filter, judged only on the host that
//! summarises its reviews, and rewrites that send only the words they carry. The GitLab project in
//! the review filter case is incidental data: that host never supplies a review decision.

use std::sync::Arc;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::{counter, list_json};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn judges_the_review_filter_only_on_a_host_that_summarises_its_reviews() {
    let h = make_service(
        vec![
            project("p1", "web", "/a", "acme/web"),
            project("p2", "on gitlab", "/b", "group/project").provider("gitlab"),
        ],
        vec![
            // GitHub answers with the field on every row: null is "nobody has decided yet".
            FakeProvider::new("github").on_list_change_requests(|_| {
                Ok(ProviderChangeRequestPage {
                    items: vec![
                        ProviderChangeRequest {
                            review_decision: Some(None),
                            ..change_request(1, "2026-07-02T00:00:00Z")
                        },
                        ProviderChangeRequest {
                            review_decision: Some(Some(
                                serde_json::from_value(json!("approved")).unwrap(),
                            )),
                            ..change_request(2, "2026-07-02T00:00:00Z")
                        },
                    ],
                    truncated: false,
                    cursor_advance: None,
                    continues: true,
                })
            }),
            // GitLab never supplies the field, so its rows are not the filter's to judge.
            FakeProvider::new("gitlab").on_list_change_requests(|_| {
                Ok(page(vec![change_request(3, "2026-07-02T00:00:00Z")], false))
            }),
        ],
    )
    .await;

    let numbers = |value: &serde_json::Value| {
        let mut numbers: Vec<u64> = value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["number"].as_u64().unwrap())
            .collect();
        numbers.sort();
        numbers
    };

    let none = list_json(
        &h.service,
        json!({ "state": "open", "filters": { "review": "none" } }),
    )
    .await;
    assert_eq!(numbers(&none), vec![1, 3]);

    let approved = list_json(
        &h.service,
        json!({ "state": "open", "filters": { "review": "approved" } }),
    )
    .await;
    assert_eq!(numbers(&approved), vec![2, 3]);
}

#[tokio::test(start_paused = true)]
async fn sends_only_the_words_a_rewrite_carries() {
    let received = Arc::new(Mutex::new(Vec::<(Option<String>, Option<String>)>::new()));
    let recorded = received.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_update_change_request(move |input| {
                recorded.lock().push((input.title, input.body));
                Ok(())
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service
        .update(&input(with_ref(
            &reference,
            json!({ "title": "A better title" }),
        )))
        .await
        .unwrap();
    h.service
        .update(&input(with_ref(&reference, json!({ "body": "" }))))
        .await
        .unwrap();
    h.service
        .update(&input(with_ref(
            &reference,
            json!({ "title": "Both", "body": "at once" }),
        )))
        .await
        .unwrap();

    assert_eq!(
        *received.lock(),
        vec![
            (Some("A better title".to_owned()), None),
            (None, Some(String::new())),
            (Some("Both".to_owned()), Some("at once".to_owned())),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn refuses_a_rewrite_that_changes_nothing_before_any_call_is_made() {
    let updated = counter();
    let counted = updated.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_update_change_request(move |_| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }),
        ],
    )
    .await;

    let error = h
        .service
        .update(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert!(operation_of(&error).1.contains("Nothing was changed."));
    assert_eq!(updated.load(std::sync::atomic::Ordering::SeqCst), 0);
}
