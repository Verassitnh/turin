//! Command-path tests for [`Orchestrator`]: receipt replay, conflicts, and the
//! rejection of kinds with no handler. Upstream's `Orchestrator.control-reads.test.ts`
//! and `Orchestrator.migration.test.ts` drive the same paths through the
//! Effect layer. These run against an in-memory database with the real
//! projection store as the event sink's applier.

use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};
use turin_contracts::{
    CommandId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Command, OrchestrationV2DomainEvent, OrchestrationV2ServerCommand,
        OrchestrationV2SettleOverride,
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

pub(super) fn thread(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

pub(super) fn command_id(id: &str) -> CommandId {
    CommandId::new(id).expect("command id")
}

pub(super) fn archive(command: &str, thread_id: &str) -> OrchestrationV2ServerCommand {
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

pub(super) fn create_command(
    command: &str,
    thread_id: &str,
    title: &str,
) -> OrchestrationV2ServerCommand {
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
                if detail == "Failed to dispatch orchestration command thread.visit (command:visit-bad)."
        ),
        "{replay:?}"
    );
}

#[tokio::test]
async fn an_imported_native_thread_creates_its_provider_thread() {
    let (orchestrator, _) = harness().await;
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

    let result = orchestrator
        .dispatch(command)
        .await
        .expect("an imported thread is created");
    let types: Vec<_> = result
        .stored_events
        .iter()
        .map(|stored| serde_json::to_value(&stored.event).unwrap()["type"].clone())
        .collect();
    assert_eq!(
        types,
        vec![json!("thread.created"), json!("provider-thread.updated")]
    );
    let projection = orchestrator
        .get_thread_projection(&thread("thread:import"))
        .await
        .expect("projection reads");
    let [provider_thread] = projection.provider_threads.as_slice() else {
        panic!("one provider thread: {:?}", projection.provider_threads);
    };
    let reference = provider_thread
        .native_thread_ref
        .as_ref()
        .expect("a native ref");
    assert_eq!(
        reference.native_id.as_ref().map(|id| id.as_str()),
        Some("native-1")
    );
    assert!(provider_thread.provider_session_id.is_none());
    assert!(provider_thread.first_run_ordinal.is_none());
}

/// A thread-row command: `kind` is the wire type, `extra` adds its fields.
pub(super) fn mutation(
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
                if detail == "Failed to dispatch orchestration command thread.unarchive (command:unarchive-live)."
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

#[tokio::test]
async fn snooze_to_the_future_then_unsnooze_round_trips() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:snoozed");
    orchestrator
        .dispatch(create_command(
            "command:create-snoozed",
            "thread:snoozed",
            "Snoozed",
        ))
        .await
        .expect("create commits");

    orchestrator
        .dispatch(mutation(
            "thread.snooze",
            "command:snooze",
            "thread:snoozed",
            json!({"snoozedUntil": "2099-01-01T00:00:00.000Z"}),
        ))
        .await
        .expect("future snooze commits");
    let snoozed = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert!(snoozed.snoozed_until.flatten().is_some());
    assert!(snoozed.snoozed_at.flatten().is_some());

    orchestrator
        .dispatch(mutation(
            "thread.unsnooze",
            "command:unsnooze",
            "thread:snoozed",
            json!({"reason": "user"}),
        ))
        .await
        .expect("unsnooze commits");
    let awake = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert!(awake.snoozed_until.flatten().is_none());
    assert!(awake.snoozed_at.flatten().is_none());
    assert!(awake.last_snooze_wake_at.flatten().is_some());
}

#[tokio::test]
async fn a_snooze_in_the_past_is_refused() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command("command:create-late", "thread:late", "Late"))
        .await
        .expect("create commits");

    let error = orchestrator
        .dispatch(mutation(
            "thread.snooze",
            "command:snooze-past",
            "thread:late",
            json!({"snoozedUntil": "2000-01-01T00:00:00.000Z"}),
        ))
        .await
        .expect_err("a wake time in the past is refused");
    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn unsettle_marks_the_thread_active_and_repeats_as_a_no_op_on_updated_at() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:unsettle");
    orchestrator
        .dispatch(create_command(
            "command:create-unsettle",
            "thread:unsettle",
            "Unsettle",
        ))
        .await
        .expect("create commits");

    orchestrator
        .dispatch(mutation(
            "thread.unsettle",
            "command:unsettle",
            "thread:unsettle",
            json!({"reason": "user"}),
        ))
        .await
        .expect("unsettle commits");
    let first = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert_eq!(
        first.settled_override,
        Some(OrchestrationV2SettleOverride::Active)
    );
    assert!(first.settled_at.is_none());
    assert!(first.unsettled_at.flatten().is_some());

    orchestrator
        .dispatch(mutation(
            "thread.unsettle",
            "command:unsettle-again",
            "thread:unsettle",
            json!({"reason": "user"}),
        ))
        .await
        .expect("a repeat unsettle commits");
    let second = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert_eq!(second.updated_at, first.updated_at);
    assert_eq!(second.unsettled_at, first.unsettled_at);
}

#[tokio::test]
async fn runtime_and_interaction_modes_update_the_thread_row() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:modes");
    orchestrator
        .dispatch(create_command(
            "command:create-modes",
            "thread:modes",
            "Modes",
        ))
        .await
        .expect("create commits");

    let runtime = orchestrator
        .dispatch(mutation(
            "thread.runtime-mode.set",
            "command:runtime",
            "thread:modes",
            json!({"runtimeMode": "auto"}),
        ))
        .await
        .expect("runtime mode commits");
    assert_eq!(runtime.stored_events.len(), 1);
    let after_runtime = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert_eq!(after_runtime.runtime_mode, RuntimeMode::Auto);

    orchestrator
        .dispatch(mutation(
            "thread.interaction-mode.set",
            "command:interaction",
            "thread:modes",
            json!({"interactionMode": "plan"}),
        ))
        .await
        .expect("interaction mode commits");
    let after_interaction = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert_eq!(
        after_interaction.interaction_mode,
        ProviderInteractionMode::Plan
    );
    assert_eq!(after_interaction.runtime_mode, RuntimeMode::Auto);
}

#[tokio::test]
async fn settle_parks_the_thread_and_clears_the_pin() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:settle");
    orchestrator
        .dispatch(create_command(
            "command:create-settle",
            "thread:settle",
            "Settle",
        ))
        .await
        .expect("create commits");
    orchestrator
        .dispatch(mutation(
            "thread.pin",
            "command:pin-settle",
            "thread:settle",
            json!({"orderKey": "a0"}),
        ))
        .await
        .expect("pin commits");

    orchestrator
        .dispatch(mutation(
            "thread.settle",
            "command:settle",
            "thread:settle",
            json!({"settledAt": "2026-06-21T00:00:01.000Z"}),
        ))
        .await
        .expect("settle commits");
    let settled = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert_eq!(
        settled.settled_override,
        Some(OrchestrationV2SettleOverride::Settled)
    );
    assert!(settled.settled_at.is_some());
    assert!(settled.pinned_at.flatten().is_none());
    assert!(settled.pin_order_key.flatten().is_none());

    // Settling an already settled, unpinned thread keeps updatedAt.
    orchestrator
        .dispatch(mutation(
            "thread.settle",
            "command:settle-again",
            "thread:settle",
            json!({}),
        ))
        .await
        .expect("repeat settle commits");
    let again = orchestrator
        .get_thread_projection(&id)
        .await
        .unwrap()
        .thread;
    assert_eq!(again.updated_at, settled.updated_at);
}

#[tokio::test]
async fn archive_sets_the_flag_then_refuses_a_second_archive_and_unarchive_restores() {
    let (orchestrator, _) = harness().await;
    let id = thread("thread:archive");
    orchestrator
        .dispatch(create_command(
            "command:create-archive",
            "thread:archive",
            "Archive",
        ))
        .await
        .expect("create commits");

    let archived = orchestrator
        .dispatch(mutation(
            "thread.archive",
            "command:archive",
            "thread:archive",
            json!({}),
        ))
        .await
        .expect("archive commits");
    assert!(matches!(
        archived.stored_events[0].event,
        OrchestrationV2DomainEvent::ThreadArchived { .. }
    ));
    assert!(
        orchestrator
            .get_thread_projection(&id)
            .await
            .unwrap()
            .thread
            .archived_at
            .is_some()
    );

    let error = orchestrator
        .dispatch(mutation(
            "thread.archive",
            "command:archive-again",
            "thread:archive",
            json!({}),
        ))
        .await
        .expect_err("a second archive is refused");
    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );

    orchestrator
        .dispatch(mutation(
            "thread.unarchive",
            "command:unarchive",
            "thread:archive",
            json!({}),
        ))
        .await
        .expect("unarchive commits");
    assert!(
        orchestrator
            .get_thread_projection(&id)
            .await
            .unwrap()
            .thread
            .archived_at
            .is_none()
    );
}

/// The refusal's cause. The top-level message names only the command, so the
/// cause is read from the error's `Debug` form.
fn refusal_text(error: &OrchestratorError) -> String {
    format!("{error:?}")
}

pub(super) fn message_command(
    command: &str,
    thread_id: &str,
    extra: Value,
) -> OrchestrationV2ServerCommand {
    let mut value = json!({
        "type": "message.dispatch",
        "commandId": command,
        "threadId": thread_id,
        "messageId": format!("message:{command}"),
        "text": "hello",
        "attachments": [],
        "createdBy": "user",
        "creationSource": "web",
        "dispatchMode": { "type": "start_immediately" },
    });
    value
        .as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    serde_json::from_value(value).expect("message.dispatch decodes")
}

#[tokio::test]
async fn a_manual_continuation_of_an_unknown_run_is_refused_and_replays() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command("command:create", "thread:msg", "Thread"))
        .await
        .expect("create commits");
    let command = message_command(
        "command:resume",
        "thread:msg",
        json!({ "manualContinuationOfRunId": "run:missing" }),
    );

    let error = orchestrator
        .dispatch(command.clone())
        .await
        .expect_err("a run that does not exist cannot be resumed");
    assert!(
        refusal_text(&error).contains("This thread can no longer be resumed from that run."),
        "{error:?}"
    );

    let replay = orchestrator
        .dispatch(command)
        .await
        .expect_err("the refusal replays");
    assert!(
        matches!(replay, OrchestratorError::CommandPreviouslyRejected { .. }),
        "{replay:?}"
    );
}

#[tokio::test]
async fn a_new_turn_without_a_provider_adapter_is_refused_before_any_event() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command("command:create", "thread:idle", "Thread"))
        .await
        .expect("create commits");

    // The default seams have no provider adapters, so a start is refused
    // rather than run without one. The start path writes nothing before that.
    let error = orchestrator
        .dispatch(message_command("command:start", "thread:idle", json!({})))
        .await
        .expect_err("no adapter is wired");
    assert!(
        refusal_text(&error).contains("Provider instance codex is not available."),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_shared_thread_executor_serializes_dispatch_with_other_writers() {
    use crate::pipeline::thread_command_executor::ThreadCommandExecutor;
    let executor = ThreadCommandExecutor::new();
    let (orchestrator, _) = harness().await;
    let orchestrator = std::sync::Arc::new(orchestrator.with_thread_executor(executor.clone()));
    let (release, hold) = tokio::sync::oneshot::channel::<()>();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let other_writer = {
        let executor = executor.clone();
        tokio::spawn(async move {
            executor
                .with_lock(&thread("thread:shared"), || async {
                    held_tx.send(()).ok();
                    hold.await.ok();
                })
                .await;
        })
    };
    held_rx
        .await
        .expect("the other writer holds the thread lock");
    let dispatching = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move {
            orchestrator
                .dispatch(create_command("command:shared", "thread:shared", "Shared"))
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !dispatching.is_finished(),
        "dispatch waits for the shared lock"
    );
    release.send(()).ok();
    dispatching
        .await
        .expect("task joins")
        .expect("create commits once the lock frees");
    other_writer.await.expect("task joins");
}
