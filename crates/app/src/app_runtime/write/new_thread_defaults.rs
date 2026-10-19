//! The model and runtime mode a new thread starts with: the project's
//! default (its settings override, or the legacy aggregate fields until the
//! server has folded them), then the server settings' `defaultModelSelection`,
//! then the first selectable provider's default model
//! (`resolveProjectSettings` in useHandleNewThread.ts / ChatView.tsx:2116).

use serde_json::{Value, json};
use turin_contracts::ProjectId;
use turin_contracts::settings::ServerSettings;
use turin_provider_core::server_provider::ServerProvider;
use turin_shared::project_settings::{LegacyProjectSettingsFields, resolve_project_settings};

/// Used only when no provider is selectable at all.
const FALLBACK_INSTANCE: &str = "claudeAgent";
const FALLBACK_MODEL: &str = "claude-sonnet-4-6";

#[derive(Debug, Clone, PartialEq)]
pub struct NewThreadDefaults {
    pub model_selection: Value,
    pub runtime_mode: String,
}

pub fn resolve_new_thread_defaults(
    settings: &ServerSettings,
    project_id: &ProjectId,
    legacy: Option<&LegacyProjectSettingsFields>,
    providers: &[ServerProvider],
) -> NewThreadDefaults {
    let resolved = resolve_project_settings(settings, Some(project_id), legacy, &|_, _, _| None);
    let effective = resolved.settings;
    let model_selection = match &effective.default_model_selection {
        Some(selection) => serde_json::to_value(selection).unwrap_or(Value::Null),
        None => {
            let (instance_id, model) =
                crate::settings::model_picker::resolve_default_selection(providers)
                    .unwrap_or_else(|| (FALLBACK_INSTANCE.to_owned(), FALLBACK_MODEL.to_owned()));
            json!({"instanceId": instance_id, "model": model})
        }
    };
    let runtime_mode = serde_json::to_value(effective.default_runtime_mode)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "full-access".to_owned());
    NewThreadDefaults {
        model_selection,
        runtime_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::provider_policy::RuntimeMode;
    use turin_contracts::settings::default_server_settings;

    fn project() -> ProjectId {
        ProjectId::new("project-1").expect("project id")
    }

    fn selection(instance: &str, model: &str) -> turin_contracts::model_selection::ModelSelection {
        serde_json::from_value(json!({"instanceId": instance, "model": model})).expect("selection")
    }

    #[test]
    fn the_server_default_model_and_runtime_mode_win_over_claude() {
        let mut settings = default_server_settings();
        settings.default_model_selection = Some(selection("codex", "gpt-5.4"));
        settings.default_runtime_mode = RuntimeMode::ApprovalRequired;
        let defaults = resolve_new_thread_defaults(&settings, &project(), None, &[]);
        assert_eq!(
            defaults.model_selection,
            json!({"instanceId": "codex", "model": "gpt-5.4"})
        );
        assert_eq!(defaults.runtime_mode, "approval-required");
    }

    #[test]
    fn the_projects_legacy_default_beats_the_server_default() {
        let mut settings = default_server_settings();
        settings.default_model_selection = Some(selection("codex", "gpt-5.4"));
        settings.project_settings_folded = false;
        let legacy = LegacyProjectSettingsFields {
            default_model_selection: Some(selection("claudeAgent", "claude-opus-5-5")),
            default_thread_env_mode: None,
        };
        let defaults = resolve_new_thread_defaults(&settings, &project(), Some(&legacy), &[]);
        assert_eq!(
            defaults.model_selection,
            json!({"instanceId": "claudeAgent", "model": "claude-opus-5-5"})
        );
    }

    #[test]
    fn with_nothing_configured_the_fallback_is_used_when_no_provider_is_ready() {
        let mut settings = default_server_settings();
        settings.default_model_selection = None;
        let defaults = resolve_new_thread_defaults(&settings, &project(), None, &[]);
        assert_eq!(
            defaults.model_selection,
            json!({"instanceId": "claudeAgent", "model": "claude-sonnet-4-6"})
        );
        assert_eq!(defaults.runtime_mode, "full-access");
    }
}
