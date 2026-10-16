//! Port of upstream/apps/web/src/sidebarProjectGrouping.ts
//!
//! The sidebar's snapshot layer over the logical grouping in
//! `state/project_grouping.rs` (`buildProjectGroups`, `derivePhysicalProjectKey`,
//! `resolveScratchProjectScopeKey`): group snapshots with their members and
//! environment presence, the scope key a filter resolves to, the settings key
//! the project menu opens, and the project picker's entry order.
//!
//! Upstream types these structurally over `Project`; here they take any
//! [`GroupableProject`], and a snapshot keeps the representative project as a
//! field instead of spreading it.
// Seams for consumers the shell wires (T22); nothing calls these yet.
#![allow(dead_code)]

use std::collections::HashMap;

use crate::state::contracts::{EnvironmentId, ScopedProjectRef};
use crate::state::project_grouping::{
    BuildProjectGroupsInput, GroupableProject, ProjectGroupingSettings, build_project_groups,
    derive_physical_project_key, resolve_scratch_project_scope_key,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentPresence {
    LocalOnly,
    RemoteOnly,
    Mixed,
}

/// `SidebarProjectGroupMember`: a physical project plus the sidebar's label for
/// its environment.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarProjectGroupMember<P> {
    pub project: P,
    pub physical_project_key: String,
    pub environment_label: Option<String>,
}

/// `SidebarProjectSnapshot`: one logical project row with its members.
/// `allRemoteMembersAreDesktopLocal` and `allRemoteMembersAreWsl` are not
/// ported (see PORTING.md Deviations).
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarProjectSnapshot<P> {
    pub representative: P,
    pub project_key: String,
    pub display_name: String,
    pub grouped_project_count: usize,
    pub environment_presence: EnvironmentPresence,
    pub member_projects: Vec<SidebarProjectGroupMember<P>>,
    pub member_project_refs: Vec<ScopedProjectRef>,
    pub remote_environment_labels: Vec<String>,
}

/// `SidebarProjectPickerEntry`.
#[derive(Debug)]
pub struct SidebarProjectPickerEntry<'a, P> {
    pub group: &'a SidebarProjectSnapshot<P>,
    pub target_project: &'a SidebarProjectGroupMember<P>,
    pub is_preferred: bool,
}

/// `projectGroupsSpanEnvironments`.
pub fn project_groups_span_environments<P: GroupableProject>(
    groups: &[SidebarProjectSnapshot<P>],
) -> bool {
    let mut first: Option<&EnvironmentId> = None;
    for member in groups.iter().flat_map(|group| &group.member_projects) {
        let environment_id = member.project.environment_id();
        match first {
            None => first = Some(environment_id),
            Some(first) if first != environment_id => return true,
            Some(_) => {}
        }
    }
    false
}

/// `getSidebarProjectSettingsKey`. A merged filter still opens settings for
/// the chosen physical project.
pub fn get_sidebar_project_settings_key<P: GroupableProject + Clone>(
    group: &SidebarProjectSnapshot<P>,
    settings: &ProjectGroupingSettings,
    projects: &[P],
    project_ref: Option<&ScopedProjectRef>,
) -> String {
    if !group.representative.is_scratch() {
        return group.project_key.clone();
    }
    let member_for = |matches: &dyn Fn(&P) -> bool| {
        group
            .member_projects
            .iter()
            .find(|member| matches(&member.project))
            .map(|member| &member.project)
    };
    let project = project_ref
        .and_then(|reference| {
            member_for(&|project: &P| {
                project.environment_id() == &reference.environment_id
                    && project.id() == &reference.project_id
            })
            .or_else(|| {
                member_for(&|project: &P| project.environment_id() == &reference.environment_id)
            })
        })
        .unwrap_or(&group.representative);
    build_physical_to_logical_project_key_map(projects, settings, None)
        .get(&derive_physical_project_key(project))
        .cloned()
        .unwrap_or_else(|| group.project_key.clone())
}

/// `resolveSidebarProjectScopeKey`. Keeps a saved physical scratch scope when
/// its environment config coalesces the group. `None` settings means the
/// upstream default (repository grouping).
pub fn resolve_sidebar_project_scope_key<P: GroupableProject + Clone>(
    groups: &[SidebarProjectSnapshot<P>],
    key: Option<&str>,
    settings: Option<&ProjectGroupingSettings>,
    can_clear_missing_scope: bool,
) -> Option<String> {
    let key = key?;
    let default_settings = ProjectGroupingSettings::default();
    let settings = settings.unwrap_or(&default_settings);
    let group = groups
        .iter()
        .find(|candidate| candidate.project_key == key)
        .or_else(|| {
            groups.iter().find(|candidate| {
                if !candidate.representative.is_scratch() {
                    return false;
                }
                let member_projects: Vec<P> = candidate
                    .member_projects
                    .iter()
                    .map(|member| member.project.clone())
                    .collect();
                resolve_scratch_project_scope_key(Some(key), &member_projects, settings).as_deref()
                    == Some(candidate.project_key.as_str())
            })
        });
    match group {
        Some(group) => Some(group.project_key.clone()),
        None if can_clear_missing_scope => None,
        None => Some(key.to_owned()),
    }
}

/// `buildPhysicalToLogicalProjectKeyMap`: physical project key → logical key.
pub fn build_physical_to_logical_project_key_map<P: GroupableProject + Clone>(
    projects: &[P],
    settings: &ProjectGroupingSettings,
    primary_environment_id: Option<&EnvironmentId>,
) -> HashMap<String, String> {
    let groups = build_project_groups(
        projects,
        &BuildProjectGroupsInput {
            settings: settings.clone(),
            preferred_environment_id: primary_environment_id,
            group_scratch_projects: false,
        },
    );
    let mut mapping = HashMap::new();
    for group in &groups {
        for member in &group.members {
            mapping.insert(member.physical_project_key.clone(), group.key.clone());
        }
    }
    mapping
}

/// A member lives outside the primary environment. With no primary, nothing is remote.
fn is_remote_member<P: GroupableProject>(
    member: &SidebarProjectGroupMember<P>,
    primary_environment_id: Option<&EnvironmentId>,
) -> bool {
    primary_environment_id.is_some_and(|primary| member.project.environment_id() != primary)
}

/// `environmentPresence`: local when every member is on the primary, remote when none is.
pub fn environment_presence<P: GroupableProject>(
    members: &[SidebarProjectGroupMember<P>],
    primary_environment_id: Option<&EnvironmentId>,
) -> EnvironmentPresence {
    let has_local = primary_environment_id.is_some_and(|primary| {
        members
            .iter()
            .any(|member| member.project.environment_id() == primary)
    });
    let has_remote = members
        .iter()
        .any(|member| is_remote_member(member, primary_environment_id));
    match (has_local, has_remote) {
        (true, true) => EnvironmentPresence::Mixed,
        (false, true) => EnvironmentPresence::RemoteOnly,
        _ => EnvironmentPresence::LocalOnly,
    }
}

/// `buildSidebarProjectSnapshots`.
pub fn build_sidebar_project_snapshots<P: GroupableProject + Clone>(
    projects: &[P],
    settings: &ProjectGroupingSettings,
    group_scratch_projects: bool,
    primary_environment_id: Option<&EnvironmentId>,
    resolve_environment_label: impl Fn(&EnvironmentId) -> Option<String>,
) -> Vec<SidebarProjectSnapshot<P>> {
    let groups = build_project_groups(
        projects,
        &BuildProjectGroupsInput {
            settings: settings.clone(),
            preferred_environment_id: primary_environment_id,
            group_scratch_projects,
        },
    );
    groups
        .into_iter()
        .map(|group| {
            let members: Vec<SidebarProjectGroupMember<P>> = group
                .members
                .iter()
                .map(|member| SidebarProjectGroupMember {
                    project: member.project.clone(),
                    physical_project_key: member.physical_project_key.clone(),
                    environment_label: resolve_environment_label(member.project.environment_id()),
                })
                .collect();

            let remote_members: Vec<&SidebarProjectGroupMember<P>> = members
                .iter()
                .filter(|member| is_remote_member(member, primary_environment_id))
                .collect();
            let mut remote_environment_labels: Vec<String> = Vec::new();
            for label in remote_members
                .iter()
                .filter_map(|member| member.environment_label.as_ref())
            {
                if !remote_environment_labels.contains(label) {
                    remote_environment_labels.push(label.clone());
                }
            }
            let environment_presence = environment_presence(&members, primary_environment_id);

            SidebarProjectSnapshot {
                representative: group.representative,
                project_key: group.key,
                display_name: group.label,
                grouped_project_count: members.len(),
                environment_presence,
                member_projects: members,
                member_project_refs: group.member_project_refs,
                remote_environment_labels,
            }
        })
        .collect()
}

/// `buildSidebarProjectPickerEntries`. The preferred project's group moves to
/// the front; the rest keep their order.
pub fn build_sidebar_project_picker_entries<'a, P: GroupableProject>(
    groups: &'a [SidebarProjectSnapshot<P>],
    preferred_project_ref: Option<&ScopedProjectRef>,
) -> Vec<SidebarProjectPickerEntry<'a, P>> {
    let mut entries: Vec<SidebarProjectPickerEntry<'a, P>> = Vec::new();
    for group in groups {
        let is_preferred = preferred_project_ref.is_some_and(|preferred| {
            group.member_project_refs.iter().any(|reference| {
                reference.environment_id == preferred.environment_id
                    && reference.project_id == preferred.project_id
            })
        });
        let preferred_project = preferred_project_ref.and_then(|preferred| {
            group
                .member_projects
                .iter()
                .find(|member| {
                    member.project.environment_id() == &preferred.environment_id
                        && member.project.id() == &preferred.project_id
                })
                .or_else(|| {
                    group
                        .member_projects
                        .iter()
                        .find(|member| member.project.environment_id() == &preferred.environment_id)
                })
        });
        let target_project = preferred_project
            .or_else(|| {
                group.member_projects.iter().find(|member| {
                    member.project.environment_id() == group.representative.environment_id()
                        && member.project.id() == group.representative.id()
                })
            })
            .or_else(|| group.member_projects.first());
        let Some(target_project) = target_project else {
            continue;
        };
        entries.push(SidebarProjectPickerEntry {
            group,
            target_project,
            is_preferred,
        });
    }

    match entries.iter().position(|entry| entry.is_preferred) {
        Some(preferred_index) if preferred_index > 0 => {
            let preferred = entries.remove(preferred_index);
            entries.insert(0, preferred);
            entries
        }
        _ => entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::{ProjectId, RepositoryIdentity};

    /// Minimal `GroupableProject` for these tests.
    #[derive(Clone, Debug, PartialEq)]
    struct TestProject {
        environment_id: EnvironmentId,
        id: ProjectId,
        title: String,
        workspace_root: String,
        is_scratch: bool,
    }

    impl GroupableProject for TestProject {
        fn environment_id(&self) -> &EnvironmentId {
            &self.environment_id
        }
        fn id(&self) -> &ProjectId {
            &self.id
        }
        fn title(&self) -> &str {
            &self.title
        }
        fn workspace_root(&self) -> &str {
            &self.workspace_root
        }
        fn repository_identity(&self) -> Option<&RepositoryIdentity> {
            None
        }
        fn is_scratch(&self) -> bool {
            self.is_scratch
        }
        fn updated_at(&self) -> Option<&str> {
            None
        }
        fn created_at(&self) -> Option<&str> {
            None
        }
    }

    fn project(environment: &str, id: &str, root: &str) -> TestProject {
        TestProject {
            environment_id: EnvironmentId::new(environment).expect("id"),
            id: ProjectId::new(id).expect("id"),
            title: id.to_owned(),
            workspace_root: root.to_owned(),
            is_scratch: false,
        }
    }

    fn label(environment_id: &EnvironmentId) -> Option<String> {
        Some(format!("env {environment_id}"))
    }

    fn snapshots(
        projects: &[TestProject],
        primary: Option<&EnvironmentId>,
    ) -> Vec<SidebarProjectSnapshot<TestProject>> {
        build_sidebar_project_snapshots(
            projects,
            &ProjectGroupingSettings::default(),
            false,
            primary,
            label,
        )
    }

    #[test]
    fn groups_a_single_environment_project_with_its_label() {
        let primary = EnvironmentId::new("local").expect("id");
        let projects = [project("local", "p1", "/work/app")];
        let groups = snapshots(&projects, Some(&primary));

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].grouped_project_count, 1);
        assert_eq!(
            groups[0].environment_presence,
            EnvironmentPresence::LocalOnly
        );
        assert_eq!(
            groups[0].member_projects[0].environment_label.as_deref(),
            Some("env local")
        );
        assert!(groups[0].remote_environment_labels.is_empty());
        assert!(!project_groups_span_environments(&groups));
    }

    #[test]
    fn a_group_with_a_primary_and_a_remote_member_is_mixed() {
        let primary = EnvironmentId::new("local").expect("id");
        let members = |envs: &[&str]| -> Vec<SidebarProjectGroupMember<TestProject>> {
            envs.iter()
                .enumerate()
                .map(|(index, environment)| SidebarProjectGroupMember {
                    project: project(environment, &format!("p{index}"), "/work/app"),
                    physical_project_key: format!("key{index}"),
                    environment_label: None,
                })
                .collect()
        };

        assert_eq!(
            environment_presence(&members(&["local", "remote"]), Some(&primary)),
            EnvironmentPresence::Mixed
        );
        assert_eq!(
            environment_presence(&members(&["local", "local"]), Some(&primary)),
            EnvironmentPresence::LocalOnly
        );
        assert_eq!(
            environment_presence(&members(&["remote"]), Some(&primary)),
            EnvironmentPresence::RemoteOnly
        );
    }

    #[test]
    fn spanning_environments_is_true_only_across_two_or_more_environments() {
        let primary = EnvironmentId::new("local").expect("id");
        let single = [project("local", "p1", "/work/app")];
        let single_groups = snapshots(&single, Some(&primary));
        assert!(!project_groups_span_environments(&single_groups));

        let two = [
            project("local", "p1", "/work/app"),
            project("remote", "p2", "/work/other"),
        ];
        let two_groups = snapshots(&two, Some(&primary));
        assert!(project_groups_span_environments(&two_groups));
    }

    #[test]
    fn a_remote_only_project_is_remote_only_without_a_primary_it_is_local() {
        let primary = EnvironmentId::new("local").expect("id");
        let projects = [project("remote", "p2", "/work/other")];

        let with_primary = snapshots(&projects, Some(&primary));
        assert_eq!(
            with_primary[0].environment_presence,
            EnvironmentPresence::RemoteOnly
        );

        let without_primary = snapshots(&projects, None);
        assert_eq!(
            without_primary[0].environment_presence,
            EnvironmentPresence::LocalOnly
        );
        assert!(without_primary[0].remote_environment_labels.is_empty());
    }

    #[test]
    fn physical_to_logical_map_points_each_member_at_its_group_key() {
        let primary = EnvironmentId::new("local").expect("id");
        let projects = [
            project("local", "p1", "/work/app"),
            project("remote", "p2", "/work/app"),
        ];
        let groups = snapshots(&projects, Some(&primary));
        let map = build_physical_to_logical_project_key_map(
            &projects,
            &ProjectGroupingSettings::default(),
            Some(&primary),
        );

        for member in &groups[0].member_projects {
            assert_eq!(
                map.get(&member.physical_project_key),
                Some(&groups[0].project_key)
            );
        }
    }

    #[test]
    fn settings_key_of_a_non_scratch_group_is_its_own_key() {
        let primary = EnvironmentId::new("local").expect("id");
        let projects = [project("local", "p1", "/work/app")];
        let groups = snapshots(&projects, Some(&primary));

        assert_eq!(
            get_sidebar_project_settings_key(
                &groups[0],
                &ProjectGroupingSettings::default(),
                &projects,
                None,
            ),
            groups[0].project_key
        );
    }

    #[test]
    fn scope_key_keeps_a_group_key_and_clears_or_keeps_a_missing_key() {
        let primary = EnvironmentId::new("local").expect("id");
        let projects = [project("local", "p1", "/work/app")];
        let groups = snapshots(&projects, Some(&primary));
        let key = groups[0].project_key.clone();

        assert_eq!(
            resolve_sidebar_project_scope_key(&groups, Some(&key), None, true),
            Some(key.clone())
        );
        assert_eq!(
            resolve_sidebar_project_scope_key(&groups, None, None, true),
            None
        );
        assert_eq!(
            resolve_sidebar_project_scope_key(&groups, Some("gone"), None, true),
            None
        );
        assert_eq!(
            resolve_sidebar_project_scope_key(&groups, Some("gone"), None, false),
            Some("gone".to_owned())
        );
    }

    #[test]
    fn picker_puts_the_preferred_project_group_first_and_targets_it() {
        let primary = EnvironmentId::new("local").expect("id");
        let projects = [
            project("local", "a", "/work/alpha"),
            project("local", "b", "/work/beta"),
        ];
        let groups = snapshots(&projects, Some(&primary));
        assert_eq!(groups.len(), 2);
        let preferred = ScopedProjectRef {
            environment_id: EnvironmentId::new("local").expect("id"),
            project_id: ProjectId::new("b").expect("id"),
        };
        let preferred_group_key = groups
            .iter()
            .find(|group| {
                group
                    .member_projects
                    .iter()
                    .any(|m| m.project.id() == &preferred.project_id)
            })
            .expect("group holding b")
            .project_key
            .clone();

        let entries = build_sidebar_project_picker_entries(&groups, Some(&preferred));

        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_preferred);
        assert_eq!(entries[0].group.project_key, preferred_group_key);
        assert_eq!(
            entries[0].target_project.project.id(),
            &preferred.project_id
        );
        assert!(!entries[1].is_preferred);
    }

    #[test]
    fn picker_without_a_preference_keeps_group_order_and_targets_the_representative() {
        let projects = [project("local", "a", "/work/alpha")];
        let groups = snapshots(&projects, None);
        let entries = build_sidebar_project_picker_entries(&groups, None);

        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_preferred);
        assert_eq!(
            entries[0].target_project.project.id(),
            groups[0].representative.id()
        );
    }
}
