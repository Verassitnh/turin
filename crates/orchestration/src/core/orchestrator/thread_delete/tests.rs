//! `thread.delete` through the command path. Upstream's `ThreadDeletion.test.ts`
//! covers the plan (the planner's own tests are in `pipeline/thread_deletion/tests.rs`);
//! these run the same command through receipts and the projection.

use super::super::test_harness::{create, harness, records, seed_run, thread, thread_only};
use crate::pipeline::orchestrator_port::{OrchestratorPort, RecordField};

#[tokio::test]
async fn delete_stamps_the_thread_and_replays_on_retry() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-del", "thread:del", "Doomed"))
        .await
        .expect("create commits");

    let first = h
        .orchestrator
        .dispatch(thread_only("thread.delete", "command:delete", "thread:del"))
        .await
        .expect("delete commits");
    let stored = first
        .stored_events
        .iter()
        .map(|stored| serde_json::to_value(&stored.event).unwrap())
        .find(|value| value["type"] == "thread.deleted")
        .expect("thread.deleted is stored");
    assert_eq!(stored["threadId"], "thread:del");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:del"))
        .await
        .unwrap();
    assert!(projection.thread.deleted_at.is_some());

    // A retry of the same command answers with what it recorded.
    let replay = h
        .orchestrator
        .dispatch(thread_only("thread.delete", "command:delete", "thread:del"))
        .await
        .expect("retry replays");
    assert_eq!(replay.sequence, first.sequence);
}

#[tokio::test]
async fn delete_cancels_an_active_run() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-run", "thread:busy", "Busy"))
        .await
        .expect("create commits");
    seed_run(
        &h.projection,
        "thread:busy",
        "run:busy",
        1,
        "running",
        false,
    )
    .await;

    h.orchestrator
        .dispatch(thread_only(
            "thread.delete",
            "command:delete-busy",
            "thread:busy",
        ))
        .await
        .expect("delete commits");

    let projection = records(&h.orchestrator, "thread:busy", &[RecordField::Runs])
        .await
        .unwrap();
    let run = projection
        .runs
        .iter()
        .find(|run| run.id.as_str() == "run:busy")
        .expect("the run is still there");
    assert_eq!(
        serde_json::to_value(run.status).unwrap(),
        "cancelled",
        "a deleted thread's active run is cancelled, not left running"
    );
}

#[tokio::test]
async fn delete_of_an_archived_thread_is_accepted() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-arch", "thread:arch", "Old"))
        .await
        .expect("create commits");
    h.orchestrator
        .dispatch(thread_only(
            "thread.archive",
            "command:archive-arch",
            "thread:arch",
        ))
        .await
        .expect("archive commits");

    h.orchestrator
        .dispatch(thread_only(
            "thread.delete",
            "command:delete-arch",
            "thread:arch",
        ))
        .await
        .expect("delete of an archived thread commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:arch"))
        .await
        .unwrap();
    assert!(projection.thread.deleted_at.is_some());
}
