//! App shell routing and desktop-adjacent coordinators (upstream apps/web).

pub mod app_route;
pub mod chat_route_shortcuts;
pub mod closed_view_store;
pub mod command_palette_bus;
pub mod confirm_dialog;
pub mod environment_coordinator;
pub mod reopen_closed_view;
pub mod thread_notifications;
pub mod thread_routes;
pub mod client_settings_persistence;
pub mod live;
pub mod live_settings;
pub mod pr_preferences;

pub use chat_route_shortcuts::bind_chat_route_keys;
