//! A modal dialog frame (upstream components/ui/{dialog,alert-dialog}.tsx +
//! dialog-styles.ts): `dialog-backdrop` fill, a `dialog-glass rounded-2xl
//! border` popup with `p-6` header and an optional muted footer.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Rems, SharedString, Styled as _, Window, deferred, div, px, relative,
    rems,
};

use super::view::SettingsView;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_2XL, TEXT_SM};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant};

type DialogAction = Rc<dyn Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>)>;

pub struct ConfirmSpec {
    pub title: SharedString,
    pub description: SharedString,
    pub confirm_label: SharedString,
    pub destructive: bool,
    pub on_confirm: DialogAction,
}

/// The dialogs the settings pages open.
pub enum Dialog {
    Confirm(ConfirmSpec),
}

/// `DialogPopup` chrome around `children` (header / panel / footer).
pub fn dialog_frame(
    theme: &AppTheme,
    max_width: Rems,
    children: Vec<AnyElement>,
    on_dismiss: impl Fn(&mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    let dark = theme.is_dark();
    let backdrop = with_alpha(theme.background, if dark { 0.64 } else { 0.60 });
    // GPUI has no backdrop blur, so the glass fill is painted opaque.
    let popup_bg = theme.background;
    let border = if dark {
        with_alpha(tokens::tw::WHITE, 0.08)
    } else {
        with_alpha(theme.foreground, 0.10)
    };
    deferred(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .bg(backdrop)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| on_dismiss(window, cx))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(max_width)
                    .max_h(relative(0.92))
                    .min_h_0()
                    .rounded(RADIUS_2XL)
                    .border_1()
                    .border_color(border)
                    .bg(popup_bg)
                    .shadow(tokens::shadow_dialog(dark))
                    .text_color(theme.popover_foreground)
                    // Clicks inside the popup must not reach the dismiss layer.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(children),
            ),
    )
    .with_priority(10)
    .into_any_element()
}

pub fn dialog_header(title: impl Into<SharedString>, description: Option<SharedString>, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_6()
        .child(
            div()
                .text_size(rems(1.25))
                .line_height(relative(1.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .children(description.map(|description| {
            div()
                .text_size(TEXT_SM.size)
                .line_height(TEXT_SM.line_height)
                .text_color(theme.muted_foreground)
                .child(description)
        }))
        .into_any_element()
}

/// `DialogFooter variant="default"`: `border-t bg-muted/72 px-6 py-4`.
pub fn dialog_footer(theme: &AppTheme, children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .px_6()
        .py_4()
        .border_t_1()
        .border_color(theme.border)
        .bg(with_alpha(theme.muted, 0.72))
        .rounded_b(px(17.))
        .children(children)
        .into_any_element()
}

impl SettingsView {
    pub(super) fn render_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        let Some(Dialog::Confirm(spec)) = &self.dialog else {
            return None;
        };
        let on_confirm = spec.on_confirm.clone();
        let cancel = cx.listener(|this, _, _, cx| {
            this.dialog = None;
            cx.notify();
        });
        let confirm = cx.listener(move |this, _, window, cx| {
            this.dialog = None;
            on_confirm(this, window, cx);
            cx.notify();
        });
        let dismiss = self.cb(cx, |this, _, cx| {
            this.dialog = None;
            cx.notify();
        });
        Some(dialog_frame(
            &theme,
            rems(32.),
            vec![
                dialog_header(spec.title.clone(), Some(spec.description.clone()), &theme),
                dialog_footer(
                    &theme,
                    vec![
                        Button::new("dialog-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Default)
                            .label("Cancel")
                            .on_click(cancel)
                            .into_any_element(),
                        Button::new("dialog-confirm")
                            .variant(if spec.destructive {
                                ButtonVariant::Destructive
                            } else {
                                ButtonVariant::Default
                            })
                            .label(spec.confirm_label.clone())
                            .on_click(confirm)
                            .into_any_element(),
                    ],
                ),
            ],
            dismiss,
        ))
    }
}

