//! Port of upstream/apps/web/src/components/ui/popover.tsx
//!
//! gpui-component's `Popover` handles triggering, positioning and dismissal;
//! [`popover`] dresses its popup the upstream way: `dropdown-glass rounded-lg`
//! with the soft popover drop shadow, and the `default` viewport padding
//! (`py-4 px-4`). `PopoverTitle` is [`popover_title`].

use gpui_kit::component::popover::Popover;
use gpui_kit::{
    App, Div, ElementId, FontWeight, ParentElement as _, SharedString, Styled, div, relative,
};

use super::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_SM};

/// Viewport inset around popover content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverPadding {
    #[default]
    Default,
    /// Dense content: a list, a code excerpt.
    Compact,
    /// Content that draws its own frame edge to edge.
    None,
}

pub fn popover(id: impl Into<ElementId>, padding: PopoverPadding, cx: &App) -> Popover {
    let theme = cx.app_theme();
    let popover = Popover::new(id)
        .rounded(RADIUS_LG)
        .bg(dropdown_glass_background(theme))
        .border_1()
        .border_color(dropdown_glass_border(theme))
        .text_color(theme.popover_foreground)
        .shadow(tokens::shadow_popover(theme.is_dark()));
    match padding {
        PopoverPadding::Default => popover.py_4().px_4(),
        PopoverPadding::Compact => popover.py_2().px_3(),
        PopoverPadding::None => popover.p_0(),
    }
}

/// `PopoverTitle`: `font-semibold text-sm leading-none`.
pub fn popover_title(text: impl Into<SharedString>) -> Div {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(TEXT_SM.size)
        .line_height(relative(1.))
        .child(text.into())
}
