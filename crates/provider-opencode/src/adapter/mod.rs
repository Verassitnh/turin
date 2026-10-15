//! OpenCode `ProviderAdapter` for both server generations.
//!
//! Port of `upstream/packages/provider-opencode/src/server/adapter.ts` and
//! `server/v2/adapter.ts`. The trait mirror in [`api`] matches the in-progress
//! provider-core trait until that module is public on this branch.

mod api;
mod driver;
mod logic;
mod v1;
mod v2;

#[cfg(test)]
mod session_tests;

pub use api::{
    ProviderAdapterV2, ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderSessionRuntime,
};
pub use logic::{select_generation, v1_capabilities, v2_capabilities};
pub use driver::{OpenCodeAdapter, OpenCodeSettings};
pub use v1::OpenCodeV1Session;
pub use v2::OpenCodeV2Session;
