//! Small ports of upstream/packages/shared modules the view-model layer needs.
//!
//! `turin-shared` (T05b) will own these; they live here until it lands so the
//! state layer only depends on `turin-contracts`. Each module names its source.

pub mod background_work;
pub mod change_request_url;
pub mod collate;
pub mod domain_event;
pub mod model;
pub mod path;
pub mod thread_error;
pub mod thread_pull_requests;
pub mod timeline_visibility;
pub mod timing;
pub mod turn_item;
