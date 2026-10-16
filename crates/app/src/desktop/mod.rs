//! Port of upstream/apps/desktop/src/window/* and desktop settings.

pub mod context_menu_image;
pub mod desktop_application_menu;
pub mod desktop_client_settings;
pub mod desktop_window;
pub mod notification_badge;
pub mod quit_hold;

pub use desktop_application_menu::bind_menu_keys;
pub use desktop_window::{
    DisplayBounds, InitialMainWindowBounds, resolve_initial_main_window_bounds,
};
pub use notification_badge::{apply_notification_badge, clear_notification_badge_on_focus};
pub use quit_hold::{QuitHoldKeyInput, QuitHoldKeyPhase, QuitShortcutEffect, QuitShortcutHandler};
