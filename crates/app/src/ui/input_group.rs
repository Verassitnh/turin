//! Port of upstream/apps/web/src/components/ui/input-group.tsx
//!
//! `rounded-[var(--control-radius)] border` with the default, popover, and
//! ghost surfaces. Addons pad `calc(--spacing(3) - 1px)` (11px), or 9px at
//! `sm`. Block addons stack; inline addons sit at the start or end.
//!
//! Not ported: the focus ring and the addon mousedown-to-focus behavior
//! (the input inside owns focus). The popover surface is
//! [`super::tooltip::dropdown_glass_background`].

use gpui_kit::{
    AnyElement, App, Hsla, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window, div, px,
    rems,
};

use super::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, CONTROL_RADIUS, RADIUS_2XL, TEXT_SM};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputGroupVariant {
    #[default]
    Default,
    Popover,
    Ghost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputGroupSize {
    #[default]
    Default,
    Lg,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputGroupAlign {
    #[default]
    InlineStart,
    InlineEnd,
    BlockStart,
    BlockEnd,
}

fn addon_pad(size: InputGroupSize) -> Pixels {
    match size {
        InputGroupSize::Default => px(11.),
        InputGroupSize::Lg => px(11.),
    }
}

fn addon_pad_sm() -> Pixels {
    px(9.)
}

#[derive(IntoElement)]
pub struct InputGroup {
    variant: InputGroupVariant,
    size: InputGroupSize,
    children: Vec<AnyElement>,
}

impl InputGroup {
    pub fn new() -> Self {
        Self {
            variant: InputGroupVariant::Default,
            size: InputGroupSize::Default,
            children: Vec::new(),
        }
    }

    pub fn variant(mut self, variant: InputGroupVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: InputGroupSize) -> Self {
        self.size = size;
        self
    }
}

impl Default for InputGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for InputGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for InputGroup {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut root = div()
            .relative()
            .flex()
            .w_full()
            .min_w_0()
            .items_center()
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .text_color(theme.foreground);
        root = match self.variant {
            InputGroupVariant::Default => root
                .rounded(CONTROL_RADIUS)
                .border_1()
                .border_color(theme.input)
                .bg(if theme.is_dark() {
                    with_alpha(theme.input, 0.32).into()
                } else {
                    theme.background
                })
                .shadow(tokens::shadow_xs(0.05)),
            InputGroupVariant::Popover => root
                .rounded(RADIUS_2XL)
                .border_1()
                .border_color(dropdown_glass_border(theme))
                .bg(dropdown_glass_background(theme)),
            InputGroupVariant::Ghost => root
                .rounded(CONTROL_RADIUS)
                .border_1()
                .border_color::<Hsla>(with_alpha(theme.foreground, 0.).into()),
        };
        if self.size == InputGroupSize::Lg {
            root = root.h(rems(2.25));
        }
        root.children(self.children)
    }
}

#[derive(IntoElement)]
pub struct InputGroupAddon {
    align: InputGroupAlign,
    /// `[[data-size=sm]+&]` padding.
    sm: bool,
    children: Vec<AnyElement>,
}

impl InputGroupAddon {
    pub fn new(align: InputGroupAlign) -> Self {
        Self {
            align,
            sm: false,
            children: Vec::new(),
        }
    }

    pub fn sm(mut self, sm: bool) -> Self {
        self.sm = sm;
        self
    }
}

impl ParentElement for InputGroupAddon {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for InputGroupAddon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let pad = if self.sm {
            addon_pad_sm()
        } else {
            addon_pad(InputGroupSize::Default)
        };
        let mut root = div()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(cx.app_theme().muted_foreground);
        root = match self.align {
            InputGroupAlign::InlineStart => root.pl(pad),
            InputGroupAlign::InlineEnd => root.pr(pad),
            InputGroupAlign::BlockStart => root.w_full().justify_start().px(pad).pt(pad),
            InputGroupAlign::BlockEnd => root.w_full().justify_start().px(pad).pb(pad),
        };
        root.children(self.children)
    }
}
