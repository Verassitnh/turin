//! Port of upstream/apps/web/src/components/ConfirmDialogHost.tsx.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{Context, IntoElement, ParentElement as _, Render, Styled as _, div, rems};

use crate::shell::confirm_dialog::{
    ConfirmDialogState, read_confirm_dialog_state, register_confirm_dialog_host,
};
use crate::theme::ActiveAppTheme;

pub struct ConfirmDialogHost {
    _guard: crate::shell::confirm_dialog::ConfirmDialogHostGuard,
}

impl ConfirmDialogHost {
    pub fn new(_: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let _ = cx;
        Self {
            _guard: register_confirm_dialog_host(),
        }
    }
}

impl Render for ConfirmDialogHost {
    fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme();
        match read_confirm_dialog_state() {
            ConfirmDialogState::Idle => div(),
            ConfirmDialogState::Confirming { message, .. }
            | ConfirmDialogState::Closing { message, .. } => div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_kit::hsla(0., 0., 0., 0.45))
                .child(
                    div()
                        .max_w(rems(24.))
                        .p(rems(1.5))
                        .rounded_lg()
                        .bg(theme.card)
                        .border_1()
                        .border_color(theme.border)
                        .text_size(rems(1.))
                        .child(message),
                ),
        }
    }
}
