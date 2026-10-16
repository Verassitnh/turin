//! Port of upstream/apps/web/src/components/ThreadRouteView.tsx (route-scoped
//! chat surface; full draft promotion wiring lands with the composer task).

use gpui_kit::{IntoElement, ParentElement as _, div};

use crate::components::chat_view::ChatView;
use crate::shell::thread_routes::{ThreadRouteRenderState, ThreadRouteTarget};

/// Renders the chat column for the active thread route target.
pub fn thread_route_view(
    target: &ThreadRouteTarget,
    render_state: ThreadRouteRenderState,
    sidebar_open: bool,
    right_panel_open: bool,
    thread_header: Option<(gpui_kit::SharedString, gpui_kit::SharedString)>,
) -> gpui_kit::AnyElement {
    match render_state {
        ThreadRouteRenderState::Ready => ChatView {
            sidebar_open,
            right_panel_open,
            thread: thread_header,
        }
        .into_any_element(),
        ThreadRouteRenderState::Loading => div().flex_1().child("Loading…").into_any_element(),
        ThreadRouteRenderState::Missing => {
            div().flex_1().child("Thread not found").into_any_element()
        }
    }
}
