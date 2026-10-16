//! Orchestrator core. Port of upstream/apps/server/src/orchestration-v2.
//!
//! T13c connects threads and turns to provider sessions. R5 reads and hands
//! off thread context outside the decider: workflow file readers, context
//! handoff, and checkpoint rollback.

// `SinkError` and `PersistenceError` are large and every write returns them;
// boxing them would leak into the pipeline API (same reason as `pipeline`).
#![allow(clippy::result_large_err)]

pub mod checkpoint_rollback_service;
pub mod context_handoff_delivery;
pub mod context_handoff_service;
pub mod projection_reads;
pub mod provider_event_ingestor;
pub mod provider_session_manager;
pub mod session_sink;
pub mod unknown_field;
pub mod workflow_agent_answers;
pub mod workflow_file_read;
pub mod workflow_script_query;
