//! Port of upstream/apps/web/src/components/ProjectMonogram.tsx
//!
//! A `size-4` tile, `rounded-[25%]`, tinted 14% with the project color and
//! the monogram in bold mono at 8.25px. `projectIconColors.ts` is not ported
//! yet; callers pass the resolved color.

use gpui_kit::{
    App, FontWeight, Hsla, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div, px, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;

#[derive(IntoElement)]
pub struct ProjectMonogram {
    text: SharedString,
    color: Hsla,
}

impl ProjectMonogram {
    pub fn new(text: impl Into<SharedString>, color: Hsla) -> Self {
        Self {
            text: text.into(),
            color,
        }
    }
}

impl RenderOnce for ProjectMonogram {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(rems(1.))
            .rounded(px(4.))
            .overflow_hidden()
            .bg(with_alpha(self.color, 0.14))
            .text_color(self.color)
            .font_family(cx.app_theme().fonts.mono.clone())
            .font_weight(FontWeight::BOLD)
            .text_size(px(8.25))
            .child(self.text)
    }
}
