//! The UI's view-model layer: port of upstream/packages/client-runtime/src/state
//! and the pure client-runtime helpers the UI reads.
//!
//! Turin is one process, so the transport half of upstream's state (RPC
//! subscriptions, snapshot HTTP, caches, connection supervision) is replaced by
//! a typed in-process event source; the reducers and selectors keep upstream's
//! shapes. See PORTING.md "No client/server split".

pub mod contracts;
pub mod entities;
pub mod helpers;
pub mod models;
pub mod orchestration_v2_projection;
pub mod shell_reducer;
pub mod subagent_display;
pub mod subagent_runtime;
pub mod support;
#[cfg(test)]
pub mod test_fixtures;
pub mod thread_execution;
pub mod thread_projection;
pub mod thread_requests;
pub mod thread_subagents;
pub mod thread_workflows;
pub mod time;
