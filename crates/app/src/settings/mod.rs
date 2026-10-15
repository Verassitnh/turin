//! Port of upstream/apps/web/src/components/settings (+ the `/settings/*`
//! routes). See PORTING.md Deviations for the panels that are cut.
//!
//! Rendering goes through [`view_model::SettingsStore`]: the panels read a
//! [`view_model::SettingsViewModel`] and send intent through a
//! [`view_model::SettingsSource`]. [`mock_source`] backs the demo; T22 wires
//! the live services.

pub mod keybindings_logic;
pub mod locale;
pub mod usage_shortcuts;
pub mod panels_logic;
pub mod custom_model_editor_logic;
pub mod add_provider_instance_logic;
pub mod github_account_logic;
pub mod project_settings_logic;
pub mod provider_settings_logic;
pub mod provider_status;
pub mod search;
pub mod mock_source;
pub mod view_model;
pub mod controls;
pub mod icons;
pub mod layout;
pub mod dialog;
pub mod model_picker;
pub mod nav;
pub mod panels;
pub mod provider_meta;
pub mod rows;
pub mod view;
pub mod demo;
