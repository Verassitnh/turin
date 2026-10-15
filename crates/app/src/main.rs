//! Turin: t3 fork but without all the bullshit.

mod assets;
mod branding;
mod components;
mod diff;
mod hooks;
mod onboarding;
mod project_icon_colors;
mod project_identity;
// The view-model layer is a library until the orchestrator and views are wired
// to it; every upstream selector is ported ahead of its callers.
#[allow(dead_code)]
mod state;
mod theme;
mod timestamp_format;
// The ui primitives are a library: every upstream variant and size is ported
// ahead of its callers.
#[allow(dead_code)]
mod ui;

use gpui_kit::*;

use components::app_sidebar_layout::{self, AppSidebarLayout};

/// Upstream's Electron window: 1100×780 default, 840×620 minimum,
/// `titleBarStyle: "hiddenInset"` with the traffic lights centred in the
/// 52px topbar (`{ x: 16, y: 52 / 2 - 7 }`).
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(
                px(1100.),
                px(std::env::var("TURIN_WINDOW_HEIGHT")
                    .ok()
                    .and_then(|height| height.parse().ok())
                    .unwrap_or(780.)),
            ),
            cx,
        ))),
        window_min_size: Some(size(px(840.), px(620.))),
        titlebar: Some(TitlebarOptions {
            title: Some(branding::APP_BASE_NAME.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(16.), px(19.))),
        }),
        // Header drag regions move the window themselves (components::drag_region).
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// Dev overrides for screenshots: `TURIN_APPEARANCE=light|dark|<theme id>` sets
/// the theme preference, `TURIN_RIGHT_PANEL=1` opens the right panel.
fn dev_overrides(cx: &mut App) -> bool {
    // Any ThemePreference: light, dark, system or a theme id.
    if let Ok(theme) = std::env::var("TURIN_APPEARANCE") {
        theme::set_preferences(
            theme::AppearancePreferences {
                theme,
                ..Default::default()
            },
            cx,
        );
    }
    std::env::var("TURIN_RIGHT_PANEL").is_ok_and(|v| v == "1")
}

/// Screenshot harness: `TURIN_SHOT=<file.png>` captures the app window once it
/// settled (`TURIN_SHOT_DELAY_MS`, default 3500) and quits. `TURIN_WINID` names
/// a helper that prints the CGWindowID of a pid's window, so only that window
/// is captured, never the screen.
fn schedule_screenshot(cx: &mut App) {
    let Ok(path) = std::env::var("TURIN_SHOT") else {
        return;
    };
    let delay = std::env::var("TURIN_SHOT_DELAY_MS")
        .ok()
        .and_then(|delay| delay.parse().ok())
        .unwrap_or(3500);
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(delay))
            .await;
        let pid = std::process::id().to_string();
        let tool = std::env::var("TURIN_WINID").unwrap_or_default();
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
                tracing::info!(?status, %id, %path, "screenshot");
            }
            None => tracing::warn!("screenshot skipped: set TURIN_WINID"),
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

fn main() {
    tracing_subscriber::fmt::init();
    // The view-model reducers run on tokio, off the UI thread. The orchestrator
    // feeds `state::store::ClientStateRuntime::sender` and drains `requests`;
    // until it is wired in, the store simply stays empty.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let client_state =
        state::store::spawn_client_state(runtime.handle(), state::local_environment_id());
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::init(cx);
            let store = state::store::bind_client_store(client_state.handle.clone(), cx);
            cx.set_global(state::store::ClientStoreGlobal(store));
            app_sidebar_layout::bind_keys(cx);
            let right_panel = dev_overrides(cx);
            gpui_kit::open_window(window_options(cx), cx, |window, cx| {
                cx.new(|cx| {
                    let mut layout = AppSidebarLayout::new(window, cx);
                    layout.set_right_panel_open(right_panel);
                    // Screenshot harness: `TURIN_PAGE=usage` opens the Usage route.
                    if std::env::var("TURIN_PAGE").is_ok_and(|page| page == "usage") {
                        layout.open_usage(window, cx);
                    }
                    layout
                })
            })
            .expect("failed to open window");
            schedule_screenshot(cx);
            cx.activate(true);
        });
}
