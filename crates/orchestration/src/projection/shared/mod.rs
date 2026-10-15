//! Contract-dependent helpers from `packages/shared` that the projection
//! store needs. Upstream keeps them in `@t3tools/shared`; Turin's
//! `turin-shared` (task T05b) has not picked them up yet, so they live next to
//! their only caller. Move them there when T05b lands; the logic is identical.

pub mod pending_background_work;
pub mod thread_error;
pub mod thread_pull_requests;
pub mod timeline;
pub mod tool_output;
