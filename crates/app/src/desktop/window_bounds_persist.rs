//! Debounced persistence for `DesktopAppSettings.mainWindowBounds`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::{App, Bounds, Window, WindowBounds, px, size};

use super::desktop_app_settings::{
    DesktopAppSettings, desktop_settings_path, read_desktop_app_settings,
};
use super::desktop_window::{
    DesktopWindowBounds, DisplayBounds, MAIN_WINDOW_BOUNDS_PERSIST_DEBOUNCE_MS,
    normalize_main_window_bounds, resolve_initial_main_window_bounds,
};

fn pixel_i32(value: gpui_kit::Pixels) -> i32 {
    f32::from(value).round() as i32
}

pub fn debounce_delay() -> Duration {
    Duration::from_millis(MAIN_WINDOW_BOUNDS_PERSIST_DEBOUNCE_MS)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingWindowGeometry {
    pub bounds: DesktopWindowBounds,
    pub maximized: bool,
}

pub struct WindowBoundsPersist {
    path: PathBuf,
    generation: u64,
    pending: Option<PendingWindowGeometry>,
}

impl WindowBoundsPersist {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            generation: 0,
            pending: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records a change and returns the generation a 500ms timer must match.
    pub fn note_bounds_changed(&mut self, window: &Window) -> Option<u64> {
        Some(self.note_geometry(capture_normal_geometry(window)?))
    }

    pub fn note_geometry(&mut self, geometry: PendingWindowGeometry) -> u64 {
        self.pending = Some(geometry);
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// `None` when a later change reset the debounce.
    pub fn pending_if_current(&self, generation: u64) -> Option<PendingWindowGeometry> {
        if self.generation == generation {
            self.pending.clone()
        } else {
            None
        }
    }

    /// Cancels the debounce and returns the geometry to write immediately.
    pub fn flush(&mut self, window: &Window) -> Option<PendingWindowGeometry> {
        self.generation = self.generation.wrapping_add(1);
        let geometry = capture_normal_geometry(window)?;
        self.pending = None;
        Some(geometry)
    }
}

pub fn capture_normal_geometry(window: &Window) -> Option<PendingWindowGeometry> {
    let window_bounds = window.window_bounds();
    let rect = window_bounds.get_bounds();
    let bounds = normalize_main_window_bounds(DesktopWindowBounds {
        x: pixel_i32(rect.origin.x),
        y: pixel_i32(rect.origin.y),
        width: pixel_i32(rect.size.width),
        height: pixel_i32(rect.size.height),
    })?;
    Some(PendingWindowGeometry {
        bounds,
        maximized: matches!(window_bounds, WindowBounds::Maximized(_)),
    })
}

pub fn initial_window_bounds(cx: &App) -> WindowBounds {
    let path = desktop_settings_path();
    let read = cx
        .background_executor()
        .spawn(async move { read_desktop_app_settings(&path).ok() });
    let persisted = cx.foreground_executor().block_on(read).unwrap_or_default();
    let displays: Vec<DisplayBounds> = cx
        .displays()
        .iter()
        .map(|display| {
            let bounds = display.bounds();
            DisplayBounds {
                x: pixel_i32(bounds.origin.x),
                y: pixel_i32(bounds.origin.y),
                width: pixel_i32(bounds.size.width),
                height: pixel_i32(bounds.size.height),
            }
        })
        .collect();
    let rect = match resolve_initial_main_window_bounds(persisted.main_window_bounds, &displays) {
        super::desktop_window::InitialMainWindowBounds::Explicit(bounds) => Bounds::new(
            gpui_kit::point(px(bounds.x as f32), px(bounds.y as f32)),
            size(px(bounds.width as f32), px(bounds.height as f32)),
        ),
        super::desktop_window::InitialMainWindowBounds::DefaultSize => {
            Bounds::centered(None, size(px(1100.), px(780.)), cx)
        }
    };
    if persisted.main_window_maximized {
        WindowBounds::Maximized(rect)
    } else {
        WindowBounds::Windowed(rect)
    }
}

pub fn merge_window_options(
    mut options: gpui_kit::WindowOptions,
    cx: &App,
) -> gpui_kit::WindowOptions {
    options.window_bounds = Some(initial_window_bounds(cx));
    options
}

pub fn geometry_document(geometry: &PendingWindowGeometry) -> DesktopAppSettings {
    DesktopAppSettings {
        main_window_bounds: Some(geometry.bounds),
        main_window_maximized: geometry.maximized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(maximized: bool) -> PendingWindowGeometry {
        PendingWindowGeometry {
            bounds: DesktopWindowBounds {
                x: 10,
                y: 20,
                width: 1100,
                height: 780,
            },
            maximized,
        }
    }

    #[test]
    fn later_change_resets_the_debounce_generation() {
        let mut persist = WindowBoundsPersist::new(PathBuf::from("unused.json"));
        let first = persist.note_geometry(geometry(false));
        let second = persist.note_geometry(geometry(true));
        assert!(persist.pending_if_current(first).is_none());
        assert!(persist.pending_if_current(second).unwrap().maximized);
        assert_eq!(debounce_delay(), Duration::from_millis(500));
    }
}
