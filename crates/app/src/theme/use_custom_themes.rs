//! Port of upstream/apps/web/src/hooks/useCustomThemes.ts.

// Ported ahead of the views that consume it (theme UI, T22 wiring).
#![allow(dead_code)]

use super::theme_palette_library::{get_custom_themes, subscribe_to_custom_themes};
use super::theme_palettes::ThemeDefinition;

/// `useCustomThemes`: the saved themes now, plus a subscription for the GPUI
/// layer to re-read on change (`useSyncExternalStore(subscribeToCustomThemes,
/// getCustomThemes)`).
pub fn use_custom_themes() -> Vec<ThemeDefinition> {
    get_custom_themes()
}

/// Subscribes to library changes; returns the unsubscribe handle.
pub fn watch_custom_themes(
    listener: impl Fn() + Send + Sync + 'static,
) -> Box<dyn FnOnce() + Send> {
    subscribe_to_custom_themes(listener)
}
