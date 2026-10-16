//! Port of upstream/apps/web/src/components/settings/ChatGptConnectionButton.tsx
//! Port of upstream/apps/web/src/components/settings/ChatGptUsageButton.tsx
//! Port of upstream/apps/web/src/components/settings/ChatGptAccountPicker.tsx
//! Port of upstream/apps/web/src/components/ui/radio-group.tsx (`Radio`)

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    rems,
};
use turin_shared::usage_limits::CHATGPT_USAGE_URL;

use super::codex_setup_logic::{
    CHATGPT_CHANGE_ACCOUNT_METHOD_ID, account_picker_selection, resolve_picked_method,
};
use super::dialog::{Dialog, dialog_footer, dialog_frame, dialog_header};
use super::view::SettingsView;
use crate::assets::PROVIDER_OPENAI;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_SM};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

/// `Radio`: `size-4` circle, `border-input bg-background shadow-xs/5`; checked it
/// fills `bg-primary` with a `size-1.5` `primary-foreground` dot.
pub(super) fn radio(selected: bool, theme: &AppTheme) -> AnyElement {
    let dark = theme.is_dark();
    div()
        .relative()
        .flex()
        .size_4()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if selected { theme.primary } else { theme.input })
        .bg(if selected {
            theme.primary
        } else if dark {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .when(!selected, |this| this.shadow(tokens::shadow_xs(0.05)))
        .when(selected, |this| {
            this.child(
                div()
                    .size(rems(0.375))
                    .rounded_full()
                    .bg(theme.primary_foreground),
            )
        })
        .into_any_element()
}

impl SettingsView {
    /// `ChatGptConnectionButton`: the OpenAI mark (`size-4 shrink-0`) then the label
    /// (default "Continue with ChatGPT").
    pub(super) fn chat_gpt_connection_button(
        &self,
        id: &str,
        size: ButtonSize,
        variant: ButtonVariant,
        label: Option<&str>,
        disabled: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        Button::new(SharedString::from(id.to_owned()))
            .variant(variant)
            .size(size)
            .disabled(disabled)
            .child(Icon::path(PROVIDER_OPENAI).size(rems(1.)))
            .child(label.unwrap_or("Continue with ChatGPT").to_owned())
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .into_any_element()
    }

    /// `ChatGptUsageButton`: ghost-muted `sm`, "Manage usage" then an external-link mark.
    pub(super) fn chat_gpt_usage_button(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        Button::new(SharedString::from(id.to_owned()))
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::Sm)
            .child("Manage usage")
            .child(Icon::new(IconName::ExternalLink).size(rems(0.875)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.store
                    .read(cx)
                    .source()
                    .open_external_url(CHATGPT_USAGE_URL, cx);
            }))
            .into_any_element()
    }

    /// Opens `ChatGptAccountPicker` for `instance_id`.
    pub(super) fn open_chat_gpt_account_picker(
        &mut self,
        instance_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.dialog = Some(Dialog::ChatGptAccountPicker {
            instance_id: instance_id.to_owned(),
            selection: None,
        });
        cx.notify();
    }

    /// `ChatGptAccountPicker`.
    pub(super) fn render_chat_gpt_account_picker(
        &mut self,
        instance_id: String,
        selection: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let methods = vm
            .provider_auth
            .get(&instance_id)
            .and_then(|auth| auth.methods.clone())
            .unwrap_or_default();
        let selected = account_picker_selection(&methods, selection.as_deref());

        let option = |id: String, label: String, name: &str| -> AnyElement {
            let checked = id == selected;
            let pick_instance = instance_id.clone();
            let pick_id = id.clone();
            div()
                .id(SharedString::from(format!("chatgpt-account-{name}-{id}")))
                .flex()
                .items_center()
                .gap_3()
                .cursor(CursorStyle::PointingHand)
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(theme.border)
                .px_3()
                .py_3()
                .text_size(TEXT_SM.size)
                .child(radio(checked, &theme))
                .child(div().min_w_0().font_weight(FontWeight::MEDIUM).child(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dialog = Some(Dialog::ChatGptAccountPicker {
                        instance_id: pick_instance.clone(),
                        selection: Some(pick_id.clone()),
                    });
                    cx.notify();
                }))
                .into_any_element()
        };

        let mut group = div().flex().flex_col().gap_3();
        for method in methods
            .iter()
            .filter(|method| method.id.as_str().starts_with("chatgpt-profile:"))
        {
            group = group.child(option(
                method.id.as_str().to_owned(),
                method.name.as_str().to_owned(),
                "profile",
            ));
        }
        group = group.child(option(
            CHATGPT_CHANGE_ACCOUNT_METHOD_ID.to_owned(),
            "Use a different account".to_owned(),
            "other",
        ));

        let dismiss = self.cb(cx, |this, _, cx| {
            this.dialog = None;
            cx.notify();
        });
        let confirm_instance = instance_id.clone();
        let confirm_methods = methods.clone();
        let confirm_selected = selected.clone();
        dialog_frame(
            &theme,
            rems(32.),
            vec![
                dialog_header(
                    "Reconnect ChatGPT",
                    Some("On OpenAI, sign in with the account you choose here.".into()),
                    &theme,
                ),
                div().px_6().pb_6().child(group).into_any_element(),
                dialog_footer(
                    &theme,
                    vec![self.chat_gpt_connection_button(
                        "chatgpt-picker-connect",
                        ButtonSize::Default,
                        ButtonVariant::Default,
                        None,
                        false,
                        cx,
                        move |this, _, cx| {
                            this.dialog = None;
                            let method = resolve_picked_method(&confirm_methods, &confirm_selected);
                            this.codex_setup_for_instance(&confirm_instance, &method, cx);
                        },
                    )],
                ),
            ],
            dismiss,
        )
    }
}
