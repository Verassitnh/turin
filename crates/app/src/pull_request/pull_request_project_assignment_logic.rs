//! Port of upstream/apps/web/src/components/pullRequest/pullRequestProjectAssignment.logic.ts

use std::collections::HashMap;

use crate::state::contracts::{EnvironmentId, EnvironmentMachineKind, ProjectId};

/// The little of a project this needs: who holds it, and which repository it is a copy of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignableProject {
    pub id: ProjectId,
    pub environment_id: EnvironmentId,
    pub canonical_key: Option<String>,
    pub workspace_root: String,
}

/// The remote's normalized URL says "same repository" across machines, not a local path.
/// The lower-case fold also guards a key assembled outside `normalizeGitRemoteUrl`.
fn repository_key(project: &AssignableProject) -> Option<String> {
    project.canonical_key.as_ref().map(|key| key.to_lowercase())
}

/// Pick one server per repository. Projects without identity are never deduplicated.
pub fn assign_projects_to_environments(
    projects: &[AssignableProject],
    environment_ids: &[EnvironmentId],
    preferred_environment_id: Option<&EnvironmentId>,
) -> HashMap<EnvironmentId, Vec<ProjectId>> {
    let rank: HashMap<_, _> = environment_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    // Which server lists each repository: the preferred one where it has it, else the first.
    let mut owner: HashMap<String, &EnvironmentId> = HashMap::new();
    for project in projects {
        let Some(key) = repository_key(project) else {
            continue;
        };
        let Some(&environment_rank) = rank.get(&project.environment_id) else {
            continue;
        };
        match owner.get(&key) {
            None => {
                owner.insert(key, &project.environment_id);
            }
            Some(current) if Some(*current) == preferred_environment_id => {}
            Some(current) => {
                if Some(&project.environment_id) == preferred_environment_id
                    || environment_rank < *rank.get(current).unwrap_or(&usize::MAX)
                {
                    owner.insert(key, &project.environment_id);
                }
            }
        }
    }
    let mut assignment: HashMap<EnvironmentId, Vec<ProjectId>> = HashMap::new();
    for project in projects {
        if !rank.contains_key(&project.environment_id) {
            continue;
        }
        if let Some(key) = repository_key(project)
            && owner.get(&key) != Some(&&project.environment_id)
        {
            continue;
        }
        assignment
            .entry(project.environment_id.clone())
            .or_default()
            .push(project.id.clone());
    }
    assignment
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickableEnvironment {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub workspace_root: String,
    pub label: String,
    pub machine: Option<EnvironmentMachineKind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentChoice {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub machine: Option<EnvironmentMachineKind>,
}

/// Listing picks one server, while an action may use any connected copy. Empty means no choice.
pub fn resolve_pickable_environments(
    current_environment_id: &EnvironmentId,
    current_project_id: &ProjectId,
    projects: &[AssignableProject],
    environments: &[EnvironmentChoice],
) -> Vec<PickableEnvironment> {
    let Some(own) = projects
        .iter()
        .find(|p| &p.environment_id == current_environment_id && &p.id == current_project_id)
    else {
        return vec![];
    };
    let Some(key) = repository_key(own) else {
        return vec![];
    };
    let Some(own_environment) = environments
        .iter()
        .find(|e| &e.environment_id == current_environment_id)
    else {
        return vec![];
    };
    let mut others = vec![];
    for environment in environments {
        if &environment.environment_id == current_environment_id {
            continue;
        }
        // One entry per server, whichever copy comes first: two worktrees are still one place.
        if let Some(copy) = projects.iter().find(|p| {
            p.environment_id == environment.environment_id
                && repository_key(p).as_deref() == Some(&key)
        }) {
            others.push(PickableEnvironment {
                environment_id: environment.environment_id.clone(),
                project_id: copy.id.clone(),
                workspace_root: copy.workspace_root.clone(),
                label: environment.label.clone(),
                machine: environment.machine,
            });
        }
    }
    if others.is_empty() {
        return vec![];
    }
    // The panel's own server first, since it is what acting means until the reader says otherwise.
    let mut result = vec![PickableEnvironment {
        environment_id: current_environment_id.clone(),
        project_id: own.id.clone(),
        workspace_root: own.workspace_root.clone(),
        label: own_environment.label.clone(),
        machine: own_environment.machine,
    }];
    result.extend(others);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn id(value: &str) -> EnvironmentId {
        EnvironmentId::new(value).unwrap()
    }
    fn pid(value: &str) -> ProjectId {
        ProjectId::new(value).unwrap()
    }
    fn project(name: &str, env: &str, key: Option<&str>) -> AssignableProject {
        AssignableProject {
            id: pid(name),
            environment_id: id(env),
            canonical_key: key.map(str::to_owned),
            workspace_root: format!("/srv/{env}/{name}"),
        }
    }
    fn assign(
        projects: &[AssignableProject],
        envs: &[&str],
        preferred: Option<&str>,
    ) -> HashMap<EnvironmentId, Vec<ProjectId>> {
        assign_projects_to_environments(
            projects,
            &envs.iter().map(|e| id(e)).collect::<Vec<_>>(),
            preferred.map(id).as_ref(),
        )
    }
    fn plain(map: HashMap<EnvironmentId, Vec<ProjectId>>) -> HashMap<String, Vec<String>> {
        map.into_iter()
            .map(|(e, ps)| {
                (
                    e.as_str().to_owned(),
                    ps.into_iter().map(|p| p.as_str().to_owned()).collect(),
                )
            })
            .collect()
    }
    fn choices(ids: &[&str]) -> Vec<EnvironmentChoice> {
        ids.iter()
            .map(|e| EnvironmentChoice {
                environment_id: id(e),
                label: format!("Server {e}"),
                machine: None,
            })
            .collect()
    }
    fn pick(
        current_env: &str,
        current_id: &str,
        projects: &[AssignableProject],
        envs: &[&str],
    ) -> Vec<PickableEnvironment> {
        resolve_pickable_environments(&id(current_env), &pid(current_id), projects, &choices(envs))
    }

    #[test]
    fn first_server_lists_shared_repository() {
        assert_eq!(
            plain(assign(
                &[
                    project("a1", "env-1", Some("github.com/acme/app")),
                    project("b1", "env-1", Some("github.com/acme/tools")),
                    project("a2", "env-2", Some("github.com/acme/app")),
                    project("c2", "env-2", Some("github.com/acme/site"))
                ],
                &["env-1", "env-2"],
                Some("env-1")
            )),
            HashMap::from([
                ("env-1".into(), vec!["a1".into(), "b1".into()]),
                ("env-2".into(), vec!["c2".into()])
            ])
        );
    }
    #[test]
    fn named_server_over_first() {
        assert_eq!(
            plain(assign(
                &[
                    project("a1", "env-1", Some("r")),
                    project("a2", "env-2", Some("r"))
                ],
                &["env-1", "env-2"],
                Some("env-2")
            )),
            HashMap::from([("env-2".into(), vec!["a2".into()])])
        );
    }
    #[test]
    fn drops_server_with_nothing_own() {
        assert!(
            !assign(
                &[
                    project("a1", "env-1", Some("r")),
                    project("a2", "env-2", Some("r"))
                ],
                &["env-1", "env-2"],
                Some("env-1")
            )
            .contains_key(&id("env-2"))
        );
    }
    #[test]
    fn unknown_identities_kept() {
        assert_eq!(
            plain(assign(
                &[project("p1", "env-1", None), project("p2", "env-2", None)],
                &["env-1", "env-2"],
                Some("env-1")
            ))
            .len(),
            2
        );
    }
    #[test]
    fn distinct_hosts_kept() {
        assert_eq!(
            plain(assign(
                &[
                    project("a1", "env-1", Some("github.com/acme/app")),
                    project("b2", "env-2", Some("gitlab.com/acme/app"))
                ],
                &["env-1", "env-2"],
                Some("env-1")
            ))
            .len(),
            2
        );
    }
    #[test]
    fn own_worktrees_kept() {
        assert_eq!(
            plain(assign(
                &[
                    project("a1", "env-1", Some("github.com/acme/app")),
                    project("a1-wt", "env-1", Some("GitHub.com/acme/app")),
                    project("a2", "env-2", Some("github.com/acme/app"))
                ],
                &["env-1", "env-2"],
                Some("env-1")
            )),
            HashMap::from([("env-1".into(), vec!["a1".into(), "a1-wt".into()])])
        );
    }
    #[test]
    fn case_insensitive_remote() {
        assert_eq!(
            plain(assign(
                &[
                    project("a1", "env-1", Some("git.example.com/Team/App")),
                    project("a2", "env-2", Some("GIT.example.com/team/app"))
                ],
                &["env-1", "env-2"],
                Some("env-1")
            )),
            HashMap::from([("env-1".into(), vec!["a1".into()])])
        );
    }
    #[test]
    fn unread_server_ignored() {
        assert_eq!(
            plain(assign(
                &[
                    project("a1", "env-1", Some("r")),
                    project("a2", "env-2", Some("r"))
                ],
                &["env-2"],
                Some("env-2")
            )),
            HashMap::from([("env-2".into(), vec!["a2".into()])])
        );
    }
    #[test]
    fn project_order_does_not_change_owner() {
        let ps = vec![
            project("a2", "env-2", Some("r")),
            project("a1", "env-1", Some("r")),
        ];
        let mut reversed = ps.clone();
        reversed.reverse();
        assert_eq!(
            plain(assign(&ps, &["env-1", "env-2"], None)),
            plain(assign(&reversed, &["env-1", "env-2"], None))
        );
    }
    #[test]
    fn every_holding_server_own_first() {
        let result = pick(
            "env-2",
            "a2",
            &[
                project("a1", "env-1", Some("r")),
                project("a2", "env-2", Some("r")),
                project("c3", "env-3", Some("s")),
            ],
            &["env-1", "env-2", "env-3"],
        );
        assert_eq!(
            result
                .iter()
                .map(|p| p.environment_id.as_str())
                .collect::<Vec<_>>(),
            vec!["env-2", "env-1"]
        );
        assert_eq!(result[0].workspace_root, "/srv/env-2/a2");
    }
    #[test]
    fn pick_case_insensitive() {
        assert_eq!(
            pick(
                "env-1",
                "a1",
                &[
                    project("a1", "env-1", Some("github.com/acme/app")),
                    project("a2", "env-2", Some("GitHub.com/ACME/app"))
                ],
                &["env-1", "env-2"]
            )
            .len(),
            2
        );
    }
    #[test]
    fn no_choice_single_server_holds_repository() {
        assert!(
            pick(
                "env-1",
                "a1",
                &[
                    project("a1", "env-1", Some("r")),
                    project("c2", "env-2", Some("s"))
                ],
                &["env-1", "env-2"]
            )
            .is_empty()
        );
    }
    #[test]
    fn no_choice_without_identity() {
        assert!(
            pick(
                "env-1",
                "p1",
                &[project("p1", "env-1", None), project("p2", "env-2", None)],
                &["env-1", "env-2"]
            )
            .is_empty()
        );
    }
    #[test]
    fn no_choice_while_projects_arrive() {
        assert!(pick("env-1", "a1", &[], &["env-1", "env-2"]).is_empty());
    }
    #[test]
    fn disconnected_server_left_out() {
        assert!(
            pick(
                "env-1",
                "a1",
                &[
                    project("a1", "env-1", Some("r")),
                    project("a2", "env-2", Some("r"))
                ],
                &["env-1"]
            )
            .is_empty()
        );
    }
    #[test]
    fn two_worktrees_one_choice() {
        let result = pick(
            "env-1",
            "a1",
            &[
                project("a1", "env-1", Some("r")),
                project("a1-wt", "env-1", Some("r")),
                project("a2", "env-2", Some("r")),
                project("a2-wt", "env-2", Some("r")),
            ],
            &["env-1", "env-2"],
        );
        assert_eq!(
            result
                .iter()
                .map(|p| p.project_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a1", "a2"]
        );
    }
    #[test]
    fn own_worktree_over_first_copy() {
        let result = pick(
            "env-1",
            "a1-wt",
            &[
                project("a1", "env-1", Some("r")),
                project("a1-wt", "env-1", Some("r")),
                project("a2", "env-2", Some("r")),
            ],
            &["env-1", "env-2"],
        );
        assert_eq!(result[0].project_id, pid("a1-wt"));
    }
}
