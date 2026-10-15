//! One import path for the contract types the view-model layer reads.
//!
//! `turin_contracts` exposes only `base_schemas` at its root; the entity types
//! live in their upstream-named modules.

#![allow(unused_imports)]

pub use turin_contracts::base_schemas::*;
pub use turin_contracts::chat_attachment::*;
pub use turin_contracts::composer_context::OrchestrationMessageContext;
pub use turin_contracts::environment::*;
pub use turin_contracts::model_selection::ModelSelection;
pub use turin_contracts::orchestration_project::OrchestrationProjectShell;
pub use turin_contracts::orchestration_v2::*;
pub use turin_contracts::project::{ProjectIconColor, ProjectIconOverride};
pub use turin_contracts::provider::ProviderUploadFeedbackResult;
pub use turin_contracts::provider_policy::*;
pub use turin_contracts::pull_request::{
    PullRequestState, RepositoryIdentityLike, pull_request_host_of,
};
pub use turin_contracts::settings::{ClientSettings, SidebarProjectGroupingMode};
pub use turin_contracts::source_control::SourceControlProviderKind;
pub use turin_contracts::thread_pull_request::*;
pub use turin_contracts::thread_search::*;
