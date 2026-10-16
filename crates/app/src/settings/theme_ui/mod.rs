//! The theme app UI (native GPUI, ported from apps/web/src/components/settings):
//! theme editor, import dialog, community theme search, color picker and the
//! appearance-panel helpers. See docs/handoff/R3.md.

// Ported ahead of the views that mount it.
#![allow(dead_code)]

pub mod icons;
pub mod theme_editor_panel;
pub mod theme_import_dialog;
pub mod theme_preview_circles;
pub mod theme_search_section;
pub mod theme_wireframe;
