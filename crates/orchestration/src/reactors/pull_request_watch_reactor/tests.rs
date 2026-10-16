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

// ---- The host-backed pass ----

/// An open pull request at `bbbbbbbbbb`, with no checks, comments or threads. The watches in these
/// tests were last seen at `aaaaaaaaaa`, so the head moved.
fn open_detail() -> PullRequestDetail {
    serde_json::from_value(json!({
        "provider": "github",
        "capabilities": {
            "diff": true,
            "comment": true,
            "actions": ["merge"],
            "mergeMethods": ["merge"],
            "search": true,
            "review": {
                "inlineComment": true,
                "reply": true,
                "resolve": true,
                "verdicts": ["comment", "approve", "request-changes"],
            },
            "reviewers": { "request": true, "listCandidates": true },
        },
        "viewerPermissions": {
            "actions": ["merge"],
            "comment": true,
            "resolve": true,
            "verdicts": ["comment", "approve", "request-changes"],
            "requestReviewers": true,
        },
        "projectId": "project:test",
        "projectTitle": "Test",
        "workspaceRoot": "/repo",
        "repository": "pingdotgg/t3code",
        "number": 42,
        "title": "Cache the title",
        "body": "",
        "url": "https://github.com/pingdotgg/t3code/pull/42",
        "author": null,
        "state": "open",
        "isDraft": false,
        "mergeability": "mergeable",
        "additions": 1,
        "deletions": 0,
        "changedFiles": 1,
        "headBranch": "feature",
        "headSha": "bbbbbbbbbb",
        "baseBranch": "main",
        "createdAt": "2026-10-02T12:00:00.000Z",
        "updatedAt": "2026-10-02T12:00:00.000Z",
        "mergedAt": null,
        "closedAt": null,
        "reviewers": [],
        "labels": [],
        "checks": [],
        "mergeCapabilities": { "merge": true, "squash": true, "rebase": true },
    }))
    .expect("detail fixture decodes")
}

fn quiet_activity() -> PullRequestActivity {
    serde_json::from_value(json!({
        "comments": [],
        "commentCount": 0,
        "commentsTruncated": false,
        "reviewThreads": [],
        "commits": [],
    }))
    .expect("activity fixture decodes")
}

/// Serves the same open pull request to every read.
struct OpenHost;

#[async_trait]
impl WatchHost for OpenHost {
    async fn invalidate(&self, _: &PullRequestRef) {}

    async fn detail(&self, _: &PullRequestRef) -> Result<PullRequestDetail, WatchHostError> {
        Ok(open_detail())
    }

    async fn activity(&self, _: &PullRequestRef) -> Result<PullRequestActivity, WatchHostError> {
        Ok(quiet_activity())
    }

    async fn thread_comments(
        &self,
        _: &PullRequestRef,
        _: &TrimmedNonEmptyString,
        _: &TrimmedNonEmptyString,
    ) -> Result<PullRequestThreadCommentsResult, WatchHostError> {
        panic!("no review thread has comments in these tests")
    }

    async fn watch_fingerprint(
        &self,
        _: &PullRequestRef,
    ) -> Result<Option<Fingerprint>, WatchHostError> {
        Ok(None)
    }
}

struct NoWatchThreads;

#[async_trait]
impl WatchProjections for NoWatchThreads {
    async fn threads(&self) -> Result<Vec<WatchThread>, BoxError> {
        Ok(Vec::new())
    }
}

/// Records the thread of each watch sync, and refuses the one for `refused`.
struct RefusingDispatch {
    refused: &'static str,
    recorded: Mutex<Vec<String>>,
}

#[async_trait]
impl ServerCommandDispatch for RefusingDispatch {
    async fn dispatch(&self, command: OrchestrationV2ServerCommand) -> Result<(), BoxError> {
        let OrchestrationV2ServerCommand::Internal(
            OrchestrationV2InternalCommand::ThreadPullRequestWatchSync { thread_id, .. },
        ) = command
        else {
            panic!("expected a watch sync");
        };
        if thread_id.as_str() == self.refused {
            return Err("dispatch refused".into());
        }
        self.recorded.lock().push(thread_id.as_str().to_owned());
        Ok(())
    }
}

fn watched(thread_id: &str) -> WatchTarget {
    WatchTarget {
        thread_id: ThreadId::new(thread_id).expect("thread id"),
        project_id: ProjectId::new("project:test").expect("project id"),
        link: link_with_snapshot("open", "2026-10-02T12:00:00.000Z"),
        watch: watch_json(0, &[]),
    }
}

#[tokio::test]
async fn a_refused_update_does_not_skip_the_rest_of_the_group_or_wait_for_the_quiet_reread() {
    // Upstream PullRequestWatchReactor.ts `eachTarget`: one target's failure is logged, the other
    // targets still get their update, and the group forgets its quiet read.
    let dispatch = Arc::new(RefusingDispatch {
        refused: "thread:one",
        recorded: Mutex::new(Vec::new()),
    });
    let reactor = PullRequestWatchReactor::new(
        Arc::new(OpenHost),
        Arc::new(NoWatchThreads),
        dispatch.clone(),
        UtcDateTime::now(),
    );
    let group = WatchGroup {
        key: "project:test pingdotgg/t3code#42".to_owned(),
        targets: vec![watched("thread:one"), watched("thread:two")],
        fingerprint: String::new(),
    };

    reactor
        .read_group(&group, None)
        .await
        .expect("a refused target does not fail the group read");

    assert_eq!(
        dispatch.recorded.lock().as_slice(),
        &["thread:two".to_owned()]
    );
    assert!(
        !reactor.last_reads.lock().contains_key(&group.key),
        "the failed group is read again on the next pass, not after the quiet interval"
    );
}
