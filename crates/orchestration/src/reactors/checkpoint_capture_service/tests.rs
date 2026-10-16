//! Guard tests for [`plan_capture`]: the branches of `CheckpointCaptureService.execute`
//! before any write. The commit itself runs against the projection and event sink.

use turin_contracts::CheckpointId;

use super::*;
use crate::pipeline::test_support::run;

fn waiting_run() -> OrchestrationV2Run {
    run("run:capture", "thread:capture", 2, "waiting")
}

fn with_checkpoint(mut run: OrchestrationV2Run) -> OrchestrationV2Run {
    run.checkpoint_id = Some(CheckpointId::new("checkpoint:existing").expect("checkpoint id"));
    run
}

#[test]
fn a_waiting_run_with_its_root_scope_and_provider_thread_captures() {
    let plan = plan_capture(Some(&waiting_run()), true, true);
    assert_eq!(plan, CapturePlan::Capture { stopped: false });
}

#[test]
fn a_completed_run_that_names_a_checkpoint_is_skipped() {
    let completed = with_checkpoint(run("run:capture", "thread:capture", 2, "completed"));
    assert_eq!(
        plan_capture(Some(&completed), true, true),
        CapturePlan::Skip
    );
}

#[test]
fn a_completed_run_without_a_checkpoint_is_not_waiting_so_it_is_invalid() {
    let completed = run("run:capture", "thread:capture", 2, "completed");
    assert_eq!(
        plan_capture(Some(&completed), true, true),
        CapturePlan::Invalid
    );
}

#[test]
fn a_rolled_back_run_is_skipped_even_though_it_has_no_checkpoint() {
    let rolled_back = run("run:capture", "thread:capture", 2, "rolled_back");
    assert_eq!(
        plan_capture(Some(&rolled_back), true, true),
        CapturePlan::Skip
    );
}

#[test]
fn an_interrupted_run_captures_without_changing_its_status_and_skips_once_checkpointed() {
    let interrupted = run("run:capture", "thread:capture", 2, "interrupted");
    assert_eq!(
        plan_capture(Some(&interrupted), true, true),
        CapturePlan::Capture { stopped: true }
    );
    assert_eq!(
        plan_capture(Some(&with_checkpoint(interrupted)), true, true),
        CapturePlan::Skip
    );
}

#[test]
fn a_cancelled_run_is_treated_as_stopped() {
    let cancelled = run("run:capture", "thread:capture", 2, "cancelled");
    assert_eq!(
        plan_capture(Some(&cancelled), true, true),
        CapturePlan::Capture { stopped: true }
    );
}

#[test]
fn a_root_node_on_another_scope_is_invalid() {
    assert_eq!(
        plan_capture(Some(&waiting_run()), false, true),
        CapturePlan::Invalid
    );
}

#[test]
fn a_missing_provider_thread_is_invalid() {
    assert_eq!(
        plan_capture(Some(&waiting_run()), true, false),
        CapturePlan::Invalid
    );
}

#[test]
fn a_missing_run_is_invalid() {
    assert_eq!(plan_capture(None, true, true), CapturePlan::Invalid);
}
