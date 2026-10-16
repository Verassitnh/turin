//! Port of upstream/apps/web/src/hooks/useSidebarProjectGroups.ts: the sidebar's project groups,
//! in the order the sidebar shows them.
//!
//! The hook's inputs (`useProjects`, `useThreadShells`, `uiStateStore.projectOrder`, the client
//! settings, the environment identities) arrive as one [`SidebarProjectGroupsInput`]; the memoized
//! assembly is a plain function.

#![allow(dead_code, unused_imports)]

pub use crate::components::sidebar::sidebar_logic::{
    ProjectSortOrder, order_items_by_preferred_ids, sort_sidebar_v2_project_groups,
};
pub use crate::logical_project::{get_project_order_key, select_project_grouping_settings};
pub use crate::state::sidebar_project_grouping::{
    SidebarProjectSnapshot, build_sidebar_project_snapshots,
};

use std::collections::HashMap;

use turin_contracts::EnvironmentId;

use crate::components::sidebar::sidebar_logic::LogicalSidebarProject;
use crate::components::sidebar::view_model::{SidebarEnvironment, SidebarProjectGroup};
use crate::state::models::{EnvironmentProject, EnvironmentThreadShell};
use crate::state::project_grouping::ProjectGroupingSettings;
use crate::state::ui_state_store::legacy_project_cwd_preference_key;

/// What `useSidebarProjectGroups` reads.
pub struct SidebarProjectGroupsInput<'a> {
    pub projects: &'a [EnvironmentProject],
    pub threads: &'a [EnvironmentThreadShell],
    /// `uiStateStore.projectOrder`: the manual order, as physical project keys.
    pub project_order: &'a [String],
    /// `sidebarProjectSortOrder`.
    pub sort_order: ProjectSortOrder,
    pub grouping_settings: &'a ProjectGroupingSettings,
    pub environments: &'a [SidebarEnvironment],
    pub primary_environment_id: Option<&'a EnvironmentId>,
}

/// The snapshots before the activity sort (`unsortedGroups`).
pub fn unsorted_sidebar_project_groups(
    input: &SidebarProjectGroupsInput<'_>,
) -> Vec<SidebarProjectSnapshot<EnvironmentProject>> {
    let ordered_projects = order_items_by_preferred_ids(
        input.projects,
        input.project_order,
        get_project_order_key,
        Some(&|project: &EnvironmentProject| {
            vec![
                get_project_order_key(project),
                legacy_project_cwd_preference_key(&project.workspace_root),
            ]
        }),
    );
    let labels: HashMap<&EnvironmentId, &str> = input
        .environments
        .iter()
        .map(|environment| (&environment.id, environment.label.as_str()))
        .collect();
    build_sidebar_project_snapshots(
        if input.sort_order == ProjectSortOrder::Manual {
            &ordered_projects
        } else {
            input.projects
        },
        input.grouping_settings,
        true,
        input.primary_environment_id,
        |environment_id| labels.get(environment_id).map(|label| (*label).to_owned()),
    )
}

/// `useSidebarProjectGroups()`: grouped, then sorted by thread activity unless the order is
/// manual.
pub fn use_sidebar_project_groups(
    input: &SidebarProjectGroupsInput<'_>,
) -> Vec<SidebarProjectGroup> {
    let snapshots = unsorted_sidebar_project_groups(input);
    let logical: Vec<LogicalSidebarProject> = snapshots
        .iter()
        .map(|snapshot| LogicalSidebarProject {
            project_key: snapshot.project_key.clone(),
            title: snapshot.display_name.clone(),
            created_at: snapshot.representative.created_at.clone(),
            updated_at: snapshot.representative.updated_at.clone(),
            member_project_keys: snapshot
                .member_project_refs
                .iter()
                .map(|member| format!("{}:{}", member.environment_id, member.project_id))
                .collect(),
        })
        .collect();
    sort_sidebar_v2_project_groups(&logical, input.threads, input.sort_order)
        .into_iter()
        .map(|project| SidebarProjectGroup {
            project_key: project.project_key,
            display_name: project.title,
            created_at: project.created_at,
            updated_at: project.updated_at,
            member_project_keys: project.member_project_keys,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::fixtures::{env, project_id};
    use pretty_assertions::assert_eq;

    fn project(id: &str, root: &str, created: &str, updated: &str) -> EnvironmentProject {
        EnvironmentProject {
            environment_id: env("local"),
            id: project_id(id),
            title: id.to_owned(),
            workspace_root: root.to_owned(),
            favicon_path: None,
            project_icon: None,
            created_at: Some(created.to_owned()),
            updated_at: Some(updated.to_owned()),
            is_scratch: false,
            source: None,
        }
    }

    fn input<'a>(
        projects: &'a [EnvironmentProject],
        threads: &'a [EnvironmentThreadShell],
        order: &'a [String],
        sort_order: ProjectSortOrder,
        settings: &'a ProjectGroupingSettings,
    ) -> SidebarProjectGroupsInput<'a> {
        SidebarProjectGroupsInput {
            projects,
            threads,
            project_order: order,
            sort_order,
            grouping_settings: settings,
            environments: &[],
            primary_environment_id: None,
        }
    }

    fn titles(groups: &[SidebarProjectGroup]) -> Vec<&str> {
        groups
            .iter()
            .map(|group| group.display_name.as_str())
            .collect()
    }

    #[test]
    fn manual_order_follows_the_saved_order_and_keeps_new_projects_last() {
        let projects = [
            project(
                "alpha",
                "/code/alpha",
                "2026-01-01T00:00:00.000Z",
                "2026-01-01T00:00:00.000Z",
            ),
            project(
                "beta",
                "/code/beta",
                "2026-01-02T00:00:00.000Z",
                "2026-01-02T00:00:00.000Z",
            ),
            project(
                "gamma",
                "/code/gamma",
                "2026-01-03T00:00:00.000Z",
                "2026-01-03T00:00:00.000Z",
            ),
        ];
        let settings = ProjectGroupingSettings::default();
        let order = vec![
            get_project_order_key(&projects[2]),
            get_project_order_key(&projects[0]),
        ];
        let groups = use_sidebar_project_groups(&input(
            &projects,
            &[],
            &order,
            ProjectSortOrder::Manual,
            &settings,
        ));
        assert_eq!(titles(&groups), ["gamma", "alpha", "beta"]);
    }

    #[test]
    fn manual_order_also_reads_the_legacy_cwd_preference() {
        let projects = [
            project(
                "alpha",
                "/code/alpha",
                "2026-01-01T00:00:00.000Z",
                "2026-01-01T00:00:00.000Z",
            ),
            project(
                "beta",
                "/code/beta",
                "2026-01-02T00:00:00.000Z",
                "2026-01-02T00:00:00.000Z",
            ),
        ];
        let settings = ProjectGroupingSettings::default();
        let order = vec![legacy_project_cwd_preference_key("/code/beta")];
        let groups = use_sidebar_project_groups(&input(
            &projects,
            &[],
            &order,
            ProjectSortOrder::Manual,
            &settings,
        ));
        assert_eq!(titles(&groups), ["beta", "alpha"]);
    }

    #[test]
    fn an_activity_order_ignores_the_saved_order() {
        let projects = [
            project(
                "older",
                "/code/older",
                "2026-01-01T00:00:00.000Z",
                "2026-01-01T00:00:00.000Z",
            ),
            project(
                "newer",
                "/code/newer",
                "2026-02-01T00:00:00.000Z",
                "2026-02-01T00:00:00.000Z",
            ),
        ];
        let settings = ProjectGroupingSettings::default();
        let order = vec![get_project_order_key(&projects[0])];
        let groups = use_sidebar_project_groups(&input(
            &projects,
            &[],
            &order,
            ProjectSortOrder::UpdatedAt,
            &settings,
        ));
        assert_eq!(titles(&groups), ["newer", "older"]);
    }

    #[test]
    fn a_group_lists_every_member_project_key() {
        let projects = [project(
            "alpha",
            "/code/alpha",
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00.000Z",
        )];
        let settings = ProjectGroupingSettings::default();
        let groups = use_sidebar_project_groups(&input(
            &projects,
            &[],
            &[],
            ProjectSortOrder::UpdatedAt,
            &settings,
        ));
        assert_eq!(groups[0].member_project_keys, [projects[0].key()]);
    }
}
