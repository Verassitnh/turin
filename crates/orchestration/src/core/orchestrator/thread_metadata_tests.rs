//! `thread.metadata.update`, `thread.title.regeneration.complete` and the
//! usage-limit recovery rules around them (`thread_metadata.rs`, the
//! `usageLimitContinuationOfRunId` guard of `message_dispatch.rs`). Ported from
//! the D1 test batch; upstream drives these through the thread mutation suite.

use serde_json::json;
use turin_contracts::{
    UtcDateTime,
    orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2ServerCommand},
};

use super::Orchestrator;
use super::message_steer_tests::{T1, harness};
use super::tests::{create_command, message_command, mutation, thread};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event, run, turn_item};

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

async fn seed(orchestrator: &Orchestrator, events: Vec<OrchestrationV2DomainEvent>) {
    apply_all(&orchestrator.projection, events).await;
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

/// A failed run that hit a usage limit resetting at `reset_at`, as the
/// projection holds it after the provider reported the limit.
async fn seed_usage_limit(orchestrator: &Orchestrator, thread_id: &str, reset_at: &str) {
    let mut failed = run(thread_id, "run:limit", 1, "failed");
    failed["completedAt"] = json!("2026-01-01T00:00:03.000Z");
    let error = turn_item(
        thread_id,
        "item:limit",
        Some("run:limit"),
        1,
        "error",
        "failed",
        json!({"failure": {"class": "usage_limit", "message": "limit reached", "code": null,
            "retryable": null, "resetAt": reset_at}}),
    );
    seed(
        orchestrator,
        vec![
            event("run.created", thread_id, "2026-01-01T00:00:00.000Z", failed),
            event(
                "turn-item.updated",
                thread_id,
                "2026-01-01T00:00:01.000Z",
                error,
            ),
        ],
    )
    .await;
}

/// A future reset, so a snooze to it is a wake that has not yet passed.
const FUTURE_RESET: &str = "2099-01-02T00:00:00.000Z";

#[tokio::test]
async fn metadata_update_sets_the_title_and_branch_and_leaves_the_rest() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:meta").await;

    let result = orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:meta",
            "thread:meta",
            json!({"title": "Renamed", "branch": "feature/rename"}),
        ))
        .await
        .expect("metadata commits");

    assert_eq!(result.stored_events.len(), 1);
    assert!(matches!(
        result.stored_events[0].event,
        OrchestrationV2DomainEvent::ThreadMetadataUpdated { .. }
    ));
    let projection = orchestrator
        .get_thread_projection(&thread("thread:meta"))
        .await
        .expect("thread reads back");
    assert_eq!(projection.thread.title.as_str(), "Renamed");
    assert_eq!(
        projection.thread.branch.as_ref().map(|b| b.as_str()),
        Some("feature/rename")
    );
    assert_eq!(projection.thread.worktree_path, None);
    // A landed title (or any title change) clears the marker: upstream writes null.
    assert_eq!(projection.thread.title_regeneration, Some(None));
}

#[tokio::test]
async fn expected_worktree_mismatch_is_refused_and_the_refusal_replays() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:worktree").await;
    let command = mutation(
        "thread.metadata.update",
        "command:worktree-guard",
        "thread:worktree",
        // The thread has no worktree, so an expectation of one is stale.
        json!({"title": "Never", "expectedWorktreePath": "/repo-other"}),
    );

    let error = orchestrator
        .dispatch(command.clone())
        .await
        .expect_err("a stale worktree expectation is refused");
    assert_eq!(
        cause(&error),
        "Thread thread:worktree worktree changed before the metadata update could be applied."
    );

    let replay = orchestrator
        .dispatch(command)
        .await
        .expect_err("the refusal replays");
    assert!(
        matches!(
            replay,
            OrchestratorError::CommandPreviouslyRejected { ref detail, .. }
                if detail == "Failed to dispatch orchestration command thread.metadata.update (command:worktree-guard)."
        ),
        "{replay:?}"
    );
    let projection = orchestrator
        .get_thread_projection(&thread("thread:worktree"))
        .await
        .expect("thread reads back");
    assert_eq!(projection.thread.title.as_str(), "Thread");
}

#[tokio::test]
async fn expected_empty_accepts_a_fresh_thread_and_refuses_once_a_run_exists() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:empty").await;

    orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:empty-ok",
            "thread:empty",
            json!({"title": "Still empty", "expectedEmpty": true}),
        ))
        .await
        .expect("an empty thread accepts the expectation");

    seed(
        orchestrator,
        vec![event(
            "run.created",
            "thread:empty",
            "2026-01-01T00:00:00.000Z",
            run("thread:empty", "run:first", 1, "completed"),
        )],
    )
    .await;
    let error = orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:empty-refused",
            "thread:empty",
            json!({"title": "Too late", "expectedEmpty": true}),
        ))
        .await
        .expect_err("a thread with a run is no longer empty");
    assert_eq!(cause(&error), "Thread thread:empty is no longer empty.");
}

#[tokio::test]
async fn limit_recovery_is_refused_unless_the_run_hit_that_usage_limit() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:limit-guard").await;

    // No failed run yet: nothing to recover from.
    let error = orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:limit-none",
            "thread:limit-guard",
            json!({"limitRecovery": {"runId": "run:limit", "resetAt": FUTURE_RESET, "autoResume": true}}),
        ))
        .await
        .expect_err("no usage limit to recover from");
    assert_eq!(
        cause(&error),
        "The provider limit changed before recovery could be configured."
    );

    seed_usage_limit(orchestrator, "thread:limit-guard", FUTURE_RESET).await;
    // The limit is real, but the choice names a reset the failure does not carry.
    let error = orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:limit-reset",
            "thread:limit-guard",
            json!({"limitRecovery": {
                "runId": "run:limit",
                "resetAt": "2099-03-01T00:00:00.000Z",
                "autoResume": true,
            }}),
        ))
        .await
        .expect_err("a different reset is a different limit");
    assert_eq!(
        cause(&error),
        "The provider limit changed before recovery could be configured."
    );
    // A reset that is already past cannot be snoozed to.
    let error = orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:limit-past",
            "thread:limit-guard",
            json!({"limitRecovery": {
                "runId": "run:limit",
                "resetAt": "2026-01-02T00:00:00.000Z",
                "snooze": true,
            }}),
        ))
        .await
        .expect_err("a past reset cannot be snoozed to");
    assert_eq!(
        cause(&error),
        "The reset time has passed. Retry the thread manually."
    );
}

#[tokio::test]
async fn snoozing_to_the_reset_wakes_then_clearing_the_recovery_clears_that_wake() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:snooze-reset").await;
    seed_usage_limit(orchestrator, "thread:snooze-reset", FUTURE_RESET).await;

    orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:snooze-reset",
            "thread:snooze-reset",
            json!({"limitRecovery": {
                "runId": "run:limit",
                "resetAt": FUTURE_RESET,
                "snooze": true,
            }}),
        ))
        .await
        .expect("a valid recovery snoozes to its reset");
    let snoozed = orchestrator
        .get_thread_projection(&thread("thread:snooze-reset"))
        .await
        .expect("thread reads back")
        .thread;
    assert_eq!(snoozed.snoozed_until, Some(Some(parse(FUTURE_RESET))));
    assert!(snoozed.snoozed_at.flatten().is_some());
    let recovery = snoozed.limit_recovery.flatten().expect("recovery is set");
    assert_eq!(recovery.snooze, Some(true));

    orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:snooze-reset-clear",
            "thread:snooze-reset",
            json!({"limitRecovery": null}),
        ))
        .await
        .expect("clearing the recovery is accepted");
    let cleared = orchestrator
        .get_thread_projection(&thread("thread:snooze-reset"))
        .await
        .expect("thread reads back")
        .thread;
    assert_eq!(cleared.snoozed_until, Some(None));
    assert_eq!(cleared.snoozed_at, Some(None));
    assert!(cleared.last_snooze_wake_at.flatten().is_some());
    assert!(
        cleared
            .limit_recovery
            .is_none_or(|recovery| recovery.is_none())
    );
}

fn parse(iso: &str) -> UtcDateTime {
    UtcDateTime::from_datetime(
        chrono::DateTime::parse_from_rfc3339(iso)
            .expect("fixture time parses")
            .with_timezone(&chrono::Utc),
    )
}

#[tokio::test]
async fn title_regeneration_marker_is_armed_and_landed_only_by_its_request() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:title").await;

    orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:title-arm",
            "thread:title",
            json!({"regenerateTitle": true}),
        ))
        .await
        .expect("arming commits");
    let armed = orchestrator
        .get_thread_projection(&thread("thread:title"))
        .await
        .expect("thread reads back")
        .thread;
    let marker = armed
        .title_regeneration
        .clone()
        .flatten()
        .expect("the marker is armed");
    assert_eq!(marker.request_id.as_str(), "command:title-arm");

    // A landing for another request leaves the title and the marker alone.
    orchestrator
        .dispatch(mutation(
            "thread.title.regeneration.complete",
            "command:title-stale",
            "thread:title",
            json!({"requestId": "command:someone-else", "title": "Stale"}),
        ))
        .await
        .expect("a superseded completion is still recorded");
    let stale = orchestrator
        .get_thread_projection(&thread("thread:title"))
        .await
        .expect("thread reads back")
        .thread;
    assert_eq!(stale.title.as_str(), "Thread");
    assert!(stale.title_regeneration.flatten().is_some());

    orchestrator
        .dispatch(mutation(
            "thread.title.regeneration.complete",
            "command:title-landed",
            "thread:title",
            json!({"requestId": "command:title-arm", "title": "Generated"}),
        ))
        .await
        .expect("the landing completes the marker");
    let landed = orchestrator
        .get_thread_projection(&thread("thread:title"))
        .await
        .expect("thread reads back")
        .thread;
    assert_eq!(landed.title.as_str(), "Generated");
    assert_eq!(landed.title_regeneration, Some(None));
}

#[tokio::test]
async fn a_worktree_change_detaches_live_provider_sessions_once() {
    let h = harness(false, None).await;
    let orchestrator = &h.orchestrator;
    create_thread(orchestrator, "thread:detach").await;
    seed(
        orchestrator,
        vec![event(
            "provider-session.attached",
            "thread:detach",
            "2026-01-01T00:00:01.000Z",
            json!({
                "id": "session:detach", "driver": "codex", "providerInstanceId": "codex",
                "status": "ready", "cwd": "/tmp", "model": null,
                "capabilities": crate::projection::test_support::capabilities(),
                "createdAt": "2026-01-01T00:00:00.000Z",
                "updatedAt": "2026-01-01T00:00:00.000Z", "lastError": null
            }),
        )],
    )
    .await;

    let moved = orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:detach-move",
            "thread:detach",
            json!({"worktreePath": "/worktrees/moved"}),
        ))
        .await
        .expect("a worktree change commits");
    let kinds: Vec<&str> = moved
        .stored_events
        .iter()
        .map(|stored| match stored.event {
            OrchestrationV2DomainEvent::ThreadMetadataUpdated { .. } => "metadata",
            OrchestrationV2DomainEvent::ProviderSessionDetached { .. } => "detached",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["metadata", "detached"]);
    let effects = orchestrator
        .get_thread_projection(&thread("thread:detach"))
        .await
        .expect("thread reads back")
        .thread;
    assert_eq!(
        effects.worktree_path.as_ref().map(|p| p.as_str()),
        Some("/worktrees/moved")
    );

    // The same worktree again is no change, so nothing else detaches.
    let same = orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:detach-same",
            "thread:detach",
            json!({"worktreePath": "/worktrees/moved"}),
        ))
        .await
        .expect("an unchanged worktree commits");
    assert_eq!(same.stored_events.len(), 1);
}

const PAST_RESET: &str = "2026-01-02T00:00:00.000Z";

/// A failed usage-limit run whose reset has passed, and an auto-resume recovery
/// for it keyed to `command:recover`.
async fn recoverable_limit(h: &super::message_steer_tests::Steer, thread_id: &str) {
    create_thread(&h.orchestrator, thread_id).await;
    seed_usage_limit(&h.orchestrator, thread_id, PAST_RESET).await;
    h.orchestrator
        .dispatch(mutation(
            "thread.metadata.update",
            "command:recover",
            thread_id,
            json!({"limitRecovery": {"runId": "run:limit", "resetAt": PAST_RESET, "autoResume": true}}),
        ))
        .await
        .expect("the recovery is configured");
}

fn continuation(thread_id: &str, command: &str) -> OrchestrationV2ServerCommand {
    message_command(
        command,
        thread_id,
        json!({
            "createdBy": "agent",
            "creationSource": "server",
            "usageLimitContinuationOfRunId": "run:limit",
            "usageLimitRecoveryRequestId": "command:recover",
        }),
    )
}

#[tokio::test]
async fn an_automatic_resume_after_the_reset_starts_a_run() {
    let h = harness(false, None).await;
    recoverable_limit(&h, "thread:resume").await;
    h.orchestrator
        .dispatch(continuation("thread:resume", "command:resume"))
        .await
        .expect("the continuation commits");
    let runs = h
        .orchestrator
        .get_thread_projection(&thread("thread:resume"))
        .await
        .expect("projection reads")
        .runs;
    assert_eq!(runs.len(), 2, "{runs:?}");
}

/// Upstream guards the resume with the thread shell's `lastErrorClass`: a
/// distinct provider-session error supersedes the limit, so no resume starts.
#[tokio::test]
async fn a_newer_session_error_blocks_the_automatic_resume() {
    let h = harness(false, None).await;
    recoverable_limit(&h, "thread:superseded").await;
    let session = json!({
        "id": "provider-session:other",
        "driver": "codex",
        "providerInstanceId": "codex",
        "status": "ready",
        "cwd": "/turin-fixture/project",
        "model": "gpt-test",
        "capabilities": h.capabilities,
        "createdAt": T1,
        "updatedAt": T1,
        "lastError": "The session crashed.",
    });
    apply_all(
        &h.projection,
        vec![event(
            "provider-session.attached",
            "thread:superseded",
            T1,
            session,
        )],
    )
    .await;
    h.orchestrator
        .dispatch(continuation("thread:superseded", "command:blocked"))
        .await
        .expect("a stale automatic delivery is accepted without work");
    let runs = h
        .orchestrator
        .get_thread_projection(&thread("thread:superseded"))
        .await
        .expect("projection reads")
        .runs;
    assert_eq!(runs.len(), 1, "no resume starts: {runs:?}");
}
