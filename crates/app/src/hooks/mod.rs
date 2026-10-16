//! Ports of upstream/apps/web/src/hooks (the pure parts; React state wiring
//! lives on the owning GPUI entity).

pub mod chat_thread_actions;
pub mod thread_undo;
pub mod use_checkpoint_diff;
pub mod use_handle_new_thread;
pub mod use_now_minute;
pub mod use_open_panel_pull_request_url;
pub mod use_preview_panel_inline_size;
pub mod use_pull_request_actions;
pub mod use_pull_request_checks_refresh;
pub mod use_pull_request_linking;
pub mod use_pull_request_stack;
pub mod use_resizable_width;
pub mod use_sidebar_project_groups;
pub mod use_supports_multiple_pull_requests;
pub mod use_thread_action_menu;
pub mod use_thread_actions;
pub mod use_turn_diff_summaries;
