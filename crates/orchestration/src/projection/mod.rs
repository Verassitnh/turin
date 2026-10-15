//! Read model for orchestration-v2. The pipeline calls [`store::ProjectionStore::apply_in_tx`]
//! in the same transaction as event append, and publishes only after commit.

pub mod assistant_streaming;
pub mod queued_run_order;
pub mod project_store;
pub mod store;
pub mod thread_history_paging;
pub mod thread_live_event_coalescer;
pub mod thread_search;
pub mod turn_item_position_store;
