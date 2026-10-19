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
#[allow(dead_code)]
mod lib_color;
mod logical_project;
mod lru_cache;
// The composer and the settings model pickers call it; the draft-store entry points
// (`resolve_app_model_selection`, the text-generation state) wait for their callers.
#[allow(dead_code)]
mod model_selection;
mod onboarding;
mod project_favicon_cache;
mod project_favicon_images;
mod project_favicon_storage;
mod project_icon_colors;
mod project_icon_options;
mod project_identity;
mod pull_request;
mod pull_request_reference;
mod settle_and_advance;
mod sidebar_pending_file_drop_store;
mod source_control_presentation;
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
// The thread route does not look bare filenames up yet.
#[allow(dead_code)]
mod workspace_basename_lookup;

use gpui_kit::*;

use components::app_sidebar_layout::{self, AppSidebarLayout};
use components::onboarding::backend::{FirstRunBackend, OnboardingBackend};
use components::onboarding::first_run_gate::FirstRunGate;
use components::onboarding::live_source::LiveOnboarding;
use components::onboarding::mock_source::{FirstRunScenario, MockOnboardingBackend};
use desktop::desktop_client_settings::client_settings_path;
use shell::client_settings_persistence::DesktopFileClientSettingsPersistence;
use shell::live_settings::start_live_client_settings;
use std::rc::Rc;

/// Upstream's Electron window: 1100×780 default, 840×620 minimum,
/// `titleBarStyle: "hiddenInset"` with the traffic lights centred in the
/// 52px topbar (`{ x: 16, y: 52 / 2 - 7 }`).
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(
                px(std::env::var("TURIN_WINDOW_WIDTH")
                    .ok()
                    .and_then(|width| width.parse().ok())
                    .unwrap_or(1100.)),
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

/// The main window and its shell, so app-level actions (menu items with no
/// focused window) can reach them.
struct MainWindow {
    handle: AnyWindowHandle,
    layout: WeakEntity<AppSidebarLayout>,
}

impl Global for MainWindow {}

/// Opens the main window: the first-run gate over the app shell.
fn open_main_window(
    right_panel: bool,
    command_bus: &app_runtime::write::bus::CommandBus,
    cx: &mut App,
) -> (AnyWindowHandle, Entity<FirstRunGate>) {
    let command_bus = command_bus.clone();
    let opened = gpui_kit::open_window(
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
            let (first, wizard): (Rc<dyn FirstRunBackend>, Rc<dyn OnboardingBackend>) =
                if std::env::var("TURIN_MOCK_UI").ok().as_deref() == Some("1")
                    || std::env::var("TURIN_SHOT").is_ok()
                    || std::env::var("TURIN_FIRST_RUN").is_ok()
                {
                    let backend = MockOnboardingBackend::shared(FirstRunScenario::from_env());
                    (backend.clone(), backend)
                } else if let (Some(data_dir), Some(store)) = (
                    app_runtime::data_dir::resolve_data_dir(),
                    cx.try_global::<state::store::ClientStoreGlobal>()
                        .map(|global| global.0.clone()),
                ) {
                    let backend = LiveOnboarding::start(store, command_bus.clone(), data_dir, cx);
                    (backend.clone(), backend)
                } else {
                    let backend = MockOnboardingBackend::shared(FirstRunScenario::from_env());
                    (backend.clone(), backend)
                };
            cx.new(|_| FirstRunGate::new(first, wizard, true, layout))
        },
    )
    .expect("failed to open window");
    let layout = opened.1.read(cx).layout().downgrade();
    cx.set_global(MainWindow {
        handle: opened.0,
        layout,
    });
    opened
}

/// Whether the main window still exists.
fn main_window_is_open(cx: &mut App) -> bool {
    let Some(handle) = cx.try_global::<MainWindow>().map(|main| main.handle) else {
        return false;
    };
    handle.update(cx, |_, _, _| ()).is_ok()
}

/// Brings the main window back (Dock click, or a menu item with no window).
fn reopen_main_window(command_bus: &app_runtime::write::bus::CommandBus, cx: &mut App) {
    if !main_window_is_open(cx) {
        open_main_window(false, command_bus, cx);
    }
    cx.activate(true);
}

/// App-level handlers for the menu, so Quit, Close Window, Settings and
/// Full Screen work with no window or with focus outside the shell (the
/// settings window, or none at all after the main window was closed).
fn install_window_lifecycle(command_bus: app_runtime::write::bus::CommandBus, cx: &mut App) {
    use desktop::desktop_application_menu::{
        AboutApplication, CloseWindow, HideApplication, HideOtherApplications, MinimizeWindow,
        OpenSettings, QuitApplication, ShowAllApplications, ToggleFullScreen, ZoomWindow,
    };
    cx.on_action(|_: &AboutApplication, _| desktop::about_panel::show_about_panel());
    cx.on_action(|_: &MinimizeWindow, cx| {
        if let Some(handle) = cx.active_window() {
            handle
                .update(cx, |_, window, _| window.minimize_window())
                .ok();
        }
    });
    cx.on_action(|_: &ZoomWindow, cx| {
        if let Some(handle) = cx.active_window() {
            handle.update(cx, |_, window, _| window.zoom_window()).ok();
        }
    });
    cx.on_action(|_: &QuitApplication, cx| cx.quit());
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(handle) = cx.active_window() {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
    });
    cx.on_action(|_: &ToggleFullScreen, cx| {
        if let Some(handle) = cx.active_window() {
            handle
                .update(cx, |_, window, _| window.toggle_fullscreen())
                .ok();
        }
    });
    cx.on_action(move |_: &OpenSettings, cx| {
        let main = cx
            .try_global::<MainWindow>()
            .map(|main| (main.handle, main.layout.clone()));
        let opened = main.is_some_and(|(handle, layout)| {
            handle
                .update(cx, |_, window, cx| {
                    layout
                        .update(cx, |layout, cx| {
                            layout.open_live_settings("general", window, cx)
                        })
                        .is_ok()
                })
                .unwrap_or(false)
        });
        if !opened {
            reopen_main_window(&command_bus, cx);
        }
    });
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
    let command_bus = app_runtime::write::bus::CommandBus::new(runtime.handle().clone());
    if std::env::var("TURIN_MOCK_UI").ok().as_deref() != Some("1") {
        let persistence = DesktopFileClientSettingsPersistence::new(client_settings_path());
        start_live_client_settings(runtime.handle(), persistence);
    }
    let _feeds =
        tokio_util::task::AbortOnDropHandle::new(runtime.handle().spawn(app_runtime::run(
            app_runtime::data_dir::resolve_data_dir(),
            sender,
            requests,
            command_bus.clone(),
        )));
    let favicon_runtime = runtime.handle().clone();
    let reopen_bus = command_bus.clone();
    let app = gpui_kit::application().with_assets(assets::AppAssets);
    // Dock click with no main window (Electron's `activate`).
    app.on_reopen(move |cx| reopen_main_window(&reopen_bus, cx));
    app.run(move |cx| {
        gpui_kit::init(cx);
        #[cfg(target_os = "macos")]
        dock_icon::set_unbundled_app_icon();
        theme::init(cx);
        crate::ui::toast::init(crate::ui::toast_logic::ToastPosition::TopRight, cx);
        // Device-local theme state (useTheme's localStorage) persists next to the
        // app data; the settings demo keeps it in memory.
        if std::env::var_os("TURIN_SETTINGS").is_none()
            && let Some(dir) = app_runtime::data_dir::resolve_data_dir()
        {
            theme::theme_runtime::persist_to(dir, cx);
        }
        // Project icons: found on disk, cached under the data dir, drawn by `ProjectFavicon`.
        project_favicon_images::ProjectFaviconImages::install(
            project_favicon_storage::project_favicon_cache(
                app_runtime::data_dir::resolve_data_dir().as_deref(),
            ),
            std::sync::Arc::new(
                turin_projects::project_favicon_resolver::ProjectFaviconResolver::new(
                    std::sync::Arc::new(
                        turin_git::workspace::workspace_paths::WorkspacePaths::new(
                            std::env::var("HOME").unwrap_or_default(),
                        ),
                    ),
                    None,
                ),
            ),
            favicon_runtime.clone(),
            cx,
        );
        let store = state::store::bind_client_store(handle.clone(), cx);
        cx.set_global(state::store::ClientStoreGlobal(store));
        cx.set_global(app_runtime::write::bus::CommandBusGlobal(
            command_bus.clone(),
        ));
        app_sidebar_layout::bind_keys(cx);
        composer::views::prompt_editor::bind_keys(cx);
        desktop::bind_menu_keys(cx);
        desktop::install_native_menus(cx);
        let right_panel = dev_overrides(cx);
        // Dev hook: `TURIN_UI_GALLERY=<page>` shows the S3 primitives instead of the shell.
        if let Some(page) = ui::gallery::page_from_env() {
            ui::toast::init(ui::toast_logic::ToastPosition::TopRight, cx);
            gpui_kit::open_window(window_options(cx), cx, |window, cx| {
                cx.new(|cx| ui::gallery::UiGallery::new(page, window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
            return;
        }
        // Dev hook: `TURIN_SETTINGS=<page>` shows the settings window on mock data.
        if settings::demo::open_if_requested(
            desktop::merge_window_options(window_options(cx), cx),
            cx,
        ) {
            schedule_screenshot(cx);
            cx.activate(true);
            return;
        }
        let handle = open_main_window(right_panel, &command_bus, cx);
        install_window_lifecycle(command_bus.clone(), cx);
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
