//! Pure helpers from upstream/packages/client-runtime/src (the files outside `state/`).
//! Plain data and free functions; nothing here touches GPUI.

pub mod composer_thread_items;
pub mod delayed_status;
pub mod diff_count;
pub mod git_actions;
pub mod handoff;
pub mod markdown_images;
pub mod media_actions;
pub mod media_reference;
pub mod media_source;
pub mod prompt_links;
pub mod provider_skills;
pub mod provider_types;
pub mod provider_update_required_models;
pub mod pull_request_markdown;
pub mod review_guide_tour;
pub mod shared_support;
pub mod source_control_clients;
pub mod text_paste;
pub mod thread_pull_request_compatibility;
pub mod usage;
pub mod usage_access;
pub mod usage_progress;
pub mod user_message;
pub mod vcs;
pub mod vcs_action;
pub mod vcs_ref;
pub mod vcs_ref_invalidation;
