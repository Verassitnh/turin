//! Turin: t3 fork but without all the bullshit.

mod app_runtime;
mod assets;
mod branding;
mod desktop;
mod shell;
// Pure composer logic is ported ahead of the views that consume it.
mod components;
#[allow(dead_code)]
mod composer;
mod diff;
#[cfg(target_os = "macos")]
mod dock_icon;
mod hooks;
mod onboarding;
mod project_icon_colors;
mod project_identity;
mod pull_request;
mod pull_request_reference;
mod settings_logic;
// The view-model layer is a library until the orchestrator and views are wired
// to it; every upstream selector is ported ahead of its callers.
#[allow(dead_code)]
mod state;
// Pure settings logic is ported ahead of the views that consume it.
#[allow(dead_code)]
mod settings;
mod theme;
mod timestamp_format;
// The ui primitives are a library: every upstream variant and size is ported
// ahead of its callers.
#[allow(dead_code)]
mod ui;
mod utils;

use gpui_kit::*;

use components::app_sidebar_layout::{self, AppSidebarLayout};
use components::onboarding::first_run_gate::FirstRunGate;
use components::onboarding::mock_source::{FirstRunScenario, MockOnboardingBackend};

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

const FIRST_RUN_DECISION_TIMEOUT_MS: u64 = 4_000;

/// Screenshot harness: `TURIN_SHOT=<file.png>` captures the app window once it
/// settled (`TURIN_SHOT_DELAY_MS`, default 3500) and quits. `TURIN_WINID` names
/// a helper that prints the CGWindowID of a pid's window, so only that window
/// is captured, never the screen.
fn schedule_screenshot(cx: &mut App) {
    let Ok(path) = std::env::var("TURIN_SHOT") else {
        return;
    };
    let mut delay = std::env::var("TURIN_SHOT_DELAY_MS")
        .ok()
        .and_then(|delay| delay.parse().ok())
        .unwrap_or(3500);
    match std::env::var("TURIN_FIRST_RUN").as_deref() {
        Ok("stalled") => delay = delay.max(FIRST_RUN_DECISION_TIMEOUT_MS + 500),
        Ok("settings-failed") => delay = delay.max(500),
        _ => {}
    }
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(delay))
            .await;
        let pid = std::process::id().to_string();
        let tool = std::env::var("TURIN_WINID").unwrap_or_default();
        let id = tool
            .split_whitespace()
            .next()
            .map(|program| {
                let mut winid = std::process::Command::new(program);
                for arg in tool.split_whitespace().skip(1) {
                    winid.arg(arg);
                }
                winid.arg(&pid).output()
            })
            .and_then(Result::ok)
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
            desktop::bind_menu_keys(cx, app_sidebar_layout::KEY_CONTEXT);
            desktop::install_native_menus(cx);
            let right_panel = dev_overrides(cx);
            // Dev hook: `TURIN_SETTINGS=<page>` shows the settings window on mock data.
            if settings::demo::open_if_requested(
                desktop::merge_window_options(window_options(cx), cx),
                cx,
            ) {
                schedule_screenshot(cx);
                cx.activate(true);
                return;
            }
            let handle = gpui_kit::open_window(
                desktop::merge_window_options(window_options(cx), cx),
                cx,
                |window, cx| {
                    let layout = cx.new(|cx| {
                        let mut layout = AppSidebarLayout::new(window, cx);
                        layout.set_right_panel_open(right_panel);
                        // Screenshot harness: `TURIN_PAGE=usage` opens the Usage route.
                        if std::env::var("TURIN_PAGE").is_ok_and(|page| page == "usage") {
                            layout.open_usage(window, cx);
                        }
                        layout
                    });
                    // T22 swaps the mock backend for the live settings and workspace state.
                    let backend = MockOnboardingBackend::shared(FirstRunScenario::from_env());
                    cx.new(|_| FirstRunGate::new(backend.clone(), backend, true, layout))
                },
            )
            .expect("failed to open window");
            // Dev hook: `TURIN_PULL_REQUESTS_DEMO` shows the pull request views on the mock source.
            if let Some(scenario) = pull_request::demo::scenario_from_env() {
                let (window_handle, gate_entity) = handle;
                let layout = gate_entity.read(cx).layout().clone();
                window_handle
                    .update(cx, |_, window, cx| {
                        pull_request::demo::install(&layout, scenario, window, cx)
                    })
                    .ok();
            }
            schedule_screenshot(cx);
            cx.activate(true);
        });
}
