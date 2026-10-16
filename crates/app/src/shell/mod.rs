//! App shell routing and desktop-adjacent coordinators (upstream apps/web).

pub mod app_route;
pub mod closed_view_store;
pub mod command_palette_bus;
pub mod confirm_dialog;
pub mod reopen_closed_view;
pub mod thread_notifications;
pub mod thread_routes;

pub use app_route::{AppRoute, NavigationStack, route_from_draft, route_from_thread};
pub use command_palette_bus::{is_command_palette_open, open_command_palette};
pub use confirm_dialog::{
    complete_confirm_dialog_close, read_confirm_dialog_state, register_confirm_dialog_host,
    request_confirm_dialog, respond_to_confirm_dialog,
};
pub use thread_routes::{ThreadRouteTarget, resolve_thread_route_target};
