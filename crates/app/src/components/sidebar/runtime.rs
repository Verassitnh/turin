//! Ports of the upstream/packages/client-runtime/src/state helpers the
//! sidebar reads (sort, settle/snooze, inbox). These belong to the client
//! view-model reducers (T14); they live here until T14 moves them to
//! `crates/app/src/state`. The shell model is `crate::state::models`.

pub mod collate;
pub mod thread_inbox;
pub mod thread_settled;
pub mod thread_sort;
