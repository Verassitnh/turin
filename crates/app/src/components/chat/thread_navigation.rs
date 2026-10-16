//! Open-thread requests from the chat column (subagent parent links, thread chips).
//! Consumed by the shell the same way as [`crate::diff::request_open_turn_diff`].

use gpui_kit::{App, Global};

#[derive(Default)]
struct ThreadOpenRequests {
    thread_id: Option<String>,
}

impl Global for ThreadOpenRequests {}

pub fn request_open_thread(thread_id: &str, cx: &mut App) {
    cx.default_global::<ThreadOpenRequests>().thread_id = Some(thread_id.to_owned());
    cx.refresh_windows();
}

pub fn take_thread_open_request(cx: &mut App) -> Option<String> {
    cx.default_global::<ThreadOpenRequests>()
        .thread_id
        .take()
}
