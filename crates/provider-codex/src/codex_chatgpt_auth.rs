//! Port of upstream/apps/server/src/provider/CodexChatGptAuth.ts.
//!
//! Split by concern: `records` holds the persisted schemas and `store` the
//! secret-backed reads and writes. The OAuth flow and controller are next.

// `ProviderSetupError` is the contract error this flow returns. Boxing it
// would change nothing on the wire but every call site.
#![allow(clippy::result_large_err)]

pub mod records;
pub mod store;
