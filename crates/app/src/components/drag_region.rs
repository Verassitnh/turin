//! Replaces upstream's `.drag-region` (`-webkit-app-region: drag`) for the
//! frameless macOS window: pressing and moving on a header moves the window,
//! double-clicking zooms it per the system preference. Interactive children
//! keep their clicks because the move only starts once the pointer moves
//! with the button held on the region itself.

use gpui_kit::component::InteractiveElementExt as _;
use gpui_kit::{App, Div, InteractiveElement as _, MouseButton, Stateful, Window};

pub trait DragRegion: Sized {
    fn drag_region(self, window: &mut Window, cx: &mut App) -> Self;
}

#[derive(Default)]
struct DragState {
    armed: bool,
}

impl DragRegion for Stateful<Div> {
    fn drag_region(mut self, window: &mut Window, cx: &mut App) -> Self {
        let id = self.interactivity().element_id.clone();
        let key = format!("drag-region-{id:?}");
        let state = window.use_keyed_state(key, cx, |_, _| DragState::default());
        self.on_mouse_down(
            MouseButton::Left,
            window.listener_for(&state, |state, _, _, _| state.armed = true),
        )
        .on_mouse_up(
            MouseButton::Left,
            window.listener_for(&state, |state, _, _, _| state.armed = false),
        )
        .on_mouse_down_out(window.listener_for(&state, |state, _, _, _| state.armed = false))
        .on_mouse_move(window.listener_for(&state, |state, _, window, _| {
            if state.armed {
                state.armed = false;
                window.start_window_move();
            }
        }))
        .on_double_click(|_, window, _| window.titlebar_double_click())
    }
}
