//! Port of upstream/apps/web/src/components/settings/useSettingsProjectGroups.ts.
//!
//! Settings uses the same logical projects as the sidebar, sorted by display
//! name. The hook's memo inputs become arguments; the caller re-runs it when
//! projects, client settings, environments or the primary environment change.

// Ported ahead of the views that consume it.
#![allow(dead_code)]

use std::collections::HashMap;

use turin_contracts::settings::ClientSettings;

use crate::settings::locale::locale_compare;
use crate::state::contracts::EnvironmentId;
use crate::state::project_grouping::{GroupableProject, select_project_grouping_settings};
use crate::state::sidebar_project_grouping::{
    SidebarProjectSnapshot, build_sidebar_project_snapshots,
};

pub fn settings_project_groups<P: GroupableProject + Clone>(
    projects: &[P],
    settings: &ClientSettings,
    primary_environment_id: Option<&EnvironmentId>,
    environment_labels: &HashMap<EnvironmentId, String>,
) -> Vec<SidebarProjectSnapshot<P>> {
    let mut groups = build_sidebar_project_snapshots(
        projects,
        &select_project_grouping_settings(settings),
        false,
        primary_environment_id,
        |id| environment_labels.get(id).cloned(),
    );
    groups.sort_by(|a, b| locale_compare(&a.display_name, &b.display_name));
    groups
}

#[cfg(test)]
mod tests {
    use turin_contracts::settings::default_client_settings;

    use super::*;
    use crate::state::contracts::ProjectId;
    use crate::state::models::EnvironmentProject;

    fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).expect("environment id")
    }

    fn project(environment: &str, id: &str, title: &str, root: &str) -> EnvironmentProject {
        EnvironmentProject {
            environment_id: EnvironmentId::new(environment).expect("environment id"),
            id: ProjectId::new(id).expect("project id"),
            title: title.to_string(),
            workspace_root: root.to_string(),
            favicon_path: None,
            project_icon: None,
            created_at: None,
            updated_at: None,
            is_scratch: false,
            source: None,
        }
    }

    #[test]
    fn groups_like_the_sidebar_and_sorts_by_display_name() {
        let projects = vec![
            project("env-1", "p1", "zeta", "/work/zeta"),
            project("env-1", "p2", "Alpha", "/work/alpha"),
            project("env-1", "p3", "mid", "/work/mid"),
        ];
        let groups = settings_project_groups(
            &projects,
            &default_client_settings(),
            Some(&env("env-1")),
            &HashMap::new(),
        );
        let names: Vec<&str> = groups.iter().map(|g| g.display_name.as_str()).collect();
        assert_eq!(names, ["Alpha", "mid", "zeta"]);
    }

    #[test]
    fn labels_remote_environments_from_the_map() {
        let projects = vec![project("env-2", "p1", "app", "/srv/app")];
        let labels = HashMap::from([(env("env-2"), "Build box".to_string())]);
        let groups = settings_project_groups(
            &projects,
            &default_client_settings(),
            Some(&env("env-1")),
            &labels,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].remote_environment_labels,
            vec!["Build box".to_string()]
        );
    }
}
