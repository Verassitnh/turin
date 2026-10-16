//! `DispatchModeLimit` under the thread lock (`Orchestrator.ts`, the check at
//! the top of `dispatchWithReceipt` and `refuseAboveDispatchModeLimit` in fork
//! and merge-back). Upstream tests the limit type (`DispatchModeLimit.test.ts`,
//! ported in `pipeline/dispatch_mode_limit.rs`) and its MCP senders; the
//! orchestrator's own re-check has no upstream test, so these cover it.

use serde_json::json;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

use super::test_harness::{
    cause_text, command, create, fork, harness, merge_back, seed_run, thread, thread_only,
};
use crate::pipeline::dispatch_mode_limit::{DispatchModeLimit, DispatchModeRefusal, DispatchModes};
use crate::pipeline::orchestrator_port::{ModeKind, OrchestratorError, OrchestratorPort};

/// What a plan-only, approval-required sender may touch.
const NARROW: DispatchModes = DispatchModes {
    runtime_mode: RuntimeMode::ApprovalRequired,
    interaction_mode: ProviderInteractionMode::Plan,
};

/// `create` makes threads in full-access/default.
const BROAD: DispatchModes = DispatchModes {
    runtime_mode: RuntimeMode::FullAccess,
    interaction_mode: ProviderInteractionMode::Default,
};

fn refused(thread_id: &str, mode: ModeKind) -> DispatchModeRefusal {
    DispatchModeRefusal {
        thread_id: thread(thread_id),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: ProviderInteractionMode::Default,
        mode,
    }
}

#[tokio::test]
async fn a_thread_above_the_limit_is_refused_and_leaves_no_receipt() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-lim", "thread:lim", "Limited"))
        .await
        .expect("create commits");
    let before = h
        .orchestrator
        .get_thread_event_sequence(&thread("thread:lim"))
        .await
        .unwrap();

    let (limit, slot) = DispatchModeLimit::with_refusal_slot(NARROW);
    let error = h
        .orchestrator
        .dispatch_with_limit(
            thread_only("thread.delete", "command:delete-lim", "thread:lim"),
            Some(&limit),
        )
        .await
        .expect_err("the thread runs above the limit");

    assert!(
        matches!(
            error,
            OrchestratorError::ThreadAboveModeLimit {
                mode: ModeKind::Runtime,
                runtime_mode: RuntimeMode::FullAccess,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "Thread thread:lim now runs in full-access/default mode, above what this caller may change."
    );
    assert_eq!(*slot.lock(), Some(refused("thread:lim", ModeKind::Runtime)));
    assert!(error.was_not_accepted());
    assert_eq!(
        h.orchestrator
            .get_thread_event_sequence(&thread("thread:lim"))
            .await
            .unwrap(),
        before,
        "nothing is recorded"
    );

    // No rejected receipt either: the same command goes through once the limit
    // no longer applies (the thread's user lowered the modes, or the sender
    // is unlimited).
    h.orchestrator
        .dispatch_with_limit(
            thread_only("thread.delete", "command:delete-lim", "thread:lim"),
            None,
        )
        .await
        .expect("the same command id is still free");
}

#[tokio::test]
async fn interaction_mode_is_named_when_only_it_exceeds_the_limit() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-int", "thread:int", "Interaction"))
        .await
        .expect("create commits");

    let limit = DispatchModes {
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: ProviderInteractionMode::Plan,
    };
    let error = h
        .orchestrator
        .dispatch_with_limit(
            thread_only("thread.delete", "command:delete-int", "thread:int"),
            Some(&DispatchModeLimit::new(limit)),
        )
        .await
        .expect_err("default interaction is broader than plan");
    assert!(
        matches!(
            error,
            OrchestratorError::ThreadAboveModeLimit {
                mode: ModeKind::Interaction,
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_thread_within_the_limit_dispatches_normally() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-ok", "thread:ok", "Within"))
        .await
        .expect("create commits");

    let result = h
        .orchestrator
        .dispatch_with_limit(
            thread_only("thread.delete", "command:delete-ok", "thread:ok"),
            Some(&DispatchModeLimit::new(BROAD)),
        )
        .await
        .expect("a limit as broad as the thread allows it");
    assert_eq!(result.stored_events.len(), 1);
}

#[tokio::test]
async fn a_new_thread_has_no_modes_to_exceed() {
    let h = harness().await;
    // `thread.create` for a thread that does not exist yet reads no shell.
    h.orchestrator
        .dispatch_with_limit(
            create("command:create-new", "thread:new", "New"),
            Some(&DispatchModeLimit::new(NARROW)),
        )
        .await
        .expect("create is not refused");
}

#[tokio::test]
async fn a_recorded_command_replays_before_the_limit_is_checked() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-rep", "thread:rep", "Replay"))
        .await
        .expect("create commits");
    let first = h
        .orchestrator
        .dispatch(thread_only("thread.pin", "command:pin-rep", "thread:rep"))
        .await
        .expect("pin commits");

    let replay = h
        .orchestrator
        .dispatch_with_limit(
            thread_only("thread.pin", "command:pin-rep", "thread:rep"),
            Some(&DispatchModeLimit::new(NARROW)),
        )
        .await
        .expect("a receipt answers before the mode check, as upstream");
    assert_eq!(replay.sequence, first.sequence);
}

#[tokio::test]
async fn fork_checks_the_source_it_copies_not_the_target() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-fsrc", "thread:fsrc", "Source"))
        .await
        .expect("create commits");
    seed_run(
        &h.projection,
        "thread:fsrc",
        "run:fsrc",
        1,
        "completed",
        true,
    )
    .await;

    // The fork's own thread does not exist yet, so only the source check can
    // refuse here.
    let (limit, slot) = DispatchModeLimit::with_refusal_slot(NARROW);
    let error = h
        .orchestrator
        .dispatch_with_limit(
            fork(
                "command:fork-lim",
                "thread:fsrc",
                "thread:fchild",
                "run:fsrc",
            ),
            Some(&limit),
        )
        .await
        .expect_err("the source runs above the limit");
    assert!(
        matches!(error, OrchestratorError::ThreadAboveModeLimit { ref thread_id, .. } if thread_id.as_str() == "thread:fsrc"),
        "{error:?}"
    );
    assert_eq!(
        *slot.lock(),
        Some(refused("thread:fsrc", ModeKind::Runtime))
    );

    // Refused like the lock check: no receipt, so it can go through unlimited.
    h.orchestrator
        .dispatch(fork(
            "command:fork-lim",
            "thread:fsrc",
            "thread:fchild",
            "run:fsrc",
        ))
        .await
        .expect("the same command id is still free");
}

#[tokio::test]
async fn merge_back_checks_the_source_when_the_target_is_within_the_limit() {
    let h = harness().await;
    h.orchestrator
        .dispatch(create("command:create-mbp", "thread:mbp", "Parent"))
        .await
        .expect("create commits");
    seed_run(&h.projection, "thread:mbp", "run:mbp", 1, "completed", true).await;
    h.orchestrator
        .dispatch(fork(
            "command:mbp-fork",
            "thread:mbp",
            "thread:mbc",
            "run:mbp",
        ))
        .await
        .expect("fork commits");
    seed_run(&h.projection, "thread:mbc", "run:mbc", 1, "completed", true).await;
    // Lower the target (the parent) to the limit; the child keeps full access.
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.runtime-mode.set",
            "commandId": "command:mbp-runtime",
            "threadId": "thread:mbp",
            "runtimeMode": "approval-required",
        })))
        .await
        .expect("runtime mode commits");
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.interaction-mode.set",
            "commandId": "command:mbp-interaction",
            "threadId": "thread:mbp",
            "interactionMode": "plan",
        })))
        .await
        .expect("interaction mode commits");

    let error = h
        .orchestrator
        .dispatch_with_limit(
            merge_back("command:mb-lim", "thread:mbc", "thread:mbp", "run:mbc"),
            Some(&DispatchModeLimit::new(NARROW)),
        )
        .await
        .expect_err("the fork being merged runs above the limit");
    assert!(
        matches!(error, OrchestratorError::ThreadAboveModeLimit { ref thread_id, .. } if thread_id.as_str() == "thread:mbc"),
        "{error:?} / {}",
        cause_text(&error)
    );
}
