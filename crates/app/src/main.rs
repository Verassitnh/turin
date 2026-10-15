//! Turin: t3 fork but without all the bullshit.

// The design system is the shared surface later UI tasks build on; not every
// token or primitive has a caller yet.
#[allow(dead_code)]
mod theme;
#[allow(dead_code)]
mod ui;

mod assets;

use gpui_kit::*;
use theme::ActiveAppTheme;

struct Root;

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child("turin")
    }
}

fn main() {
    tracing_subscriber::fmt::init();
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Root))
                .expect("failed to open window");
        });
}
