//! Port of upstream/apps/web/src/workspaceTitlebar.ts plus the titlebar
//! geometry AppSidebarLayout.tsx sets as CSS variables.
//!
//! - `--workspace-controls-left`: `var(--desktop-window-controls-inset, 90px)`
//!   on macOS desktop while windowed (room for the traffic lights), the
//!   default `env(safe-area-inset-left) + 0.75rem` otherwise — in fullscreen
//!   macOS hides the traffic lights.
//! - `--workspace-titlebar-content-left`: controls-left + the titlebar
//!   control size (1.75rem) + its gap (0.75rem). Headers pad to it while the
//!   sidebar is collapsed (`COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS`), and the
//!   sidebar brand starts there.

use gpui_kit::{Pixels, Window, px};

use crate::theme::tokens::{
    WORKSPACE_CONTROLS_INSET, WORKSPACE_TITLEBAR_CONTROL_GAP, WORKSPACE_TITLEBAR_CONTROL_SIZE,
};

/// `--desktop-window-controls-inset` (preload.ts: `90 / zoomFactor`).
pub const DESKTOP_WINDOW_CONTROLS_INSET: f32 = 90.;

pub fn workspace_controls_left(window: &Window) -> Pixels {
    if cfg!(target_os = "macos") && !window.is_fullscreen() {
        px(DESKTOP_WINDOW_CONTROLS_INSET)
    } else {
        WORKSPACE_CONTROLS_INSET.to_pixels(window.rem_size())
    }
}

pub fn workspace_titlebar_content_left(window: &Window) -> Pixels {
    let rem = window.rem_size();
    workspace_controls_left(window)
        + WORKSPACE_TITLEBAR_CONTROL_SIZE.to_pixels(rem)
        + WORKSPACE_TITLEBAR_CONTROL_GAP.to_pixels(rem)
}
