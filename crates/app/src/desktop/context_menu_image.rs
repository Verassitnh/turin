//! Port of upstream/apps/desktop/src/window/ContextMenuImage.ts.
//!
//! Browser `WebContents` image copy is cut (no embedded preview browser). The
//! native shell uses GPUI clipboard APIs for in-app images instead.

#[derive(Debug, thiserror::Error)]
#[error("context menu image copy is unavailable without the cut browser surface")]
pub struct ContextMenuImageUnavailable;
