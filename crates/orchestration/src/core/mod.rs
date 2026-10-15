//! Orchestrator core (T13c): the services that connect threads and turns to
//! provider sessions. Port of upstream/apps/server/src/orchestration-v2.

// `SinkError` and `PersistenceError` are large and every write returns them;
// boxing them would leak into the pipeline API (same reason as `pipeline`).
#![allow(clippy::result_large_err)]

pub mod orchestrator;
pub mod projection_applier;
pub mod projection_reads;
pub mod provider_event_ingestor;
pub mod provider_session_manager;
pub mod session_sink;
