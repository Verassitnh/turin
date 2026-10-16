//! Dev hook for screenshots and manual checks (`TURIN_SETTINGS=<page>`): shows
//! the settings window on the [`MockSettingsSource`]. T22 opens the same view
//! from the app's `/settings` route.
//!
//! - `TURIN_SETTINGS=general|appearance|keybindings|providers|source-control|
//!   projects|storage|archived|licenses`: the page to open.
//! - `TURIN_SETTINGS_OPEN=<id>[,<id>..]`: opens selects / pickers / dialogs (`default-model`,
//!   `expand:<row>`, `provider:<instance>`, `scroll:<px>`, `add-hub`, ...).
//! - `TURIN_SETTINGS_QUERY=<text>`: types into the sidebar search.
//! - `TURIN_SETTINGS_SHOT=<file.png>`: captures the window after it settled and quits.

use std::time::Duration;

use gpui_kit::{App, AppContext as _, WindowOptions};

use super::mock_source::MockSettingsSource;
use super::view::{SettingsRoute, SettingsView};

/// Opens the settings window when `TURIN_SETTINGS` is set. Returns whether it did.
pub fn open_if_requested(options: WindowOptions, cx: &mut App) -> bool {
    let Ok(page) = std::env::var("TURIN_SETTINGS") else {
        return false;
    };
    let route = SettingsRoute::from_name(&page).unwrap_or_else(|| {
        tracing::warn!(%page, "unknown settings page, showing General");
        SettingsRoute::from_name("general").expect("general route")
    });
    let open_menu = std::env::var("TURIN_SETTINGS_OPEN").ok();
    let query = std::env::var("TURIN_SETTINGS_QUERY").ok();
    let store = MockSettingsSource::store(cx);
    let (handle, _view) = gpui_kit::open_window(options, cx, move |window, cx| {
        let view = cx.new(|cx| SettingsView::new(store, window, cx));
        view.update(cx, |view, cx| {
            view.set_route(route, cx);
            for menu in open_menu
                .iter()
                .flat_map(|menus| menus.split(','))
                .filter(|menu| !menu.is_empty())
            {
                view.open_menu_for_demo(menu.to_owned(), cx);
            }
            if let Some(query) = &query {
                view.set_nav_query_for_demo(query.clone(), window, cx);
            }
        });
        view
    })
    .expect("failed to open window");
    if let Ok(path) = std::env::var("TURIN_SETTINGS_SHOT") {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(3500))
                .await;
            let _ = &handle;
            // Capture only this window (never the screen): `TURIN_SETTINGS_WINID`
            // names a helper that prints the CGWindowID of a pid's window.
            let pid = std::process::id().to_string();
            let tool = std::env::var("TURIN_SETTINGS_WINID").unwrap_or_default();
            let id = std::process::Command::new(&tool)
                .arg(&pid)
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|id| id.trim().to_owned())
                .filter(|id| !id.is_empty());
            match id {
                Some(id) => {
                    let status = std::process::Command::new("screencapture")
                        .args(["-x", "-o", "-l", &id, &path])
                        .status();
                    tracing::info!(?status, %id, %path, "settings screenshot");
                }
                None => tracing::warn!("settings screenshot skipped: set TURIN_SETTINGS_WINID"),
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
    cx.activate(true);
    true
}
