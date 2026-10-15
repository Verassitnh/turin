//! Port of upstream/packages/contracts/src (kept modules only; see PORTING.md).
#![allow(clippy::large_enum_variant)] // Wire unions mirror upstream shapes; boxing would leak into every consumer.

#[macro_use]
pub mod base_schemas;
#[macro_use]
pub mod macros;
pub mod agent_sessions;
pub mod assistant_citations;
pub mod chat_attachment;
pub mod checkpoint_diff;
pub mod composer_context;
pub mod composer_context_clipboard;
pub mod editor;
pub mod environment;
pub mod filesystem;
pub mod git;
pub mod keybindings;
pub mod model;
pub mod model_selection;
pub mod orchestration_dispatch;
pub mod orchestration_project;
pub mod orchestration_v2;
pub mod project;
pub mod provider;
pub mod provider_instance;
pub mod provider_policy;
pub mod provider_runtime;
pub mod provider_setup;
pub mod provider_usage_limits;
pub mod pull_request;
pub mod pull_request_review_guide;
pub mod review;
pub mod settings;
pub mod source_control;
pub mod thread_pull_request;
pub mod thread_search;
pub mod thread_title;
pub mod usage;
pub mod usage_limit_source_id;
pub mod vcs;

pub use base_schemas::*;
