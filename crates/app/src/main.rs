//! Turin: t3 fork but without all the bullshit.

// The design system is the shared surface later UI tasks build on; not every
// token or primitive has a caller yet.
#[allow(dead_code)]
mod theme;
#[allow(dead_code)]
mod ui;

mod assets;
mod components;
mod hooks;
#[cfg(feature = "screenshot")]
mod screenshot;

use gpui_kit::*;

use components::app_sidebar_layout::{self, AppSidebarLayout};
use theme::theme_palettes::ThemeAppearance;

/// Upstream's Electron window: 1100×780 default, 840×620 minimum,
/// `titleBarStyle: "hiddenInset"` with the traffic lights centred in the
/// 52px topbar (`{ x: 16, y: 52 / 2 - 7 }`).
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1100.), px(780.)),
            cx,
        ))),
        window_min_size: Some(size(px(840.), px(620.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Turin".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(16.), px(19.))),
        }),
        // Header drag regions move the window themselves (components::drag_region).
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// Dev overrides for screenshots: `TURIN_APPEARANCE=light|dark` pins the
/// appearance, `TURIN_RIGHT_PANEL=1` opens the right panel.
fn dev_overrides(cx: &mut App) -> bool {
    let appearance = match std::env::var("TURIN_APPEARANCE").as_deref() {
        Ok("dark") => Some(ThemeAppearance::Dark),
        Ok("light") => Some(ThemeAppearance::Light),
        _ => None,
    };
    if appearance.is_some() {
        theme::set_preferences(
            theme::AppearancePreferences {
                appearance,
                ..Default::default()
            },
            cx,
        );
    }
    std::env::var("TURIN_RIGHT_PANEL").is_ok_and(|v| v == "1")
}

fn main() {
    tracing_subscriber::fmt::init();
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::init(cx);
            app_sidebar_layout::bind_keys(cx);
            let right_panel = dev_overrides(cx);
            let (_window, _) = gpui_kit::open_window(window_options(cx), cx, |window, cx| {
                cx.new(|cx| {
                    let mut layout = AppSidebarLayout::new(window, cx);
                    layout.set_right_panel_open(right_panel);
                    layout
                })
            })
            .expect("failed to open window");
            cx.activate(true);
            #[cfg(feature = "screenshot")]
            screenshot::schedule(_window, cx);
        });
}
