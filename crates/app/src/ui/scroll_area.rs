//! Port of upstream/apps/web/src/components/ui/scroll-area.tsx
//!
//! `relative size-full min-h-0 overflow-hidden` with an overlay scrollbar:
//! 6px thumb in `--app-scrollbar-thumb`, revealed while scrolling or hovered
//! (gpui-component's scrollbar, themed in `theme::component_theme`).
//! `scrollFade` masks are not ported (GPUI has no alpha masks).

use gpui_kit::component::scroll::{Scrollable, ScrollableElement as _};
use gpui_kit::{
    Div, ElementId, InteractiveElement as _, Stateful, StatefulInteractiveElement as _,
    Styled as _, div,
};

/// A vertical scroll area with the app's overlay scrollbar.
#[track_caller]
pub fn scroll_area(id: impl Into<ElementId>) -> Scrollable<Stateful<Div>> {
    div().id(id).size_full().min_h_0().overflow_y_scrollbar()
}

/// `hideScrollbars`: scrolls without drawing a scrollbar (the sidebar list).
pub fn scroll_area_hidden(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).size_full().min_h_0().overflow_y_scroll()
}
