//! Dev aid behind the `screenshot` feature: renders the window offscreen and
//! writes `TURIN_SCREENSHOT` as PNG, then quits. Used to compare the shell
//! against upstream without screen-recording permission.

use std::time::Duration;

use gpui_kit::{AnyWindowHandle, App};

pub fn schedule(window: AnyWindowHandle, cx: &mut App) {
    let Ok(path) = std::env::var("TURIN_SCREENSHOT") else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(1500))
            .await;
        let image = cx.update_window(window, |_, window, _| window.render_to_image());
        match image {
            Ok(Ok(image)) => match image.save(&path) {
                Ok(()) => eprintln!("screenshot: wrote {path}"),
                Err(error) => eprintln!("screenshot: {error}"),
            },
            Ok(Err(error)) => eprintln!("screenshot: render failed: {error}"),
            Err(error) => eprintln!("screenshot: window gone: {error}"),
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}
