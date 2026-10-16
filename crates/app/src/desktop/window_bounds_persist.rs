//! Debounced persistence for `DesktopAppSettings.mainWindowBounds`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::{App, Bounds, Window, WindowBounds, px, size};

use super::desktop_app_settings::{read_desktop_app_settings, write_desktop_app_settings};
use super::desktop_window::{
    DesktopWindowBounds, DisplayBounds, MAIN_WINDOW_BOUNDS_PERSIST_DEBOUNCE_MS,
    resolve_initial_main_window_bounds,
};

fn pixel_i32(value: gpui_kit::Pixels) -> i32 {
    f32::from(value).round() as i32
}

pub fn desktop_settings_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    super::desktop_app_settings::default_desktop_app_settings_path(&home)
}

pub fn initial_window_bounds(cx: &App) -> WindowBounds {
    let path = desktop_settings_path();
    let persisted = read_desktop_app_settings(&path)
        .ok()
        .and_then(|doc| doc.main_window_bounds);
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
    match resolve_initial_main_window_bounds(persisted, &displays) {
        super::desktop_window::InitialMainWindowBounds::Explicit(bounds) => {
            WindowBounds::Windowed(Bounds::new(
                gpui_kit::point(px(bounds.x as f32), px(bounds.y as f32)),
                size(px(bounds.width as f32), px(bounds.height as f32)),
            ))
        }
        super::desktop_window::InitialMainWindowBounds::DefaultSize => {
            WindowBounds::Windowed(Bounds::centered(None, size(px(1100.), px(780.)), cx))
        }
    }
}

pub struct WindowBoundsPersist {
    path: PathBuf,
    debounce: Option<Instant>,
}

impl WindowBoundsPersist {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            debounce: None,
        }
    }

    pub fn note_bounds_changed(&mut self, window: &Window) {
        self.debounce = Some(Instant::now());
        let _ = window;
    }

    pub fn flush_if_due(&mut self, window: &Window) {
        let Some(started) = self.debounce else {
            return;
        };
        if started.elapsed() < Duration::from_millis(MAIN_WINDOW_BOUNDS_PERSIST_DEBOUNCE_MS) {
            return;
        }
        self.debounce = None;
        let bounds = window.window_bounds();
        let WindowBounds::Windowed(rect) = bounds else {
            return;
        };
        let next = DesktopWindowBounds {
            x: pixel_i32(rect.origin.x),
            y: pixel_i32(rect.origin.y),
            width: pixel_i32(rect.size.width),
            height: pixel_i32(rect.size.height),
        };
        let mut doc = read_desktop_app_settings(&self.path).unwrap_or_default();
        if doc.main_window_bounds == Some(next) {
            return;
        }
        doc.main_window_bounds = Some(next);
        let _ = write_desktop_app_settings(&self.path, &doc);
    }
}

pub fn merge_window_options(
    mut options: gpui_kit::WindowOptions,
    cx: &App,
) -> gpui_kit::WindowOptions {
    options.window_bounds = Some(initial_window_bounds(cx));
    options
}
