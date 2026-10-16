//! The GitHub halves of three upstream `PullRequestService.test.ts` cases whose other halves are
//! GitLab and cut: which unavailable host a listing reports (1104), that a host filter never reads
//! the other host (1191), and that a reader's search reaches the host (3276).

use std::sync::Arc;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn reports_an_unusable_host_over_a_merely_failing_one() {
    // Two GitHub hosts: the first merely fails, the second cannot run its CLI at all. The listing
    // names the one that cannot run, because that is the fix; the failing one is not the cause.
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "enterprise", "/b", "acme/api").host("github.acme.dev"),
        ],
        vec![FakeProvider::new("github").on_get_viewer(|_, host| {
            if host.as_deref() == Some("github.acme.dev") {
                Err(unusable(
                    "github",
                    PullRequestProviderErrorReason::MissingTool,
                ))
            } else {
                Err(provider_error(
                    "github",
                    "getViewer",
                    PullRequestProviderErrorReason::Failed,
                    "HTTP 500",
                ))
            }
        })],
    )
    .await;

    let error = h
        .service
        .list(&input(json!({ "state": "open" })))
        .await
        .unwrap_err();

    assert!(is_unavailable(&error));
    assert!(error.to_string().contains("gh"));
}

#[tokio::test(start_paused = true)]
async fn narrowing_to_one_host_never_reads_the_other() {
    let read_hosts = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = read_hosts.clone();
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "enterprise", "/b", "acme/api").host("github.acme.dev"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |input| {
                recorded.lock().push(input.repo.host.clone());
                Ok(page(vec![change_request(2, "2026-07-05T00:00:00Z")], false))
            }),
        ],
    )
    .await;

    let result = h
        .service
        .list(&input(
            json!({ "state": "open", "host": "github.acme.dev" }),
        ))
        .await
        .unwrap();

    assert_eq!(*read_hosts.lock(), vec!["github.acme.dev".to_owned()]);
    let serialized = serde_json::to_value(&result).unwrap();
    assert_eq!(
        serialized["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["host"].clone())
            .collect::<Vec<_>>(),
        vec![json!("github.acme.dev")]
    );
}

#[tokio::test(start_paused = true)]
async fn forwards_the_readers_search_to_every_host_it_reads() {
    let queries = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let (single, batched) = (queries.clone(), queries.clone());
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "web", "/b", "acme/web"),
        ],
        vec![
            FakeProvider::new("github")
                .on_list_change_requests(move |input| {
                    single.lock().push(input.query.clone());
                    Ok(page(Vec::new(), false))
                })
                .on_list_change_requests_across(move |input| {
                    batched.lock().push(input.query.clone());
                    Ok(ProviderBatchedChangeRequestPage {
                        items: Vec::new(),
                        truncated: false,
                    })
                }),
        ],
    )
    .await;

    h.service
        .list(&input(
            json!({ "state": "open", "query": "pull requests page" }),
        ))
        .await
        .unwrap();

    let asked = queries.lock().clone();
    assert!(!asked.is_empty());
    assert!(
        asked
            .iter()
            .all(|query| query.as_deref() == Some("pull requests page"))
    );
}
