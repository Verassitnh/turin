//! Port of upstream/apps/web/src/components/settings/ProviderSettingsPanel.logic.ts
//! (and its test file), reduced to what survives a single local environment.
//!
//! Not ported: `buildProviderEnvironmentOptions`,
//! `resolveSelectedProviderEnvironmentId`, `classifyProviderEnvironmentAccess`,
//! `resolve{Primary,Remote}OperateAccess` and `isProviderSettingsEnvironmentAvailable`.
//! They pick between remote environments and check auth-session scopes
//! (`AuthProvidersManageScope`); remote access and auth are cut, and the one
//! local environment is always editable. See PORTING.md Deviations.

use indexmap::IndexMap;
use serde_json::{Map, Value};
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::settings::ServerSettings;

/// The patch for one instance's project override, following the envelope
/// replacement rule for `projectSettingsOverrides`: the returned entry
/// replaces the project's whole override set, and `None` removes it (`null`).
/// `value: None` resets that single instance back to inheriting the
/// environment's enablement, leaving any other overridden keys untouched.
///
/// `providerInstances` is always sent explicitly, even as `{}`, because the
/// server keeps a stored `providerInstances` map when an incoming entry omits
/// the key (so older clients that don't know the field cannot wipe it by
/// editing another setting). Omitting it here would undo the very clear this
/// function means to make.
///
/// Every checkout of a project on one environment shares the same choice.
/// Entries are raw JSON objects (the wire shape of `ProjectSettingsOverrides`).
pub fn build_provider_instance_override_patch(
    settings: &ServerSettings,
    project_ids: &[ProjectId],
    instance_id: &ProviderInstanceId,
    value: Option<bool>,
) -> IndexMap<ProjectId, Option<Value>> {
    let overrides = serde_json::to_value(&settings.project_settings_overrides)
        .unwrap_or(Value::Object(Map::new()));
    let mut patch = IndexMap::new();
    for project_id in project_ids {
        let current = overrides
            .get(project_id.as_str())
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut instances = current
            .get("providerInstances")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut instance = instances
            .get(instance_id.as_str())
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        match value {
            None => {
                instance.remove("enabled");
            }
            Some(enabled) => {
                instance.insert("enabled".into(), Value::Bool(enabled));
            }
        }
        if instance.is_empty() {
            instances.remove(instance_id.as_str());
        } else {
            instances.insert(instance_id.as_str().to_owned(), Value::Object(instance));
        }

        let mut rest = current;
        rest.remove("providerInstances");
        let rest_is_empty = rest.is_empty();
        let instances_is_empty = instances.is_empty();
        let mut next_entry = rest;
        next_entry.insert("providerInstances".into(), Value::Object(instances));
        patch.insert(
            project_id.clone(),
            if rest_is_empty && instances_is_empty {
                None
            } else {
                Some(Value::Object(next_entry))
            },
        );
    }
    patch
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::settings::default_server_settings;

    use super::*;

    fn settings(overrides: Value) -> ServerSettings {
        let mut settings = default_server_settings();
        settings.project_settings_overrides =
            serde_json::from_value(overrides).expect("overrides decode");
        settings
    }

    fn project(id: &str) -> ProjectId {
        ProjectId::new(id).unwrap()
    }

    fn instance() -> ProviderInstanceId {
        ProviderInstanceId::new("codex_work").unwrap()
    }

    fn entry(project: &str, value: Value) -> (ProjectId, Option<Value>) {
        (self::project(project), Some(value))
    }

    #[test]
    fn writes_the_same_choice_to_every_checkout_of_the_project() {
        let settings =
            settings(json!({ "project-a-worktree": { "newWorktreesStartFromOrigin": true } }));
        let patch = build_provider_instance_override_patch(
            &settings,
            &[project("project-a"), project("project-a-worktree")],
            &instance(),
            Some(false),
        );
        assert_eq!(
            patch,
            IndexMap::from([
                entry(
                    "project-a",
                    json!({ "providerInstances": { "codex_work": { "enabled": false } } })
                ),
                entry(
                    "project-a-worktree",
                    json!({
                        "newWorktreesStartFromOrigin": true,
                        "providerInstances": { "codex_work": { "enabled": false } }
                    })
                ),
            ])
        );
    }

    #[test]
    fn sets_an_override_when_the_project_has_none_yet() {
        let patch = build_provider_instance_override_patch(
            &settings(json!({})),
            &[project("project-a")],
            &instance(),
            Some(true),
        );
        assert_eq!(
            patch,
            IndexMap::from([entry(
                "project-a",
                json!({ "providerInstances": { "codex_work": { "enabled": true } } })
            )])
        );
    }

    #[test]
    fn adds_an_instance_override_alongside_the_projects_other_overrides() {
        let settings = settings(json!({
            "project-a": {
                "newWorktreesStartFromOrigin": true,
                "providerInstances": { "claudeAgent_personal": { "enabled": false } }
            }
        }));
        let patch = build_provider_instance_override_patch(
            &settings,
            &[project("project-a")],
            &instance(),
            Some(true),
        );
        assert_eq!(
            patch,
            IndexMap::from([entry(
                "project-a",
                json!({
                    "newWorktreesStartFromOrigin": true,
                    "providerInstances": {
                        "claudeAgent_personal": { "enabled": false },
                        "codex_work": { "enabled": true }
                    }
                })
            )])
        );
    }

    #[test]
    fn resets_the_only_override_by_clearing_the_whole_project_entry() {
        let settings = settings(json!({
            "project-a": { "providerInstances": { "codex_work": { "enabled": true } } }
        }));
        let patch = build_provider_instance_override_patch(
            &settings,
            &[project("project-a")],
            &instance(),
            None,
        );
        assert_eq!(patch, IndexMap::from([(project("project-a"), None)]));
    }

    #[test]
    fn resets_one_instance_while_another_instances_override_and_other_keys_survive() {
        let settings = settings(json!({
            "project-a": {
                "newWorktreesStartFromOrigin": true,
                "providerInstances": {
                    "codex_work": { "enabled": true },
                    "claudeAgent_personal": { "enabled": false }
                }
            }
        }));
        let patch = build_provider_instance_override_patch(
            &settings,
            &[project("project-a")],
            &instance(),
            None,
        );
        assert_eq!(
            patch,
            IndexMap::from([entry(
                "project-a",
                json!({
                    "newWorktreesStartFromOrigin": true,
                    "providerInstances": { "claudeAgent_personal": { "enabled": false } }
                })
            )])
        );
    }

    #[test]
    fn sends_provider_instances_empty_explicitly_when_clearing_the_last_instance_override_leaves_other_keys()
     {
        // The server keeps a stored providerInstances map when an incoming entry
        // omits the key (so older clients cannot wipe it by editing another
        // setting). This clear must say so explicitly, or the server would put
        // the just-cleared override right back.
        let settings = settings(json!({
            "project-a": {
                "newWorktreesStartFromOrigin": true,
                "providerInstances": { "codex_work": { "enabled": true } }
            }
        }));
        let patch = build_provider_instance_override_patch(
            &settings,
            &[project("project-a")],
            &instance(),
            None,
        );
        assert_eq!(
            patch,
            IndexMap::from([entry(
                "project-a",
                json!({ "newWorktreesStartFromOrigin": true, "providerInstances": {} })
            )])
        );
    }
}
