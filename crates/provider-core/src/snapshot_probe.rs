//! Port of upstream/packages/provider-core/src/server/snapshotProbe.ts.
//!
//! `build_server_provider` is the draft every driver snapshot starts from.
//! Process spawning (`spawnAndCollect`) stays with the driver crates; this
//! module keeps the pure construction and the CLI version scan.

use regex::Regex;
use std::sync::OnceLock;
use turin_contracts::{
    IsoDateTime, TrimmedNonEmptyString,
    provider_instance::ProviderDriverKind,
    provider_policy::RuntimeMode,
    provider_usage_limits::ServerProviderUsageLimits,
};

use crate::{
    maintenance::ProviderMaintenanceCapabilities,
    maintenance_resolver::{ProviderVersionAdvisoryInput, create_provider_version_advisory},
    server_provider::{
        ServerProviderAuth, ServerProviderDraft, ServerProviderModel, ServerProviderSkill,
        ServerProviderSlashCommand, ServerProviderState, ServerProviderUpdateRequiredModel,
    },
};

pub struct ProviderProbeResult {
    pub installed: bool,
    pub version: Option<String>,
    pub status: ServerProviderState,
    pub auth: ServerProviderAuth,
    pub message: Option<String>,
    pub usage_limits: Option<ServerProviderUsageLimits>,
}

pub struct ServerProviderPresentation {
    pub display_name: String,
    pub badge_label: Option<String>,
    pub show_interaction_mode_toggle: Option<bool>,
    pub reports_context_window: Option<bool>,
    pub supported_runtime_modes: Option<Vec<RuntimeMode>>,
    pub requires_new_thread_for_model_change: Option<bool>,
    pub supports_conversation_rollback: Option<bool>,
}

pub struct BuildServerProviderInput<'a> {
    pub driver: Option<&'a ProviderDriverKind>,
    pub presentation: ServerProviderPresentation,
    pub enabled: bool,
    pub checked_at: IsoDateTime,
    pub models: Vec<ServerProviderModel>,
    pub update_required_models: Vec<ServerProviderUpdateRequiredModel>,
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    pub skills: Vec<ServerProviderSkill>,
    pub probe: ProviderProbeResult,
    pub maintenance: Option<&'a ProviderMaintenanceCapabilities>,
}

pub fn build_server_provider(input: BuildServerProviderInput<'_>) -> ServerProviderDraft {
    let version_advisory = input.driver.map(|driver| {
        create_provider_version_advisory(ProviderVersionAdvisoryInput {
            driver,
            current_version: input.probe.version.as_deref(),
            latest_version: None,
            checked_at: Some(&input.checked_at),
            maintenance: input.maintenance,
        })
    });
    ServerProviderDraft {
        display_name: trimmed(&input.presentation.display_name),
        accent_color: None,
        icon_url: None,
        badge_label: input.presentation.badge_label.as_deref().and_then(trimmed),
        continuation: None,
        show_interaction_mode_toggle: input.presentation.show_interaction_mode_toggle,
        reports_context_window: input.presentation.reports_context_window,
        supported_runtime_modes: input.presentation.supported_runtime_modes,
        requires_new_thread_for_model_change: input.presentation.requires_new_thread_for_model_change,
        supports_conversation_rollback: input.presentation.supports_conversation_rollback,
        supports_text_generation: None,
        setup: None,
        native_sessions: None,
        configurable_providers: None,
        runtime_paths: None,
        enabled: input.enabled,
        installed: input.probe.installed,
        version: input.probe.version.as_deref().and_then(trimmed),
        status: if input.enabled {
            input.probe.status
        } else {
            ServerProviderState::Disabled
        },
        auth: input.probe.auth,
        checked_at: input.checked_at,
        message: input.probe.message.as_deref().and_then(trimmed),
        availability: None,
        unavailable_reason: None,
        models: input.models,
        update_required_models: if input.update_required_models.is_empty() {
            None
        } else {
            Some(input.update_required_models)
        },
        slash_commands: input.slash_commands,
        skills: input.skills,
        workspace_snapshots: None,
        usage_limits: input.probe.usage_limits,
        version_advisory,
        compatibility_advisory: None,
        update_state: None,
    }
}

/// Return the first semantic version found in CLI output, or null.
/// Accepts a leading "v" (for example `opencode v2.0.3`).
pub fn parse_generic_cli_version(output: &str) -> Option<String> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| Regex::new(r"\bv?(\d+\.\d+\.\d+)\b").expect("version regex"));
    pattern
        .captures(output)
        .and_then(|captures| captures.get(1))
        .map(|version| version.as_str().to_owned())
}

fn trimmed(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_leading_v_before_the_word_boundary() {
        assert_eq!(parse_generic_cli_version("opencode v2.0.3\n").as_deref(), Some("2.0.3"));
        assert_eq!(parse_generic_cli_version("no version here"), None);
    }

    #[test]
    fn disabled_snapshot_overrides_the_probe_status() {
        let draft = build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: ServerProviderPresentation {
                display_name: "Codex".to_owned(),
                badge_label: None,
                show_interaction_mode_toggle: None,
                reports_context_window: None,
                supported_runtime_modes: None,
                requires_new_thread_for_model_change: None,
                supports_conversation_rollback: None,
            },
            enabled: false,
            checked_at: "2026-01-01T00:00:00.000Z".to_owned(),
            models: Vec::new(),
            update_required_models: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            probe: ProviderProbeResult {
                installed: false,
                version: None,
                status: ServerProviderState::Error,
                auth: ServerProviderAuth::with_status(crate::server_provider::ServerProviderAuthStatus::Unknown),
                message: Some("missing".to_owned()),
                usage_limits: None,
            },
            maintenance: None,
        });
        assert_eq!(draft.status, ServerProviderState::Disabled);
        assert!(draft.version_advisory.is_none());
        assert_eq!(draft.display_name.unwrap().as_str(), "Codex");
    }
}
