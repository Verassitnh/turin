//! Port of upstream/apps/web/src/components/ui/empty.tsx.
//!
//! `Empty` is the centred column (`gap-6 p-6 md:p-12` by default, `compact` is a card-sized
//! notice with a 17px title), `EmptyHeader` the `max-w-sm` text block, `EmptyMedia` the icon
//! tile (`variant="icon"` draws the two tilted ghost tiles behind it as rotated cards are not
//! available in GPUI, they are drawn as offset flat tiles).

use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, px, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{self, RADIUS_MD, TEXT_SM, TEXT_XL};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmptySize {
    Compact,
    #[default]
    Default,
    Hero,
}

#[derive(IntoElement)]
pub struct Empty {
    size: EmptySize,
    children: Vec<AnyElement>,
}

impl Empty {
    pub fn new() -> Self {
        Self {
            size: EmptySize::Default,
            children: Vec::new(),
        }
    }

    pub fn size(mut self, size: EmptySize) -> Self {
        self.size = size;
        self
    }
}

impl Default for Empty {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Empty {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Empty {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let base = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_center()
            .text_center();
        match self.size {
            EmptySize::Compact => base.min_h(rems(16.)).gap_4().p(rems(2.5)),
            EmptySize::Default | EmptySize::Hero => base.gap_6().p(rems(3.)),
        }
        .children(self.children)
    }
}

/// `EmptyHeader`: `flex max-w-sm flex-col items-center text-center`.
pub fn empty_header() -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .max_w(rems(24.))
        .text_center()
}

/// `EmptyTitle`: `font-semibold text-xl` (17px / 24px when compact).
pub fn empty_title(text: impl Into<SharedString>, compact: bool) -> gpui_kit::Div {
    let title = div().font_weight(FontWeight::SEMIBOLD).child(text.into());
    if compact {
        title.text_size(rems(1.0625)).line_height(rems(1.5))
    } else {
        title
            .text_size(TEXT_XL.size)
            .line_height(TEXT_XL.line_height)
    }
}

/// `EmptyDescription`: `text-muted-foreground text-sm`, `mt-1` under a title.
pub fn empty_description(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .mt_1()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

/// `EmptyMedia variant="icon"`: a `size-9` bordered tile with two ghost tiles behind it.
pub fn empty_media_icon(icon: AnyElement, cx: &App) -> gpui_kit::Div {
    let theme = cx.app_theme();
    let tile = |bg: Hsla| {
        div()
            .size(rems(2.25))
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(theme.border)
            .bg(bg)
    };
    div()
        .relative()
        .mb_6()
        .child(
            tile(theme.card)
                .absolute()
                .bottom(px(1.))
                .left(px(-5.))
                .opacity(0.6),
        )
        .child(
            tile(theme.card)
                .absolute()
                .bottom(px(1.))
                .left(px(5.))
                .opacity(0.6),
        )
        .child(
            tile(theme.card)
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.foreground)
                .shadow(tokens::shadow_sm(0.05))
                .child(icon),
        )
}
