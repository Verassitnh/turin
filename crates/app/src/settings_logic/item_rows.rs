//! Port of upstream/apps/web/src/components/settings/itemRows.ts.
//!
//! Tailwind class strings. The GPUI settings rows apply the same values
//! (`rounded-t-xl` / `rounded-b-xl` on the first/last row, `px-3 py-3 sm:px-4`)
//! in `settings/layout.rs`; the constants are kept for diffing against
//! upstream.

// Ported ahead of the views that consume it.
#![allow(dead_code)]

/// Direct row in a grouped settings section. Round only outer corners; the
/// parent owns borders and separators.
pub const ITEM_ROW_CLASSNAME: &str = "first:rounded-t-xl last:rounded-b-xl px-3 py-3 sm:px-4";

pub const ITEM_ROW_INNER_CLASSNAME: &str =
    "flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between";
