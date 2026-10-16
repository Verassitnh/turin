//! Port of upstream/apps/web/src/components/ui/textarea.tsx
//!
//! The control chrome at the desktop (`sm:`) breakpoint: `rounded-lg border
//! border-input bg-background shadow-xs/5`, dark `bg-input/32`, `text-sm`.
//! Min heights are `min-h-17.5` / `16.5` / `18.5` (the `sm` size, default, and
//! `lg`), capped at `max-h-64`. Padding is `calc(--spacing(n) - 1px)`.
//! Put `gpui_kit::component::input::Textarea::new(&state).appearance(false)`
//! inside; gpui-component's own chrome does not match this frame.
//!
//! Not ported: the focus ring, the invalid border, and the inner lip shadow.

use gpui_kit::{App, Div, Styled, div, px, rems};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_SM};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextareaSize {
    Sm,
    #[default]
    Default,
    Lg,
}

/// `min-h-*` in rems (`17.5` is 17.5 × 0.25rem).
fn min_height(size: TextareaSize) -> f32 {
    match size {
        TextareaSize::Sm => 4.125,
        TextareaSize::Default => 4.375,
        TextareaSize::Lg => 4.625,
    }
}

fn padding(size: TextareaSize) -> (f32, f32) {
    match size {
        // px calc(spacing(2.5)-1), py calc(spacing(1)-1)
        TextareaSize::Sm => (9., 3.),
        // px calc(spacing(3)-1), py calc(spacing(1.5)-1)
        TextareaSize::Default => (11., 5.),
        // px same as default, py calc(spacing(2)-1)
        TextareaSize::Lg => (11., 7.),
    }
}

pub fn textarea_frame(size: TextareaSize, cx: &App) -> Div {
    let theme = cx.app_theme();
    let (px_pad, py_pad) = padding(size);
    div()
        .relative()
        .flex()
        .w_full()
        .min_w_0()
        .min_h(rems(min_height(size)))
        .max_h(rems(16.))
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.input)
        .bg(if theme.is_dark() {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .text_color(theme.foreground)
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .shadow(tokens::shadow_xs(0.05))
        .px(px(px_pad))
        .py(px(py_pad))
}
