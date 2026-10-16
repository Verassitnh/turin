//! Orchestrator core. Port of upstream/apps/server/src/orchestration-v2.
//!
//! T13c connects threads and turns to provider sessions. R5 reads and hands
//! off thread context outside the decider: workflow file readers, context
//! handoff, and checkpoint rollback.

// `SinkError` and `PersistenceError` are large and every write returns them;
// boxing them would leak into the pipeline API (same reason as `pipeline`).
#![allow(clippy::result_large_err)]

pub mod context_handoff_delivery;
pub mod context_handoff_service;
pub mod orchestrator;
pub mod projection_applier;
pub mod projection_reads;
pub mod provider_continuation_service;
pub mod provider_event_ingestor;
pub mod provider_runtime_recovery_service;
pub mod provider_session_manager;
pub mod provider_session_transition_policy;
pub mod provider_switch_service;
pub mod provider_turn_control_service;
pub mod provider_turn_start_service;
pub mod restart_background_note;
pub mod restart_continuation;
pub mod restart_continuation_service;
pub mod run_execution_service;
pub mod run_finalization_service;
pub mod runtime_request_service;
pub mod session_sink;
pub mod steer_follow_up;
pub mod thread_find;
