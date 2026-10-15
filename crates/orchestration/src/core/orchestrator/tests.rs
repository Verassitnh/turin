//! Command-path tests for [`Orchestrator`]: receipt replay, conflicts, and the
//! rejection of kinds with no handler. Upstream's `Orchestrator.control-reads.test.ts`
//! and `Orchestrator.migration.test.ts` drive the same paths through the
//! Effect layer. These run against an in-memory database with the real
//! projection store as the event sink's applier.

use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};
use turin_contracts::{
    CommandId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Command, OrchestrationV2DomainEvent, OrchestrationV2ServerCommand,
    },
};
use turin_persistence::{Db, OrchestrationEventStore, ReceiptStatus};

use super::Orchestrator;
use super::services::NoRestartRecovery;
use crate::pipeline::command_receipt_store::{CommandReceipt, CommandReceiptStore};
use crate::pipeline::effect_outbox::EffectOutbox;
use crate::pipeline::event_sink::{EventSink, StreamOptions};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::store::ProjectionStore;

async fn harness() -> (Orchestrator, CommandReceiptStore) {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let projection = Arc::new(ProjectionStore::new(db.clone()));
    let sink = EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db.clone()),
        projection.clone(),
    );
    let receipts = CommandReceiptStore::new(db.clone());
    let orchestrator = Orchestrator::new(
        projection,
        sink,
        CommandReceiptStore::new(db),
        Arc::new(NoRestartRecovery),
    );
    (orchestrator, receipts)
}

fn thread(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

fn command_id(id: &str) -> CommandId {
    CommandId::new(id).expect("command id")
}

fn archive(command: &str, thread_id: &str) -> OrchestrationV2ServerCommand {
    OrchestrationV2ServerCommand::Client(OrchestrationV2Command::ThreadArchive {
        command_id: command_id(command),
        thread_id: thread(thread_id),
    })
}

fn receipt(command: &str, thread_id: &str, status: ReceiptStatus) -> CommandReceipt {
    CommandReceipt {
        command_id: command_id(command),
        thread_id: thread(thread_id),
        command_type: "thread.archive".to_owned(),
        accepted_at: UtcDateTime::now(),
        result_sequence: 7,
        status,
        error: (status == ReceiptStatus::Rejected).then(|| "Thread is busy.".to_owned()),
    }
}

#[tokio::test]
async fn reads_report_an_unknown_thread_as_absent() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:missing");

    assert!(orchestrator.get_thread_shell(&id).await.unwrap().is_none());
    assert_eq!(
        orchestrator.get_thread_event_sequence(&id).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn a_kind_without_a_handler_is_rejected_and_leaves_no_receipt() {
    let (orchestrator, receipts) = harness().await;

    let error = orchestrator
        .dispatch(archive("command:unported", "thread:a"))
        .await
        .expect_err("unported kinds are rejected");

    assert!(
        matches!(error, OrchestratorError::CommandRejected { ref command_type, .. } if command_type == "thread.archive"),
        "{error:?}"
    );
    assert!(error.was_not_accepted());
    // No receipt, so the command can run once its handler is ported.
    assert!(
        receipts
            .get_by_command_id(command_id("command:unported"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_rejected_receipt_replays_its_refusal() {
    let (orchestrator, receipts) = harness().await;
    receipts
        .insert_if_absent(receipt(
            "command:refused",
            "thread:a",
            ReceiptStatus::Rejected,
        ))
        .await
        .unwrap();

    let error = orchestrator
        .dispatch(archive("command:refused", "thread:a"))
        .await
        .expect_err("a rejected receipt is an error");

    assert!(
        matches!(
            error,
            OrchestratorError::CommandPreviouslyRejected { ref detail, .. } if detail == "Thread is busy."
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_receipt_for_another_thread_is_a_conflict_not_a_replay() {
    let (orchestrator, receipts) = harness().await;
    receipts
        .insert_if_absent(receipt(
            "command:shared",
            "thread:a",
            ReceiptStatus::Accepted,
        ))
        .await
        .unwrap();

    let error = orchestrator
        .dispatch(archive("command:shared", "thread:b"))
        .await
        .expect_err("another thread's receipt is not replayed");

    assert!(
        matches!(
            error,
            OrchestratorError::CommandIdConflict { ref receipt_thread_id, ref command_thread_id, .. }
                if receipt_thread_id == &thread("thread:a") && command_thread_id == &thread("thread:b")
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_accepted_receipt_replays_its_result_sequence() {
    let (orchestrator, receipts) = harness().await;
    receipts
        .insert_if_absent(receipt("command:done", "thread:a", ReceiptStatus::Accepted))
        .await
        .unwrap();

    let result = orchestrator
        .dispatch(archive("command:done", "thread:a"))
        .await
        .expect("an accepted receipt replays");

    assert_eq!(result.sequence, 7);
    assert!(result.stored_events.is_empty());
}

#[tokio::test]
async fn the_event_log_is_readable_through_the_port() {
    let (orchestrator, _) = harness().await;

    let mut stream = orchestrator
        .stream_stored_events_from(StreamOptions {
            thread_id: Some(thread("thread:a")),
            ..StreamOptions::default()
        })
        .await
        .expect("stream opens on an empty log");
    // Replay is empty and the live tail is open, so nothing is ready yet.
    assert!(stream.next().now_or_never().is_none());
}

fn create_command(command: &str, thread_id: &str, title: &str) -> OrchestrationV2ServerCommand {
    serde_json::from_value(json!({
        "type": "thread.create",
        "createdBy": "user",
        "creationSource": "web",
        "commandId": command,
        "threadId": thread_id,
        "projectId": "project:test",
        "title": title,
        "modelSelection": {"instanceId": "codex", "model": "gpt-test"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
    }))
    .expect("thread.create decodes")
}

fn visit_command(command: &str, thread_id: &str, visited_at: &str) -> OrchestrationV2ServerCommand {
    serde_json::from_value(json!({
        "type": "thread.visit",
        "commandId": command,
        "threadId": thread_id,
        "visitedAt": visited_at,
    }))
    .expect("thread.visit decodes")
}

#[tokio::test]
async fn thread_create_writes_the_thread_once_and_replays_on_retry() {
    let (orchestrator, _) = harness().await;
    let command = create_command("command:create", "thread:new", "First thread");

    let first = orchestrator
        .dispatch(command.clone())
        .await
        .expect("create commits");

    assert_eq!(first.stored_events.len(), 1);
    assert!(matches!(
        first.stored_events[0].event,
        OrchestrationV2DomainEvent::ThreadCreated { .. }
    ));
    let projection = orchestrator
        .get_thread_projection(&thread("thread:new"))
        .await
        .expect("thread reads back");
    assert_eq!(projection.thread.title.as_str(), "First thread");
    assert_eq!(projection.thread.snoozed_until, Some(None));

    // The same command again answers from its receipt and writes nothing.
    let replay = orchestrator.dispatch(command).await.expect("replays");
    assert_eq!(replay.sequence, first.sequence);
    assert_eq!(replay.stored_events.len(), 1);
}

#[tokio::test]
async fn thread_visit_moves_last_visited_forward_only() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:visited");
    orchestrator
        .dispatch(create_command(
            "command:create-visited",
            "thread:visited",
            "Visited",
        ))
        .await
        .expect("create commits");

    let later = orchestrator
        .dispatch(visit_command(
            "command:visit-late",
            "thread:visited",
            "2026-01-01T00:00:05.000Z",
        ))
        .await
        .expect("visit commits");
    assert!(matches!(
        later.stored_events[0].event,
        OrchestrationV2DomainEvent::ThreadVisited { .. }
    ));
    let visited = orchestrator.get_thread_projection(&id).await.unwrap();
    assert!(visited.thread.last_visited_at.is_some());
    let stamp = visited.thread.last_visited_at;

    // An earlier visit is still recorded, but it does not move the watermark.
    orchestrator
        .dispatch(visit_command(
            "command:visit-early",
            "thread:visited",
            "2026-01-01T00:00:02.000Z",
        ))
        .await
        .expect("earlier visit commits");
    let after = orchestrator.get_thread_projection(&id).await.unwrap();
    assert_eq!(after.thread.last_visited_at, stamp);
}

#[tokio::test]
async fn an_invalid_visit_time_is_refused_and_the_refusal_is_replayed() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command(
            "command:create-bad",
            "thread:bad",
            "Bad visit",
        ))
        .await
        .expect("create commits");
    let command = visit_command("command:visit-bad", "thread:bad", "not-a-time");

    let error = orchestrator
        .dispatch(command.clone())
        .await
        .expect_err("an invalid timestamp is refused");
    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );

    // The receipt stores the port's `Dispatch` text (main's T13b wording), not the
    // detail behind it. Upstream's text differs: see the PORTING.md T13c row.
    let replay = orchestrator
        .dispatch(command)
        .await
        .expect_err("the rejected receipt replays");
    assert!(
        matches!(
            replay,
            OrchestratorError::CommandPreviouslyRejected { ref detail, .. }
                if detail == "Orchestration command thread.visit (command:visit-bad) failed."
        ),
        "{replay:?}"
    );
}

#[tokio::test]
async fn an_imported_native_thread_is_not_ported_and_leaves_no_receipt() {
    let (orchestrator, receipts) = harness().await;
    let mut command = serde_json::to_value(create_command(
        "command:import",
        "thread:import",
        "Imported",
    ))
    .expect("command encodes");
    command["importedNativeThread"] = json!({
        "ref": {"driver": "codex", "nativeId": "native-1", "strength": "strong"},
    });
    let command: OrchestrationV2ServerCommand =
        serde_json::from_value(command).expect("imported create decodes");

    let error = orchestrator
        .dispatch(command)
        .await
        .expect_err("imported threads are not ported");
    assert!(
        matches!(error, OrchestratorError::CommandRejected { .. }),
        "{error:?}"
    );
    assert!(
        receipts
            .get_by_command_id(command_id("command:import"))
            .await
            .unwrap()
            .is_none()
    );
}

/// A thread-row command: `kind` is the wire type, `extra` adds its fields.
fn mutation(
    kind: &str,
    command: &str,
    thread_id: &str,
    mut extra: Value,
) -> OrchestrationV2ServerCommand {
    let object = extra.as_object_mut().expect("extra is an object");
    object.insert("type".to_owned(), json!(kind));
    object.insert("commandId".to_owned(), json!(command));
    object.insert("threadId".to_owned(), json!(thread_id));
    serde_json::from_value(extra).expect("thread mutation decodes")
}

#[tokio::test]
async fn unarchive_of_a_live_thread_is_refused_and_the_refusal_replays() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command("command:create-live", "thread:live", "Live"))
        .await
        .expect("create commits");
    let command = mutation(
        "thread.unarchive",
        "command:unarchive-live",
        "thread:live",
        json!({}),
    );

    let error = orchestrator
        .dispatch(command.clone())
        .await
        .expect_err("a live thread is not archived");
    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );

    let replay = orchestrator
        .dispatch(command)
        .await
        .expect_err("the rejected receipt replays");
    assert!(
        matches!(
            replay,
            OrchestratorError::CommandPreviouslyRejected { ref detail, .. }
                if detail == "Orchestration command thread.unarchive (command:unarchive-live) failed."
        ),
        "{replay:?}"
    );
}

#[tokio::test]
async fn pin_reorder_and_unpin_each_write_the_thread_row() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:pinned");
    orchestrator
        .dispatch(create_command(
            "command:create-pinned",
            "thread:pinned",
            "Pinned",
        ))
        .await
        .expect("create commits");

    orchestrator
        .dispatch(mutation(
            "thread.pin",
            "command:pin",
            "thread:pinned",
            json!({"orderKey": "a0"}),
        ))
        .await
        .expect("pin commits");
    let pinned = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert!(pinned.pinned_at.flatten().is_some());
    assert_eq!(
        pinned
            .pin_order_key
            .clone()
            .flatten()
            .map(|key| key.as_str().to_owned()),
        Some("a0".to_owned())
    );

    orchestrator
        .dispatch(mutation(
            "thread.pin.reorder",
            "command:reorder",
            "thread:pinned",
            json!({"orderKey": "a1"}),
        ))
        .await
        .expect("reorder commits");
    let reordered = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert_eq!(
        reordered
            .pin_order_key
            .clone()
            .flatten()
            .map(|key| key.as_str().to_owned()),
        Some("a1".to_owned())
    );

    orchestrator
        .dispatch(mutation(
            "thread.unpin",
            "command:unpin",
            "thread:pinned",
            json!({}),
        ))
        .await
        .expect("unpin commits");
    let unpinned = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert!(unpinned.pinned_at.flatten().is_none());
    assert!(unpinned.pin_order_key.clone().flatten().is_none());
}

#[tokio::test]
async fn reordering_an_unpinned_thread_is_refused() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command(
            "command:create-loose",
            "thread:loose",
            "Loose",
        ))
        .await
        .expect("create commits");

    let error = orchestrator
        .dispatch(mutation(
            "thread.pin.reorder",
            "command:reorder-loose",
            "thread:loose",
            json!({"orderKey": "a1"}),
        ))
        .await
        .expect_err("an unpinned thread has no slot to move");
    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn auto_settle_toggles_and_records_the_disable_time() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:auto");
    orchestrator
        .dispatch(create_command("command:create-auto", "thread:auto", "Auto"))
        .await
        .expect("create commits");

    orchestrator
        .dispatch(mutation(
            "thread.auto-settle.set",
            "command:auto-off",
            "thread:auto",
            json!({"enabled": false}),
        ))
        .await
        .expect("disable commits");
    let disabled = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert!(disabled.auto_settle_disabled_at.flatten().is_some());

    orchestrator
        .dispatch(mutation(
            "thread.auto-settle.set",
            "command:auto-on",
            "thread:auto",
            json!({"enabled": true}),
        ))
        .await
        .expect("enable commits");
    let enabled = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert!(enabled.auto_settle_disabled_at.flatten().is_none());
}

#[tokio::test]
async fn mark_unread_without_a_completed_run_is_refused() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command(
            "command:create-unread",
            "thread:unread",
            "Unread",
        ))
        .await
        .expect("create commits");

    let error = orchestrator
        .dispatch(mutation(
            "thread.mark-unread",
            "command:unread",
            "thread:unread",
            json!({}),
        ))
        .await
        .expect_err("no run has completed yet");
    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );
}
