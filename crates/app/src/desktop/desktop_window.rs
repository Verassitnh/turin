#![allow(dead_code)]

//! Port of upstream/apps/desktop/src/window/DesktopWindow.ts (GPUI window
//! geometry; renderer/Electron paths are cut).

use serde::{Deserialize, Serialize};

pub const MAIN_WINDOW_BOUNDS_PERSIST_DEBOUNCE_MS: u64 = 500;
pub const MACOS_WORKSPACE_TOPBAR_HEIGHT: f32 = 52.;
pub const MACOS_WINDOW_BUTTON_RADIUS: f32 = 7.;

pub const DEFAULT_MAIN_WINDOW_WIDTH: i32 = 1100;
pub const DEFAULT_MAIN_WINDOW_HEIGHT: i32 = 780;
pub const MIN_MAIN_WINDOW_WIDTH: i32 = 840;
pub const MIN_MAIN_WINDOW_HEIGHT: i32 = 620;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialMainWindowBounds {
    Explicit(DesktopWindowBounds),
    DefaultSize,
}

impl InitialMainWindowBounds {
    pub fn width_height(self) -> (i32, i32) {
        match self {
            Self::Explicit(bounds) => (bounds.width, bounds.height),
            Self::DefaultSize => (DEFAULT_MAIN_WINDOW_WIDTH, DEFAULT_MAIN_WINDOW_HEIGHT),
        }
    }
}

/// `windowFitsWithinDisplay`.
pub fn window_fits_within_display(window: DesktopWindowBounds, display: DisplayBounds) -> bool {
    window.x >= display.x
        && window.y >= display.y
        && window.x + window.width <= display.x + display.width
        && window.y + window.height <= display.y + display.height
}

/// `resolveInitialMainWindowBounds`.
pub fn resolve_initial_main_window_bounds(
    persisted: Option<DesktopWindowBounds>,
    displays: &[DisplayBounds],
) -> InitialMainWindowBounds {
    if let Some(bounds) = persisted
        && displays
            .iter()
            .any(|display| window_fits_within_display(bounds, *display))
    {
        return InitialMainWindowBounds::Explicit(bounds);
    }
    InitialMainWindowBounds::DefaultSize
}

/// macOS traffic-light Y (`getWindowTitleBarOptions` / `syncMacosWindowButtons`).
pub fn macos_traffic_light_y(zoom_factor: f32) -> f32 {
    (MACOS_WORKSPACE_TOPBAR_HEIGHT * zoom_factor) / 2. - MACOS_WINDOW_BUTTON_RADIUS
}

/// `concealPendingQuitWindow` maps to zero opacity on macOS/Windows; GPUI uses
/// window hide where supported.
pub fn should_conceal_window_for_pending_quit() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_persisted_bounds_when_visible() {
        let persisted = DesktopWindowBounds {
            x: 100,
            y: 100,
            width: 1100,
            height: 780,
        };
        let displays = [DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        assert_eq!(
            resolve_initial_main_window_bounds(Some(persisted), &displays),
            InitialMainWindowBounds::Explicit(persisted)
        );
        assert_eq!(
            resolve_initial_main_window_bounds(Some(persisted), &[]),
            InitialMainWindowBounds::DefaultSize
        );
    }
}
