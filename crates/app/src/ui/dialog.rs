//! Port of upstream/apps/web/src/components/ui/dialog.tsx and dialog-styles.ts (default variant).
//!
//! Base UI's portal, viewport and popup become one overlay element: a full-window layer with the
//! `dialog-backdrop` fill (background at 60%, 64% in dark), the popup centred in it (`max-w-lg` by
//! default, `rounded-2xl border`, `dialog-glass` surface) and the pieces upstream composes it from
//! ([`dialog_header`], [`dialog_title`], [`dialog_description`], [`dialog_panel`],
//! [`dialog_footer`]). Not ported: enter/exit transitions, nested-dialog scaling, the mobile
//! sheet, backdrop blur and glass blur (the popup is a high-opacity fill instead), and the
//! `media` variant.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, ClickEvent, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement, Pixels, SharedString, Styled, Window, div, px, relative, rems,
};

use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{self, RADIUS_2XL, TEXT_SM, TEXT_XL};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName};

/// `dialog-backdrop`: `color-mix(in srgb, var(--background) 60%, transparent)`, 64% in dark.
pub fn dialog_backdrop_color(theme: &AppTheme) -> Hsla {
    with_alpha(theme.background, if theme.is_dark() { 0.64 } else { 0.6 }).into()
}

/// `dialog-glass` without the blur: the popup surface over the backdrop.
pub(crate) fn dialog_surface(theme: &AppTheme) -> Hsla {
    let glass = with_alpha(theme.background, 0.8);
    color_mix(MixSpace::Srgb, theme.background, 0.8, glass).into()
}

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// `<Dialog open><DialogPopup className="max-w-…">children</DialogPopup></Dialog>`.
pub fn dialog_overlay(
    id: impl Into<SharedString>,
    max_width: Pixels,
    on_dismiss: Dismiss,
    children: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let dismiss_backdrop = on_dismiss.clone();
    let dismiss_close = on_dismiss;
    let id: SharedString = id.into();
    div()
        .id(SharedString::from(format!("{id}-backdrop")))
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .bg(dialog_backdrop_color(theme))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            dismiss_backdrop(window, cx)
        })
        .child(
            div()
                .id(id)
                .relative()
                .flex()
                .flex_col()
                .min_h_0()
                .w_full()
                .min_w_0()
                .max_w(max_width)
                .max_h(relative(1.))
                .rounded(RADIUS_2XL)
                .border_1()
                .border_color::<Hsla>(if theme.is_dark() {
                    with_alpha(tokens::tw::WHITE, 0.08).into()
                } else {
                    with_alpha(theme.foreground, 0.10).into()
                })
                .bg(dialog_surface(theme))
                .text_color(theme.popover_foreground)
                .shadow(tokens::shadow_dialog(theme.is_dark()))
                // Presses inside the popup never reach the backdrop's dismiss.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(children)
                .child(
                    div().absolute().right(rems(0.5)).top(rems(0.5)).child(
                        Button::new("dialog-close")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Icon)
                            .icon(IconName::X)
                            .on_click(move |_: &ClickEvent, window, cx| dismiss_close(window, cx)),
                    ),
                ),
        )
        .into_any_element()
}

/// `DialogHeader`: `flex flex-col gap-2 p-6` (`pb-3` when a panel follows).
pub fn dialog_header(has_panel: bool) -> Div {
    let header = div().flex().flex_col().gap_2().p(rems(1.5));
    if has_panel {
        header.pb(rems(0.75))
    } else {
        header
    }
}

/// `DialogTitle`: `font-semibold text-xl leading-none`.
pub fn dialog_title(text: impl Into<SharedString>) -> Div {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(TEXT_XL.size)
        .line_height(relative(1.))
        .child(text.into())
}

/// `DialogDescription`: `text-muted-foreground text-sm`.
pub fn dialog_description(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

/// `DialogPanel`: `space-y-4 p-6`, `pt-1` under a header.
pub fn dialog_panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .px(rems(1.5))
        .pb(rems(1.5))
        .pt(px(4.))
}

/// `DialogFooter`: `border-t bg-muted/72 px-6 py-4`, buttons right-aligned.
pub fn dialog_footer(cx: &App) -> Div {
    let theme = cx.app_theme();
    div()
        .flex()
        .justify_end()
        .gap_2()
        .px(rems(1.5))
        .py_4()
        .border_t_1()
        .border_color(theme.border)
        .bg(with_alpha(theme.muted, 0.72))
        .rounded_b(px(17.))
}
