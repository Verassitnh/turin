//! `thread.fork` through the command path. Upstream's `ThreadForkService.test.ts`
//! covers the plan (its port is in `pipeline/thread_fork_service.rs`); these run
//! the command through the source-point reads and the commit.

use super::super::test_harness::{cause_text, create, fork, harness, records, seed_run, thread};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort, RecordField};

#[tokio::test]
async fn fork_without_a_stable_source_run_is_refused() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-src", "thread:src", "Source"))
        .await
        .expect("create commits");

    let error = h
        .orchestrator
        .dispatch(super::super::test_harness::command(serde_json::json!({
            "type": "thread.fork",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": "command:fork-none",
            "sourceThreadId": "thread:src",
            "targetThreadId": "thread:child",
            "sourcePoint": {"type": "latest_stable"},
        })))
        .await
        .expect_err("no completed run with a checkpoint");

    assert!(
        matches!(error, OrchestratorError::Dispatch { ref command_type, .. } if command_type == "thread.fork"),
        "{error:?}"
    );
    assert_eq!(
        cause_text(&error),
        "No stable source run was found for fork source latest_stable."
    );
}

#[tokio::test]
async fn fork_of_a_completed_run_creates_the_thread_and_a_pending_transfer() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-parent", "thread:parent", "Parent"))
        .await
        .expect("create commits");
    seed_run(
        &h.projection,
        "thread:parent",
        "run:done",
        1,
        "completed",
        true,
    )
    .await;

    h.orchestrator
        .dispatch(fork(
            "command:fork",
            "thread:parent",
            "thread:child",
            "run:done",
        ))
        .await
        .expect("fork commits");

    let child = h
        .orchestrator
        .get_thread_projection(&thread("thread:child"))
        .await
        .unwrap()
        .thread;
    // No title given: the fork is named after its source.
    assert_eq!(child.title.as_str(), "Parent fork");
    assert_eq!(
        child
            .lineage
            .parent_thread_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("thread:parent")
    );
    assert!(child.snoozed_until.is_some_and(|until| until.is_none()));

    let transfers = records(
        &h.orchestrator,
        "thread:child",
        &[RecordField::ContextTransfers],
    )
    .await
    .unwrap()
    .context_transfers;
    assert_eq!(transfers.len(), 1);
    let transfer = serde_json::to_value(&transfers[0]).unwrap();
    assert_eq!(transfer["type"], "fork");
    assert_eq!(transfer["status"], "pending");
    assert_eq!(transfer["sourceThreadId"], "thread:parent");
    assert_eq!(transfer["targetThreadId"], "thread:child");
    assert_eq!(transfer["sourcePoint"]["runId"], "run:done");
}

#[tokio::test]
async fn fork_takes_the_title_it_is_given() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-titled", "thread:titled", "Titled"))
        .await
        .expect("create commits");
    seed_run(
        &h.projection,
        "thread:titled",
        "run:titled",
        1,
        "completed",
        true,
    )
    .await;

    h.orchestrator
        .dispatch(
            serde_json::from_value(serde_json::json!({
                "type": "thread.fork",
                "createdBy": "user",
                "creationSource": "web",
                "commandId": "command:fork-titled",
                "sourceThreadId": "thread:titled",
                "targetThreadId": "thread:titled-child",
                "sourcePoint": {"type": "run", "runId": "run:titled"},
                "title": "Explore",
            }))
            .unwrap(),
        )
        .await
        .expect("fork commits");

    let child = h
        .orchestrator
        .get_thread_projection(&thread("thread:titled-child"))
        .await
        .unwrap()
        .thread;
    assert_eq!(child.title.as_str(), "Explore");
}

#[tokio::test]
async fn fork_of_an_in_progress_run_is_refused() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-busy", "thread:working", "Working"))
        .await
        .expect("create commits");
    seed_run(
        &h.projection,
        "thread:working",
        "run:working",
        1,
        "running",
        false,
    )
    .await;

    let error = h
        .orchestrator
        .dispatch(fork(
            "command:fork-busy",
            "thread:working",
            "thread:busy-child",
            "run:working",
        ))
        .await
        .expect_err("an in-progress run is not forkable");

    assert!(cause_text(&error).contains("cannot be forked"), "{error:?}");
    assert!(cause_text(&error).contains("run:working"), "{error:?}");
}
