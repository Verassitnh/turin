//! The "Run action on default branch?" confirmation of GitActionsControl.tsx:
//! Abort, continue on the ref, or check out a feature branch and continue.

use gpui_kit::AppContext as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    Context, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div, px,
};

use super::GitActionsControl;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_SM;
use crate::ui::{Button, ButtonSize, ButtonVariant};

pub struct DefaultBranchForm {
    control: WeakEntity<GitActionsControl>,
    description: String,
    continue_label: String,
}

impl Render for DefaultBranchForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let abort = self.control.clone();
        let proceed = self.control.clone();
        let feature = self.control.clone();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(520.))
            .child(
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child(self.description.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div().mr_auto().child(
                            Button::new("default-branch-abort")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .label("Abort")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    abort
                                        .update(cx, |control, cx| {
                                            control.pending_default = None;
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        ),
                    )
                    .child(
                        Button::new("default-branch-continue")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::SmMultiline)
                            .label(self.continue_label.clone())
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                proceed
                                    .update(cx, |control, cx| {
                                        control.continue_pending_default_branch_action(false, window, cx)
                                    })
                                    .ok();
                            }),
                    )
                    .child(
                        Button::new("default-branch-feature")
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::SmMultiline)
                            .label("Check out feature branch & continue")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                feature
                                    .update(cx, |control, cx| {
                                        control.continue_pending_default_branch_action(true, window, cx)
                                    })
                                    .ok();
                            }),
                    ),
            )
    }
}

/// Opens the confirmation for the control's pending action.
pub fn open(control: WeakEntity<GitActionsControl>, window: &mut Window, cx: &mut gpui_kit::App) {
    let copy = control
        .upgrade()
        .and_then(|control| {
            let status = control.read(cx).status_for(cx).status;
            control.read(cx).pending_default_copy(status.as_ref())
        });
    let (title, description, continue_label) = match copy {
        Some(copy) => (copy.title, copy.description, copy.continue_label),
        None => (
            "Run action on default branch?".to_owned(),
            String::new(),
            "Continue".to_owned(),
        ),
    };
    let form = cx.new(|_| DefaultBranchForm {
        control,
        description,
        continue_label,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title(title.clone()).child(form.clone())
    });
}
