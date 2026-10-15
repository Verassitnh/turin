//! Write-side orchestration: the command pipeline of upstream's
//! `orchestration-v2` (event sink/store, command receipts, effect outbox and
//! worker, thread services). Projection application is supplied by the read
//! side through [`event_sink::ProjectionApplier`].

// `PersistenceError` is ~128 bytes and every store call returns it; boxing it
// would leak into the persistence API this crate does not own.
#![allow(clippy::result_large_err)]

pub mod attachment_claims;
pub mod attachment_store;
pub mod command_policy;
pub mod command_receipt_store;
pub mod dispatch_mode_limit;
pub mod domain_event;
pub mod effect_outbox;
pub mod effect_worker;
pub mod event_sink;
pub mod event_store;
pub mod hooks_bridge;
pub mod notification_mailbox;
pub mod orchestrator_port;
pub mod project_commands;
pub mod resource_cleanup_service;
pub mod runtime_policy;
pub mod seams;
pub mod task;
#[cfg(test)]
pub(crate) mod test_support;
pub mod thread_command_executor;
pub mod thread_deletion;
pub mod thread_fork_service;
pub mod thread_lifecycle_service;
pub mod thread_management_service;
pub mod thread_message_intake;
pub mod user_facing_errors;
pub mod user_input_attachments;
