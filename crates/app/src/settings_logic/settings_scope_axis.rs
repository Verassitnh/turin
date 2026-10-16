//! Port of upstream/apps/web/src/components/settings/settingsScopeAxis.ts

#![allow(dead_code)]

use crate::state::contracts::EnvironmentId;
use crate::state::helpers::usage_access::EnvironmentConnectionPhase;

use super::settings_scope::{ResolvedSettingsScope, SettingsScopeKind, SettingsScopeSearch};

pub const ALL_ENVIRONMENTS_VALUE: &str = "all";
pub const ALL_PROJECTS_VALUE: &str = "all";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeEnvironment {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub display_url: Option<String>,
}

pub fn settings_scope_environment_label(
    environment: &ScopeEnvironment,
    environments: &[ScopeEnvironment],
) -> String {
    let duplicate = environments.iter().any(|other| {
        other.environment_id != environment.environment_id && other.label == environment.label
    });
    if !duplicate {
        return environment.label.clone();
    }
    if let Some(url) = &environment.display_url {
        format!("{} · {url}", environment.label)
    } else {
        format!(
            "{} · {}",
            environment.label,
            environment.environment_id.as_str()
        )
    }
}

pub fn environment_axis_value(
    search: &SettingsScopeSearch,
    resolved_environment_id: Option<&str>,
) -> String {
    search
        .machine
        .clone()
        .or_else(|| resolved_environment_id.map(str::to_owned))
        .unwrap_or_else(|| ALL_ENVIRONMENTS_VALUE.to_owned())
}

pub fn project_axis_value(search: &SettingsScopeSearch) -> String {
    search
        .project
        .clone()
        .unwrap_or_else(|| ALL_PROJECTS_VALUE.to_owned())
}

pub fn select_environment_axis(search: &SettingsScopeSearch, value: &str) -> SettingsScopeSearch {
    let mut next = SettingsScopeSearch::default();
    if let Some(project) = &search.project {
        next.project = Some(project.clone());
    }
    if value != ALL_ENVIRONMENTS_VALUE {
        next.machine = Some(value.to_owned());
    }
    next
}

pub fn select_project_axis(search: &SettingsScopeSearch, value: &str) -> SettingsScopeSearch {
    let mut next = SettingsScopeSearch::default();
    if value != ALL_PROJECTS_VALUE {
        next.project = Some(value.to_owned());
    }
    if let Some(machine) = &search.machine {
        next.machine = Some(machine.clone());
    }
    next
}

#[derive(Clone, Debug)]
pub struct SelectSingleEnvironmentCandidate {
    pub environment_id: EnvironmentId,
    pub connection_phase: EnvironmentConnectionPhase,
}

pub fn select_single_environment_scope<P>(
    search: &SettingsScopeSearch,
    scope: &ResolvedSettingsScope<P>,
    environments: &[SelectSingleEnvironmentCandidate],
    primary_environment_id: Option<&EnvironmentId>,
) -> SettingsScopeSearch {
    if search.machine.is_some() {
        return search.clone();
    }
    if matches!(scope.kind, SettingsScopeKind::Unavailable { .. }) {
        return search.clone();
    }
    let selected_ids: std::collections::HashSet<_> =
        scope.environment_ids.iter().cloned().collect();
    let candidates: Vec<_> = environments
        .iter()
        .filter(|environment| selected_ids.contains(&environment.environment_id))
        .collect();
    let selected = primary_environment_id
        .and_then(|id| candidates.iter().find(|e| e.environment_id == *id).copied())
        .or_else(|| {
            candidates
                .iter()
                .find(|e| e.connection_phase == EnvironmentConnectionPhase::Connected)
                .copied()
        })
        .or_else(|| candidates.first().copied());
    let Some(selected) = selected else {
        return search.clone();
    };
    SettingsScopeSearch {
        machine: Some(selected.environment_id.as_str().to_owned()),
        ..search.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::EnvironmentId;

    fn env(id: &str, label: &str, display_url: Option<&str>) -> ScopeEnvironment {
        ScopeEnvironment {
            environment_id: EnvironmentId::new(id).expect("id"),
            label: label.to_owned(),
            display_url: display_url.map(str::to_owned),
        }
    }

    #[test]
    fn distinguishes_same_name_environments_by_address() {
        let environments = vec![
            env("first", "Development", Some("https://first.example.com")),
            env("second", "Development", Some("https://second.example.com")),
        ];
        let labels: Vec<_> = environments
            .iter()
            .map(|environment| settings_scope_environment_label(environment, &environments))
            .collect();
        assert_eq!(
            labels,
            vec![
                "Development · https://first.example.com",
                "Development · https://second.example.com",
            ]
        );
    }

    #[test]
    fn falls_back_to_environment_ids_when_duplicate_names_have_no_display_url() {
        let environments = vec![
            env("first", "Development", None),
            env("second", "Development", None),
        ];
        let labels: Vec<_> = environments
            .iter()
            .map(|environment| settings_scope_environment_label(environment, &environments))
            .collect();
        assert_eq!(labels, vec!["Development · first", "Development · second"]);
    }

    #[test]
    fn keeps_unique_names_compact_and_removes_disambiguation_after_rename() {
        let first = env("first", "Development", Some("https://first.example.com"));
        assert_eq!(
            settings_scope_environment_label(&first, std::slice::from_ref(&first)),
            "Development"
        );
        let second = env("second", "Production", Some("https://second.example.com"));
        assert_eq!(
            settings_scope_environment_label(&first, &[first.clone(), second]),
            "Development"
        );
    }

    #[test]
    fn maps_each_axis_to_its_search_key_and_back() {
        assert_eq!(project_axis_value(&SettingsScopeSearch::default()), "all");
        assert_eq!(
            project_axis_value(&SettingsScopeSearch {
                project: Some("app".into()),
                ..Default::default()
            }),
            "app"
        );
        assert_eq!(
            select_project_axis(
                &SettingsScopeSearch {
                    machine: Some("second".into()),
                    ..Default::default()
                },
                "app"
            ),
            SettingsScopeSearch {
                project: Some("app".into()),
                machine: Some("second".into()),
                checkout: None,
            }
        );
        assert_eq!(
            select_project_axis(
                &SettingsScopeSearch {
                    machine: Some("second".into()),
                    project: Some("app".into()),
                    ..Default::default()
                },
                "all"
            ),
            SettingsScopeSearch {
                machine: Some("second".into()),
                ..Default::default()
            }
        );
        assert_eq!(
            select_environment_axis(
                &SettingsScopeSearch {
                    project: Some("app".into()),
                    ..Default::default()
                },
                "first"
            ),
            SettingsScopeSearch {
                project: Some("app".into()),
                machine: Some("first".into()),
                checkout: None,
            }
        );
        assert_eq!(
            select_environment_axis(
                &SettingsScopeSearch {
                    project: Some("app".into()),
                    machine: Some("first".into()),
                    ..Default::default()
                },
                "all"
            ),
            SettingsScopeSearch {
                project: Some("app".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn drops_checkout_narrowing_when_either_axis_changes() {
        let checkout = SettingsScopeSearch {
            project: Some("app".into()),
            checkout: Some("app@first".into()),
            machine: Some("first".into()),
        };
        assert_eq!(
            select_environment_axis(&checkout, "second"),
            SettingsScopeSearch {
                project: Some("app".into()),
                machine: Some("second".into()),
                checkout: None,
            }
        );
        assert_eq!(
            select_project_axis(&checkout, "app"),
            SettingsScopeSearch {
                project: Some("app".into()),
                machine: Some("first".into()),
                checkout: None,
            }
        );
    }

    #[test]
    fn environment_axis_value_shows_checkout_environment_for_legacy_links() {
        assert_eq!(
            environment_axis_value(
                &SettingsScopeSearch {
                    project: Some("p".into()),
                    checkout: Some("c".into()),
                    ..Default::default()
                },
                Some("laptop")
            ),
            "laptop"
        );
        assert_eq!(
            environment_axis_value(
                &SettingsScopeSearch {
                    project: Some("p".into()),
                    ..Default::default()
                },
                None
            ),
            "all"
        );
        assert_eq!(
            environment_axis_value(
                &SettingsScopeSearch {
                    machine: Some("desk".into()),
                    ..Default::default()
                },
                Some("laptop")
            ),
            "desk"
        );
    }
}
