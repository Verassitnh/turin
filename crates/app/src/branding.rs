//! Port of upstream/apps/web/src/branding.ts (the product name only).
//!
//! Upstream reads the name from the desktop bridge and adds a stage label
//! (Dev / Alpha / Nightly) for hosted channels; Turin is one desktop build,
//! so the name is a constant.

/// `APP_BASE_NAME` (upstream default "T3 Code").
pub const APP_BASE_NAME: &str = "Turin";
