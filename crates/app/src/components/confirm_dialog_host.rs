//! Port of upstream/apps/web/src/components/ConfirmDialogHost.tsx.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, div, rems,
};

use crate::shell::confirm_dialog::{
    ConfirmDialogState, complete_confirm_dialog_close, read_confirm_dialog_state,
    register_confirm_dialog_host, respond_to_confirm_dialog, subscribe_confirm_dialog,
    unsubscribe_confirm_dialog,
};
use crate::theme::ActiveAppTheme;

pub struct ConfirmDialogHost {
    _guard: crate::shell::confirm_dialog::ConfirmDialogHostGuard,
    subscription: u64,
    state: ConfirmDialogState,
}

impl ConfirmDialogHost {
    pub fn new(_: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let subscription = subscribe_confirm_dialog(move || {
            let _ = tx.send(());
        });
        cx.spawn(async move |this, cx| {
            while rx.recv().await.is_some() {
                this.update(cx, |this, cx| {
                    this.state = read_confirm_dialog_state();
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        Self {
            _guard: register_confirm_dialog_host(),
            subscription,
            state: read_confirm_dialog_state(),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.state = read_confirm_dialog_state();
        cx.notify();
    }

    fn choose(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        respond_to_confirm_dialog(confirmed);
        complete_confirm_dialog_close();
        self.refresh(cx);
    }
}

impl Drop for ConfirmDialogHost {
    fn drop(&mut self) {
        unsubscribe_confirm_dialog(self.subscription);
    }
}

impl Render for ConfirmDialogHost {
    fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme();
        let message = match &self.state {
            ConfirmDialogState::Idle => return div(),
            ConfirmDialogState::Confirming { message, .. }
            | ConfirmDialogState::Closing { message, .. } => message.clone(),
        };
        let confirm = cx.listener(|this, _: &gpui_kit::ClickEvent, _, cx| {
            this.choose(true, cx);
        });
        let cancel = cx.listener(|this, _: &gpui_kit::ClickEvent, _, cx| {
            this.choose(false, cx);
        });
        div()
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
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(message)
                    .child(
                        div()
                            .flex()
                            .gap(rems(0.5))
                            .child(
                                div()
                                    .id("confirm-dialog-cancel")
                                    .px(rems(0.75))
                                    .py(rems(0.4))
                                    .child("Cancel")
                                    .on_click(cancel),
                            )
                            .child(
                                div()
                                    .id("confirm-dialog-confirm")
                                    .px(rems(0.75))
                                    .py(rems(0.4))
                                    .child("Confirm")
                                    .on_click(confirm),
                            ),
                    ),
            )
    }
}
