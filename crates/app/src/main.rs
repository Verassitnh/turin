//! Turin: t3 fork but without all the bullshit.

use gpui_kit::*;

struct Root;

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .items_center()
            .justify_center()
            .child("turin")
    }
}

fn main() {
    tracing_subscriber::fmt::init();
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Root))
            .expect("failed to open window");
    });
}
