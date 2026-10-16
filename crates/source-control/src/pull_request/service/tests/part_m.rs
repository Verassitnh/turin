//! `PullRequestService.test.ts` lines 5502-5627: filters a host that ignored them is narrowed by
//! locally. The fixtures name GitLab only as incidental data: every host other than GitHub
//! answers unnarrowed, and the assertions are about the service's own filtering.

use pretty_assertions::assert_eq;
use serde_json::json;

use super::{list_json, rows};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;
use turin_contracts::pull_request::PullRequestActor;

/// A change request carrying `labels` by name, as a host lists them.
fn sized(number: u64, updated_at: &str, labels: &[&str]) -> ProviderChangeRequest {
    ProviderChangeRequest {
        labels: labels
            .iter()
            .map(|name| serde_json::from_value(json!({ "name": name, "color": null })).unwrap())
            .collect(),
        ..change_request(number, updated_at)
    }
}

#[tokio::test(start_paused = true)]
async fn narrows_the_rows_of_a_host_that_ignored_the_filters_it_was_handed() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web").provider("gitlab")],
        // Only GitHub narrows a listing for itself; every other host answers unnarrowed, and
        // sending it a draft filter it quietly ignores used to put drafts on a filtered page.
        vec![FakeProvider::new("gitlab").on_list_change_requests(|_| {
            Ok(ProviderChangeRequestPage {
                items: vec![
                    ProviderChangeRequest {
                        is_draft: true,
                        ..change_request(1, "2026-07-02T00:00:00Z")
                    },
                    change_request(2, "2026-07-01T00:00:00Z"),
                ],
                truncated: false,
                cursor_advance: None,
                continues: false,
            })
        })],
    )
    .await;

    let result = list_json(
        &h.service,
        json!({ "state": "open", "filters": { "draft": "hide" } }),
    )
    .await;

    assert_eq!(rows(&result, "entries", &["number"]), json!([[2]]));
}

#[tokio::test(start_paused = true)]
async fn keeps_a_row_of_a_host_that_ignored_the_filters_if_any_name_of_a_label_group_holds() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web").provider("gitlab")],
        vec![FakeProvider::new("gitlab").on_list_change_requests(|_| {
            Ok(ProviderChangeRequestPage {
                items: vec![
                    sized(1, "2026-07-04T00:00:00Z", &["size:S", "bug"]),
                    sized(2, "2026-07-03T00:00:00Z", &["size:XS", "bug"]),
                    sized(3, "2026-07-02T00:00:00Z", &["size:L", "bug"]),
                    sized(4, "2026-07-01T00:00:00Z", &["size:S"]),
                ],
                truncated: false,
                cursor_advance: None,
                continues: false,
            })
        })],
    )
    .await;

    // Either size satisfies the first group; the second group is its own question, so the row
    // carrying a size but no bug goes.
    let result = list_json(
        &h.service,
        json!({
            "state": "open",
            "filters": { "labels": [["size:S", "size:XS"], ["bug"]] },
        }),
    )
    .await;

    assert_eq!(rows(&result, "entries", &["number"]), json!([[1], [2]]));
}

#[tokio::test(start_paused = true)]
async fn resolves_an_author_filter_of_me_to_the_viewer_before_narrowing_a_hosts_rows() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web").provider("gitlab")],
        // Only GitHub narrows a listing for itself, so this fixture's "me" has to be resolved
        // locally too, the same helper both call sites lean on.
        vec![FakeProvider::new("gitlab").on_list_change_requests(|_| {
            Ok(ProviderChangeRequestPage {
                items: vec![
                    change_request(1, "2026-07-02T00:00:00Z"),
                    ProviderChangeRequest {
                        author: Some(PullRequestActor {
                            is_bot: None,
                            login: tne("bilal"),
                            name: None,
                            avatar_url: None,
                        }),
                        ..change_request(2, "2026-07-01T00:00:00Z")
                    },
                ],
                truncated: false,
                cursor_advance: None,
                continues: false,
            })
        })],
    )
    .await;

    let result = list_json(
        &h.service,
        json!({ "state": "open", "filters": { "author": "me" } }),
    )
    .await;

    assert_eq!(rows(&result, "entries", &["number"]), json!([[2]]));
}
