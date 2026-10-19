//! Port of upstream/apps/web/src/components/ProjectMonogram.tsx
//!
//! A `size-4` tile, `rounded-[25%]`, tinted 14% with the project color and
//! the monogram in bold mono at 8.25px. `projectIconColors.ts` is not ported
//! yet; callers pass the resolved color.

use crate::theme::color::solid_over;
use gpui_kit::{
    App, FontWeight, Hsla, IntoElement, ParentElement as _, Rems, RenderOnce, SharedString,
    Styled as _, Window, div, px, rems,
};

use crate::theme::ActiveAppTheme;

#[derive(IntoElement)]
pub struct ProjectMonogram {
    text: SharedString,
    color: Hsla,
    size: Rems,
}

impl ProjectMonogram {
    pub fn new(text: impl Into<SharedString>, color: Hsla) -> Self {
        Self {
            text: text.into(),
            color,
            size: rems(1.),
        }
    }

    /// The `size-*` box (default `size-4`). The SVG upstream scales with its
    /// box, so radius (25%) and glyph (8.25 of 16) scale too.
    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for ProjectMonogram {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let box_px = f32::from(self.size.to_pixels(window.rem_size()));
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(self.size)
            .rounded(px(box_px * 0.25))
            .overflow_hidden()
            .bg(solid_over(self.color, 0.14, cx.app_theme().background))
            .text_color(self.color)
            .font_family(cx.app_theme().fonts.mono.clone())
            .font_weight(FontWeight::BOLD)
            .text_size(px(box_px * 8.25 / 16.))
            .child(self.text)
    }
}
