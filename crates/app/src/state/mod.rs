//! The UI's view-model layer: port of upstream/packages/client-runtime/src/state
//! and the pure client-runtime helpers the UI reads.
//!
//! Turin is one process, so the transport half of upstream's state (RPC
//! subscriptions, snapshot HTTP, caches, connection supervision) is replaced by
//! a typed in-process event source; the reducers and selectors keep upstream's
//! shapes. See PORTING.md "No client/server split".

pub mod helpers;
