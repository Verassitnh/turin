//! `thread.merge_back` through the command path. Upstream's merge-back coverage
//! is the provider-replay integration suite (`testkit/ThreadMergeBack.integration.test.ts`),
//! which needs a live provider; these cover the decider's refusals and its
//! transfer bookkeeping on seeded projections.

use super::super::test_harness::{
    cause_text, create, fork, harness, merge_back, records, seed_run,
};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort, RecordField};

/// A parent with a completed run, forked into `thread:child` from that run.
/// The child gets its own completed run `run:child-1` for merge-back to copy.
async fn forked_pair(h: &super::super::test_harness::Harness) {
    h.orchestrator
        .dispatch(create(
            "command:create-mb-parent",
            "thread:mb-parent",
            "Parent",
        ))
        .await
        .expect("create commits");
    seed_run(
        &h.projection,
        "thread:mb-parent",
        "run:mb-parent",
        1,
        "completed",
        true,
    )
    .await;
    h.orchestrator
        .dispatch(fork(
            "command:mb-fork",
            "thread:mb-parent",
            "thread:mb-child",
            "run:mb-parent",
        ))
        .await
        .expect("fork commits");
    seed_run(
        &h.projection,
        "thread:mb-child",
        "run:mb-child-1",
        1,
        "completed",
        true,
    )
    .await;
}

#[tokio::test]
async fn merge_back_into_a_thread_that_is_not_the_parent_is_refused() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-loner", "thread:loner", "Loner"))
        .await
        .expect("create commits");
    h.orchestrator
        .dispatch(create("command:create-other", "thread:other", "Other"))
        .await
        .expect("create commits");

    let error = h
        .orchestrator
        .dispatch(merge_back(
            "command:mb-loner",
            "thread:loner",
            "thread:other",
            "run:none",
        ))
        .await
        .expect_err("a thread that is not a fork cannot merge back");

    assert_eq!(
        cause_text(&error),
        "Thread thread:loner is not a fork of thread:other."
    );
}

#[tokio::test]
async fn merge_back_of_an_unfinished_child_run_is_refused() {
    let h = harness().await;
    forked_pair(&h).await;
    seed_run(
        &h.projection,
        "thread:mb-child",
        "run:mb-child-busy",
        2,
        "running",
        false,
    )
    .await;

    let error = h
        .orchestrator
        .dispatch(merge_back(
            "command:mb-busy",
            "thread:mb-child",
            "thread:mb-parent",
            "run:mb-child-busy",
        ))
        .await
        .expect_err("only provider-finished runs merge back");

    assert!(
        matches!(error, OrchestratorError::Dispatch { .. }),
        "{error:?}"
    );
    assert!(
        cause_text(&error).contains("only provider-finished runs are supported"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_second_merge_back_supersedes_the_pending_one() {
    let h = harness().await;
    forked_pair(&h).await;

    h.orchestrator
        .dispatch(merge_back(
            "command:mb-first",
            "thread:mb-child",
            "thread:mb-parent",
            "run:mb-child-1",
        ))
        .await
        .expect("first merge-back commits");
    seed_run(
        &h.projection,
        "thread:mb-child",
        "run:mb-child-2",
        2,
        "completed",
        true,
    )
    .await;
    h.orchestrator
        .dispatch(merge_back(
            "command:mb-second",
            "thread:mb-child",
            "thread:mb-parent",
            "run:mb-child-2",
        ))
        .await
        .expect("second merge-back commits");

    let transfers = records(
        &h.orchestrator,
        "thread:mb-parent",
        &[RecordField::ContextTransfers],
    )
    .await
    .unwrap()
    .context_transfers;
    let merges: Vec<_> = transfers
        .iter()
        .map(|transfer| serde_json::to_value(transfer).unwrap())
        .filter(|value| value["type"] == "merge_back")
        .collect();
    assert_eq!(merges.len(), 2, "{merges:?}");
    let superseded = merges
        .iter()
        .filter(|value| value["status"] == "superseded")
        .count();
    let pending = merges
        .iter()
        .filter(|value| value["status"] == "pending")
        .count();
    assert_eq!((superseded, pending), (1, 1), "{merges:?}");
    let pending = merges
        .iter()
        .find(|value| value["status"] == "pending")
        .unwrap();
    assert_eq!(pending["sourcePoint"]["runId"], "run:mb-child-2");
    assert_eq!(pending["basePoint"]["runId"], "run:mb-parent");
}
