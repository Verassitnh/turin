//! Replaces upstream's `.drag-region` (`-webkit-app-region: drag`, index.css)
//! for the frameless macOS window: pressing on a header and dragging past a
//! small threshold moves the window; double-clicking the header itself zooms
//! it per the system preference.
//!
//! Upstream exempts interactive descendants with `.drag-region button, input,
//! textarea, select, a { -webkit-app-region: no-drag }`. Here every
//! interactive element over a drag region (ui `Button`/`Toggle`, sidebar icon
//! buttons, panel tabs) must call [`no_drag`] on left mouse-down: GPUI
//! bubbles mouse events front to back, so stopping propagation keeps the
//! region underneath from arming (no window move that would swallow the
//! click, no zoom from a double-click on a button). The element's own click
//! still fires because GPUI registers its click tracking ahead of its
//! mouse-down listeners.

use gpui_kit::component::InteractiveElementExt as _;
use gpui_kit::{
    App, Div, InteractiveElement as _, MouseButton, MouseDownEvent, Pixels, Point, Stateful, Window,
};

/// GPUI's own drag threshold (elements/div.rs `DRAG_THRESHOLD`).
const DRAG_THRESHOLD: f32 = 2.;

pub trait DragRegion: Sized {
    fn drag_region(self, window: &mut Window, cx: &mut App) -> Self;
}

/// The `no-drag` half: stop a left mouse-down from reaching a drag region
/// beneath an interactive element.
pub fn no_drag(_: &MouseDownEvent, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

#[derive(Default)]
struct DragState {
    /// Where the press landed on the region itself, while the button is held.
    pressed_at: Option<Point<Pixels>>,
}

impl DragRegion for Stateful<Div> {
    fn drag_region(mut self, window: &mut Window, cx: &mut App) -> Self {
        let id = self.interactivity().element_id.clone();
        let key = format!("drag-region-{id:?}");
        let state = window.use_keyed_state(key, cx, |_, _| DragState::default());
        self.on_mouse_down(
            MouseButton::Left,
            window.listener_for(&state, |state, event: &MouseDownEvent, _, _| {
                state.pressed_at = Some(event.position);
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            window.listener_for(&state, |state, _, _, _| state.pressed_at = None),
        )
        .on_mouse_down_out(window.listener_for(&state, |state, _, _, _| state.pressed_at = None))
        .on_mouse_move(window.listener_for(
            &state,
            |state, event: &gpui_kit::MouseMoveEvent, window, _| {
                let Some(start) = state.pressed_at else {
                    return;
                };
                if !event.dragging() {
                    state.pressed_at = None;
                    return;
                }
                let delta = event.position - start;
                let distance = f32::from(delta.x).hypot(f32::from(delta.y));
                if distance > DRAG_THRESHOLD {
                    state.pressed_at = None;
                    window.start_window_move();
                }
            },
        ))
        .on_double_click(|_, window, _| window.titlebar_double_click())
    }
}
