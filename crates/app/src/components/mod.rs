//! Ports of upstream/apps/web/src/components (app-level screens and chrome).

pub mod app_sidebar_layout;
pub mod branch_toolbar;
// Composer helpers are ported ahead of their callers; main's clippy gate flags
// them dead until the chat view wires them (merge note from R3).
#[allow(dead_code)]
pub mod chat;
// First batch of ChatView.logic.ts; the chat view does not call these yet.
pub mod chat_view;
#[allow(dead_code)]
pub mod chat_view_logic;
pub mod command_block;
pub mod command_palette;
#[allow(dead_code)]
pub mod composer_inline_token_paste;
pub mod confirm_dialog_host;
pub mod context_presentation_registry;
pub mod drag_region;
pub mod git_actions_control;
pub mod jetbrains_icons;
pub mod keybindings_config_warning;
pub mod keybindings_update_toast_logic;
// Shown by the thread route when no thread is open; the shell always routes a thread today.
#[allow(dead_code)]
pub mod no_active_thread_state;
pub mod onboarding;
pub mod preview_panel_shell;
pub mod project_favicon;
pub mod project_monogram;
pub mod prompt_links;
pub mod provider_instance_icon;
pub mod provider_update_dismissal;
pub mod provider_update_environment_rows;
pub mod provider_update_launch_notification;
pub mod provider_update_launch_notification_environments;
pub mod provider_update_launch_notification_logic;
pub mod provider_update_primary_notification;
pub mod provider_updates_action;
pub mod quit_hold_overlay;
pub mod render_error_boundary;
pub mod reopen_closed_view_shortcut;
pub mod right_panel_tabs;
pub mod search;
pub mod sidebar;
pub mod thread_action_menu_logic;
pub mod thread_command_subtitle;
pub mod thread_notification_coordinator;
pub mod thread_route_view;
pub mod thread_search_match;
pub mod thread_sidebar_width;
pub mod usage;
pub mod workspace_breadcrumb;
pub mod workspace_titlebar;
