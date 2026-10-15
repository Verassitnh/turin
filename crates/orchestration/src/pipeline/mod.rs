//! Write-side orchestration: the command pipeline of upstream's
//! `orchestration-v2` (event sink/store, command receipts, effect outbox and
//! worker, thread services). Projection application is supplied by the read
//! side through [`event_sink::ProjectionApplier`].

pub mod command_policy;
pub mod command_receipt_store;
pub mod dispatch_mode_limit;
pub mod domain_event;
pub mod effect_outbox;
pub mod effect_worker;
pub mod event_sink;
pub mod event_store;
pub mod notification_mailbox;
pub mod runtime_policy;
pub mod task;
pub mod thread_command_executor;
pub mod user_facing_errors;
