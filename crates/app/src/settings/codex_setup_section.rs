//! Port of upstream/apps/web/src/components/settings/CodexSetupSection.tsx
//! (settings presentation) and CodexSetupSection.logic.ts (`readCodexSetupMode`,
//! in `add_provider_instance_logic.rs`).
//!
//! The managed ChatGPT sign-in: install the runtime, start the OAuth flow,
//! open the page, wait for the snapshot. The state machine lives in
//! [`super::codex_setup_logic`]; this file draws it and sends the requests.
//! See that module's header for what is not ported (primary-environment
//! handoff, browser callback bridge, the Welcome wizard's `onboarding`
//! presentation).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};
use turin_provider_core::server_provider::ServerProvider;

use super::codex_setup_logic::{
    AccountDescription, AwaitingProvider, CHATGPT_CHANGE_ACCOUNT_METHOD_ID, CHATGPT_METHOD_ID,
    CodexSetupEffect, ManagedCodexState, ManagedDerived, ManagedInputs, SetupStep, callback_url,
    connect_button_disabled, connect_button_label, derive, plan_effects, setup_step,
    waiting_control_disabled, waiting_control_label,
};
use super::controls::{FieldSize, password_field, redacted_sensitive_text, static_text_field};
use super::layout::SettingsRow;
use super::panels::providers::InstanceRow;
use super::view::SettingsView;
use super::view_model::{SettingsViewModel, SetupResult};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

/// Who is asking for the managed setup: a provider card, or the Add ChatGPT
/// account dialog (`AddManagedCodexAccountDialog`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexSetupHost {
    Card,
    /// `allowExistingCli={false}`, `autoStart`, `onSignInCancelled={onClose}`.
    AccountDialog,
}

impl SettingsView {
    /// `CodexSetupSection` (settings presentation) for a managed instance.
    pub(super) fn codex_setup_section(
        &mut self,
        row: &InstanceRow,
        live: Option<&ServerProvider>,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match live.filter(|provider| provider.setup.is_some()) {
            None => SettingsRow::new("ChatGPT account")
                .description("Preparing sign-in.")
                .into_any_element(),
            Some(_) => self.managed_codex_setup(
                &row.id,
                super::panels::providers::instance_enabled(row),
                read_only,
                CodexSetupHost::Card,
                window,
                cx,
            ),
        }
    }

    pub(super) fn codex_state(&mut self, instance_id: &str) -> &mut ManagedCodexState {
        self.panels
            .providers
            .codex
            .entry(instance_id.to_owned())
            .or_default()
    }

    /// `run()` of `ManagedCodexSetup`: one request at a time, errors land in the
    /// card.
    fn codex_run(
        &mut self,
        instance_id: &str,
        request: impl FnOnce(&mut gpui_kit::App) -> SetupResult,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.codex_state(instance_id).pending {
            return false;
        }
        {
            let state = self.codex_state(instance_id);
            state.pending = true;
            state.error = None;
        }
        let result = request(cx);
        let state = self.codex_state(instance_id);
        state.pending = false;
        cx.notify();
        match result {
            Ok(()) => true,
            Err(message) => {
                state.error = Some(if message.is_empty() {
                    "Codex setup failed. Try again.".to_owned()
                } else {
                    message
                });
                false
            }
        }
    }

    /// `signIn(methodId)`.
    pub(super) fn codex_sign_in(
        &mut self,
        instance_id: &str,
        method_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.codex_state(instance_id).pending {
            return;
        }
        {
            let state = self.codex_state(instance_id);
            state.open_requested = true;
            state.requested_method_id = method_id.to_owned();
        }
        let source = self.store.read(cx).source();
        let id = instance_id.to_owned();
        let method = method_id.to_owned();
        let started = self.codex_run(
            instance_id,
            move |cx| source.start_provider_auth(&id, Some(&method), cx),
            cx,
        );
        let state = self.codex_state(instance_id);
        if started {
            state.awaiting_provider = Some(AwaitingProvider::SignIn);
        } else {
            state.open_requested = false;
        }
    }

    /// `setup(methodId)`: install first when the runtime is missing or outdated.
    pub(super) fn codex_setup(
        &mut self,
        instance_id: &str,
        method_id: &str,
        derived: &ManagedDerived,
        cx: &mut Context<Self>,
    ) {
        match setup_step(derived) {
            SetupStep::Skip => {}
            SetupStep::SignIn => self.codex_sign_in(instance_id, method_id, cx),
            SetupStep::Install => {
                self.codex_state(instance_id).continue_with_sign_in = Some(method_id.to_owned());
                let source = self.store.read(cx).source();
                let id = instance_id.to_owned();
                if !self.codex_run(
                    instance_id,
                    move |cx| source.start_provider_install(&id, cx),
                    cx,
                ) {
                    self.codex_state(instance_id).continue_with_sign_in = None;
                }
            }
        }
    }

    /// `cancelSignIn()`.
    fn codex_cancel_sign_in(&mut self, instance_id: &str, flow_id: &str, cx: &mut Context<Self>) {
        self.codex_state(instance_id).awaiting_provider = None;
        let source = self.store.read(cx).source();
        let id = instance_id.to_owned();
        let flow = flow_id.to_owned();
        self.codex_run(
            instance_id,
            move |cx| source.cancel_provider_auth(&id, &flow, cx),
            cx,
        );
    }

    fn codex_cancel_install(
        &mut self,
        instance_id: &str,
        operation_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.codex_state(instance_id).continue_with_sign_in = None;
        let source = self.store.read(cx).source();
        let id = instance_id.to_owned();
        let operation = operation_id.to_owned();
        self.codex_run(
            instance_id,
            move |cx| source.cancel_provider_install(&id, &operation, cx),
            cx,
        );
    }

    /// The `useEffect`s of `ManagedCodexSetup`, run once per render of the
    /// section.
    fn codex_run_effects(
        &mut self,
        instance_id: &str,
        enabled: bool,
        read_only: bool,
        host: CodexSetupHost,
        cx: &mut Context<Self>,
    ) {
        let vm = self.vm(cx);
        let provider = provider_of(&vm, instance_id);
        let mut state = std::mem::take(self.codex_state(instance_id));
        let inputs = managed_inputs(&vm, provider, instance_id, enabled, read_only, host);
        let effects = plan_effects(&inputs, &mut state);
        let derived = derive(&inputs, &state);
        *self.codex_state(instance_id) = state;
        for effect in effects {
            match effect {
                CodexSetupEffect::RefreshProviders => {
                    self.store.read(cx).source().refresh_providers(cx);
                }
                CodexSetupEffect::AutoStart => {
                    self.codex_setup(instance_id, CHATGPT_METHOD_ID, &derived, cx);
                }
                CodexSetupEffect::ContinueWithSignIn(method) => {
                    self.codex_sign_in(instance_id, &method, cx);
                }
                CodexSetupEffect::OpenPage { url } => {
                    self.store.read(cx).source().open_external_url(&url, cx);
                }
            }
        }
    }

    /// `ManagedCodexSetup` in the settings presentation.
    pub(super) fn managed_codex_setup(
        &mut self,
        instance_id: &str,
        enabled: bool,
        read_only: bool,
        host: CodexSetupHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if host == CodexSetupHost::AccountDialog {
            // `autoStart` is the dialog's whole point: it never waits for a click.
            self.codex_state(instance_id);
        }
        self.codex_run_effects(instance_id, enabled, read_only, host, cx);
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let provider = provider_of(&vm, instance_id);
        let state = self.codex_state(instance_id).clone();
        let inputs = managed_inputs(&vm, provider, instance_id, enabled, read_only, host);
        let derived = derive(&inputs, &state);
        let id = instance_id.to_owned();

        let description: AnyElement = match &derived.account_description {
            AccountDescription::Text(text) => div().child(text.clone()).into_any_element(),
            AccountDescription::SignedInAs(email) => {
                let revealed = self.panels.providers.revealed_emails.contains(instance_id);
                let toggle_id = instance_id.to_owned();
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
                        SharedString::from(format!("codex-email-{instance_id}")),
                        email,
                        revealed,
                        cx,
                        on_toggle,
                    ))
                    .child(".")
                    .into_any_element()
            }
        };

        let waiting_control = {
            let url = derived.url.clone();
            let click_id = id.clone();
            Button::new(SharedString::from(format!("codex-open-sign-in-{id}")))
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .label(waiting_control_label(&derived))
                .disabled(waiting_control_disabled(&derived))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(url) = &url {
                        this.store.read(cx).source().open_external_url(url, cx);
                    }
                    let _ = &click_id;
                }))
        };

        let controls = if derived.auth_active {
            let flow = derived.flow_id.clone();
            let cancel_id = id.clone();
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .child(waiting_control)
                .when(!derived.finishing_sign_in, |this| {
                    this.child(
                        Button::new(SharedString::from(format!("codex-cancel-sign-in-{id}")))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Sm)
                            .label("Cancel")
                            .disabled(state.pending || read_only)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some(flow) = &flow else { return };
                                this.codex_state(&cancel_id).open_requested = false;
                                this.codex_cancel_sign_in(&cancel_id, flow, cx);
                                if host == CodexSetupHost::AccountDialog {
                                    this.dialog = None;
                                    cx.notify();
                                }
                            })),
                    )
                })
        } else if derived.install_active {
            let operation = inputs
                .installation
                .and_then(|installation| installation.operation_id.as_ref())
                .map(|operation| operation.as_str().to_owned());
            let cancel_id = id.clone();
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("codex-cancel-install-{id}")))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .label("Cancel")
                        .disabled(state.pending || read_only)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(operation) = &operation else { return };
                            this.codex_cancel_install(&cancel_id, operation, cx);
                        })),
                )
        } else if derived.authenticated {
            let change_id = id.clone();
            let logout_id = id.clone();
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("codex-change-account-{id}")))
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("Change account")
                        .disabled(derived.unavailable || derived.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_chat_gpt_account_picker(&change_id, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("codex-disconnect-{id}")))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .label("Disconnect")
                        .disabled(derived.unavailable || derived.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let source = this.store.read(cx).source();
                            let logout_id = logout_id.clone();
                            this.codex_run(
                                &logout_id.clone(),
                                move |cx| source.logout_provider(&logout_id, cx),
                                cx,
                            );
                        })),
                )
        } else {
            let connect_id = id.clone();
            let other_id = id.clone();
            let derived_for_connect = derived.clone();
            let derived_for_other = derived.clone();
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .child(self.chat_gpt_connection_button(
                    &format!("codex-connect-{id}"),
                    ButtonSize::Sm,
                    ButtonVariant::Default,
                    Some(connect_button_label(&derived, state.pending)),
                    connect_button_disabled(&derived, provider),
                    cx,
                    move |this, _, cx| {
                        let has_saved = derived_for_connect.has_saved_account;
                        if has_saved {
                            this.open_chat_gpt_account_picker(&connect_id, cx);
                        } else {
                            this.codex_setup(
                                &connect_id,
                                CHATGPT_METHOD_ID,
                                &derived_for_connect,
                                cx,
                            );
                        }
                    },
                ))
                .when(derived.has_saved_account, |this| {
                    this.child(
                        Button::new(SharedString::from(format!("codex-other-account-{id}")))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Sm)
                            .label("Use a different account")
                            .disabled(derived.unavailable || derived.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.codex_setup(
                                    &other_id,
                                    CHATGPT_CHANGE_ACCOUNT_METHOD_ID,
                                    &derived_for_other,
                                    cx,
                                );
                            })),
                    )
                })
        };
        let account_row = SettingsRow::new("ChatGPT account")
            .description(description)
            .control(controls);

        let divider = with_alpha(theme.border, 0.5);
        let mut section = div().flex().flex_col();
        let mut parts: Vec<AnyElement> = vec![account_row.into_any_element()];
        if derived.authenticated {
            parts.push(
                div()
                    .flex()
                    .justify_end()
                    .px_4()
                    .py_1()
                    .child(self.chat_gpt_usage_button(&format!("codex-usage-{id}"), cx))
                    .into_any_element(),
            );
        }
        if let Some(warning) = &derived.logout_warning {
            parts.push(
                div()
                    .px_4()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(warning.clone())
                    .into_any_element(),
            );
        }
        if let Some(fallback) =
            self.codex_callback_fallback(instance_id, &state, &derived, read_only, window, cx)
        {
            parts.push(div().px_4().py_3().child(fallback).into_any_element());
        }
        if let Some(error) = state.error.clone().or_else(|| {
            inputs
                .installation
                .filter(|installation| {
                    installation.phase
                        == turin_contracts::provider_setup::ProviderInstallPhase::Failed
                })
                .map(|installation| {
                    installation.message.clone().unwrap_or_else(|| {
                        "Could not read Codex setup status. Reconnect and try again.".to_owned()
                    })
                })
        }) {
            parts.push(
                div()
                    .px_4()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.destructive)
                    .child(error)
                    .into_any_element(),
            );
        }
        for (index, part) in parts.into_iter().enumerate() {
            section = section.child(
                div()
                    .when(index > 0, |this| this.border_t_1().border_color(divider))
                    .child(part),
            );
        }
        section.into_any_element()
    }

    /// `callbackFallback`: the "Having trouble signing in?" disclosure with the
    /// paste-the-redirect-URL form.
    fn codex_callback_fallback(
        &mut self,
        instance_id: &str,
        state: &ManagedCodexState,
        derived: &ManagedDerived,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !derived.callback_available {
            return None;
        }
        let theme = cx.app_theme().clone();
        let toggle_id = instance_id.to_owned();
        let toggle = codex_sign_in_description(
            &theme,
            format!("codex-help-{instance_id}"),
            "Having trouble signing in?",
            state.callback_help_open,
            cx.listener(move |this, _, _, cx| {
                let state = this.codex_state(&toggle_id);
                state.callback_help_open = !state.callback_help_open;
                cx.notify();
            }),
        );
        let mut column = div()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(TEXT_XS.size)
            .line_height(gpui_kit::relative(1.625))
            .text_color(theme.muted_foreground)
            .child(toggle);
        if state.callback_help_open {
            let completion =
                self.codex_callback_completion(instance_id, state, derived, read_only, window, cx);
            column = column.child(
                div()
                    .w_full()
                    .border_t_1()
                    .border_color(with_alpha(theme.border, 0.5))
                    .pt_3()
                    .child(completion),
            );
        }
        Some(column.into_any_element())
    }

    /// `callbackCompletion`.
    fn codex_callback_completion(
        &mut self,
        instance_id: &str,
        state: &ManagedCodexState,
        derived: &ManagedDerived,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let flow = derived.flow_id.clone();
        let value = callback_url(state, flow.as_deref());
        let input_id = format!("codex-callback-{instance_id}");
        let draft_id = instance_id.to_owned();
        let draft_flow = flow.clone().unwrap_or_default();
        let input = self.password_input_live(
            &input_id,
            &value,
            "Paste the URL from the sign-in tab",
            window,
            cx,
            move |this, text, _, cx| {
                let state = this.codex_state(&draft_id);
                state.callback_flow_id = draft_flow.clone();
                state.callback_value = text;
                cx.notify();
            },
        );
        let submit_id = instance_id.to_owned();
        let submit = move |this: &mut SettingsView, cx: &mut Context<SettingsView>| {
            this.codex_submit_callback(&submit_id, cx);
        };
        let enter_submit = submit.clone();
        self.on_input_submit(&input_id, move |this, _, cx| enter_submit(this, cx));
        let url = derived.url.clone().unwrap_or_default();
        div()
            .flex()
            .w_full()
            .flex_col()
            .gap_3()
            .text_size(TEXT_XS.size)
            .line_height(gpui_kit::relative(1.625))
            .text_color(theme.muted_foreground)
            .child("If sign-in doesn't return to T3 Code, paste the URL from the final localhost page.")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(password_field(&input, None, FieldSize::Default, cx)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("codex-callback-connect-{instance_id}")))
                            .variant(ButtonVariant::Outline)
                            .label("Connect")
                            .disabled(state.pending || read_only || value.trim().is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| submit(this, cx))),
                    ),
            )
            .child(
                div().child(
                    Button::new(SharedString::from(format!("codex-try-browser-{instance_id}")))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::Xs)
                        .label("Try sign-in in your browser")
                        .child(Icon::new(IconName::ExternalLink).size(rems(0.875)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.store.read(cx).source().open_external_url(&url, cx);
                        })),
                ),
            )
            .into_any_element()
    }

    /// The Connect form's `onSubmit`.
    fn codex_submit_callback(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let vm = self.vm(cx);
        let flow = vm
            .provider_auth
            .get(instance_id)
            .and_then(|auth| auth.flow_id.as_ref())
            .map(|flow| flow.as_str().to_owned());
        let Some(flow) = flow else { return };
        let state = self.codex_state(instance_id);
        let value = callback_url_for(state, &flow);
        let value = value.trim().to_owned();
        if value.is_empty() {
            return;
        }
        let source = self.store.read(cx).source();
        let id = instance_id.to_owned();
        let submitted_flow = flow.clone();
        let connected = self.codex_run(
            instance_id,
            move |cx| source.complete_provider_auth(&id, &submitted_flow, &value, cx),
            cx,
        );
        if connected {
            let state = self.codex_state(instance_id);
            state.callback_flow_id = flow;
            state.callback_value = String::new();
        }
    }
}

fn callback_url_for(state: &ManagedCodexState, flow: &str) -> String {
    callback_url(state, Some(flow))
}

fn provider_of<'a>(vm: &'a SettingsViewModel, instance_id: &str) -> Option<&'a ServerProvider> {
    vm.providers
        .iter()
        .find(|provider| provider.instance_id.as_str() == instance_id)
}

fn managed_inputs<'a>(
    vm: &'a SettingsViewModel,
    provider: Option<&'a ServerProvider>,
    instance_id: &str,
    enabled: bool,
    read_only: bool,
    host: CodexSetupHost,
) -> ManagedInputs<'a> {
    ManagedInputs {
        provider,
        auth: vm.provider_auth.get(instance_id),
        installation: vm.provider_install.get(instance_id),
        enabled,
        read_only,
        auto_start: host == CodexSetupHost::AccountDialog,
    }
}

/// `CodexSignInDescription` with `onToggle`: the disclosure button.
fn codex_sign_in_description(
    _theme: &crate::theme::AppTheme,
    id: String,
    label: &'static str,
    expanded: bool,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .flex()
        .w_full()
        .items_center()
        .gap(rems(0.375))
        .rounded(px(4.))
        .cursor_pointer()
        .on_click(on_toggle)
        .child(label)
        .child(
            // No transforms in GPUI: the chevron swaps instead of rotating 90°.
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(rems(0.75)),
        )
        .into_any_element()
}

impl SettingsView {
    /// `setup(methodId)` for a picked account, outside the card's render.
    pub(super) fn codex_setup_for_instance(
        &mut self,
        instance_id: &str,
        method_id: &str,
        cx: &mut Context<Self>,
    ) {
        let enabled = self
            .instance_rows(cx)
            .iter()
            .find(|row| row.id == instance_id)
            .map(super::panels::providers::instance_enabled)
            .unwrap_or(true);
        let vm = self.vm(cx);
        let provider = provider_of(&vm, instance_id);
        let state = self.codex_state(instance_id).clone();
        let inputs = managed_inputs(
            &vm,
            provider,
            instance_id,
            enabled,
            false,
            CodexSetupHost::Card,
        );
        let derived = derive(&inputs, &state);
        self.codex_setup(instance_id, method_id, &derived, cx);
    }
}

impl SettingsView {
    /// `CodexManagedRuntimeFields`: the server selects the executable for managed
    /// instances; local config cannot override it.
    pub(super) fn codex_managed_runtime_fields(
        &mut self,
        instance_id: &str,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let vm = self.vm(cx);
        let provider = provider_of(&vm, instance_id);
        let executable_path = vm
            .provider_install
            .get(instance_id)
            .and_then(|installation| installation.executable_path.clone().flatten())
            .map(|path| path.as_str().to_owned())
            .unwrap_or_default();
        let paths = provider.and_then(|provider| provider.runtime_paths.as_ref());
        let home = paths
            .map(|paths| paths.home_path.as_str().to_owned())
            .unwrap_or_default();
        let shadow = paths
            .and_then(|paths| paths.shadow_home_path.as_ref())
            .map(|path| path.as_str().to_owned())
            .unwrap_or_default();
        let width = Some(rems(20.).to_pixels(px(16.)));
        vec![
            SettingsRow::new("Binary path")
                .description("Selected by T3 Code.")
                .control(static_text_field(
                    &executable_path,
                    "Not installed",
                    width,
                    cx,
                ))
                .into_any_element(),
            SettingsRow::new("CODEX_HOME path")
                .description("Shared Codex config, sessions, and state.")
                .control(static_text_field(&home, "Unavailable", width, cx))
                .into_any_element(),
            SettingsRow::new("Shadow home path")
                .description(if shadow.is_empty() {
                    "This instance uses the shared Codex home directly."
                } else {
                    "Account-specific home sharing the Codex state above."
                })
                .control(static_text_field(
                    &shadow,
                    if paths.is_some() {
                        "Not used"
                    } else {
                        "Unavailable"
                    },
                    width,
                    cx,
                ))
                .into_any_element(),
        ]
    }
}
