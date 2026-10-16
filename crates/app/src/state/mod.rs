//! The UI's view-model layer: port of upstream/packages/client-runtime/src/state
//! and the pure client-runtime helpers the UI reads.
//!
//! Turin is one process, so the transport half of upstream's state (RPC
//! subscriptions, snapshot HTTP, caches, connection supervision) is replaced by
//! a typed in-process event source; the reducers and selectors keep upstream's
//! shapes. See PORTING.md "No client/server split".

pub mod archived_threads;
pub mod checkpoint_diff;
pub mod client_state;
pub mod command_result;
pub mod composer_dispatch;
pub mod composer_path_search;
pub mod contracts;
pub mod entities;
pub mod events;
pub mod file_context_menu;
pub mod file_metadata;
pub mod first_run;
pub mod helpers;
pub mod item_support;
pub mod live_refresh;
pub mod models;
pub mod orchestration_v2_projection;
pub mod project_grouping;
pub mod projects;
pub mod shell_reducer;
pub mod shell_state;
pub mod sidebar_project_grouping;
pub mod store;
pub mod subagent_display;
pub mod subagent_runtime;
pub mod support;
#[cfg(test)]
pub mod test_fixtures;
pub mod thread_checkpoints;
pub mod thread_detail;
pub mod thread_execution;
pub mod thread_feedback;
pub mod thread_history_merge;
pub mod thread_lifecycle;
pub mod thread_projection;
pub mod thread_relationships;
pub mod thread_requests;
pub mod thread_search;
pub mod thread_shell;
pub mod thread_state;
pub mod thread_subagents;
pub mod thread_workflows;
pub mod time;
pub mod turn_item_presentation;
pub mod ui_state_store;

/// The one environment a single-process Turin has: this machine.
pub fn local_environment_id() -> contracts::EnvironmentId {
    contracts::EnvironmentId::new("local").expect("static id is non-blank")
}
pub mod operations;
