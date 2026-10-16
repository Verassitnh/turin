//! Port of upstream/apps/web/src/components/settings/settingsScopeNavigation.ts
//!
//! TanStack Router middleware is modeled as pure search merging for GPUI routes.

#![allow(dead_code)]

use indexmap::IndexMap;
use serde_json::Value;

use super::settings_scope::{SettingsScopeSearch, validate_settings_scope_search};

const TARGET_INPUT_KEYS: [&str; 4] = ["project", "machine", "checkout", "environmentId"];

/// Accept legacy provider links without replacing an explicit settings scope.
pub fn validate_settings_route_search(raw: &IndexMap<String, Value>) -> SettingsScopeSearch {
    let legacy_machine = raw
        .get("environmentId")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.trim().is_empty() && !raw.contains_key("machine") && !raw.contains_key("project")
        });
    if let Some(machine) = legacy_machine {
        return validate_settings_scope_search(None, Some(machine), None);
    }
    validate_settings_scope_search(
        raw.get("project").and_then(Value::as_str),
        raw.get("machine").and_then(Value::as_str),
        raw.get("checkout").and_then(Value::as_str),
    )
}

/// Upstream `retainSettingsScope` without the router: `next_own_keys` lists keys
/// explicitly present on the incoming search update (including clears).
pub fn retain_settings_scope(
    previous: &SettingsScopeSearch,
    next: SettingsScopeSearch,
    next_own_keys: &[&str],
) -> SettingsScopeSearch {
    if next_own_keys
        .iter()
        .any(|key| TARGET_INPUT_KEYS.contains(key))
    {
        return next;
    }
    let mut merged = SettingsScopeSearch::default();
    if previous.project.is_some() {
        merged.project = previous.project.clone();
    }
    if previous.machine.is_some() {
        merged.machine = previous.machine.clone();
    }
    if previous.checkout.is_some() {
        merged.checkout = previous.checkout.clone();
    }
    if next.project.is_some() {
        merged.project = next.project;
    }
    if next.machine.is_some() {
        merged.machine = next.machine;
    }
    if next.checkout.is_some() {
        merged.checkout = next.checkout;
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use serde_json::json;

    fn map(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn maps_legacy_environment_id_to_machine() {
        assert_eq!(
            validate_settings_route_search(&map(&[
                ("environmentId", json!("provider-server")),
                ("instanceId", json!("codex-work")),
            ])),
            SettingsScopeSearch {
                machine: Some("provider-server".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn does_not_override_explicit_scope_with_legacy_environment_id() {
        assert_eq!(
            validate_settings_route_search(&map(&[
                ("environmentId", json!("provider-server")),
                ("project", json!("app")),
            ])),
            SettingsScopeSearch {
                project: Some("app".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn replaces_scope_when_navigation_sets_an_explicit_target() {
        let previous = SettingsScopeSearch {
            machine: Some("online".into()),
            ..Default::default()
        };
        let next = SettingsScopeSearch {
            machine: Some("removed".into()),
            ..Default::default()
        };
        assert_eq!(
            retain_settings_scope(&previous, next.clone(), &["machine"]),
            next
        );
    }

    #[test]
    fn preserves_checkout_when_navigating_categories_without_target_keys() {
        let checkout = SettingsScopeSearch {
            project: Some("repository:t3code".into()),
            machine: Some("remote-server".into()),
            checkout: Some("remote-server:/home/user/T3 Code".into()),
        };
        assert_eq!(
            retain_settings_scope(&checkout, SettingsScopeSearch::default(), &[]),
            checkout
        );
    }

    #[test]
    fn respects_explicit_clearing_instead_of_restoring_previous_checkout() {
        let previous = SettingsScopeSearch {
            project: Some("repository:t3code".into()),
            machine: Some("remote-server".into()),
            checkout: Some("remote-server:/home/user/T3 Code".into()),
        };
        let next = SettingsScopeSearch {
            project: Some("different-project".into()),
            ..Default::default()
        };
        assert_eq!(
            retain_settings_scope(&previous, next.clone(), &["project", "machine"]),
            next
        );
    }

    #[test]
    fn clears_scope_when_all_axes_are_explicitly_cleared() {
        let previous = SettingsScopeSearch {
            project: Some("repository:t3code".into()),
            machine: Some("remote-server".into()),
            checkout: Some("remote-server:/home/user/T3 Code".into()),
        };
        let next = SettingsScopeSearch::default();
        assert_eq!(
            retain_settings_scope(&previous, next, &["project", "machine", "checkout"]),
            SettingsScopeSearch::default()
        );
    }
}
