//! Port of upstream/packages/provider-core/src/server/instanceIdentity.ts.
//!
//! Drivers build a draft without knowing which instance they are. This stamps
//! the routing identity on before the snapshot is published.

use turin_contracts::{
    TrimmedNonEmptyString,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};

use crate::server_provider::{ServerProvider, ServerProviderContinuation, ServerProviderDraft};

pub struct InstanceIdentity {
    pub instance_id: ProviderInstanceId,
    pub driver_kind: ProviderDriverKind,
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub continuation_group_key: String,
}

pub fn with_instance_identity(
    identity: &InstanceIdentity,
    mut draft: ServerProviderDraft,
) -> ServerProvider {
    if let Some(name) = identity.display_name.as_deref().and_then(trimmed) {
        draft.display_name = Some(name);
    }
    if let Some(color) = identity.accent_color.as_deref().and_then(trimmed) {
        draft.accent_color = Some(color);
    }
    if let Some(group_key) = trimmed(&identity.continuation_group_key) {
        draft.continuation = Some(ServerProviderContinuation { group_key });
    }
    ServerProvider {
        instance_id: identity.instance_id.clone(),
        driver: identity.driver_kind.clone(),
        draft,
    }
}

fn trimmed(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        server_provider::{ServerProviderAuth, ServerProviderAuthStatus, ServerProviderState},
        snapshot_probe::{
            BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
            build_server_provider,
        },
    };

    #[test]
    fn stamps_identity_and_keeps_an_empty_display_name_off_the_draft() {
        let draft = build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: ServerProviderPresentation {
                display_name: "from-probe".to_owned(),
                badge_label: None,
                show_interaction_mode_toggle: None,
                reports_context_window: None,
                supported_runtime_modes: None,
                requires_new_thread_for_model_change: None,
                supports_conversation_rollback: None,
            },
            enabled: true,
            checked_at: "2026-01-01T00:00:00.000Z".to_owned(),
            models: Vec::new(),
            update_required_models: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            probe: ProviderProbeResult {
                installed: true,
                version: None,
                status: ServerProviderState::Ready,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
                message: None,
                usage_limits: None,
            },
            maintenance: None,
        });
        let provider = with_instance_identity(
            &InstanceIdentity {
                instance_id: ProviderInstanceId::new("codex_work").unwrap(),
                driver_kind: ProviderDriverKind::new("codex").unwrap(),
                display_name: Some("  ".to_owned()),
                accent_color: Some("#112233".to_owned()),
                continuation_group_key: "codex:instance:codex_work".to_owned(),
            },
            draft,
        );
        assert_eq!(provider.instance_id.as_str(), "codex_work");
        assert_eq!(provider.driver.as_str(), "codex");
        assert_eq!(
            provider.display_name.as_ref().unwrap().as_str(),
            "from-probe"
        );
        assert_eq!(provider.accent_color.as_ref().unwrap().as_str(), "#112233");
        assert_eq!(
            provider.continuation.as_ref().unwrap().group_key.as_str(),
            "codex:instance:codex_work"
        );
    }
}
