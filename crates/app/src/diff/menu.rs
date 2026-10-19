//! Dropdown surface and radio items for the scope and comparison menus
//! (upstream `ui/menu.tsx` `MenuPopup` / `MenuRadioItem`).
//!
//! gpui-component's `DropdownMenu` only attaches to its own `Button`, so the
//! panel positions this surface itself under its trigger.

use crate::theme::color::solid_over;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, CursorStyle, Div, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, div, px, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_SM};
use crate::ui::tooltip::dropdown_glass_border;

/// `MenuPopup`: the `dropdown-glass` surface, `rounded-lg`, `p-1`. Opaque
/// (`--popover`): without backdrop blur the translucent glass lets the rows
/// underneath show through the text.
pub fn menu_surface(cx: &App) -> Div {
    let theme = cx.app_theme();
    div()
        .occlude()
        .flex()
        .flex_col()
        .p_1()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(dropdown_glass_border(theme))
        .bg(theme.popover)
        .shadow(tokens::shadow_popover(theme.is_dark()))
        .text_color(theme.popover_foreground)
        .text_size(TEXT_SM.size)
}

/// `MenuRadioItem`: `min-h-7 rounded-sm px-2 py-1 text-sm`, checked
/// `bg-foreground/[0.08]`, highlighted `bg-accent`.
pub fn menu_radio_item(
    id: impl Into<ElementId>,
    checked: bool,
    child: impl IntoElement,
    cx: &App,
) -> gpui_kit::Stateful<Div> {
    let theme = cx.app_theme();
    let accent = theme.accent;
    let accent_fg = theme.accent_foreground;
    div()
        .id(id)
        .flex()
        .min_h(rems(1.75))
        .items_center()
        .rounded(px(6.))
        .px_2()
        .py_1()
        .cursor(CursorStyle::PointingHand)
        .text_color(theme.foreground)
        .when(checked, |this| {
            this.bg(solid_over(theme.foreground, 0.08, theme.background))
        })
        .hover(move |style| style.bg(accent).text_color(accent_fg))
        .child(div().min_w_0().flex_1().child(child))
}

/// `DropdownMenuSubTrigger`: a row with a trailing chevron.
pub fn menu_sub_trigger(
    id: impl Into<ElementId>,
    label: &'static str,
    open: bool,
    cx: &App,
) -> gpui_kit::Stateful<Div> {
    let theme = cx.app_theme();
    let accent = theme.accent;
    let accent_fg = theme.accent_foreground;
    div()
        .id(id)
        .flex()
        .min_h(rems(1.75))
        .items_center()
        .justify_between()
        .gap_2()
        .rounded(px(6.))
        .px_2()
        .py_1()
        .cursor(CursorStyle::PointingHand)
        .text_color(theme.foreground)
        .when(open, |this| this.bg(accent).text_color(accent_fg))
        .hover(move |style| style.bg(accent).text_color(accent_fg))
        .child(label)
        .child(
            crate::ui::Icon::new(crate::ui::IconName::ChevronRight)
                .size(rems(1.))
                .color(theme.muted_foreground),
        )
}

#[allow(dead_code)]
pub fn into_any(element: impl IntoElement) -> AnyElement {
    element.into_any_element()
}
