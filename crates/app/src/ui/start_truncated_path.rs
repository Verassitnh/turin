//! Port of upstream/apps/web/src/components/StartTruncatedPath.tsx
//!
//! `dir=rtl` plus `text-ellipsis` keeps the end of a path visible. GPUI's
//! `text_ellipsis_start` is that truncation. The full path is the `code`
//! tooltip.

use gpui_kit::{
    App, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window, div,
};

use super::tooltip::{Tooltip, TooltipVariant};

#[derive(IntoElement)]
pub struct StartTruncatedPath {
    id: SharedString,
    path: SharedString,
}

impl StartTruncatedPath {
    pub fn new(id: impl Into<SharedString>, path: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
        }
    }
}

impl RenderOnce for StartTruncatedPath {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let path = self.path;
        let tooltip = path.clone();
        div()
            .id(self.id)
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis_start()
            .text_left()
            .child(path)
            .tooltip(move |window, cx| {
                Tooltip::new(tooltip.clone())
                    .variant(TooltipVariant::Code)
                    .build(window, cx)
            })
    }
}
