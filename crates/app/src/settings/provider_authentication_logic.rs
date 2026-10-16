//! Port of upstream/apps/web/src/components/settings/ProviderAuthenticationSection.tsx
//! (description / control visibility rules only).

use turin_contracts::provider_setup::{
    ProviderAuthInteraction, ProviderAuthPhase, ProviderAuthState,
};
use turin_provider_core::server_provider::{ServerProvider, ServerProviderAuthStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInButtonLabel {
    SignIn,
    ChangeAccount,
    RetrySignIn,
}

pub fn auth_phase_active(phase: ProviderAuthPhase) -> bool {
    matches!(
        phase,
        ProviderAuthPhase::Starting | ProviderAuthPhase::Waiting | ProviderAuthPhase::Verifying
    )
}

pub fn provider_signed_in(
    auth_status: ServerProviderAuthStatus,
    auth_state: Option<&ProviderAuthState>,
) -> bool {
    if auth_status == ServerProviderAuthStatus::Authenticated {
        return true;
    }
    auth_status == ServerProviderAuthStatus::Unknown
        && auth_state.is_some_and(|state| state.phase == ProviderAuthPhase::Succeeded)
}

pub fn auth_is_discovering(
    driver: &str,
    signed_in: bool,
    query_error: bool,
    auth_state: Option<&ProviderAuthState>,
) -> bool {
    driver == "acpRegistry"
        && !signed_in
        && !query_error
        && auth_state.is_none_or(|state| state.methods.is_none())
}

pub fn needs_external_setup(
    signed_in: bool,
    active: bool,
    provider: &ServerProvider,
    auth_state: Option<&ProviderAuthState>,
) -> bool {
    if active || signed_in {
        return false;
    }
    if provider
        .setup
        .as_ref()
        .is_some_and(|setup| !setup.can_authenticate)
    {
        return true;
    }
    provider.driver.as_str() == "acpRegistry"
        && auth_state.is_some_and(|state| state.methods.as_ref().is_some_and(|m| m.is_empty()))
}

pub fn resolved_interaction(
    auth_state: Option<&ProviderAuthState>,
) -> Option<&ProviderAuthInteraction> {
    auth_state
        .and_then(|state| state.interaction.as_ref())
        .and_then(|inner| inner.as_ref())
}

pub fn auth_sign_in_url<'a>(
    auth_state: Option<&'a ProviderAuthState>,
    interaction: Option<&'a ProviderAuthInteraction>,
) -> Option<&'a str> {
    if let Some(ProviderAuthInteraction::Browser { url, .. })
    | Some(ProviderAuthInteraction::DeviceCode { url, .. }) = interaction
    {
        return Some(url.as_str());
    }
    auth_state.and_then(|state| state.authorization_url.as_deref())
}

pub fn account_description(
    environment_label: &str,
    signed_in: bool,
    active: bool,
    is_discovering: bool,
    needs_external: bool,
    auth_state: Option<&ProviderAuthState>,
    interaction: Option<&ProviderAuthInteraction>,
) -> String {
    if active {
        let phase = auth_state.map(|state| state.phase);
        return match phase {
            Some(ProviderAuthPhase::Starting) => "Starting sign-in…".to_owned(),
            Some(ProviderAuthPhase::Verifying) => "Checking your account…".to_owned(),
            _ => match interaction {
                Some(ProviderAuthInteraction::Terminal { .. }) => {
                    "Complete sign-in in the terminal below.".to_owned()
                }
                Some(ProviderAuthInteraction::Credentials { .. }) => {
                    "Enter your credentials below.".to_owned()
                }
                _ => "Finish signing in in your browser.".to_owned(),
            },
        };
    }
    if signed_in {
        return "Signed in.".to_owned();
    }
    if is_discovering {
        return "Discovering sign-in methods…".to_owned();
    }
    if needs_external {
        return "No in-app sign-in advertised. Follow the provider's docs to finish setup."
            .to_owned();
    }
    format!("Sign in on {environment_label}.")
}

pub fn sign_in_button_label(
    signed_in: bool,
    auth_state: Option<&ProviderAuthState>,
) -> Option<SignInButtonLabel> {
    if signed_in {
        return Some(SignInButtonLabel::ChangeAccount);
    }
    match auth_state.map(|state| state.phase) {
        Some(ProviderAuthPhase::Failed) | Some(ProviderAuthPhase::Cancelled) => {
            Some(SignInButtonLabel::RetrySignIn)
        }
        _ => Some(SignInButtonLabel::SignIn),
    }
}

pub fn show_sign_in_button(active: bool, needs_external: bool, can_authenticate: bool) -> bool {
    !active && !needs_external && can_authenticate
}

pub fn show_sign_out_button(active: bool, signed_in: bool, can_logout: bool) -> bool {
    !active && signed_in && can_logout
}

pub fn show_cancel_button(active: bool, flow_id_present: bool) -> bool {
    active && flow_id_present
}

pub fn show_callback_form(
    url: Option<&str>,
    interaction: Option<&ProviderAuthInteraction>,
) -> bool {
    let Some(url) = url else {
        return false;
    };
    !url.is_empty()
        && match interaction {
            Some(ProviderAuthInteraction::Browser {
                accepts_callback: Some(true),
                ..
            }) => true,
            Some(ProviderAuthInteraction::Browser { .. }) => false,
            None => true,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::provider_setup::SetupOperationId;

    fn minimal_provider(can_authenticate: bool) -> ServerProvider {
        serde_json::from_value(json!({
            "instanceId": "cursor",
            "driver": "cursor",
            "enabled": true,
            "installed": true,
            "status": "ready",
            "auth": { "status": "unauthenticated" },
            "checkedAt": "2026-01-01T00:00:00.000Z",
            "setup": { "canAuthenticate": can_authenticate, "canInstall": false },
            "models": [],
        }))
        .expect("test provider")
    }

    #[test]
    fn signed_in_when_authenticated_or_unknown_with_succeeded_phase() {
        let state: ProviderAuthState = serde_json::from_value(json!({
            "instanceId": "x",
            "phase": "succeeded"
        }))
        .unwrap();
        assert!(provider_signed_in(
            ServerProviderAuthStatus::Authenticated,
            None
        ));
        assert!(provider_signed_in(
            ServerProviderAuthStatus::Unknown,
            Some(&state)
        ));
        assert!(!provider_signed_in(
            ServerProviderAuthStatus::Unauthenticated,
            Some(&state)
        ));
    }

    #[test]
    fn account_description_covers_active_and_idle_paths() {
        assert_eq!(
            account_description("Device", false, true, false, false, None, None),
            "Finish signing in in your browser."
        );
        assert_eq!(
            account_description("Device", false, false, true, false, None, None),
            "Discovering sign-in methods…"
        );
        assert_eq!(
            account_description("Device", false, false, false, false, None, None),
            "Sign in on Device."
        );
    }

    #[test]
    fn needs_external_when_can_authenticate_is_false() {
        let provider = minimal_provider(false);
        assert!(needs_external_setup(false, false, &provider, None));
    }

    #[test]
    fn auth_url_prefers_interaction_url() {
        let interaction = ProviderAuthInteraction::Browser {
            id: SetupOperationId::new("f").unwrap(),
            url: TrimmedNonEmptyString::new("https://example.com").unwrap(),
            requires_consent: false,
            accepts_callback: None,
        };
        assert_eq!(
            auth_sign_in_url(None, Some(&interaction)),
            Some("https://example.com")
        );
    }

    #[test]
    fn retry_label_after_failed_phase() {
        let state: ProviderAuthState = serde_json::from_value(json!({
            "instanceId": "x",
            "phase": "failed",
            "message": "nope"
        }))
        .unwrap();
        assert_eq!(
            sign_in_button_label(false, Some(&state)),
            Some(SignInButtonLabel::RetrySignIn)
        );
    }

    #[test]
    fn callback_form_only_for_browser_with_flag_or_missing_interaction() {
        let browser = ProviderAuthInteraction::Browser {
            id: SetupOperationId::new("f").unwrap(),
            url: TrimmedNonEmptyString::new("https://x").unwrap(),
            requires_consent: false,
            accepts_callback: Some(true),
        };
        assert!(show_callback_form(Some("https://x"), Some(&browser)));
        let terminal = ProviderAuthInteraction::Terminal {
            id: SetupOperationId::new("t").unwrap(),
            output: String::new(),
            output_offset: None,
        };
        assert!(!show_callback_form(Some("https://x"), Some(&terminal)));
    }
}
