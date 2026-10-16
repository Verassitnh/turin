//! Port of upstream/apps/web/src/components/settings (+ the `/settings/*`
//! routes). See PORTING.md Deviations for the panels that are cut.
//!
//! Rendering goes through [`view_model::SettingsStore`]: the panels read a
//! [`view_model::SettingsViewModel`] and send intent through a
//! [`view_model::SettingsSource`]. [`mock_source`] backs the demo; T22 wires
//! the live services.

// GPUI element builders take many handles and boxed callbacks; splitting them
// into one-use structs would only move the arguments around.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod add_codex_account_dialog;
pub mod add_provider_instance_logic;
pub mod chat_gpt_welcome_coordinator;
pub mod chat_gpt_widgets;
pub mod codex_setup_logic;
pub mod codex_setup_section;
pub mod color_picker;
pub mod controls;
pub mod custom_model_editor;
pub mod custom_model_editor_logic;
pub mod demo;
pub mod dialog;
pub mod expandable_text;
pub mod folded_settings_section;
pub mod github_account_logic;
pub mod icons;
pub mod keybindings_logic;
pub mod layout;
pub mod locale;
pub mod mock_source;
pub mod model_picker;
pub mod nav;
pub mod panels;
pub mod panels_logic;
pub mod project_favicon_picker_dialog;
pub mod project_file_picker_logic;
pub mod project_icon_options;
pub mod project_icon_picker_dialog;
pub mod project_scripts;
pub mod project_settings_logic;
pub mod provider_accent_color_picker;
pub mod provider_auth_callback_coordinator;
pub mod provider_authentication;
pub mod provider_authentication_logic;
pub mod provider_form;
pub mod provider_meta;
pub mod provider_settings_logic;
pub mod provider_status;
pub mod provider_wizard_authentication_step;
pub mod rows;
pub mod scope;
pub mod search;
pub mod setting_inheritance;
pub mod slider;
pub mod time_format;
pub mod usage_shortcuts;
pub mod view;
pub mod view_model;
pub mod when_tree;
pub mod wizard;
