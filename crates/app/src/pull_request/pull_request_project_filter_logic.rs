//! Port of upstream/apps/web/src/components/pullRequest/pullRequestProjectFilter.logic.ts

use std::collections::HashMap;

use crate::state::contracts::{EnvironmentId, ProjectId};
use crate::state::support::collate::locale_compare;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterProject {
    pub id: ProjectId,
    pub environment_id: EnvironmentId,
    pub canonical_key: Option<String>,
    pub title: String,
    pub workspace_root: String,
    pub favicon_path: Option<String>,
}

fn distinguish_titles(
    projects: Vec<FilterProject>,
    suffix: impl Fn(&FilterProject) -> String,
) -> Vec<FilterProject> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for project in &projects {
        *counts.entry(project.title.clone()).or_default() += 1;
    }
    projects
        .into_iter()
        .map(|mut project| {
            if counts.get(project.title.as_str()).copied().unwrap_or(0) > 1 {
                project.title = format!("{} · {}", project.title, suffix(&project));
            }
            project
        })
        .collect()
}

/// One choice per repository per server, retaining the selected checkout for saved scopes.
pub fn pull_request_filter_projects(
    projects: &[FilterProject],
    environment_labels: &HashMap<EnvironmentId, String>,
    selected_project: Option<(&ProjectId, &EnvironmentId)>,
) -> Vec<FilterProject> {
    let mut by_repository: Vec<(EnvironmentId, Option<String>, ProjectId, FilterProject)> = vec![];
    for project in projects {
        let repository = project
            .canonical_key
            .as_ref()
            .map(|key| key.to_lowercase())
            .filter(|key| !key.is_empty());
        let selected = selected_project.is_some_and(|(id, environment_id)| {
            id == &project.id && environment_id == &project.environment_id
        });
        let existing = by_repository
            .iter()
            .position(|(environment_id, key, id, _)| {
                environment_id == &project.environment_id
                    && match &repository {
                        Some(repository) => key.as_ref() == Some(repository),
                        None => key.is_none() && id == &project.id,
                    }
            });
        match existing {
            Some(index) if selected => by_repository[index].3 = project.clone(),
            Some(_) => {}
            None => by_repository.push((
                project.environment_id.clone(),
                repository,
                project.id.clone(),
                project.clone(),
            )),
        }
    }
    let by_server = distinguish_titles(
        by_repository
            .into_iter()
            .map(|(_, _, _, project)| project)
            .collect(),
        |project| {
            environment_labels
                .get(&project.environment_id)
                .cloned()
                .unwrap_or_else(|| project.environment_id.as_str().to_owned())
        },
    );
    let by_path = distinguish_titles(by_server, |project| project.workspace_root.clone());
    // Separate environments can share both their display name and their checkout path.
    let by_environment = distinguish_titles(by_path, |project| {
        project.environment_id.as_str().to_owned()
    });
    let mut result = distinguish_titles(by_environment, |project| project.id.as_str().to_owned());
    result.sort_by(|left, right| locale_compare(&left.title, &right.title));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn eid(value: &str) -> EnvironmentId {
        EnvironmentId::new(value).unwrap()
    }
    fn pid(value: &str) -> ProjectId {
        ProjectId::new(value).unwrap()
    }
    fn labels() -> HashMap<EnvironmentId, String> {
        HashMap::from([
            (eid("env-cups"), "cups".into()),
            (eid("env-nucbox"), "nucbox-1".into()),
        ])
    }
    fn project(id: &str, environment_id: &str, canonical_key: Option<&str>) -> FilterProject {
        FilterProject {
            id: pid(id),
            environment_id: eid(environment_id),
            canonical_key: canonical_key.map(str::to_owned),
            title: "t3code".into(),
            workspace_root: format!("/work/{id}"),
            favicon_path: Some(format!("{id}/favicon.png")),
        }
    }
    fn main(id: &str) -> FilterProject {
        project(id, "env-nucbox", Some("github.com/pingdotgg/t3code"))
    }
    fn filter(
        projects: &[FilterProject],
        selected: Option<(&ProjectId, &EnvironmentId)>,
    ) -> Vec<FilterProject> {
        pull_request_filter_projects(projects, &labels(), selected)
    }
    #[test]
    fn collapses_checkouts_per_server() {
        let choices = filter(
            &[
                main("main"),
                main("worktree-1"),
                main("worktree-2"),
                project("main", "env-cups", Some("github.com/pingdotgg/t3code")),
            ],
            None,
        );
        assert_eq!(
            choices
                .iter()
                .map(|p| (p.id.as_str(), p.environment_id.as_str(), p.title.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("main", "env-cups", "t3code · cups"),
                ("main", "env-nucbox", "t3code · nucbox-1")
            ]
        );
        assert_eq!(choices[1].favicon_path.as_deref(), Some("main/favicon.png"));
    }
    #[test]
    fn keeps_saved_worktree_selection() {
        let ps = vec![
            main("main"),
            main("worktree"),
            project("worktree", "env-cups", Some("github.com/pingdotgg/t3code")),
        ];
        let choices = filter(&ps, Some((&ps[1].id, &ps[1].environment_id)));
        assert_eq!(
            choices
                .iter()
                .find(|p| p.environment_id == eid("env-nucbox"))
                .unwrap()
                .id,
            pid("worktree")
        );
        assert!(choices.iter().any(|p| p.environment_id == eid("env-cups")));
    }
    #[test]
    fn canonical_case_insensitive() {
        let a = main("main");
        let b = project(
            "worktree",
            "env-nucbox",
            Some("GitHub.com/PingDotGG/T3Code"),
        );
        assert_eq!(filter(&[a.clone(), b], None), vec![a]);
    }
    #[test]
    fn no_suffix_after_collapse() {
        let a = main("main");
        assert_eq!(filter(&[a.clone(), main("worktree")], None), vec![a]);
    }
    #[test]
    fn same_name_same_server_by_path() {
        let choices = filter(
            &[
                main("upstream"),
                project(
                    "fork",
                    "env-nucbox",
                    Some("github.com/juliusmarminge/t3code"),
                ),
            ],
            None,
        );
        assert_eq!(
            choices.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
            vec![
                "t3code · nucbox-1 · /work/fork",
                "t3code · nucbox-1 · /work/upstream"
            ]
        );
    }
    #[test]
    fn different_hosts_separate() {
        let choices = filter(
            &[
                main("github"),
                project(
                    "enterprise",
                    "env-nucbox",
                    Some("git.example.com/pingdotgg/t3code"),
                ),
            ],
            None,
        );
        assert_eq!(
            choices.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["enterprise", "github"]
        );
    }
    #[test]
    fn unknown_identity_not_merged() {
        let choices = filter(
            &[
                project("first", "env-nucbox", None),
                project("second", "env-nucbox", None),
            ],
            None,
        );
        assert_eq!(
            choices.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
            vec![
                "t3code · nucbox-1 · /work/first",
                "t3code · nucbox-1 · /work/second"
            ]
        );
    }
    #[test]
    fn same_server_label_and_path() {
        let labels = HashMap::from([
            (eid("env-cups"), "nucbox-1".into()),
            (eid("env-nucbox"), "nucbox-1".into()),
        ]);
        let choices = pull_request_filter_projects(
            &[
                main("main"),
                project("main", "env-cups", Some("github.com/pingdotgg/t3code")),
            ],
            &labels,
            None,
        );
        assert_eq!(
            choices.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
            vec![
                "t3code · nucbox-1 · /work/main · env-cups",
                "t3code · nucbox-1 · /work/main · env-nucbox"
            ]
        );
    }
    #[test]
    fn missing_label_uses_environment_id() {
        let choices = pull_request_filter_projects(
            &[
                main("main"),
                project("remote", "env-cups", Some("github.com/pingdotgg/t3code")),
            ],
            &HashMap::new(),
            None,
        );
        assert_eq!(
            choices.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
            vec!["t3code · env-cups", "t3code · env-nucbox"]
        );
    }
    #[test]
    fn same_path_unresolved_records_use_id() {
        let a = project("first", "env-nucbox", None);
        let mut b = project("second", "env-nucbox", None);
        b.workspace_root = a.workspace_root.clone();
        let choices = filter(&[a, b], None);
        assert_eq!(
            choices.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
            vec![
                "t3code · nucbox-1 · /work/first · env-nucbox · first",
                "t3code · nucbox-1 · /work/first · env-nucbox · second"
            ]
        );
    }
    #[test]
    fn unrelated_names_alphabetical() {
        let mut app = main("app");
        app.title = "Zebra".into();
        let mut tools = project("tools", "env-nucbox", Some("github.com/acme/tools"));
        tools.title = "Alpha".into();
        assert_eq!(
            filter(&[app.clone(), tools.clone()], None),
            vec![tools, app]
        );
        assert!(filter(&[], None).is_empty());
    }
}
