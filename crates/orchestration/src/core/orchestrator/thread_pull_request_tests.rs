//! `thread.pull-request.*`, `thread.pull-request-link.sync` and the internal
//! `thread.pull-request-watch.sync` (`thread_pull_request.rs`). Upstream
//! covers them through the thread mutation tests.

use serde_json::{Value, json};
use turin_contracts::{
    orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2ServerCommand},
    thread_pull_request::ThreadPullRequestLinkSource,
};

use super::Orchestrator;
use super::message_steer_tests::harness;
use super::tests::{create_command, mutation, thread};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

async fn create_thread(orchestrator: &Orchestrator, thread_id: &str) {
    orchestrator
        .dispatch(create_command(
            &format!("command:create-{thread_id}"),
            thread_id,
            "Thread",
        ))
        .await
        .expect("create commits");
}

const REPO: &str = "pingdotgg/t3code";

fn pr_url(number: u64) -> String {
    format!("https://github.com/{REPO}/pull/{number}")
}

/// A pull-request command for `number` on the test repository.
fn pr_command(
    kind: &str,
    command: &str,
    thread_id: &str,
    number: u64,
    extra: Value,
) -> OrchestrationV2ServerCommand {
    let mut body = json!({"host": "github.com", "repository": REPO, "number": number});
    body.as_object_mut()
        .expect("body is an object")
        .extend(extra.as_object().cloned().unwrap_or_default());
    mutation(kind, command, thread_id, body)
}

/// The thread's stored links, read through the projection.
async fn links_of(
    orchestrator: &Orchestrator,
    thread_id: &str,
) -> Vec<turin_contracts::thread_pull_request::ThreadPullRequestLink> {
    orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("thread reads back")
        .thread
        .pull_requests
        .unwrap_or_default()
}

#[tokio::test]
async fn linking_a_pull_request_adds_it_and_relinking_keeps_its_watch() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:pr-link").await;

    let linked = orchestrator
        .dispatch(pr_command(
            "thread.pull-request.link",
            "command:pr-link",
            "thread:pr-link",
            8,
            json!({"url": pr_url(8), "source": "manual"}),
        ))
        .await
        .expect("link commits");
    assert!(matches!(
        linked.stored_events[0].event,
        OrchestrationV2DomainEvent::ThreadPullRequestSynced { .. }
    ));
    let links = links_of(orchestrator, "thread:pr-link").await;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].number.get(), 8);
    assert_eq!(links[0].source, ThreadPullRequestLinkSource::Manual);
    assert!(links[0].watch.is_none());

    orchestrator
        .dispatch(pr_command(
            "thread.pull-request.watch",
            "command:pr-watch",
            "thread:pr-link",
            8,
            json!({"watching": true}),
        ))
        .await
        .expect("watch commits");
    let watched = links_of(orchestrator, "thread:pr-link").await;
    let started_at = watched[0]
        .watch
        .as_ref()
        .expect("watch is on")
        .started_at
        .clone();

    // Relinking the same pull request keeps its watch, so the agent is not re-armed.
    orchestrator
        .dispatch(pr_command(
            "thread.pull-request.link",
            "command:pr-relink",
            "thread:pr-link",
            8,
            json!({"url": pr_url(8), "source": "manual"}),
        ))
        .await
        .expect("relink commits");
    let relinked = links_of(orchestrator, "thread:pr-link").await;
    assert_eq!(relinked.len(), 1);
    assert_eq!(
        relinked[0]
            .watch
            .as_ref()
            .map(|watch| watch.started_at.clone()),
        Some(started_at)
    );
}

#[tokio::test]
async fn unlinking_a_stack_member_dismisses_it_and_unlinking_a_manual_link_removes_it() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:pr-unlink").await;
    for (command, number, source) in [
        ("command:pr-stack", 21, "stack"),
        ("command:pr-manual", 22, "manual"),
    ] {
        orchestrator
            .dispatch(pr_command(
                "thread.pull-request.link",
                command,
                "thread:pr-unlink",
                number,
                json!({"url": pr_url(number), "source": source}),
            ))
            .await
            .expect("link commits");
    }

    orchestrator
        .dispatch(pr_command(
            "thread.pull-request.unlink",
            "command:unlink-stack",
            "thread:pr-unlink",
            21,
            json!({}),
        ))
        .await
        .expect("unlink commits");
    orchestrator
        .dispatch(pr_command(
            "thread.pull-request.unlink",
            "command:unlink-manual",
            "thread:pr-unlink",
            22,
            json!({}),
        ))
        .await
        .expect("unlink commits");

    let links = links_of(orchestrator, "thread:pr-unlink").await;
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0].number.get(), 21);
    assert_eq!(links[0].source, ThreadPullRequestLinkSource::StackDismissed);
}

#[tokio::test]
async fn link_sync_records_the_snapshot_and_reports_the_link_it_changed() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:pr-sync").await;
    orchestrator
        .dispatch(pr_command(
            "thread.pull-request.link",
            "command:sync-link",
            "thread:pr-sync",
            8,
            json!({"url": pr_url(8), "source": "manual"}),
        ))
        .await
        .expect("link commits");
    let snapshot = json!({
        "state": "open", "title": "Port links", "headBranch": "feature/links",
        "baseBranch": "main", "isDraft": false, "updatedAt": null,
        "syncedAt": "2026-01-01T00:00:05.000Z",
    });

    let synced = orchestrator
        .dispatch(pr_command(
            "thread.pull-request-link.sync",
            "command:sync-snapshot",
            "thread:pr-sync",
            8,
            json!({"snapshot": snapshot, "stack": null}),
        ))
        .await
        .expect("sync commits");
    let OrchestrationV2DomainEvent::ThreadPullRequestLinkUpdated { payload, .. } =
        &synced.stored_events[0].event
    else {
        panic!(
            "expected a link update, got {:?}",
            synced.stored_events[0].event
        );
    };
    assert_eq!(payload.number.get(), 8);
    assert_eq!(
        payload
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.title.as_str()),
        Some("Port links")
    );

    // A snapshot for a link the thread does not have changes nothing and records nothing.
    let error = orchestrator
        .dispatch(pr_command(
            "thread.pull-request-link.sync",
            "command:sync-missing",
            "thread:pr-sync",
            9,
            json!({"snapshot": snapshot, "stack": null}),
        ))
        .await
        .expect_err("a snapshot for an unlinked PR is a no-op");
    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_watch_on_an_archived_thread_is_refused() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:pr-archived").await;
    orchestrator
        .dispatch(super::tests::archive(
            "command:pr-archive",
            "thread:pr-archived",
        ))
        .await
        .expect("archive commits");

    let error = orchestrator
        .dispatch(pr_command(
            "thread.pull-request.watch",
            "command:pr-watch-archived",
            "thread:pr-archived",
            8,
            json!({"watching": true, "link": {"url": pr_url(8), "source": "manual"}}),
        ))
        .await
        .expect_err("an archived thread cannot watch");
    assert_eq!(
        cause(&error),
        "Thread thread:pr-archived is settled or archived and cannot watch pull requests."
    );
}

#[tokio::test]
async fn sync_applies_only_while_the_project_and_snapshot_still_match() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:pr-discover").await;
    let branch = json!({
        "projectId": "project:test", "repository": REPO, "number": 8, "url": pr_url(8),
    });
    let sync = |command: &str, project: &str, sequence: u64, root: &str| {
        serde_json::from_value::<OrchestrationV2ServerCommand>(json!({
            "type": "thread.pull-request.sync",
            "commandId": command,
            "threadId": "thread:pr-discover",
            "projectId": project,
            "snapshotSequence": sequence,
            "expected": {
                "workspaceRoot": root, "branch": null, "worktreePath": null,
                "linkedPullRequest": null, "branchPullRequest": null,
            },
            "branchPullRequest": branch,
        }))
        .expect("sync decodes")
    };

    let error = orchestrator
        .dispatch(sync(
            "command:discover-moved",
            "project:test",
            1000,
            "/elsewhere",
        ))
        .await
        .expect_err("a moved project root refuses discovery");
    assert_eq!(
        cause(&error),
        "Thread thread:pr-discover changed before pull request discovery."
    );

    // A snapshot older than the thread's own events was read before they landed.
    let error = orchestrator
        .dispatch(sync(
            "command:discover-stale",
            "project:test",
            0,
            "/turin-fixture/project",
        ))
        .await
        .expect_err("a stale snapshot refuses discovery");
    assert_eq!(
        cause(&error),
        "Thread thread:pr-discover changed before pull request discovery."
    );

    orchestrator
        .dispatch(sync(
            "command:discover-ok",
            "project:test",
            1000,
            "/turin-fixture/project",
        ))
        .await
        .expect("a matching snapshot applies");
    let projection = orchestrator
        .get_thread_projection(&thread("thread:pr-discover"))
        .await
        .expect("thread reads back")
        .thread;
    assert_eq!(
        projection
            .branch_pull_request
            .flatten()
            .map(|linked| linked.number.get()),
        Some(8)
    );
}

#[tokio::test]
async fn watch_sync_ends_the_watch_its_read_started_and_refuses_a_stale_read() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:pr-wake").await;
    orchestrator
        .dispatch(pr_command(
            "thread.pull-request.watch",
            "command:wake-watch",
            "thread:pr-wake",
            8,
            json!({"watching": true, "link": {"url": pr_url(8), "source": "agent"}}),
        ))
        .await
        .expect("watch commits");
    let started_at = links_of(orchestrator, "thread:pr-wake").await[0]
        .watch
        .as_ref()
        .expect("watch is on")
        .started_at
        .clone();
    let sync = |command: &str, started: &str| {
        serde_json::from_value::<OrchestrationV2ServerCommand>(json!({
            "type": "thread.pull-request-watch.sync",
            "commandId": command,
            "threadId": "thread:pr-wake",
            "host": "github.com",
            "repository": REPO,
            "number": 8,
            "startedAt": started,
            "watch": null,
        }))
        .expect("watch sync decodes")
    };

    let error = orchestrator
        .dispatch(sync("command:wake-stale", "2020-01-01T00:00:00.000Z"))
        .await
        .expect_err("a read from an earlier watch is refused");
    assert_eq!(
        cause(&error),
        "The pull request watch ended or its thread settled while it was read."
    );

    let ended = orchestrator
        .dispatch(sync("command:wake-end", &started_at))
        .await
        .expect("the matching read ends the watch");
    let OrchestrationV2DomainEvent::ThreadPullRequestLinkUpdated { payload, .. } =
        &ended.stored_events[0].event
    else {
        panic!(
            "expected a link update, got {:?}",
            ended.stored_events[0].event
        );
    };
    assert!(payload.watch.is_none());
    assert!(
        links_of(orchestrator, "thread:pr-wake").await[0]
            .watch
            .is_none()
    );
}

#[tokio::test]
async fn watch_sync_with_a_wake_queues_the_message_behind_the_running_run() {
    let h = harness(false, None).await;
    h.running_run("thread:pr-wake2").await;
    h.orchestrator
        .dispatch(pr_command(
            "thread.pull-request.watch",
            "command:wake2-watch",
            "thread:pr-wake2",
            8,
            json!({"watching": true, "link": {"url": pr_url(8), "source": "agent"}}),
        ))
        .await
        .expect("watch commits");
    let started_at = links_of(&h.orchestrator, "thread:pr-wake2").await[0]
        .watch
        .as_ref()
        .expect("watch is on")
        .started_at
        .clone();
    let wake = serde_json::from_value::<OrchestrationV2ServerCommand>(json!({
        "type": "thread.pull-request-watch.sync",
        "commandId": "command:wake2-sync",
        "threadId": "thread:pr-wake2",
        "host": "github.com",
        "repository": REPO,
        "number": 8,
        "startedAt": started_at,
        "watch": null,
        "wake": {
            "messageId": "message:pr-wake",
            "text": "CI failed",
            "notification": {
                "source": {"kind": "background_task"},
                "outcome": "updated",
                "summary": "Pull request update",
            },
        },
    }))
    .expect("watch sync decodes");
    h.orchestrator
        .dispatch(wake)
        .await
        .expect("the wake commits");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:pr-wake2"))
        .await
        .expect("projection reads");
    assert!(
        projection
            .messages
            .iter()
            .any(|message| message.id.as_str() == "message:pr-wake" && message.text == "CI failed")
    );
    assert!(projection.runs.len() >= 2, "the wake queues its own run");
}
