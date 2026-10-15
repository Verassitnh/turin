//! Command-path tests for [`Orchestrator`]: receipt replay, conflicts, and the
//! rejection of kinds with no handler. Upstream's `Orchestrator.control-reads.test.ts`
//! and `Orchestrator.migration.test.ts` drive the same paths through the
//! Effect layer. These run against an in-memory database with the real
//! projection store as the event sink's applier.

use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use turin_contracts::{
    CommandId, ThreadId, UtcDateTime,
    orchestration_v2::{OrchestrationV2Command, OrchestrationV2ServerCommand},
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
