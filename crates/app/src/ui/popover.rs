//! Port of upstream/apps/web/src/components/ui/popover.tsx
//!
//! gpui-component's `Popover` handles triggering, positioning and dismissal;
//! [`popover`] dresses its popup the upstream way (`PopoverPopup`): the
//! `dropdown-glass rounded-lg` surface with the soft popover shadow, the
//! `padding` / `width` presets, the `panel` variant and `tooltipStyle`.
//! `PopoverTitle` is [`popover_title`]. Not ported: `side`/`align`/offsets
//! (gpui-component positions from the trigger anchor) and the viewport
//! transition between triggers.

use gpui_kit::component::popover::Popover;
use gpui_kit::{
    App, Div, ElementId, FontWeight, ParentElement as _, SharedString, Styled, Window, div,
    relative, rems,
};

use super::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{
    self, RADIUS_LG, RADIUS_MD, TEXT_SM, TEXT_XS, THREAD_DETAILS_PANEL_WIDTH,
};

/// The inset around the content. "compact" suits dense content (a list, a
/// code excerpt, a row of reactions); "none" is for content that draws its
/// own frame edge to edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverPadding {
    #[default]
    Default,
    Compact,
    None,
}

/// Popovers hold prose and forms, so a width is fixed rather than a minimum,
/// and every width is capped to the viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverWidth {
    #[default]
    Auto,
    /// `w-64`
    Sm,
    /// `w-80`
    Md,
    /// `w-96`
    Lg,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverVariant {
    #[default]
    Default,
    /// A docked card: no glass, border or shadow; the content draws the card.
    Panel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PopoverOptions {
    pub padding: PopoverPadding,
    pub width: PopoverWidth,
    pub variant: PopoverVariant,
    /// Tooltip look for hover-opened popovers: `rounded-md text-xs
    /// shadow-md/5 w-fit`, `py-1 px-2` with default padding.
    pub tooltip_style: bool,
}

pub fn popover(
    id: impl Into<ElementId>,
    options: PopoverOptions,
    window: &Window,
    cx: &App,
) -> Popover {
    let theme = cx.app_theme();
    let dark = theme.is_dark();
    let mut popover = Popover::new(id).text_color(theme.popover_foreground);

    if options.variant == PopoverVariant::Panel {
        // `w-[min(calc(var(--thread-details-panel-width)+--spacing(4)), …)]`,
        // transparent, unbordered, `py-2 px-2` viewport.
        return popover
            .w(rems(THREAD_DETAILS_PANEL_WIDTH.0 + 1.))
            .rounded_none()
            .border_0()
            .py_2()
            .px_2();
    }

    popover = popover
        .bg(dropdown_glass_background(theme))
        .border_1()
        .border_color(dropdown_glass_border(theme));
    popover = if options.tooltip_style {
        popover
            .rounded(RADIUS_MD)
            .text_size(TEXT_XS.size)
            .shadow(tokens::shadow_md(0.05))
    } else {
        popover
            .rounded(RADIUS_LG)
            .shadow(tokens::shadow_popover(dark))
    };

    let viewport_cap = window.viewport_size().width - rems(2.).to_pixels(window.rem_size());
    popover = match options.width {
        PopoverWidth::Auto => popover,
        PopoverWidth::Sm => popover.w(rems(16.)).max_w(viewport_cap),
        PopoverWidth::Md => popover.w(rems(20.)).max_w(viewport_cap),
        PopoverWidth::Lg => popover.w(rems(24.)).max_w(viewport_cap),
    };

    match (options.tooltip_style, options.padding) {
        (true, PopoverPadding::Default) => popover.py_1().px_2(),
        (_, PopoverPadding::Default) => popover.py_4().px_4(),
        (_, PopoverPadding::Compact) => popover.py_2().px_3(),
        (_, PopoverPadding::None) => popover.p_0(),
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
