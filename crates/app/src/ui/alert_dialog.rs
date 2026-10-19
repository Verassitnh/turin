//! Port of upstream/apps/web/src/components/ui/alert-dialog.tsx
//!
//! Desktop popup: `dialog-glass`, `rounded-2xl border`, `max-w-lg` (32rem),
//! centered in the same backdrop as [`super::dialog`]. Header is `p-6 gap-2`
//! and left-aligned. The footer is `border-t bg-muted/72 py-4` or bare
//! (`pb-6`, no rule). The description scrolls inside `max-h-[min(24rem,60vh)]`.
//! There is no corner close; callers put the dismiss control in the footer.
//!
//! Not ported: the mobile bottom sheet and the enter/exit scale.

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    px, relative, rems,
};

use super::dialog::{dialog_backdrop_color, dialog_surface};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_2XL, TEXT_SM, TEXT_XL};

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertDialogFooterVariant {
    #[default]
    Default,
    Bare,
}

pub fn alert_dialog_overlay(
    id: impl Into<SharedString>,
    on_dismiss: Dismiss,
    children: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let dismiss_backdrop = on_dismiss;
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
                .max_w(rems(32.))
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
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(children),
        )
        .into_any_element()
}

pub fn alert_dialog_header() -> Div {
    div().flex().flex_col().gap_2().p(rems(1.5)).min_h_0()
}

pub fn alert_dialog_title(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(TEXT_XL.size)
        .line_height(relative(1.))
        .child(text.into())
}

pub fn alert_dialog_description(
    text: impl Into<SharedString>,
    window: &Window,
    cx: &App,
) -> gpui_kit::Stateful<Div> {
    let cap = (window.viewport_size().height * 0.6_f32).min(rems(24.).to_pixels(window.rem_size()));
    div()
        .id("alert-dialog-description")
        .max_h(cap)
        .min_h_0()
        .overflow_y_scroll()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

pub fn alert_dialog_footer(variant: AlertDialogFooterVariant, cx: &App) -> Div {
    let theme = cx.app_theme();
    let footer = div().flex().justify_end().gap_2().px(rems(1.5));
    match variant {
        AlertDialogFooterVariant::Default => footer
            .py_4()
            .border_t_1()
            .border_color(theme.border)
            .bg(solid_over(theme.muted, 0.72, theme.background))
            .rounded_b(px(17.)),
        AlertDialogFooterVariant::Bare => footer.pb(rems(1.5)),
    }
}

#[derive(IntoElement)]
pub struct AlertDialog {
    id: SharedString,
    on_dismiss: Dismiss,
    children: Vec<AnyElement>,
}

impl AlertDialog {
    pub fn new(
        id: impl Into<SharedString>,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            on_dismiss: Rc::new(on_dismiss),
            children: Vec::new(),
        }
    }
}

impl ParentElement for AlertDialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for AlertDialog {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        alert_dialog_overlay(self.id, self.on_dismiss, div().children(self.children), cx)
    }
}
