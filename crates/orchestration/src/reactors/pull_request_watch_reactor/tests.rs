//! Tests for the watch pass's decisions: what to read, when a watch ends without a read, when two
//! watches differ, and the sync fingerprint. Upstream has no test file for the reactor itself; these
//! cover each branch of the TypeScript functions. The host-backed pass is not covered here.

use serde_json::json;
use turin_contracts::thread_pull_request::{ThreadPullRequestLink, ThreadPullRequestWatch};

use super::*;

const NOW: i64 = 1_800_000_000_000;

fn last_read(watches: &[&str], fingerprint: &str) -> LastRead {
    LastRead {
        at: NOW,
        fingerprint: fingerprint.to_owned(),
        in_flight: false,
        remarks_complete: true,
        watches: watches.iter().map(|watch| (*watch).to_owned()).collect(),
        status: Some("status-1".to_owned()),
        remarks: Some("remarks-1".to_owned()),
    }
}

fn fingerprint(status: &str, remarks: &str) -> Fingerprint {
    Fingerprint {
        status: status.to_owned(),
        remarks: remarks.to_owned(),
    }
}

fn watch_json(wakes: u64, remark_ids: &[&str]) -> ThreadPullRequestWatch {
    serde_json::from_value(json!({
        "startedAt": "2026-10-02T12:00:00.000Z",
        "headSha": "aaaaaaaaaa",
        "failedChecks": [],
        "passed": false,
        "passedChecks": [],
        "remarksThrough": "2026-10-02T12:00:00.000Z",
        "remarkIds": remark_ids,
        "conflicting": false,
        "wakes": wakes,
    }))
    .expect("watch fixture decodes")
}

fn link_with_snapshot(state: &str, updated_at: &str) -> ThreadPullRequestLink {
    serde_json::from_value(json!({
        "host": "github.com",
        "repository": "pingdotgg/t3code",
        "number": 42,
        "url": "https://github.com/pingdotgg/t3code/pull/42",
        "source": "agent",
        "linkedAt": "2026-10-02T12:00:00.000Z",
        "snapshot": {
            "state": state,
            "title": "Title",
            "headBranch": "feature",
            "baseBranch": "main",
            "isDraft": true,
            "updatedAt": updated_at,
            "syncedAt": "2026-10-02T12:00:00.000Z",
            "checksState": "failing",
            "mergeability": "conflicting",
            "reviewDecision": "approved",
        },
        "stack": null,
    }))
    .expect("link fixture decodes")
}

// ---- plan_read ----

#[test]
fn a_watch_never_seen_by_the_last_read_takes_a_full_look() {
    let plan = plan_read(
        "fp",
        &["thread-1 start".to_owned()],
        Some(&last_read(&[], "fp")),
        None,
        NOW,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: true
        }
    );
}

#[test]
fn no_last_read_reads_everything() {
    let plan = plan_read("fp", &[], None, None, NOW);
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: true
        }
    );
}

#[test]
fn without_a_fingerprint_a_quiet_unchanged_pull_request_is_not_read() {
    let key = "thread-1 start".to_owned();
    let last = last_read(&[key.as_str()], "fp");
    let plan = plan_read(
        "fp",
        std::slice::from_ref(&key),
        Some(&last),
        None,
        NOW + 60_000,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: false,
            activity: false
        }
    );
}

#[test]
fn without_a_fingerprint_a_change_in_the_sync_snapshot_is_read() {
    let key = "thread-1 start".to_owned();
    let last = last_read(&[key.as_str()], "old");
    let plan = plan_read(
        "new",
        std::slice::from_ref(&key),
        Some(&last),
        None,
        NOW + 60_000,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: true
        }
    );
}

#[test]
fn without_a_fingerprint_a_check_in_flight_is_read() {
    let key = "thread-1 start".to_owned();
    let mut last = last_read(&[key.as_str()], "fp");
    last.in_flight = true;
    let plan = plan_read(
        "fp",
        std::slice::from_ref(&key),
        Some(&last),
        None,
        NOW + 60_000,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: true
        }
    );
}

#[test]
fn without_a_fingerprint_a_quiet_pull_request_is_read_again_after_the_quiet_interval() {
    let key = "thread-1 start".to_owned();
    let last = last_read(&[key.as_str()], "fp");
    let plan = plan_read(
        "fp",
        std::slice::from_ref(&key),
        Some(&last),
        None,
        NOW + QUIET_REREAD_MS,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: true
        }
    );
}

#[test]
fn with_a_fingerprint_unchanged_parts_read_nothing() {
    let key = "thread-1 start".to_owned();
    let last = last_read(&[key.as_str()], "fp");
    let same = fingerprint("status-1", "remarks-1");
    let plan = plan_read(
        "fp",
        std::slice::from_ref(&key),
        Some(&last),
        Some(&same),
        NOW + 60_000,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: false,
            activity: false
        }
    );
}

#[test]
fn with_a_fingerprint_moved_remarks_read_the_activity_and_the_detail() {
    let key = "thread-1 start".to_owned();
    let last = last_read(&[key.as_str()], "fp");
    let moved = fingerprint("status-1", "remarks-2");
    let plan = plan_read(
        "fp",
        std::slice::from_ref(&key),
        Some(&last),
        Some(&moved),
        NOW + 60_000,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: true
        }
    );
}

#[test]
fn with_a_fingerprint_moved_status_reads_only_the_detail() {
    let key = "thread-1 start".to_owned();
    let last = last_read(&[key.as_str()], "fp");
    let moved = fingerprint("status-2", "remarks-1");
    let plan = plan_read(
        "fp",
        std::slice::from_ref(&key),
        Some(&last),
        Some(&moved),
        NOW + 60_000,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: false
        }
    );
}

#[test]
fn with_a_fingerprint_an_incomplete_remark_list_is_read_again() {
    let key = "thread-1 start".to_owned();
    let mut last = last_read(&[key.as_str()], "fp");
    last.remarks_complete = false;
    let same = fingerprint("status-1", "remarks-1");
    let plan = plan_read(
        "fp",
        std::slice::from_ref(&key),
        Some(&last),
        Some(&same),
        NOW + 60_000,
    );
    assert_eq!(
        plan,
        ReadPlan {
            detail: true,
            activity: true
        }
    );
}

// ---- ends_without_read ----

#[test]
fn a_merged_pull_request_ends_its_watch_without_a_read() {
    assert_eq!(
        ends_without_read(Some(PullRequestState::Merged), false, false),
        Some(WatchEndReason::Merged)
    );
}

#[test]
fn a_settled_thread_ends_its_watch_without_a_read() {
    assert_eq!(
        ends_without_read(Some(PullRequestState::Open), true, false),
        Some(WatchEndReason::Settled)
    );
}

#[test]
fn a_subagent_thread_ends_its_watch_without_a_read() {
    assert_eq!(
        ends_without_read(Some(PullRequestState::Open), false, true),
        Some(WatchEndReason::Subagent)
    );
}

#[test]
fn an_open_pull_request_on_an_active_thread_is_read() {
    assert_eq!(
        ends_without_read(Some(PullRequestState::Open), false, false),
        None
    );
    assert_eq!(ends_without_read(None, false, false), None);
}

#[test]
fn a_merged_pull_request_wins_over_settled_and_subagent() {
    assert_eq!(
        ends_without_read(Some(PullRequestState::Merged), true, true),
        Some(WatchEndReason::Merged)
    );
}

// ---- watches_equal ----

#[test]
fn identical_watches_are_equal() {
    assert!(watches_equal(
        &watch_json(0, &["r1"]),
        &watch_json(0, &["r1"])
    ));
}

#[test]
fn a_changed_wake_count_makes_watches_differ() {
    assert!(!watches_equal(&watch_json(0, &[]), &watch_json(1, &[])));
}

#[test]
fn changed_remark_ids_make_watches_differ() {
    assert!(!watches_equal(
        &watch_json(0, &["r1"]),
        &watch_json(0, &["r2"])
    ));
}

// ---- snapshot_fingerprint ----

#[test]
fn the_snapshot_fingerprint_joins_its_fields_in_upstream_order() {
    let link = link_with_snapshot("open", "2026-10-02T12:05:00.000Z");
    assert_eq!(
        snapshot_fingerprint(&link),
        "open 2026-10-02T12:05:00.000Z failing conflicting approved true"
    );
}

#[test]
fn a_link_without_a_snapshot_has_an_empty_fingerprint() {
    let link: ThreadPullRequestLink = serde_json::from_value(json!({
        "host": "github.com",
        "repository": "pingdotgg/t3code",
        "number": 42,
        "url": "https://github.com/pingdotgg/t3code/pull/42",
        "source": "agent",
        "linkedAt": "2026-10-02T12:00:00.000Z",
        "snapshot": null,
        "stack": null,
    }))
    .expect("link fixture decodes");
    assert_eq!(snapshot_fingerprint(&link), "");
}

// ---- end reasons ----

#[test]
fn end_reasons_use_the_wire_names_upstream_logs() {
    assert_eq!(WatchEndReason::CommentLimit.as_str(), "comment-limit");
    assert_eq!(WatchEndReason::Unreadable.as_str(), "unreadable");
    assert_eq!(WatchEndReason::Stopped.as_str(), "stopped");
}
