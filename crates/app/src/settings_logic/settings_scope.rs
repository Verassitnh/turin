//! Port of upstream/apps/web/src/components/settings/settingsScope.ts.
//!
//! Two axes. `machine` narrows the environment axis (absent = all
//! environments); `project` and `checkout` narrow the project axis (absent =
//! environment defaults). Device-local preferences are not a scope: they render
//! regardless of the selection because they never touch a server.
//!
//! Stale ids resolve to an unavailable scope rather than widening to "all", so
//! a write can never reach more environments than the user selected.

#![allow(dead_code)] // Consumed by the Settings screens (T20).

use crate::state::contracts::EnvironmentId;
use crate::state::project_grouping::GroupableProject;
use crate::state::sidebar_project_grouping::{SidebarProjectGroupMember, SidebarProjectSnapshot};

/// Upstream `SettingsScopeSearch`: the three URL-backed selectors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsScopeSearch {
    pub project: Option<String>,
    pub machine: Option<String>,
    pub checkout: Option<String>,
}

/// Upstream `validateSettingsScopeSearch`: blank values are dropped. Unknown
/// keys (the retired `scope` key) are not representable here, so they are
/// ignored by construction.
pub fn validate_settings_scope_search(
    project: Option<&str>,
    machine: Option<&str>,
    checkout: Option<&str>,
) -> SettingsScopeSearch {
    let keep = |value: Option<&str>| {
        value
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    SettingsScopeSearch {
        project: keep(project),
        machine: keep(machine),
        checkout: keep(checkout),
    }
}

/// Upstream `Pick<EnvironmentPresentation, "environmentId" | "label">`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsScopeEnvironment {
    pub environment_id: EnvironmentId,
    pub label: String,
}

/// Why a selection could not be resolved (upstream `reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    ProjectRequired,
    ProjectMissing,
    EnvironmentMissing,
    CheckoutMissing,
}

/// The kind-specific part of [`ResolvedSettingsScope`].
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsScopeKind<P> {
    All,
    Environment {
        environment_id: EnvironmentId,
    },
    Project {
        group: SidebarProjectSnapshot<P>,
        environment_id: Option<EnvironmentId>,
    },
    Checkout {
        group: SidebarProjectSnapshot<P>,
        checkout: SidebarProjectGroupMember<P>,
        environment_id: EnvironmentId,
    },
    Unavailable {
        reason: UnavailableReason,
        message: String,
    },
}

/// Upstream `ResolvedSettingsScope`: the shared `label`, `members` and
/// `environmentIds`, plus the kind.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSettingsScope<P> {
    pub label: String,
    pub members: Vec<SidebarProjectGroupMember<P>>,
    pub environment_ids: Vec<EnvironmentId>,
    pub kind: SettingsScopeKind<P>,
}

fn unavailable<P>(reason: UnavailableReason, message: &str) -> ResolvedSettingsScope<P> {
    ResolvedSettingsScope {
        label: "Unavailable selection".to_owned(),
        members: Vec::new(),
        environment_ids: Vec::new(),
        kind: SettingsScopeKind::Unavailable {
            reason,
            message: message.to_owned(),
        },
    }
}

/// Upstream `resolveSettingsScope`. Resolves only existing targets.
pub fn resolve_settings_scope<P: GroupableProject + Clone>(
    search: &SettingsScopeSearch,
    groups: &[SidebarProjectSnapshot<P>],
    environments: &[SettingsScopeEnvironment],
) -> ResolvedSettingsScope<P> {
    if search.checkout.is_some() && search.project.is_none() {
        return unavailable(
            UnavailableReason::ProjectRequired,
            "Select a project to choose one of its checkouts.",
        );
    }

    let environment = search.machine.as_deref().and_then(|machine| {
        environments
            .iter()
            .find(|candidate| candidate.environment_id.as_str() == machine)
    });
    if search.machine.is_some() && environment.is_none() {
        return unavailable(
            UnavailableReason::EnvironmentMissing,
            "This environment is no longer available.",
        );
    }

    if let Some(project_key) = search.project.as_deref() {
        let Some(group) = groups
            .iter()
            .find(|candidate| candidate.project_key == project_key)
        else {
            return unavailable(
                UnavailableReason::ProjectMissing,
                "This project is no longer available.",
            );
        };
        let members: Vec<SidebarProjectGroupMember<P>> = group
            .member_projects
            .iter()
            .filter(|member| {
                search
                    .machine
                    .as_deref()
                    .is_none_or(|machine| member.project.environment_id().as_str() == machine)
                    && search
                        .checkout
                        .as_deref()
                        .is_none_or(|checkout| member.physical_project_key == checkout)
            })
            .cloned()
            .collect();
        if members.is_empty() {
            let message = if search.checkout.is_some() {
                "This checkout is no longer available in the selected project and environment."
            } else {
                "This project has no checkout on this environment."
            };
            return unavailable(UnavailableReason::CheckoutMissing, message);
        }

        if search.checkout.is_some() {
            let checkout = members[0].clone();
            let checkout_environment_id = checkout.project.environment_id().clone();
            let Some(checkout_environment) = environments
                .iter()
                .find(|candidate| candidate.environment_id == checkout_environment_id)
            else {
                return unavailable(
                    UnavailableReason::EnvironmentMissing,
                    "This checkout's environment is no longer available.",
                );
            };
            let shares_environment = group.member_projects.iter().any(|member| {
                member.project.environment_id() == &checkout_environment_id
                    && member.physical_project_key != checkout.physical_project_key
            });
            let suffix = if shares_environment {
                format!(" · {}", checkout.project.workspace_root())
            } else {
                String::new()
            };
            return ResolvedSettingsScope {
                label: format!(
                    "{} / {}{suffix}",
                    group.display_name, checkout_environment.label
                ),
                environment_ids: vec![checkout_environment_id.clone()],
                members: vec![checkout.clone()],
                kind: SettingsScopeKind::Checkout {
                    group: group.clone(),
                    checkout,
                    environment_id: checkout_environment_id,
                },
            };
        }

        let environment_ids = unique_environment_ids(&members);
        return ResolvedSettingsScope {
            label: format!(
                "{} / {}",
                group.display_name,
                environment.map_or("All checkouts", |environment| environment.label.as_str())
            ),
            environment_ids,
            members,
            kind: SettingsScopeKind::Project {
                group: group.clone(),
                environment_id: environment.map(|environment| environment.environment_id.clone()),
            },
        };
    }

    if let Some(environment) = environment {
        return ResolvedSettingsScope {
            label: environment.label.clone(),
            members: Vec::new(),
            environment_ids: vec![environment.environment_id.clone()],
            kind: SettingsScopeKind::Environment {
                environment_id: environment.environment_id.clone(),
            },
        };
    }

    ResolvedSettingsScope {
        label: "All environments".to_owned(),
        members: Vec::new(),
        environment_ids: environments
            .iter()
            .map(|candidate| candidate.environment_id.clone())
            .collect(),
        kind: SettingsScopeKind::All,
    }
}

/// `[...new Set(members.map(m => m.environmentId))]`: first occurrence order.
fn unique_environment_ids<P: GroupableProject>(
    members: &[SidebarProjectGroupMember<P>],
) -> Vec<EnvironmentId> {
    let mut ids: Vec<EnvironmentId> = Vec::new();
    for member in members {
        let id = member.project.environment_id();
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/components/settings/settingsScope.test.ts

    use super::*;
    use crate::state::contracts::{ProjectId, RepositoryIdentity};
    use crate::state::sidebar_project_grouping::EnvironmentPresence;

    #[derive(Clone, Debug, PartialEq)]
    struct Project {
        environment_id: EnvironmentId,
        id: ProjectId,
        workspace_root: String,
    }

    impl GroupableProject for Project {
        fn environment_id(&self) -> &EnvironmentId {
            &self.environment_id
        }
        fn id(&self) -> &ProjectId {
            &self.id
        }
        fn title(&self) -> &str {
            "T3 Code"
        }
        fn workspace_root(&self) -> &str {
            &self.workspace_root
        }
        fn repository_identity(&self) -> Option<&RepositoryIdentity> {
            None
        }
        fn is_scratch(&self) -> bool {
            false
        }
        fn updated_at(&self) -> Option<&str> {
            None
        }
        fn created_at(&self) -> Option<&str> {
            None
        }
    }

    type Member = SidebarProjectGroupMember<Project>;

    fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).expect("id")
    }

    fn member(id: &str, environment: &str) -> Member {
        let root = format!("/repos/{id}");
        let environment_id = env(environment);
        Member {
            physical_project_key: format!("{environment}:{root}"),
            environment_label: None,
            project: Project {
                id: ProjectId::new(id).expect("id"),
                environment_id,
                workspace_root: root,
            },
        }
    }

    fn group(project_key: &str, members: &[Member]) -> SidebarProjectSnapshot<Project> {
        SidebarProjectSnapshot {
            representative: members[0].project.clone(),
            project_key: project_key.to_owned(),
            display_name: project_key.to_owned(),
            grouped_project_count: members.len(),
            environment_presence: EnvironmentPresence::Mixed,
            member_projects: members.to_vec(),
            member_project_refs: Vec::new(),
            remote_environment_labels: Vec::new(),
        }
    }

    struct Fixture {
        laptop: EnvironmentId,
        server: EnvironmentId,
        first: Member,
        second: Member,
        third: Member,
        other: Member,
        groups: Vec<SidebarProjectSnapshot<Project>>,
        environments: Vec<SettingsScopeEnvironment>,
    }

    fn fixture() -> Fixture {
        let laptop = env("laptop");
        let server = env("server");
        let first = member("first", "laptop");
        let second = member("second", "laptop");
        let third = member("third", "server");
        let other = member("other", "server");
        Fixture {
            groups: vec![
                group("t3code", &[first.clone(), second.clone(), third.clone()]),
                group("other", std::slice::from_ref(&other)),
            ],
            environments: vec![
                SettingsScopeEnvironment {
                    environment_id: laptop.clone(),
                    label: "Laptop".to_owned(),
                },
                SettingsScopeEnvironment {
                    environment_id: server.clone(),
                    label: "Server".to_owned(),
                },
            ],
            laptop,
            server,
            first,
            second,
            third,
            other,
        }
    }

    fn search(
        project: Option<&str>,
        machine: Option<&str>,
        checkout: Option<&str>,
    ) -> SettingsScopeSearch {
        validate_settings_scope_search(project, machine, checkout)
    }

    #[test]
    fn ignores_blank_selectors() {
        assert_eq!(
            validate_settings_scope_search(Some("t3code"), Some("  "), Some("")),
            SettingsScopeSearch {
                project: Some("t3code".to_owned()),
                machine: None,
                checkout: None,
            }
        );
        assert_eq!(
            validate_settings_scope_search(None, None, None),
            SettingsScopeSearch::default()
        );
    }

    #[test]
    fn retains_legacy_project_and_machine_links() {
        let f = fixture();
        let laptop = f.laptop.as_str().to_owned();
        assert_eq!(
            validate_settings_scope_search(Some("t3code"), Some(laptop.as_str()), None),
            SettingsScopeSearch {
                project: Some("t3code".to_owned()),
                machine: Some(laptop),
                checkout: None,
            }
        );
    }

    #[test]
    fn retains_an_orphan_checkout_so_it_cannot_turn_into_all_environments() {
        let f = fixture();
        let key = f.first.physical_project_key.clone();
        let s = search(None, None, Some(key.as_str()));
        assert_eq!(s.checkout.as_deref(), Some(key.as_str()));
        let resolved = resolve_settings_scope(&s, &f.groups, &f.environments);
        assert!(matches!(
            resolved.kind,
            SettingsScopeKind::Unavailable {
                reason: UnavailableReason::ProjectRequired,
                ..
            }
        ));
        assert!(resolved.members.is_empty());
        assert!(resolved.environment_ids.is_empty());
    }

    #[test]
    fn defaults_to_every_environment_with_no_project() {
        let f = fixture();
        let resolved =
            resolve_settings_scope(&search(None, None, None), &f.groups, &f.environments);
        assert_eq!(resolved.kind, SettingsScopeKind::All);
        assert!(resolved.members.is_empty());
        assert_eq!(
            resolved.environment_ids,
            vec![f.laptop.clone(), f.server.clone()]
        );
    }

    #[test]
    fn resolves_one_environment_without_targeting_its_project_overrides() {
        let f = fixture();
        let server = f.server.as_str().to_owned();
        let resolved = resolve_settings_scope(
            &search(None, Some(server.as_str()), None),
            &f.groups,
            &f.environments,
        );
        assert_eq!(
            resolved.kind,
            SettingsScopeKind::Environment {
                environment_id: f.server.clone()
            }
        );
        assert_eq!(resolved.environment_ids, vec![f.server.clone()]);
        assert!(resolved.members.is_empty());
    }

    #[test]
    fn keeps_all_physical_members_in_a_project_aggregate() {
        let f = fixture();
        let resolved = resolve_settings_scope(
            &search(Some("t3code"), None, None),
            &f.groups,
            &f.environments,
        );
        assert!(matches!(
            resolved.kind,
            SettingsScopeKind::Project {
                environment_id: None,
                ..
            }
        ));
        assert_eq!(
            resolved.members,
            vec![f.first.clone(), f.second.clone(), f.third.clone()]
        );
        assert_eq!(
            resolved.environment_ids,
            vec![f.laptop.clone(), f.server.clone()]
        );
    }

    #[test]
    fn preserves_legacy_project_plus_machine_aggregates_with_multiple_checkouts() {
        let f = fixture();
        let laptop = f.laptop.as_str().to_owned();
        let resolved = resolve_settings_scope(
            &search(Some("t3code"), Some(laptop.as_str()), None),
            &f.groups,
            &f.environments,
        );
        assert_eq!(resolved.label, "t3code / Laptop");
        assert_eq!(
            resolved.kind,
            SettingsScopeKind::Project {
                group: f.groups[0].clone(),
                environment_id: Some(f.laptop.clone()),
            }
        );
        assert_eq!(resolved.members, vec![f.first.clone(), f.second.clone()]);
        assert_eq!(resolved.environment_ids, vec![f.laptop.clone()]);
    }

    #[test]
    fn narrows_a_checkout_to_one_member_and_derives_its_environment() {
        let f = fixture();
        let key = f.second.physical_project_key.clone();
        let resolved = resolve_settings_scope(
            &search(Some("t3code"), None, Some(key.as_str())),
            &f.groups,
            &f.environments,
        );
        assert_eq!(resolved.label, "t3code / Laptop · /repos/second");
        assert_eq!(resolved.members, vec![f.second.clone()]);
        assert_eq!(resolved.environment_ids, vec![f.laptop.clone()]);
        assert!(matches!(
            resolved.kind,
            SettingsScopeKind::Checkout { ref checkout, ref environment_id, .. }
                if *checkout == f.second && *environment_id == f.laptop
        ));
    }

    #[test]
    fn never_widens_an_invalid_or_stale_target() {
        let f = fixture();
        let laptop = f.laptop.as_str().to_owned();
        let server = f.server.as_str().to_owned();
        let first_key = f.first.physical_project_key.clone();
        let cases = [
            search(Some("missing"), None, None),
            search(None, Some("removed"), None),
            search(Some("t3code"), Some("removed"), None),
            search(Some("t3code"), None, Some("deleted")),
            search(Some("other"), Some(laptop.as_str()), None),
            search(Some("other"), None, Some(first_key.as_str())),
            search(
                Some("t3code"),
                Some(server.as_str()),
                Some(first_key.as_str()),
            ),
        ];
        for case in cases {
            let resolved = resolve_settings_scope(&case, &f.groups, &f.environments);
            assert!(
                matches!(resolved.kind, SettingsScopeKind::Unavailable { .. }),
                "{case:?} resolved to {:?}",
                resolved.kind
            );
            assert!(resolved.members.is_empty(), "{case:?}");
            assert!(resolved.environment_ids.is_empty(), "{case:?}");
        }
    }

    #[test]
    fn leaves_a_removed_checkout_unavailable_while_siblings_remain() {
        let f = fixture();
        let laptop = f.laptop.as_str().to_owned();
        let key = f.first.physical_project_key.clone();
        let s = search(Some("t3code"), Some(laptop.as_str()), Some(key.as_str()));
        let resolved = resolve_settings_scope(&s, &f.groups, &f.environments);
        assert!(matches!(resolved.kind, SettingsScopeKind::Checkout { .. }));
        assert_eq!(resolved.members, vec![f.first.clone()]);

        let reduced = [group("t3code", &[f.second.clone(), f.third.clone()])];
        let resolved = resolve_settings_scope(&s, &reduced, &f.environments);
        assert!(matches!(
            resolved.kind,
            SettingsScopeKind::Unavailable { .. }
        ));
        assert!(resolved.members.is_empty());
        assert!(resolved.environment_ids.is_empty());
    }

    #[test]
    fn does_not_select_another_environment_after_removing_a_projects_last_local_checkout() {
        let f = fixture();
        let laptop = f.laptop.as_str().to_owned();
        let s = search(Some("t3code"), Some(laptop.as_str()), None);
        let resolved = resolve_settings_scope(&s, &f.groups, &f.environments);
        assert_eq!(resolved.members, vec![f.first.clone(), f.second.clone()]);

        let remote_only = [group("t3code", std::slice::from_ref(&f.third))];
        let resolved = resolve_settings_scope(&s, &remote_only, &f.environments);
        assert!(matches!(
            resolved.kind,
            SettingsScopeKind::Unavailable { .. }
        ));
        assert!(resolved.members.is_empty());
        assert!(resolved.environment_ids.is_empty());
    }

    #[test]
    fn rejects_a_cached_checkout_whose_environment_was_removed() {
        let f = fixture();
        let key = f.third.physical_project_key.clone();
        let s = search(Some("t3code"), None, Some(key.as_str()));
        let only_laptop = &f.environments[..1];
        let resolved = resolve_settings_scope(&s, &f.groups, only_laptop);
        assert!(matches!(
            resolved.kind,
            SettingsScopeKind::Unavailable {
                reason: UnavailableReason::EnvironmentMissing,
                ..
            }
        ));
        assert!(resolved.members.is_empty());
        assert!(resolved.environment_ids.is_empty());
        // `other` is referenced only to keep the fixture symmetric with upstream.
        assert_eq!(f.other.project.id.as_str(), "other");
    }
}
