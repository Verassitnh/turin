//! The chrome of upstream/apps/web/src/components/ui/input.tsx around gpui-component's text
//! input: `rounded-lg border border-input bg-background shadow-xs/5`, dark `bg-input/32`, height
//! 32px (`h-7.5` input in a 1px border). Focus ring (`ring-[3px] ring-ring/24`) is the input's own.
//!
//! Port of upstream/apps/web/src/components/ui/input.tsx (chrome around the text field).

use gpui_kit::{App, Div, Styled, div, px};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG};

/// The bordered frame; add the `Input::new(&state).appearance(false)` as its child.
pub fn text_input_frame(cx: &App) -> Div {
    let theme = cx.app_theme();
    div()
        .flex()
        .w_full()
        .min_w_0()
        .items_center()
        .h(px(32.))
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.input)
        .bg(if theme.is_dark() {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .shadow(tokens::shadow_xs(0.05))
        .px(px(11.))
}
