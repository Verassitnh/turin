//! Orchestrator core (T13c): the services that connect threads and turns to
//! provider sessions. Port of upstream/apps/server/src/orchestration-v2.

pub mod projection_reads;
pub mod provider_session_manager;
pub mod session_sink;
