//! The composer's provider catalogue from live provider status
//! (`ProviderRegistry::get_providers`): one [`ProviderInstanceEntry`] per
//! configured instance, with its models, skills and slash commands.
//! Replaces the fixture catalogue for a routed thread.

use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuthStatus, ServerProviderModel, ServerProviderModelBadge,
    ServerProviderState,
};

use crate::composer::catalog::{
    AuthStatus, ProviderInstanceEntry, ProviderSkill, ProviderSlashCommand, ProviderStatus,
};
use crate::composer::provider_options::ModelCapabilities;
use crate::composer::provider_state::ProviderModel;

fn model(model: &ServerProviderModel) -> ProviderModel {
    ProviderModel {
        slug: model.slug.to_string(),
        name: model.name.to_string(),
        short_name: model.short_name.as_ref().map(ToString::to_string),
        sub_provider: model.sub_provider.as_ref().map(ToString::to_string),
        aliases: model
            .aliases
            .iter()
            .flatten()
            .map(ToString::to_string)
            .collect(),
        is_new: model.badge == Some(ServerProviderModelBadge::New),
        is_custom: model.is_custom,
        is_default: model.is_default.unwrap_or(false),
        is_legacy: model.is_legacy.unwrap_or(false),
        is_unavailable: false,
        capabilities: model.capabilities.as_ref().map(ModelCapabilities::from),
    }
}

pub fn provider_entries(providers: &[ServerProvider]) -> Vec<ProviderInstanceEntry> {
    providers
        .iter()
        .map(|provider| {
            let mut entry = ProviderInstanceEntry::new(
                provider.instance_id.as_str(),
                provider.driver.as_str(),
                provider
                    .display_name
                    .as_ref()
                    .map_or(provider.driver.as_str(), |name| name.as_str()),
            );
            entry.accent_color = provider.accent_color.as_ref().map(ToString::to_string);
            entry.continuation_group_key = provider
                .continuation
                .as_ref()
                .map(|continuation| continuation.group_key.to_string());
            entry.enabled = provider.enabled;
            entry.installed = provider.installed;
            entry.status = match provider.status {
                ServerProviderState::Ready => ProviderStatus::Ready,
                ServerProviderState::Warning => ProviderStatus::Warning,
                ServerProviderState::Error => ProviderStatus::Error,
                ServerProviderState::Disabled => ProviderStatus::Disabled,
            };
            entry.is_available = provider.is_available();
            entry.auth_status = match provider.auth.status {
                ServerProviderAuthStatus::Authenticated => AuthStatus::Authenticated,
                ServerProviderAuthStatus::Unauthenticated => AuthStatus::Unauthenticated,
                ServerProviderAuthStatus::Unknown => AuthStatus::Unknown,
            };
            entry.can_setup = provider
                .setup
                .as_ref()
                .is_some_and(|setup| setup.can_authenticate || setup.can_install);
            entry.status_message = provider.message.as_ref().map(ToString::to_string);
            entry.models = provider.models.iter().map(model).collect();
            entry.skills = provider
                .skills
                .iter()
                .map(|skill| ProviderSkill {
                    name: skill.name.to_string(),
                    description: skill.description.as_ref().map(ToString::to_string),
                    path: skill.path.to_string(),
                    scope: skill.scope.as_ref().map(ToString::to_string),
                    enabled: skill.enabled,
                    display_name: skill.display_name.as_ref().map(ToString::to_string),
                    short_description: skill.short_description.as_ref().map(ToString::to_string),
                    user_invocation_only: skill.user_invocation_only.unwrap_or(false),
                    user_invocable: skill.user_invocable.unwrap_or(true),
                })
                .collect();
            entry.slash_commands = provider
                .slash_commands
                .iter()
                .map(|command| ProviderSlashCommand {
                    name: command.name.to_string(),
                    description: command.description.as_ref().map(ToString::to_string),
                    input_hint: command.input.as_ref().map(|input| input.hint.to_string()),
                })
                .collect();
            entry.reports_context_window = provider.reports_context_window.unwrap_or(false);
            entry.show_interaction_mode_toggle =
                provider.show_interaction_mode_toggle.unwrap_or(false);
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_a_status_snapshot_to_a_picker_entry() {
        let provider: ServerProvider = serde_json::from_value(json!({
            "instanceId": "claudeAgent", "driver": "claudeAgent", "displayName": "Claude",
            "enabled": true, "installed": true, "version": "2.1.0", "status": "warning",
            "auth": {"status": "unauthenticated"}, "checkedAt": "2026-01-01T00:00:00.000Z",
            "message": "Not signed in",
            "models": [{"slug": "claude-sonnet-4-6", "name": "Sonnet 4.6", "isCustom": false,
                        "isDefault": true, "badge": "new", "capabilities": null}],
            "slashCommands": [{"name": "review", "description": "Review changes",
                               "input": {"hint": "[path]"}}],
            "skills": [],
        }))
        .expect("snapshot");
        let entries = provider_entries(&[provider]);
        let entry = &entries[0];
        assert_eq!(entry.instance_id, "claudeAgent");
        assert_eq!(entry.status, ProviderStatus::Warning);
        assert_eq!(entry.auth_status, AuthStatus::Unauthenticated);
        assert_eq!(entry.status_message.as_deref(), Some("Not signed in"));
        assert!(entry.models[0].is_default && entry.models[0].is_new);
        assert_eq!(
            entry.slash_commands[0].input_hint.as_deref(),
            Some("[path]")
        );
        assert!(entry.is_default, "instance id equals driver id");
    }
}
