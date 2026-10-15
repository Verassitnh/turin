//! Port of upstream/apps/server/src/provider/unavailableProviderSnapshot.ts.
//!
//! A configured instance whose driver this build does not ship still has to
//! round-trip on the wire. The snapshot says `unavailable` and refuses turns.

use turin_contracts::{
    IsoDateTime, TrimmedNonEmptyString, UtcDateTime,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};

use crate::{
    server_provider::{
        ServerProvider, ServerProviderAuth, ServerProviderAuthStatus, ServerProviderAvailability,
        ServerProviderState,
    },
    snapshot_probe::{
        BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
        build_server_provider,
    },
};

pub struct UnavailableProviderSnapshotInput {
    pub driver_kind: ProviderDriverKind,
    pub instance_id: ProviderInstanceId,
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub reason: String,
    /// Override for `checkedAt`. Defaults to now so tests can pin it.
    pub checked_at: Option<IsoDateTime>,
}

pub fn build_unavailable_provider_snapshot(
    input: UnavailableProviderSnapshotInput,
) -> ServerProvider {
    let checked_at = input
        .checked_at
        .unwrap_or_else(|| UtcDateTime::now().to_string());
    let display_name = input
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| input.driver_kind.as_str())
        .to_owned();
    let mut draft = build_server_provider(BuildServerProviderInput {
        driver: None,
        presentation: ServerProviderPresentation {
            display_name,
            badge_label: None,
            show_interaction_mode_toggle: None,
            reports_context_window: None,
            supported_runtime_modes: None,
            requires_new_thread_for_model_change: None,
            supports_conversation_rollback: None,
        },
        enabled: false,
        checked_at,
        models: Vec::new(),
        update_required_models: Vec::new(),
        slash_commands: Vec::new(),
        skills: Vec::new(),
        probe: ProviderProbeResult {
            installed: false,
            version: None,
            status: ServerProviderState::Error,
            auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
            message: Some(input.reason.clone()),
            usage_limits: None,
        },
        maintenance: None,
    });
    draft.accent_color = input
        .accent_color
        .as_deref()
        .and_then(|color| TrimmedNonEmptyString::new(color).ok());
    draft.availability = Some(ServerProviderAvailability::Unavailable);
    draft.unavailable_reason = TrimmedNonEmptyString::new(&input.reason).ok();
    ServerProvider {
        instance_id: input.instance_id,
        driver: input.driver_kind,
        draft,
    }
}
