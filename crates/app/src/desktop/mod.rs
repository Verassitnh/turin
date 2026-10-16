//! Port of upstream/apps/desktop/src/window/* and desktop settings.

pub mod context_menu_image;
pub mod desktop_app_settings;
pub mod desktop_application_menu;
pub mod desktop_client_settings;
pub mod desktop_window;
pub mod notification_badge;
pub mod quit_hold;
pub mod window_bounds_persist;

pub use desktop_application_menu::{bind_menu_keys, install_native_menus};
pub use window_bounds_persist::merge_window_options;
