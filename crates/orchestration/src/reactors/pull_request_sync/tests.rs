//! Tests for the pure sync decisions. The sweep's host reads are not here.

use serde_json::json;
use turin_contracts::thread_pull_request::{ThreadPullRequestLink, ThreadPullRequestSnapshot};

use super::*;

const NOW_MS: i64 = 1_800_000_000_000;

fn snapshot(state: &str) -> ThreadPullRequestSnapshot {
    serde_json::from_value(json!({
        "state": state,
        "title": "Add reactors",
        "headBranch": "feature",
        "baseBranch": "main",
        "isDraft": false,
        "updatedAt": "2026-10-14T00:00:00.000Z",
        "syncedAt": "2026-10-14T00:00:00.000Z",
    }))
    .expect("snapshot fixture decodes")
}

fn link(state: Option<&str>) -> ThreadPullRequestLink {
    let snapshot = state.map(snapshot);
    serde_json::from_value(json!({
        "host": "github.com",
        "repository": "pingdotgg/t3code",
        "number": 42,
        "url": "https://github.com/pingdotgg/t3code/pull/42",
        "source": "agent",
        "linkedAt": "2026-10-14T00:00:00.000Z",
        "snapshot": snapshot,
        "stack": null,
    }))
    .expect("link fixture decodes")
}

fn entry(unsettled: bool, link: &ThreadPullRequestLink) -> LinkEntry<'_> {
    LinkEntry { unsettled, link }
}

#[test]
fn a_link_without_a_snapshot_is_due() {
    let cadence = SyncCadence::default();
    let link = link(None);
    assert!(cadence.is_due("k", &[entry(true, &link)], NOW_MS));
}

#[test]
fn an_explicit_request_is_due_even_for_a_merged_pull_request() {
    let mut cadence = SyncCadence::default();
    let merged = link(Some("merged"));
    cadence.request("k", 1);
    assert!(cadence.is_due("k", &[entry(true, &merged)], NOW_MS));
}

#[test]
fn a_retried_stack_is_due() {
    let mut cadence = SyncCadence::default();
    let merged = link(Some("merged"));
    cadence.retry_stack("k");
    assert!(cadence.is_due("k", &[entry(true, &merged)], NOW_MS));
}

#[test]
fn an_all_merged_set_of_active_links_is_never_due() {
    let mut cadence = SyncCadence::default();
    cadence.record_sync("k", NOW_MS - 60 * 60 * 1_000);
    let merged = link(Some("merged"));
    assert!(!cadence.is_due("k", &[entry(true, &merged)], NOW_MS));
}

#[test]
fn an_open_active_link_is_due_on_every_pass() {
    let mut cadence = SyncCadence::default();
    cadence.record_sync("k", NOW_MS - 1_000);
    let open = link(Some("open"));
    assert!(cadence.is_due("k", &[entry(true, &open)], NOW_MS));
}

#[test]
fn a_closed_link_waits_for_the_slow_interval() {
    let mut cadence = SyncCadence::default();
    let closed = link(Some("closed"));
    cadence.record_sync("k", NOW_MS - 60_000);
    assert!(!cadence.is_due("k", &[entry(true, &closed)], NOW_MS));

    cadence.record_sync("k", NOW_MS - SLOW_SYNC_INTERVAL_MS);
    assert!(cadence.is_due("k", &[entry(true, &closed)], NOW_MS));
}

#[test]
fn a_closed_link_never_synced_is_due() {
    let cadence = SyncCadence::default();
    let closed = link(Some("closed"));
    assert!(cadence.is_due("k", &[entry(true, &closed)], NOW_MS));
}

#[test]
fn settled_threads_do_not_keep_a_merged_set_due_but_unsettling_one_does() {
    let mut cadence = SyncCadence::default();
    cadence.record_sync("k", NOW_MS - 60_000);
    let closed = link(Some("closed"));
    // The only thread is settled: no active link, so the all-merged rule holds and nothing is due.
    assert!(!cadence.is_due("k", &[entry(false, &closed)], NOW_MS));
    // Once unsettled, the closed link waits on its cadence clock, which has kept running.
    cadence.record_sync("k", NOW_MS - SLOW_SYNC_INTERVAL_MS);
    assert!(cadence.is_due("k", &[entry(true, &closed)], NOW_MS));
}

#[test]
fn snapshot_fields_compare_equal_when_only_synced_at_differs() {
    let stored = snapshot("open");
    let mut later = stored.clone();
    later.synced_at = "2026-10-15T00:00:00.000Z".to_owned();
    assert!(snapshot_fields_equal(
        &stored_snapshot_fields(&stored),
        &stored_snapshot_fields(&later),
    ));
}

#[test]
fn snapshot_fields_differ_when_the_title_changes() {
    let stored = snapshot("open");
    let mut renamed = stored.clone();
    renamed.title = turin_contracts::base_schemas::TrimmedNonEmptyString::new("Renamed")
        .expect("non-empty title");
    assert!(!snapshot_fields_equal(
        &stored_snapshot_fields(&stored),
        &stored_snapshot_fields(&renamed),
    ));
}

#[test]
fn an_absent_closed_time_and_a_null_one_are_the_same_field_value() {
    let absent = stored_snapshot_fields(&snapshot("open"));
    let mut null = absent.clone();
    null.closed_at = None;
    assert!(snapshot_fields_equal(&absent, &null));
}

#[test]
fn stacks_compare_by_identity_and_layer_state() {
    let stack = |layer_state: &str| -> ThreadPullRequestStack {
        serde_json::from_value(json!({
            "kind": "native",
            "id": "stack-1",
            "number": 7,
            "url": "https://github.com/pingdotgg/t3code/pull/7",
            "base": "main",
            "layers": [{
                "number": 7,
                "headBranch": "feature",
                "state": layer_state,
            }],
        }))
        .expect("stack fixture decodes")
    };
    let open = stack("open");
    assert!(stacks_equal(Some(&open), Some(&stack("open"))));
    assert!(!stacks_equal(Some(&open), Some(&stack("merged"))));
    assert!(!stacks_equal(Some(&open), None));
    assert!(stacks_equal(None, None));
}
