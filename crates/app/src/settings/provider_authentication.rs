//! Port of upstream/apps/web/src/components/settings/ProviderAuthenticationSection.tsx

use std::collections::HashMap;

use gpui_kit::{
    AnyElement, ClipboardItem, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};
use indexmap::IndexMap;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_setup::{
    ProviderAuthBrowserAction, ProviderAuthInteraction, ProviderAuthMethod, ProviderAuthPhase,
    ProviderAuthRespondInput, ProviderAuthResponse, SetupOperationId,
};
use turin_provider_core::server_provider::ServerProvider;

use super::controls::redacted_sensitive_text;
use super::controls::{
    FieldSize, PopupAlign, SelectItem, SelectSize, SelectWidth, password_field, popup_list,
    select_trigger, text_field, toggle_menu, trigger_height, with_popup,
};
use super::layout::SettingsRow;
use super::provider_authentication_logic::{
    PrimaryAuthAction, SignInButtonLabel, account_description, auth_is_discovering,
    auth_phase_active, auth_sign_in_url, needs_external_setup, primary_auth_action,
    provider_signed_in, resolved_interaction, show_callback_form, show_sign_out_button,
    sign_in_button_label,
};
use super::view::SettingsView;
use super::view_model::SetupResult;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

/// Per-instance state of `ProviderAuthenticationSection` (`error`, `pending`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderAuthUiState {
    pub error: Option<String>,
    /// `methodId`: the chosen sign-in method ("" is the provider default).
    pub method_id: String,
    /// `draft.id` (`flowId:interactionId`) and `draft.values`.
    pub draft_id: String,
    pub values: HashMap<String, String>,
}

fn interaction_id(interaction: &ProviderAuthInteraction) -> String {
    match interaction {
        ProviderAuthInteraction::Browser { id, .. }
        | ProviderAuthInteraction::DeviceCode { id, .. }
        | ProviderAuthInteraction::Terminal { id, .. }
        | ProviderAuthInteraction::Credentials { id, .. } => id.as_str().to_owned(),
    }
}

impl SettingsView {
    pub(super) fn auth_ui(&mut self, row_id: &str) -> &mut ProviderAuthUiState {
        self.panels
            .providers
            .auth_ui
            .entry(row_id.to_owned())
            .or_default()
    }

    /// `run()`: sends one request and keeps its failure for the row.
    fn auth_run(
        &mut self,
        row_id: &str,
        request: impl FnOnce(&mut gpui_kit::App) -> SetupResult,
        cx: &mut Context<Self>,
    ) -> bool {
        self.auth_ui(row_id).error = None;
        let result = request(cx);
        cx.notify();
        match result {
            Ok(()) => true,
            Err(message) => {
                self.auth_ui(row_id).error = Some(if message.is_empty() {
                    "Provider sign-in failed. Try again.".to_owned()
                } else {
                    message
                });
                false
            }
        }
    }

    /// `send(response)`: answers the live interaction.
    fn auth_send(
        &mut self,
        row_id: &str,
        flow_id: &str,
        interaction_id: &str,
        response: ProviderAuthResponse,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Ok(instance_id), Ok(flow_id), Ok(interaction_id)) = (
            ProviderInstanceId::new(row_id),
            SetupOperationId::new(flow_id),
            SetupOperationId::new(interaction_id),
        ) else {
            return false;
        };
        let source = self.store.read(cx).source();
        let input = ProviderAuthRespondInput {
            instance_id,
            flow_id,
            interaction_id,
            response,
        };
        self.auth_run(
            row_id,
            move |cx| source.respond_provider_auth(input, cx),
            cx,
        )
    }

    /// `openBrowser()`: records consent on the environment, then opens the page.
    fn auth_open_browser(
        &mut self,
        row_id: &str,
        url: &str,
        read_only: bool,
        cx: &mut Context<Self>,
    ) {
        if url.is_empty() || read_only {
            return;
        }
        let vm = self.vm(cx);
        let auth = vm.provider_auth.get(row_id);
        let requires_consent = matches!(
            resolved_interaction(auth),
            Some(ProviderAuthInteraction::Browser {
                requires_consent: true,
                ..
            })
        );
        if requires_consent
            && let (Some(flow), Some(interaction)) = (
                auth.and_then(|state| state.flow_id.as_ref()),
                resolved_interaction(auth).map(interaction_id),
            )
            && !self.auth_send(
                row_id,
                flow.as_str(),
                &interaction,
                ProviderAuthResponse::Browser {
                    action: ProviderAuthBrowserAction::Accept,
                },
                cx,
            )
        {
            return;
        }
        self.store.read(cx).source().open_external_url(url, cx);
        self.auth_ui(row_id).error = None;
        cx.notify();
    }

    /// The current draft values; they reset when the flow or interaction changes
    /// (`draftId`).
    fn auth_draft_values(
        &mut self,
        row_id: &str,
        auth: Option<&turin_contracts::provider_setup::ProviderAuthState>,
        interaction: Option<&ProviderAuthInteraction>,
    ) -> HashMap<String, String> {
        let draft_id = format!(
            "{}:{}",
            auth.and_then(|state| state.flow_id.as_ref())
                .map(|flow| flow.as_str())
                .unwrap_or(""),
            interaction.map(interaction_id).unwrap_or_default()
        );
        let state = self.auth_ui(row_id);
        if state.draft_id == draft_id {
            state.values.clone()
        } else {
            HashMap::new()
        }
    }

    fn auth_update_draft(
        &mut self,
        row_id: &str,
        draft_id: String,
        name: &str,
        value: String,
        cx: &mut Context<Self>,
    ) {
        let state = self.auth_ui(row_id);
        if state.draft_id != draft_id {
            state.draft_id = draft_id;
            state.values.clear();
        }
        state.values.insert(name.to_owned(), value);
        cx.notify();
    }

    /// `ProviderAuthenticationSection`.
    pub(super) fn provider_authentication_section(
        &mut self,
        row_id: &str,
        provider: &ServerProvider,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let environment_label = vm.environment.label.clone();
        let instance_id = row_id.to_owned();
        let section_error = self.auth_ui(row_id).error.clone();
        let auth_state = vm.provider_auth.get(row_id).cloned();
        let auth_ref = auth_state.as_ref();
        let signed_in = provider_signed_in(provider.auth.status, auth_ref);
        let active = auth_ref.is_some_and(|state| auth_phase_active(state.phase));
        let query_error = false;
        let is_discovering = auth_is_discovering(
            provider.driver.as_str(),
            active,
            signed_in,
            query_error,
            auth_ref,
        );
        let needs_external = needs_external_setup(signed_in, active, provider, auth_ref);
        let interaction = resolved_interaction(auth_ref);
        let url = auth_sign_in_url(auth_ref, interaction).map(str::to_owned);
        let flow_id = auth_ref
            .and_then(|state| state.flow_id.as_ref())
            .map(|flow| flow.as_str().to_owned());
        let disabled = read_only || query_error || is_discovering;
        let methods: Vec<ProviderAuthMethod> = auth_ref
            .and_then(|state| state.methods.clone())
            .unwrap_or_default();
        let selected_method = {
            let method_id = self.auth_ui(row_id).method_id.clone();
            methods
                .iter()
                .any(|method| method.id.as_str() == method_id)
                .then_some(method_id)
        };

        let signed_in_email = provider
            .auth
            .email
            .as_ref()
            .map(|email| email.as_str().trim().to_owned())
            .filter(|email| !email.is_empty());
        let description: AnyElement = match signed_in_email {
            Some(email) if signed_in && !active => {
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
                    .child("Signed in as")
                    .child(redacted_sensitive_text(
                        SharedString::from(format!("provider-email-{row_id}")),
                        &email,
                        revealed,
                        cx,
                        on_toggle,
                    ))
                    .into_any_element()
            }
            _ => description_element(
                &account_description(
                    &environment_label,
                    signed_in,
                    active,
                    is_discovering,
                    needs_external,
                    auth_ref,
                    interaction,
                ),
                &theme,
            ),
        };

        let status_message = auth_ref
            .filter(|state| state.phase == ProviderAuthPhase::Failed)
            .and_then(|state| state.message.clone());
        let can_authenticate_flag = provider.setup.as_ref().map(|setup| setup.can_authenticate);

        let mut controls = div()
            .flex()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .gap(rems(0.375));
        if !active && methods.len() > 1 {
            controls = controls.child(self.auth_method_select(row_id, &methods, disabled, cx));
        }
        if let Some(url) = url.clone().filter(|u| !u.is_empty()) {
            let open_row = instance_id.clone();
            let open_url = url.clone();
            let copy_row = instance_id.clone();
            let copy_url = url.clone();
            let consent = match interaction {
                Some(ProviderAuthInteraction::Browser {
                    requires_consent: true,
                    id,
                    ..
                }) => Some(id.as_str().to_owned()),
                _ => None,
            };
            let copy_consent = consent.clone();
            let copy_flow = flow_id.clone();
            controls = controls
                .child(
                    Button::new(SharedString::from(format!(
                        "provider-open-browser-{row_id}"
                    )))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Open browser")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.auth_open_browser(&open_row, &open_url, read_only, cx);
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Copy first; the link only works once consent is recorded.
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                        if let (Some(interaction), Some(flow)) = (&copy_consent, &copy_flow) {
                            this.auth_send(
                                &copy_row,
                                flow,
                                interaction,
                                ProviderAuthResponse::Browser {
                                    action: ProviderAuthBrowserAction::Accept,
                                },
                                cx,
                            );
                        }
                    })),
                );
        }
        let docs_url = provider
            .setup
            .as_ref()
            .and_then(|setup| setup.documentation_url.as_ref())
            .map(|docs| docs.as_str().to_string());
        match primary_auth_action(
            needs_external,
            docs_url.is_some(),
            active,
            flow_id.is_some(),
            can_authenticate_flag,
        ) {
            PrimaryAuthAction::OpenDocs => {
                let docs = docs_url.unwrap_or_default();
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
            PrimaryAuthAction::Cancel => {
                let id = instance_id.clone();
                let flow = flow_id.clone().unwrap_or_default();
                controls = controls.child(
                    Button::new(SharedString::from(format!("provider-auth-cancel-{row_id}")))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::Sm)
                        .label("Cancel")
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let source = this.store.read(cx).source();
                            let (id, flow) = (id.clone(), flow.clone());
                            this.auth_run(
                                &id.clone(),
                                move |cx| source.cancel_provider_auth(&id, &flow, cx),
                                cx,
                            );
                        })),
                );
            }
            PrimaryAuthAction::SignIn => {
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
                        .disabled(
                            disabled
                                || !provider.enabled
                                || !provider.installed
                                || auth_ref.is_none(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let source = this.store.read(cx).source();
                            let id = id.clone();
                            let method = selected_method.clone();
                            this.auth_run(
                                &id.clone(),
                                move |cx| source.start_provider_auth(&id, method.as_deref(), cx),
                                cx,
                            );
                        })),
                );
            }
            PrimaryAuthAction::None => {}
        }
        if show_sign_out_button(
            active,
            signed_in,
            provider.auth.can_logout,
            can_authenticate_flag,
        ) {
            let logout_id = instance_id.clone();
            let name = provider
                .display_name
                .as_ref()
                .map(|d| d.as_str())
                .unwrap_or(provider.driver.as_str())
                .to_owned();
            let label = environment_label.clone();
            controls = controls.child(
                Button::new(SharedString::from(format!("provider-signout-{row_id}")))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .label("Sign out")
                    .disabled(disabled || auth_ref.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let description = format!(
                            "Sign out of {name} on {label}? This stops running threads that share this sign-in. Thread history is kept."
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
                                        let source = this.store.read(cx).source();
                                        let id = logout_id.clone();
                                        this.auth_run(
                                            &logout_id,
                                            move |cx| source.logout_provider(&id, cx),
                                            cx,
                                        );
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
            row = row.status(div().child(message));
        }

        // Interaction panels under the row.
        let show_callback = show_callback_form(url.as_deref(), interaction);
        let mut extras: Vec<AnyElement> = Vec::new();
        if let Some(ProviderAuthInteraction::DeviceCode { user_code, .. }) = interaction {
            extras.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap_1()
                    .py_2()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("Enter code")
                    .child(
                        div()
                            .font_family(theme.fonts.mono.clone())
                            .text_color(theme.foreground)
                            .child(user_code.as_str().to_owned()),
                    )
                    .child("in your browser.")
                    .into_any_element(),
            );
        }
        if let Some(ProviderAuthInteraction::Terminal { output, .. }) = interaction {
            extras.push(
                div()
                    .py_2()
                    .child(
                        div()
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
                            .child(output.clone()),
                    )
                    .into_any_element(),
            );
        }
        if let Some(ProviderAuthInteraction::Credentials { fields, id, .. }) = interaction {
            let values = self.auth_draft_values(row_id, auth_ref, interaction);
            let draft_id = format!("{}:{}", flow_id.clone().unwrap_or_default(), id.as_str());
            let mut form = div()
                .flex()
                .flex_col()
                .gap_2()
                .py_2()
                .text_size(TEXT_SM.size)
                .line_height(gpui_kit::relative(1.5));
            for field in fields {
                let field_name = field.name.as_str().to_owned();
                let input_id = format!("provider-auth-field-{row_id}-{}", field.name.as_str());
                let draft = values.get(&field_name).cloned().unwrap_or_default();
                let row_id_owned = row_id.to_owned();
                let draft_id_owned = draft_id.clone();
                let name_owned = field_name.clone();
                let commit =
                    move |this: &mut Self, text: String, _: &mut Window, cx: &mut Context<Self>| {
                        this.auth_update_draft(
                            &row_id_owned,
                            draft_id_owned.clone(),
                            &name_owned,
                            text,
                            cx,
                        );
                    };
                let input = if field.secret {
                    self.password_input_live(&input_id, &draft, "", window, cx, commit)
                } else {
                    self.input_live(&input_id, &draft, "", window, cx, commit)
                };
                let control = if field.secret {
                    password_field(&input, None, FieldSize::Sm, cx)
                } else {
                    text_field(&input, None, FieldSize::Sm, false, cx)
                };
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(field.label.as_str().to_owned())
                        .child(control),
                );
            }
            let submit_row = instance_id.clone();
            let submit_flow = flow_id.clone();
            let submit_interaction = id.as_str().to_owned();
            let submit_values = values.clone();
            let submit = std::rc::Rc::new(move |this: &mut Self, cx: &mut Context<Self>| {
                let Some(flow) = &submit_flow else { return };
                let map: IndexMap<SetupOperationId, String> = submit_values
                    .iter()
                    .filter_map(|(name, value)| {
                        SetupOperationId::new(name)
                            .ok()
                            .map(|name| (name, value.clone()))
                    })
                    .collect();
                if this.auth_send(
                    &submit_row,
                    flow,
                    &submit_interaction,
                    ProviderAuthResponse::Credentials { values: map },
                    cx,
                ) {
                    let state = this.auth_ui(&submit_row);
                    state.draft_id.clear();
                    state.values.clear();
                }
            });
            let click_submit = submit.clone();
            form = form.child(
                div().child(
                    Button::new(SharedString::from(format!(
                        "provider-auth-connect-{row_id}"
                    )))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Connect")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| click_submit(this, cx))),
                ),
            );
            extras.push(form.into_any_element());
        }
        if show_callback {
            let values = self.auth_draft_values(row_id, auth_ref, interaction);
            let callback = values.get("callback").cloned().unwrap_or_default();
            let draft_id = format!(
                "{}:{}",
                flow_id.clone().unwrap_or_default(),
                interaction.map(interaction_id).unwrap_or_default()
            );
            let row_id_cb = row_id.to_owned();
            let input_id = format!("provider-auth-callback-{row_id}");
            let input = self.input_live(
                &input_id,
                &callback,
                "",
                window,
                cx,
                move |this, text, _, cx| {
                    this.auth_update_draft(&row_id_cb, draft_id.clone(), "callback", text, cx);
                },
            );
            let id = instance_id.clone();
            let flow = flow_id.clone();
            let continue_value = callback.clone();
            let submit = std::rc::Rc::new(move |this: &mut Self, cx: &mut Context<Self>| {
                let Some(flow) = &flow else { return };
                if continue_value.trim().is_empty() {
                    return;
                }
                let source = this.store.read(cx).source();
                let (request_id, request_flow, request_url) =
                    (id.clone(), flow.clone(), continue_value.clone());
                if this.auth_run(
                    &id,
                    move |cx| {
                        source.complete_provider_auth(&request_id, &request_flow, &request_url, cx)
                    },
                    cx,
                ) {
                    let state = this.auth_ui(&id);
                    state.draft_id.clear();
                    state.values.clear();
                }
            });
            let enter_submit = submit.clone();
            self.on_input_submit(&input_id, move |this, _, cx| enter_submit(this, cx));
            extras.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_2()
                    .text_size(TEXT_SM.size)
                    .line_height(gpui_kit::relative(1.5))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("If the final localhost page does not load, paste its full URL here.")
                            .child(text_field(&input, None, FieldSize::Sm, false, cx)),
                    )
                    .child(
                        div().child(
                            Button::new(SharedString::from(format!(
                                "provider-auth-continue-{row_id}"
                            )))
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Continue")
                            .disabled(disabled || callback.trim().is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| submit(this, cx))),
                        ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(error) = section_error {
            extras.push(
                div()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.destructive)
                    .child(error)
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
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu_id = SharedString::from(format!("provider-auth-method-{row_id}"));
        let open = self.menus.is_open(&menu_id);
        let selected = {
            let method_id = self.auth_ui(row_id).method_id.clone();
            if methods.iter().any(|method| method.id.as_str() == method_id) {
                method_id
            } else {
                String::new()
            }
        };
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
            disabled,
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
                            this.auth_ui(&row_id_pick).method_id = value.into();
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
