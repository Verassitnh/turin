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
