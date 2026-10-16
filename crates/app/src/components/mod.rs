//! Ports of upstream/apps/web/src/components (app-level screens and chrome).

pub mod app_sidebar_layout;
pub mod branch_toolbar;
pub mod chat;
// First batch of ChatView.logic.ts; the chat view does not call these yet.
pub mod chat_view;
#[allow(dead_code)]
pub mod chat_view_logic;
pub mod command_block;
pub mod command_palette;
pub mod drag_region;
pub mod git_actions_control;
pub mod project_favicon;
pub mod project_monogram;
pub mod provider_update_launch_notification_logic;
pub mod right_panel_tabs;
pub mod sidebar;
pub mod thread_action_menu_logic;
pub mod thread_command_subtitle;
pub mod thread_search_match;
pub mod thread_sidebar_width;
pub mod workspace_breadcrumb;
pub mod workspace_titlebar;
