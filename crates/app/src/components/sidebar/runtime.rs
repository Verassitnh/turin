//! Ports of the upstream/packages/client-runtime/src/state helpers the
//! sidebar reads (shell model, sort, settle/snooze, inbox). These belong to
//! the client view-model reducers (T14); they live here until T14 moves
//! them to `crates/app/src/state`.

pub mod collate;
pub mod models;
pub mod thread_inbox;
pub mod thread_settled;
pub mod thread_sort;
