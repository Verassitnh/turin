//! Port of upstream/apps/web/src/components/settings/ProviderAuthenticationSection.tsx

use gpui_kit::{
    AnyElement, ClipboardItem, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};
use turin_contracts::provider_setup::{
    ProviderAuthInteraction, ProviderAuthMethod, ProviderAuthPhase,
};
use turin_provider_core::server_provider::ServerProvider;

use super::controls::redacted_sensitive_text;
use super::controls::{
    FieldSize, PopupAlign, SelectItem, SelectSize, SelectWidth, popup_list, select_trigger,
    text_field, toggle_menu, trigger_height, with_popup,
};
use super::layout::SettingsRow;
use super::provider_authentication_logic::{
    SignInButtonLabel, account_description, auth_is_discovering, auth_phase_active,
    auth_sign_in_url, needs_external_setup, provider_signed_in, resolved_interaction,
    show_callback_form, show_cancel_button, show_sign_in_button, show_sign_out_button,
    sign_in_button_label,
};
use super::view::SettingsView;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

const ENVIRONMENT_LABEL: &str = "this device";

impl SettingsView {
    pub(super) fn provider_authentication_section(
        &mut self,
        row_id: &str,
        provider: &ServerProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let instance_id = row_id.to_owned();
        let auth_state = self.vm(cx).provider_auth.get(row_id).cloned();
        let auth_ref = auth_state.as_ref();
        let signed_in = provider_signed_in(provider.auth.status, auth_ref);
        let active = auth_ref.is_some_and(|state| auth_phase_active(state.phase));
        let query_error = false;
        let is_discovering =
            auth_is_discovering(provider.driver.as_str(), signed_in, query_error, auth_ref);
        let needs_external = needs_external_setup(signed_in, active, provider, auth_ref);
        let interaction = resolved_interaction(auth_ref);
        let url = auth_sign_in_url(auth_ref, interaction);
        let description = if signed_in
            && !active
            && provider
                .auth
                .email
                .as_ref()
                .is_some_and(|email| !email.as_str().trim().is_empty())
        {
            let email = provider.auth.email.as_ref().unwrap().as_str();
            let revealed = self.panels.providers.revealed_emails.contains(row_id);
            let toggle_id = row_id.to_owned();
            let on_toggle = self.cb(cx, move |this, _, cx| {
                let revealed = &mut this.panels.providers.revealed_emails;
                if !revealed.remove(&toggle_id) {
                    revealed.insert(toggle_id.clone());
                }
                cx.notify();
            });
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .child("Signed in as ")
                .child(redacted_sensitive_text(
                    SharedString::from(format!("provider-email-{row_id}")),
                    email,
                    revealed,
                    cx,
                    on_toggle,
                ))
                .child(".")
                .into_any_element()
        } else {
            description_element(
                &account_description(
                    ENVIRONMENT_LABEL,
                    signed_in,
                    active,
                    is_discovering,
                    needs_external,
                    auth_ref,
                    interaction,
                ),
                &theme,
            )
        };

        let status_message = auth_ref
            .and_then(|state| state.message.as_deref())
            .filter(|_| auth_ref.is_some_and(|s| s.phase == ProviderAuthPhase::Failed));
        let can_authenticate = provider
            .setup
            .as_ref()
            .map(|setup| setup.can_authenticate)
            .unwrap_or(true);
        let can_logout = provider.auth.can_logout.unwrap_or(can_authenticate);
        let disabled = is_discovering || !provider.enabled || !provider.installed;

        let mut controls = div().flex().min_w_0().flex_wrap().items_center().gap_1();
        if let Some(methods) = auth_ref.and_then(|s| s.methods.as_ref())
            && !active
            && methods.len() > 1
        {
            controls = controls.child(self.auth_method_select(row_id, methods, cx));
        }
        if let Some(url) = url.filter(|u| !u.is_empty()) {
            let url_owned = url.to_owned();
            let url_copy = url_owned.clone();
            controls = controls
                .child(
                    Button::new(SharedString::from(format!(
                        "provider-open-browser-{row_id}"
                    )))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Open browser")
                    .disabled(disabled || active && auth_ref.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.store
                            .read(cx)
                            .source()
                            .open_external_url(&url_owned, cx);
                    })),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "provider-copy-auth-url-{row_id}"
                    )))
                    .variant(ButtonVariant::GhostMuted)
                    .size(ButtonSize::IconSm)
                    .icon(IconName::Copy)
                    .icon_size(rems(0.875))
                    .tooltip(Tooltip::new("Copy sign-in link"))
                    .disabled(disabled)
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(url_copy.clone()));
                    }),
                );
        }
        if show_cancel_button(active, auth_ref.and_then(|s| s.flow_id.as_ref()).is_some()) {
            let id = instance_id.clone();
            let flow = auth_ref
                .and_then(|s| s.flow_id.clone())
                .map(|f| f.as_str().to_owned());
            controls = controls.child(
                Button::new(SharedString::from(format!("provider-auth-cancel-{row_id}")))
                    .variant(ButtonVariant::GhostMuted)
                    .size(ButtonSize::Sm)
                    .label("Cancel")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(flow_id) = &flow {
                            this.store
                                .read(cx)
                                .source()
                                .cancel_provider_auth(&id, flow_id, cx);
                        }
                    })),
            );
        }
        if show_sign_in_button(active, needs_external, can_authenticate) {
            let label = match sign_in_button_label(signed_in, auth_ref) {
                Some(SignInButtonLabel::ChangeAccount) => "Change account",
                Some(SignInButtonLabel::RetrySignIn) => "Retry sign-in",
                Some(SignInButtonLabel::SignIn) | None => "Sign in",
            };
            let id = instance_id.clone();
            controls = controls.child(
                Button::new(SharedString::from(format!("provider-signin-{row_id}")))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label(label)
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.store.read(cx).source().start_provider_auth(&id, cx);
                    })),
            );
        } else if needs_external
            && let Some(docs) = provider
                .setup
                .as_ref()
                .and_then(|s| s.documentation_url.as_ref())
        {
            let docs = docs.as_str().to_string();
            controls = controls.child(
                Button::new(SharedString::from(format!("provider-auth-docs-{row_id}")))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Open docs")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.store.read(cx).source().open_external_url(&docs, cx);
                    })),
            );
        }
        if show_sign_out_button(active, signed_in, can_logout) {
            let logout_id = instance_id.clone();
            let name = provider
                .display_name
                .as_ref()
                .map(|d| d.as_str())
                .unwrap_or(provider.driver.as_str())
                .to_owned();
            controls = controls.child(
                Button::new(SharedString::from(format!("provider-signout-{row_id}")))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .label("Sign out")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let description = format!(
                            "Sign out of {name} on {ENVIRONMENT_LABEL}? Thread history is kept."
                        );
                        this.dialog =
                            Some(super::dialog::Dialog::Confirm(super::dialog::ConfirmSpec {
                                title: "Sign out?".into(),
                                description: description.into(),
                                confirm_label: "Sign out".into(),
                                destructive: true,
                                on_confirm: std::rc::Rc::new({
                                    let logout_id = logout_id.clone();
                                    move |this, _, cx| {
                                        this.store
                                            .read(cx)
                                            .source()
                                            .logout_provider(&logout_id, cx);
                                    }
                                }),
                            }));
                        cx.notify();
                    })),
            );
        }

        let mut row = SettingsRow::new("Account")
            .description(description)
            .control(controls);
        if let Some(message) = status_message {
            row = row.status(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.destructive)
                    .child(message.to_owned()),
            );
        }
        let mut extras: Vec<AnyElement> = Vec::new();
        if let Some(ProviderAuthInteraction::DeviceCode { user_code, .. }) = interaction {
            extras.push(
                div()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(format!("Enter code {user_code} in your browser."))
                    .into_any_element(),
            );
        }
        if let Some(ProviderAuthInteraction::Terminal { output, .. }) = interaction {
            extras.push(
                div()
                    .py_2()
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .bg(with_alpha(theme.muted, 0.35))
                    .p_2()
                    .font_family(theme.fonts.mono.clone())
                    .text_size(TEXT_XS.size)
                    .text_color(with_alpha(theme.foreground, 0.9))
                    .max_h(rems(24.))
                    .id(SharedString::from(format!(
                        "provider-auth-terminal-{row_id}"
                    )))
                    .overflow_y_scroll()
                    .child(output.clone())
                    .into_any_element(),
            );
        }
        if let Some(ProviderAuthInteraction::Credentials { fields, .. }) = interaction {
            for field in fields {
                let field_name = field.name.as_str().to_owned();
                let input_id = format!("provider-auth-field-{row_id}-{}", field.name);
                let draft = self
                    .panels
                    .providers
                    .auth_field_draft
                    .get(row_id)
                    .and_then(|map| map.get(&field_name))
                    .cloned()
                    .unwrap_or_default();
                let row_id_owned = row_id.to_owned();
                let input = if field.secret {
                    self.secret_commit_input(
                        &input_id,
                        &draft,
                        "",
                        false,
                        window,
                        cx,
                        move |this, text, _, cx| {
                            this.panels
                                .providers
                                .auth_field_draft
                                .entry(row_id_owned.clone())
                                .or_default()
                                .insert(field_name.clone(), text);
                            cx.notify();
                        },
                    )
                } else {
                    self.input(
                        &input_id,
                        &draft,
                        "",
                        window,
                        cx,
                        move |this, text, _, cx| {
                            this.panels
                                .providers
                                .auth_field_draft
                                .entry(row_id_owned.clone())
                                .or_default()
                                .insert(field_name.clone(), text);
                            cx.notify();
                        },
                    )
                };
                extras.push(
                    SettingsRow::new(field.label.as_str().to_owned())
                        .control(text_field(&input, None, FieldSize::Sm, false, cx))
                        .into_any_element(),
                );
            }
            let connect_id = instance_id.clone();
            extras.push(
                Button::new(SharedString::from(format!(
                    "provider-auth-connect-{row_id}"
                )))
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .label("Connect")
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.store
                        .read(cx)
                        .source()
                        .start_provider_auth(&connect_id, cx);
                }))
                .into_any_element(),
            );
        }
        if show_callback_form(url, interaction) {
            let draft = self
                .panels
                .providers
                .auth_callback_draft
                .get(row_id)
                .cloned()
                .unwrap_or_default();
            let row_id_cb = row_id.to_owned();
            let input = self.input(
                &format!("provider-auth-callback-{row_id}"),
                &draft,
                "",
                window,
                cx,
                move |this, text, _, cx| {
                    this.panels
                        .providers
                        .auth_callback_draft
                        .insert(row_id_cb.clone(), text);
                    cx.notify();
                },
            );
            extras.push(
                SettingsRow::new("Callback URL")
                    .description(
                        "If the final localhost page does not load, paste its full URL here.",
                    )
                    .control(text_field(&input, None, FieldSize::Sm, false, cx))
                    .into_any_element(),
            );
            let id = instance_id.clone();
            let flow = auth_ref.and_then(|s| s.flow_id.clone());
            let callback = self
                .panels
                .providers
                .auth_callback_draft
                .get(row_id)
                .cloned()
                .unwrap_or_default();
            extras.push(
                Button::new(SharedString::from(format!(
                    "provider-auth-continue-{row_id}"
                )))
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .label("Continue")
                .disabled(disabled || callback.trim().is_empty())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(flow_id) = flow.as_ref().map(|f| f.as_str()) {
                        this.store
                            .read(cx)
                            .source()
                            .complete_provider_auth(&id, flow_id, &callback, cx);
                    }
                }))
                .into_any_element(),
            );
        }
        for extra in extras {
            row = row.child(extra);
        }
        row.into_any_element()
    }

    fn auth_method_select(
        &mut self,
        row_id: &str,
        methods: &[ProviderAuthMethod],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu_id = SharedString::from(format!("provider-auth-method-{row_id}"));
        let open = self.menus.is_open(&menu_id);
        let selected = self
            .panels
            .providers
            .auth_method_id
            .get(row_id)
            .cloned()
            .unwrap_or_default();
        let label = methods
            .iter()
            .find(|method| method.id.as_str() == selected)
            .map(|method| SharedString::from(method.name.as_str()))
            .unwrap_or_else(|| SharedString::from("Provider default"));
        let trigger = select_trigger(
            SharedString::from(format!("provider-auth-method-trigger-{row_id}")),
            label,
            SelectSize::Sm,
            SelectWidth::Fixed(11.),
            open,
            false,
            cx,
        )
        .on_click(toggle_menu(cx, |this| &mut this.menus, menu_id.clone()));
        let mut popup_items =
            vec![SelectItem::new("", "Provider default").selected(selected.is_empty())];
        popup_items.extend(methods.iter().map(|method| {
            SelectItem::new(
                SharedString::from(method.id.as_str()),
                SharedString::from(method.name.as_str()),
            )
            .selected(method.id.as_str() == selected)
        }));
        let popup = open.then(move || {
            let items = popup_items;
            let row_id_pick = row_id.to_owned();
            let row_id_popup = row_id.to_owned();
            let entity = cx.entity().downgrade();
            let on_pick = std::rc::Rc::new(
                move |value: SharedString, _: &mut Window, cx: &mut gpui_kit::App| {
                    entity
                        .update(cx, |this, cx| {
                            this.panels
                                .providers
                                .auth_method_id
                                .insert(row_id_pick.clone(), value.into());
                            this.menus.close();
                            cx.notify();
                        })
                        .ok();
                },
            );
            popup_list(
                SharedString::from(format!("provider-auth-method-popup-{row_id_popup}")),
                items,
                Some(rems(11.).to_pixels(gpui_kit::px(16.))),
                cx,
                on_pick,
            )
            .into_any_element()
        });
        with_popup(
            trigger,
            popup,
            PopupAlign::End,
            trigger_height(SelectSize::Sm),
        )
        .into_any_element()
    }
}

fn description_element(text: &str, theme: &crate::theme::AppTheme) -> AnyElement {
    div()
        .text_size(TEXT_XS.size)
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
        .into_any_element()
}
