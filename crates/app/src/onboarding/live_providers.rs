//! Maps live provider registry snapshots onto [`OnboardingProvider`].

use turin_contracts::provider_instance::{ProviderInstanceConfigMap, ProviderInstanceId};
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuthStatus, ServerProviderState,
};

use crate::onboarding::provider_readiness_logic::{
    OnboardingProvider, ProviderAuthStatus, ProviderSummary,
};
use crate::settings::provider_status::get_provider_summary;
use crate::state::helpers::provider_types::ServerProviderState as LegacyState;

pub fn onboarding_providers_from_registry(providers: &[ServerProvider]) -> Vec<OnboardingProvider> {
    providers
        .iter()
        .map(onboarding_provider_from_server)
        .collect()
}

fn onboarding_provider_from_server(provider: &ServerProvider) -> OnboardingProvider {
    let summary = get_provider_summary(Some(provider));
    OnboardingProvider {
        instance_id: provider.instance_id.to_string(),
        driver: provider.driver.to_string(),
        display_name: provider
            .display_name
            .as_ref()
            .map(|name| name.to_string()),
        enabled: provider.enabled,
        installed: provider.installed,
        status: map_state(provider.status),
        auth_status: map_auth(provider.auth.status),
        summary: ProviderSummary {
            headline: summary.headline,
            detail: summary.detail,
        },
    }
}

fn map_state(state: ServerProviderState) -> LegacyState {
    match state {
        ServerProviderState::Ready => LegacyState::Ready,
        ServerProviderState::Warning => LegacyState::Warning,
        ServerProviderState::Error => LegacyState::Error,
        ServerProviderState::Disabled => LegacyState::Disabled,
    }
}

fn map_auth(status: ServerProviderAuthStatus) -> ProviderAuthStatus {
    match status {
        ServerProviderAuthStatus::Authenticated => ProviderAuthStatus::Authenticated,
        ServerProviderAuthStatus::Unauthenticated => ProviderAuthStatus::Unauthenticated,
        ServerProviderAuthStatus::Unknown => ProviderAuthStatus::Unknown,
    }
}

pub fn instance_config_from_map(
    instances: &ProviderInstanceConfigMap,
    instance_id: &str,
) -> Option<serde_json::Value> {
    let id = ProviderInstanceId::new(instance_id).ok()?;
    instances.get(&id).and_then(|entry| entry.config.clone())
}
