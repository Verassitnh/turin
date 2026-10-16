//! Upstream `Orchestrator.migration.test.ts`: the pure helpers of the V1
//! import handoff and the receipt replay rule.

use turin_contracts::{ContextHandoffId, ThreadId};

use super::dispatch::can_replay_command_receipt;
use super::message_dispatch::append_context_handoff_id;
use super::message_transfer::should_prepare_legacy_import_handoff;

#[test]
fn imported_context_is_reissued_until_a_v2_run_completes() {
    assert!(should_prepare_legacy_import_handoff(true, false, 2));
    assert!(!should_prepare_legacy_import_handoff(true, true, 2));
    assert!(!should_prepare_legacy_import_handoff(false, false, 2));
    assert!(!should_prepare_legacy_import_handoff(true, false, 0));
}

#[test]
fn a_reissued_legacy_handoff_is_recorded_on_an_existing_provider_thread() {
    let existing = ContextHandoffId::new("handoff:legacy-import:existing").expect("id");
    let retry = ContextHandoffId::new("handoff:legacy-import:retry").expect("id");
    assert_eq!(
        append_context_handoff_id(std::slice::from_ref(&existing), Some(&retry)),
        vec![existing.clone(), retry]
    );
    assert_eq!(
        append_context_handoff_id(std::slice::from_ref(&existing), Some(&existing)),
        vec![existing.clone()]
    );
    assert_eq!(
        append_context_handoff_id(std::slice::from_ref(&existing), None),
        vec![existing]
    );
}

#[test]
fn a_receipt_replays_only_for_the_thread_it_was_recorded_against() {
    let thread_a = ThreadId::new("thread-a").expect("id");
    let thread_b = ThreadId::new("thread-b").expect("id");
    assert!(can_replay_command_receipt(&thread_a, &thread_a));
    assert!(!can_replay_command_receipt(&thread_a, &thread_b));
}
