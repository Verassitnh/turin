//! turin-launcher (see PORTING.md for upstream mapping)
//!
//! macOS-only: the Linux and Windows launch branches of upstream
//! `externalLauncher.ts` are cut (see PORTING.md Deviations).

pub mod editor;
pub mod editor_labels;
pub mod editor_preferences;
pub mod external_launcher;
pub mod host;
pub mod icns;
pub mod native_app_icon_resolver;
pub mod open_in_picker_logic;
pub mod open_in_picker_shortcut;
