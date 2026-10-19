//! Startup failure (the database could not be opened) shown as a toast; the
//! sidebar alone would look like a fresh install with no data.

use gpui_kit::App;

use crate::app_runtime::write::bus::CommandBus;
use crate::ui::toast::{ToastOptions, toast_manager};
use crate::ui::toast_logic::ToastType;

pub fn start(bus: CommandBus, cx: &mut App) {
    let mut failure = bus.startup_failure();
    cx.spawn(async move |cx| {
        let message = loop {
            if let Some(message) = failure.borrow_and_update().clone() {
                break message;
            }
            if failure.changed().await.is_err() {
                return;
            }
        };
        cx.update(|app| {
            toast_manager(app).update(app, |store, cx| {
                store.add(
                    ToastOptions::new("Turin could not start")
                        .type_(ToastType::Error)
                        .description(message)
                        .timeout(0),
                    cx,
                );
            });
        });
    })
    .detach();
}
