//! OpenCode `ProviderAdapter` for both server generations.
//!
//! Port of `upstream/packages/provider-opencode/src/server/adapter.ts` and
//! `server/v2/adapter.ts`. The traits are `turin_provider_core`'s
//! `ProviderAdapterV2` and `ProviderSessionRuntime`.

mod api;
mod driver;
mod logic;
mod records;
mod v1;
mod v2;

#[cfg(test)]
mod session_tests;

pub use api::{
    ProviderAdapterV2, ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderSessionRuntime,
};
pub use driver::{OpenCodeAdapter, OpenCodeSettings, shared_provider_session_id};
pub use logic::{
    approves, commands_from_list, format_probe_error, models_from_provider_list,
    open_code_child_permission_rules, opencode_driver, pending_status, permission_rule_equals,
    select_generation, session_grant_label, skills_from_list, v1_capabilities, v1_rules_as_session,
    v2_capabilities,
};
pub use v1::OpenCodeV1Session;
pub use v2::OpenCodeV2Session;
