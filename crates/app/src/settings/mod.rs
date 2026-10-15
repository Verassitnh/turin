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
