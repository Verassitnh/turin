//! Turin: t3 fork but without all the bullshit.

mod app_runtime;
mod assets;
mod branding;
// Pure composer logic is ported ahead of the views that consume it.
mod components;
#[allow(dead_code)]
mod composer;
mod diff;
#[cfg(target_os = "macos")]
mod dock_icon;
mod hooks;
#[allow(dead_code)]
mod lib_color;
mod project_icon_colors;
mod project_identity;
mod pull_request;
mod pull_request_reference;
// The view-model layer is a library until the orchestrator and views are wired
// to it; every upstream selector is ported ahead of its callers.
#[allow(dead_code)]
mod state;
// Pure settings logic is ported ahead of the views that consume it.
#[allow(dead_code)]
mod settings;
// Scope resolution and settings planning from task/R3-settings-logic (the
// scope UI of `settings` is its first caller).
#[allow(dead_code)]
mod settings_logic;
mod theme;
mod timestamp_format;
// The ui primitives are a library: every upstream variant and size is ported
// ahead of its callers.
#[allow(dead_code)]
mod ui;
mod utils;

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

fn main() {
    tracing_subscriber::fmt::init();
    // The view-model reducers and the read path run on tokio, off the UI thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let state::store::ClientStateRuntime {
        sender,
        requests,
        handle,
    } = state::store::spawn_client_state(runtime.handle(), state::local_environment_id());
    // Opens Turin's database and feeds the store. Kept alive for the whole
    // process; dropping it aborts the feeds.
    let _feeds = tokio_util::task::AbortOnDropHandle::new(runtime.handle().spawn(
        app_runtime::run(app_runtime::data_dir::resolve_data_dir(), sender, requests),
    ));
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            #[cfg(target_os = "macos")]
            dock_icon::set_unbundled_app_icon();
            theme::init(cx);
            let store = state::store::bind_client_store(handle.clone(), cx);
            cx.set_global(state::store::ClientStoreGlobal(store));
            app_sidebar_layout::bind_keys(cx);
            let right_panel = dev_overrides(cx);
            // Dev hook: `TURIN_SETTINGS=<page>` shows the settings window on mock data.
            if settings::demo::open_if_requested(window_options(cx), cx) {
                return;
            }
            let handle = gpui_kit::open_window(window_options(cx), cx, |window, cx| {
                cx.new(|cx| {
                    let mut layout = AppSidebarLayout::new(window, cx);
                    layout.set_right_panel_open(right_panel);
                    layout
                })
            })
            .expect("failed to open window");
            // Dev hook: `TURIN_PULL_REQUESTS_DEMO` shows the pull request views on the mock source.
            if let Some(scenario) = pull_request::demo::scenario_from_env() {
                let (window_handle, layout) = handle;
                window_handle
                    .update(cx, |_, window, cx| {
                        pull_request::demo::install(&layout, scenario, window, cx)
                    })
                    .ok();
            }
            cx.activate(true);
        });
}
