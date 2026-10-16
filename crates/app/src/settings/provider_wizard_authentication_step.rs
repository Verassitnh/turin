//! Port of upstream/apps/web/src/components/settings/ProviderWizardAuthenticationStep.tsx.
//!
//! The wizard's post-create sign-in step. Sign-in uses the saved instance's
//! environment and credentials, just like chat. Upstream reaches it only for
//! ACP registry agents; Turin's wizard reaches it whenever the wizard holds a
//! created instance id (see `add_provider.rs`).

use gpui_kit::{AnyElement, Context, IntoElement as _, SharedString, Window};
use turin_contracts::provider_setup::{ProviderAuthPhase, ProviderAuthState};
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuthStatus, ServerProviderSetup,
};

use super::layout::{SettingsRow, settings_group_plain};
use super::view::SettingsView;
use super::wizard::{wizard_footer, wizard_panel};
use crate::theme::ActiveAppTheme;
use crate::ui::{Button, ButtonSize, ButtonVariant};

/// What the step derives from the provider snapshot and the auth query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardAuthStep {
    pub active: bool,
    pub signed_in: bool,
    pub is_discovering: bool,
    pub can_authenticate: bool,
}

pub fn wizard_auth_step(
    provider: Option<&ServerProvider>,
    auth: Option<&ProviderAuthState>,
    query_error: bool,
) -> WizardAuthStep {
    let phase = auth.map(|auth| auth.phase);
    let active = matches!(
        phase,
        Some(
            ProviderAuthPhase::Starting | ProviderAuthPhase::Waiting | ProviderAuthPhase::Verifying
        )
    );
    let status = provider.map(|provider| provider.auth.status);
    let signed_in = status == Some(ServerProviderAuthStatus::Authenticated)
        || (status == Some(ServerProviderAuthStatus::Unknown)
            && phase == Some(ProviderAuthPhase::Succeeded));
    let methods = auth.and_then(|auth| auth.methods.as_ref());
    WizardAuthStep {
        active,
        signed_in,
        is_discovering: !signed_in && !query_error && methods.is_none(),
        can_authenticate: methods.is_some_and(|methods| !methods.is_empty()),
    }
}

/// The text under "Account" when no in-app sign-in is available.
pub fn wizard_auth_description(step: &WizardAuthStep, auth: Option<&ProviderAuthState>) -> String {
    if step.is_discovering {
        "Discovering sign-in methods…".to_owned()
    } else {
        auth.and_then(|auth| auth.message.clone())
            .unwrap_or_else(|| {
                "No in-app sign-in advertised. Follow the provider's docs to finish setup."
                    .to_owned()
            })
    }
}

/// The footer button's label: "Done" once signed in, else "Skip for now".
pub fn wizard_auth_finish_label(signed_in: bool) -> &'static str {
    if signed_in { "Done" } else { "Skip for now" }
}

impl SettingsView {
    /// The step's panel and footer, as `[panel, footer]`.
    pub(super) fn provider_wizard_authentication_step(
        &mut self,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let provider = vm
            .providers
            .iter()
            .find(|candidate| candidate.instance_id.as_str() == instance_id)
            .cloned();
        let auth = vm.provider_auth.get(instance_id);
        let step = wizard_auth_step(provider.as_ref(), auth, false);

        let content: AnyElement = match provider {
            Some(mut provider) if step.can_authenticate || step.signed_in => {
                let previous = provider.setup.take();
                provider.setup = Some(ServerProviderSetup {
                    can_install: previous.as_ref().is_some_and(|setup| setup.can_install),
                    can_authenticate: step.can_authenticate,
                    documentation_url: previous.and_then(|setup| setup.documentation_url),
                });
                self.provider_authentication_section(instance_id, &provider, false, window, cx)
            }
            provider => {
                let docs = provider
                    .as_ref()
                    .and_then(|provider| provider.setup.as_ref())
                    .and_then(|setup| setup.documentation_url.as_ref())
                    .map(|url| url.as_str().to_owned());
                let control: Option<AnyElement> = if step.is_discovering {
                    Some(
                        Button::new("wizard-auth-sign-in")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Sign in")
                            .disabled(true)
                            .into_any_element(),
                    )
                } else {
                    docs.map(|docs| {
                        Button::new("wizard-auth-docs")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Open docs")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.store.read(cx).source().open_external_url(&docs, cx);
                            }))
                            .into_any_element()
                    })
                };
                let mut row =
                    SettingsRow::new("Account").description(wizard_auth_description(&step, auth));
                if let Some(control) = control {
                    row = row.control(control);
                }
                row.into_any_element()
            }
        };

        let panel = wizard_panel(
            &theme,
            Some(gpui_kit::rems(18.)),
            settings_group_plain(vec![content]).into_any_element(),
        );
        let footer = wizard_footer(
            None,
            vec![
                Button::new(SharedString::from("wizard-auth-finish"))
                    .variant(if step.signed_in {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Outline
                    })
                    .size(ButtonSize::Sm)
                    .label(wizard_auth_finish_label(step.signed_in))
                    .disabled(step.active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dialog = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            ],
        );
        vec![panel, footer]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(status: &str) -> ServerProvider {
        serde_json::from_value(json!({
            "instanceId": "acp_x", "driver": "acpRegistry", "enabled": true, "installed": true,
            "status": "ready", "auth": { "status": status },
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [], "slashCommands": [], "skills": []
        }))
        .expect("provider")
    }

    fn auth(phase: &str, extra: serde_json::Value) -> ProviderAuthState {
        let mut value = json!({ "instanceId": "acp_x", "phase": phase });
        if let (Some(object), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            object.extend(extra.clone());
        }
        serde_json::from_value(value).expect("auth")
    }

    #[test]
    fn discovers_until_the_methods_arrive() {
        let step = wizard_auth_step(Some(&provider("unauthenticated")), None, false);
        assert!(step.is_discovering);
        assert!(!step.can_authenticate && !step.signed_in && !step.active);
        assert_eq!(
            wizard_auth_description(&step, None),
            "Discovering sign-in methods…"
        );
    }

    #[test]
    fn offers_sign_in_when_methods_are_advertised() {
        let state = auth(
            "idle",
            json!({ "methods": [{ "id": "oauth", "name": "OAuth", "type": "agent" }] }),
        );
        let step = wizard_auth_step(Some(&provider("unauthenticated")), Some(&state), false);
        assert!(step.can_authenticate && !step.is_discovering);
    }

    #[test]
    fn reports_that_no_in_app_sign_in_is_advertised() {
        let state = auth("idle", json!({ "methods": [] }));
        let step = wizard_auth_step(Some(&provider("unauthenticated")), Some(&state), false);
        assert!(!step.can_authenticate && !step.is_discovering);
        assert_eq!(
            wizard_auth_description(&step, Some(&state)),
            "No in-app sign-in advertised. Follow the provider's docs to finish setup."
        );
        let failed = auth(
            "failed",
            json!({ "message": "Sign-in failed.", "methods": [] }),
        );
        assert_eq!(
            wizard_auth_description(&step, Some(&failed)),
            "Sign-in failed."
        );
    }

    #[test]
    fn counts_an_unknown_status_with_a_succeeded_flow_as_signed_in() {
        let succeeded = auth("succeeded", json!({}));
        assert!(wizard_auth_step(Some(&provider("unknown")), Some(&succeeded), false).signed_in);
        assert!(wizard_auth_step(Some(&provider("authenticated")), None, false).signed_in);
        assert!(
            !wizard_auth_step(Some(&provider("unauthenticated")), Some(&succeeded), false)
                .signed_in
        );
    }

    #[test]
    fn holds_the_finish_button_while_a_flow_is_active() {
        let waiting = auth("waiting", json!({}));
        assert!(wizard_auth_step(Some(&provider("unauthenticated")), Some(&waiting), false).active);
        assert_eq!(wizard_auth_finish_label(true), "Done");
        assert_eq!(wizard_auth_finish_label(false), "Skip for now");
    }
}
