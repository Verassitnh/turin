//! Port of upstream/packages/client-runtime/src/state/projectGrouping.ts
//!
//! Builds logical project groups without losing the physical projects that
//! remain the actual navigation and task-creation targets.
//!
//! Presentation-specific metadata, filtering, and activity sorting stay in
//! each client. Grouping modes, overrides, physical deduplication, labels, and
//! member preservation live here so web and mobile cannot drift.
//!
//! Upstream types these functions structurally (`Pick<EnvironmentProject, …>`);
//! here they read through [`GroupableProject`].

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::state::contracts::{
    ClientSettings, EnvironmentId, ProjectId, RepositoryIdentity, ScopedProjectRef,
    SidebarProjectGroupingMode, repository_grouping_display_name_of, repository_grouping_key_of,
};
use crate::state::models::EnvironmentProject;
use crate::state::projects::normalize_project_path_for_comparison;
use crate::state::time::parse_ms;

const SCRATCH_PROJECT_SCOPE_KEY: &str = "\0no-project";

/// The fields of `EnvironmentProject` grouping reads.
pub trait GroupableProject {
    fn environment_id(&self) -> &EnvironmentId;
    fn id(&self) -> &ProjectId;
    fn title(&self) -> &str;
    fn workspace_root(&self) -> &str;
    fn repository_identity(&self) -> Option<&RepositoryIdentity>;
    fn is_scratch(&self) -> bool;
    fn updated_at(&self) -> Option<&str>;
    fn created_at(&self) -> Option<&str>;
}

impl GroupableProject for EnvironmentProject {
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
        self.source
            .as_ref()
            .and_then(|source| source.repository_identity.as_ref())
            .and_then(|identity| identity.as_ref())
    }

    fn is_scratch(&self) -> bool {
        self.is_scratch
    }

    fn updated_at(&self) -> Option<&str> {
        self.updated_at.as_deref()
    }

    fn created_at(&self) -> Option<&str> {
        self.created_at.as_deref()
    }
}

impl<T: GroupableProject> GroupableProject for std::sync::Arc<T> {
    fn environment_id(&self) -> &EnvironmentId {
        (**self).environment_id()
    }

    fn id(&self) -> &ProjectId {
        (**self).id()
    }

    fn title(&self) -> &str {
        (**self).title()
    }

    fn workspace_root(&self) -> &str {
        (**self).workspace_root()
    }

    fn repository_identity(&self) -> Option<&RepositoryIdentity> {
        (**self).repository_identity()
    }

    fn is_scratch(&self) -> bool {
        (**self).is_scratch()
    }

    fn updated_at(&self) -> Option<&str> {
        (**self).updated_at()
    }

    fn created_at(&self) -> Option<&str> {
        (**self).created_at()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectGroupingSettings {
    pub sidebar_project_grouping_mode: SidebarProjectGroupingMode,
    pub sidebar_project_grouping_overrides: HashMap<String, SidebarProjectGroupingMode>,
}

impl Default for ProjectGroupingSettings {
    fn default() -> Self {
        Self {
            sidebar_project_grouping_mode: SidebarProjectGroupingMode::Repository,
            sidebar_project_grouping_overrides: HashMap::new(),
        }
    }
}

pub type ProjectGroupingMode = SidebarProjectGroupingMode;

pub fn select_project_grouping_settings(settings: &ClientSettings) -> ProjectGroupingSettings {
    ProjectGroupingSettings {
        sidebar_project_grouping_mode: settings.sidebar_project_grouping_mode,
        sidebar_project_grouping_overrides: settings
            .sidebar_project_grouping_overrides
            .iter()
            .map(|(key, mode)| (key.as_str().to_owned(), *mode))
            .collect(),
    }
}

fn unique_non_empty_values<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut unique = Vec::new();
    for value in values {
        let Some(trimmed) = value.map(crate::state::contracts::js_trim) else {
            continue;
        };
        if trimmed.is_empty() || !seen.insert(trimmed) {
            continue;
        }
        unique.push(trimmed.to_owned());
    }
    unique
}

fn derive_repository_relative_project_path(project: &impl GroupableProject) -> Option<String> {
    let root_path = project
        .repository_identity()
        .and_then(|identity| identity.root_path.as_ref())
        .map(|path| crate::state::contracts::js_trim(path.as_str()))
        .filter(|path| !path.is_empty())?;

    let normalized_project_path = normalize_project_path_for_comparison(project.workspace_root());
    let normalized_root_path = normalize_project_path_for_comparison(root_path);
    if normalized_project_path.is_empty() || normalized_root_path.is_empty() {
        return None;
    }

    if normalized_project_path == normalized_root_path {
        return Some(String::new());
    }

    let separator = if normalized_root_path.contains('\\') {
        '\\'
    } else {
        '/'
    };
    let root_prefix = format!("{normalized_root_path}{separator}");
    let relative = normalized_project_path.strip_prefix(&root_prefix)?;
    Some(relative.replace('\\', "/"))
}

pub fn derive_physical_project_key_from_path(environment_id: &str, cwd: &str) -> String {
    format!(
        "{environment_id}:{}",
        normalize_project_path_for_comparison(cwd)
    )
}

pub fn derive_physical_project_key(project: &impl GroupableProject) -> String {
    derive_physical_project_key_from_path(
        project.environment_id().as_str(),
        project.workspace_root(),
    )
}

pub fn derive_project_grouping_override_key(project: &impl GroupableProject) -> String {
    derive_physical_project_key(project)
}

pub fn get_project_order_key(project: &impl GroupableProject) -> String {
    derive_physical_project_key(project)
}

pub fn resolve_project_grouping_mode(
    project: &impl GroupableProject,
    settings: &ProjectGroupingSettings,
) -> SidebarProjectGroupingMode {
    settings
        .sidebar_project_grouping_overrides
        .get(&derive_project_grouping_override_key(project))
        .copied()
        .unwrap_or(settings.sidebar_project_grouping_mode)
}

fn derive_repository_scoped_key(
    project: &impl GroupableProject,
    grouping_mode: SidebarProjectGroupingMode,
) -> Option<String> {
    let canonical_key = project
        .repository_identity()
        .map(repository_grouping_key_of);
    // A NUL reserves the scratch key namespace outside valid repository identities.
    let canonical_key = canonical_key.filter(|key| !key.is_empty() && !key.contains('\0'))?;

    if grouping_mode == SidebarProjectGroupingMode::Repository {
        return Some(canonical_key.to_owned());
    }

    match derive_repository_relative_project_path(project) {
        None => Some(canonical_key.to_owned()),
        Some(relative) if relative.is_empty() => Some(canonical_key.to_owned()),
        Some(relative) => Some(format!("{canonical_key}::{relative}")),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeriveLogicalProjectKeyOptions {
    pub grouping_mode: Option<SidebarProjectGroupingMode>,
    pub group_scratch_projects: bool,
}

pub fn derive_logical_project_key(
    project: &impl GroupableProject,
    options: DeriveLogicalProjectKeyOptions,
) -> String {
    if options.group_scratch_projects && project.is_scratch() {
        return SCRATCH_PROJECT_SCOPE_KEY.to_owned();
    }
    let grouping_mode = options
        .grouping_mode
        .unwrap_or(SidebarProjectGroupingMode::Repository);
    if grouping_mode == SidebarProjectGroupingMode::Separate {
        return derive_physical_project_key(project);
    }
    derive_repository_scoped_key(project, grouping_mode)
        .unwrap_or_else(|| derive_physical_project_key(project))
}

pub fn derive_logical_project_key_from_settings(
    project: &impl GroupableProject,
    settings: &ProjectGroupingSettings,
    group_scratch_projects: bool,
) -> String {
    derive_logical_project_key(
        project,
        DeriveLogicalProjectKeyOptions {
            grouping_mode: Some(resolve_project_grouping_mode(project, settings)),
            group_scratch_projects,
        },
    )
}

/// Resolves a scratch filter selected before its environment config identified the project.
pub fn resolve_scratch_project_scope_key<P: GroupableProject>(
    key: Option<&str>,
    projects: &[P],
    settings: &ProjectGroupingSettings,
) -> Option<String> {
    let key = key?;
    if key == SCRATCH_PROJECT_SCOPE_KEY {
        return Some(key.to_owned());
    }
    if projects
        .iter()
        .any(|project| derive_logical_project_key_from_settings(project, settings, true) == key)
    {
        return Some(key.to_owned());
    }
    let matches_scratch = projects.iter().any(|project| {
        if !project.is_scratch() {
            return false;
        }
        let mode = resolve_project_grouping_mode(project, settings);
        derive_physical_project_key(project) == key
            || (mode != SidebarProjectGroupingMode::Separate
                && derive_repository_scoped_key(project, mode).as_deref() == Some(key))
    });
    Some(if matches_scratch {
        SCRATCH_PROJECT_SCOPE_KEY.to_owned()
    } else {
        key.to_owned()
    })
}

pub fn derive_project_group_label<P: GroupableProject>(
    representative: &P,
    members: &[&P],
) -> String {
    let shared_titles = unique_non_empty_values(members.iter().map(|member| Some(member.title())));
    let shared_display_names = unique_non_empty_values(members.iter().map(|member| {
        member
            .repository_identity()
            .and_then(repository_grouping_display_name_of)
    }));
    let shared_repository_names = unique_non_empty_values(members.iter().map(|member| {
        member
            .repository_identity()
            .and_then(|identity| identity.name.as_ref())
            .map(|name| name.as_str())
    }));
    if shared_titles.len() == 1
        && !shared_display_names.contains(&shared_titles[0])
        && !shared_repository_names.contains(&shared_titles[0])
    {
        return shared_titles[0].clone();
    }
    if shared_display_names.len() == 1 {
        return shared_display_names[0].clone();
    }
    if shared_repository_names.len() == 1 {
        return shared_repository_names[0].clone();
    }
    representative.title().to_owned()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectGroupMember<P> {
    pub physical_project_key: String,
    pub project: P,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectGroup<P> {
    pub key: String,
    pub label: String,
    pub representative: P,
    pub members: Vec<ProjectGroupMember<P>>,
    pub member_project_refs: Vec<ScopedProjectRef>,
}

fn project_freshness_time(project: &impl GroupableProject) -> i64 {
    if let Some(updated) = project.updated_at().and_then(parse_ms) {
        return updated;
    }
    project.created_at().and_then(parse_ms).unwrap_or(0)
}

fn should_replace_physical_project_winner(
    existing: &impl GroupableProject,
    candidate: &impl GroupableProject,
) -> bool {
    let freshness_delta = project_freshness_time(candidate) - project_freshness_time(existing);
    freshness_delta > 0
        || (freshness_delta == 0 && candidate.id().as_str() > existing.id().as_str())
}

fn select_project_identity_source<'a, P: GroupableProject>(
    projects: &'a [P],
    winner: &'a P,
) -> &'a P {
    if winner.repository_identity().is_some() {
        return winner;
    }
    let mut freshest_identified: Option<&P> = None;
    for project in projects {
        if project.repository_identity().is_none() {
            continue;
        }
        if freshest_identified
            .is_none_or(|freshest| should_replace_physical_project_winner(freshest, project))
        {
            freshest_identified = Some(project);
        }
    }
    freshest_identified.unwrap_or(winner)
}

#[derive(Clone, Debug, Default)]
pub struct BuildProjectGroupsInput<'a> {
    pub settings: ProjectGroupingSettings,
    pub preferred_environment_id: Option<&'a EnvironmentId>,
    /// Filter equivalence does not change the default ownership of mutable project settings.
    pub group_scratch_projects: bool,
}

/// Builds logical project groups without losing the physical projects that
/// remain the actual navigation and task-creation targets.
pub fn build_project_groups<P: GroupableProject + Clone>(
    projects: &[P],
    input: &BuildProjectGroupsInput<'_>,
) -> Vec<ProjectGroup<P>> {
    let mut projects_by_physical_key: IndexMap<String, Vec<P>> = IndexMap::new();
    for project in projects {
        projects_by_physical_key
            .entry(derive_physical_project_key(project))
            .or_default()
            .push(project.clone());
    }

    let mut logical_key_by_physical_key: HashMap<String, String> = HashMap::new();
    let mut grouped_members: IndexMap<String, Vec<ProjectGroupMember<P>>> = IndexMap::new();
    for (physical_project_key, physical_projects) in &projects_by_physical_key {
        let winner = physical_projects
            .iter()
            .fold(&physical_projects[0], |current, candidate| {
                if should_replace_physical_project_winner(current, candidate) {
                    candidate
                } else {
                    current
                }
            });
        let identity_source = select_project_identity_source(physical_projects, winner);
        let logical_key = derive_logical_project_key(
            identity_source,
            DeriveLogicalProjectKeyOptions {
                grouping_mode: Some(resolve_project_grouping_mode(winner, &input.settings)),
                group_scratch_projects: input.group_scratch_projects,
            },
        );
        logical_key_by_physical_key.insert(physical_project_key.clone(), logical_key.clone());
        grouped_members
            .entry(logical_key)
            .or_default()
            .push(ProjectGroupMember {
                physical_project_key: physical_project_key.clone(),
                project: winner.clone(),
            });
    }

    let mut project_refs_by_logical_key: HashMap<String, Vec<ScopedProjectRef>> = HashMap::new();
    let mut seen_project_refs: HashSet<(String, String)> = HashSet::new();
    for project in projects {
        let physical_project_key = derive_physical_project_key(project);
        let logical_key = logical_key_by_physical_key
            .get(&physical_project_key)
            .cloned()
            .unwrap_or_else(|| {
                derive_logical_project_key_from_settings(
                    project,
                    &input.settings,
                    input.group_scratch_projects,
                )
            });
        let ref_key = (
            project.environment_id().to_string(),
            project.id().to_string(),
        );
        if !seen_project_refs.insert(ref_key) {
            continue;
        }
        project_refs_by_logical_key
            .entry(logical_key)
            .or_default()
            .push(ScopedProjectRef {
                environment_id: project.environment_id().clone(),
                project_id: project.id().clone(),
            });
    }

    grouped_members
        .into_iter()
        .map(|(key, members)| {
            let representative = input
                .preferred_environment_id
                .and_then(|preferred| {
                    members
                        .iter()
                        .find(|member| member.project.environment_id() == preferred)
                        .map(|member| member.project.clone())
                })
                .unwrap_or_else(|| members[0].project.clone());
            let label = if input.group_scratch_projects && representative.is_scratch() {
                "No project".to_owned()
            } else if members.len() > 1 {
                let member_projects: Vec<&P> =
                    members.iter().map(|member| &member.project).collect();
                derive_project_group_label(&representative, &member_projects)
            } else {
                representative.title().to_owned()
            };
            ProjectGroup {
                member_project_refs: project_refs_by_logical_key.remove(&key).unwrap_or_default(),
                key,
                label,
                representative,
                members,
            }
        })
        .collect()
}

/// Orders projects the way a sorted group list shows them: by the first group holding each
/// project. Projects in no group follow, and ties keep their incoming order.
pub fn sort_projects_by_group_order<P: Clone>(
    projects: &[P],
    groups: &[Vec<ScopedProjectRef>],
    key_of: impl Fn(&P) -> (String, String),
) -> Vec<P> {
    let mut rank_by_project: HashMap<(String, String), usize> = HashMap::new();
    for (rank, member_refs) in groups.iter().enumerate() {
        for reference in member_refs {
            rank_by_project
                .entry((
                    reference.environment_id.to_string(),
                    reference.project_id.to_string(),
                ))
                .or_insert(rank);
        }
    }
    let rank_of = |project: &P| {
        rank_by_project
            .get(&key_of(project))
            .copied()
            .unwrap_or(groups.len())
    };
    let mut sorted = projects.to_vec();
    sorted.sort_by_key(|project| rank_of(project));
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::from_json;
    use serde_json::{Value, json};

    #[derive(Clone, Debug, PartialEq)]
    struct Project {
        environment_id: EnvironmentId,
        id: ProjectId,
        title: String,
        workspace_root: String,
        identity: Option<RepositoryIdentity>,
        is_scratch: bool,
        updated_at: String,
    }

    impl GroupableProject for Project {
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
            self.identity.as_ref()
        }
        fn is_scratch(&self) -> bool {
            self.is_scratch
        }
        fn updated_at(&self) -> Option<&str> {
            Some(&self.updated_at)
        }
        fn created_at(&self) -> Option<&str> {
            Some("2026-07-01T00:00:00.000Z")
        }
    }

    fn base_identity() -> Value {
        json!({
            "canonicalKey": "github.com/t3tools/t3code",
            "locator": { "source": "git-remote", "remoteName": "upstream", "remoteUrl": "https://github.com/t3tools/t3code.git" },
            "provider": "github", "owner": "t3tools", "name": "t3code", "displayName": "T3 Code",
        })
    }

    fn identity(patch: Value) -> Option<RepositoryIdentity> {
        let mut value = base_identity();
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        Some(from_json(value))
    }

    fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).unwrap()
    }

    fn make(id: &str, root: &str) -> Project {
        Project {
            environment_id: env("environment"),
            id: ProjectId::new(id).unwrap(),
            title: id.to_owned(),
            workspace_root: root.to_owned(),
            identity: identity(json!({})),
            is_scratch: false,
            updated_at: "2026-07-01T00:00:00.000Z".to_owned(),
        }
    }

    fn settings(
        mode: SidebarProjectGroupingMode,
        overrides: &[(String, SidebarProjectGroupingMode)],
    ) -> ProjectGroupingSettings {
        ProjectGroupingSettings {
            sidebar_project_grouping_mode: mode,
            sidebar_project_grouping_overrides: overrides.iter().cloned().collect(),
        }
    }

    const MODES: [SidebarProjectGroupingMode; 3] = [
        SidebarProjectGroupingMode::Repository,
        SidebarProjectGroupingMode::RepositoryPath,
        SidebarProjectGroupingMode::Separate,
    ];

    fn group(
        projects: &[Project],
        settings: ProjectGroupingSettings,
    ) -> Vec<ProjectGroup<Project>> {
        build_project_groups(
            projects,
            &BuildProjectGroupsInput {
                settings,
                ..Default::default()
            },
        )
    }

    fn ref_of(project: &Project) -> ScopedProjectRef {
        ScopedProjectRef {
            environment_id: project.environment_id.clone(),
            project_id: project.id.clone(),
        }
    }

    #[test]
    fn keeps_scratch_project_ownership_separate_by_default_in_every_mode() {
        for mode in MODES {
            let projects = vec![
                Project {
                    is_scratch: true,
                    identity: None,
                    ..make("scratch-local", "/local/scratch")
                },
                Project {
                    environment_id: env("remote"),
                    is_scratch: true,
                    identity: None,
                    ..make("scratch-remote", "/remote/scratch")
                },
            ];
            let groups = group(&projects, settings(mode, &[]));
            assert_eq!(
                groups.iter().map(|g| g.key.clone()).collect::<Vec<_>>(),
                projects
                    .iter()
                    .map(derive_physical_project_key)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                groups
                    .iter()
                    .map(|g| g.member_project_refs.clone())
                    .collect::<Vec<_>>(),
                projects.iter().map(|p| vec![ref_of(p)]).collect::<Vec<_>>()
            );
            let grouped = build_project_groups(
                &projects,
                &BuildProjectGroupsInput {
                    settings: settings(mode, &[]),
                    group_scratch_projects: true,
                    ..Default::default()
                },
            );
            assert_eq!(grouped.len(), 1);
        }
    }

    #[test]
    fn migrates_a_pre_config_scratch_selection_and_retains_it_after_the_original_host_is_removed() {
        for mode in MODES {
            let local = Project {
                identity: identity(json!({ "rootPath": "/local/repo" })),
                ..make("scratch-local", "/local/repo/scratch")
            };
            let remote = Project {
                environment_id: env("remote"),
                identity: None,
                is_scratch: true,
                ..make("scratch-remote", "/remote/scratch")
            };
            let grouping_settings = settings(mode, &[]);
            let selected_before_config =
                derive_logical_project_key_from_settings(&local, &grouping_settings, false);
            let loaded = vec![
                Project {
                    is_scratch: true,
                    ..local.clone()
                },
                remote.clone(),
            ];
            let migrated = resolve_scratch_project_scope_key(
                Some(&selected_before_config),
                &loaded,
                &grouping_settings,
            );
            let scratch_key = build_project_groups(
                &loaded,
                &BuildProjectGroupsInput {
                    settings: grouping_settings.clone(),
                    group_scratch_projects: true,
                    ..Default::default()
                },
            )[0]
            .key
            .clone();
            assert_eq!(migrated.as_deref(), Some(scratch_key.as_str()));
            assert_eq!(
                resolve_scratch_project_scope_key(
                    migrated.as_deref(),
                    std::slice::from_ref(&remote),
                    &grouping_settings
                )
                .as_deref(),
                Some(scratch_key.as_str())
            );
            let only_remote = build_project_groups(
                std::slice::from_ref(&remote),
                &BuildProjectGroupsInput {
                    settings: grouping_settings,
                    group_scratch_projects: true,
                    ..Default::default()
                },
            );
            assert_eq!(only_remote[0].member_project_refs, vec![ref_of(&remote)]);
        }
    }

    #[test]
    fn retains_a_repository_filter_that_still_belongs_to_an_ordinary_project() {
        let scratch = Project {
            is_scratch: true,
            ..make("scratch", "/scratch")
        };
        let ordinary = make("ordinary", "/ordinary");
        let repo_settings = settings(SidebarProjectGroupingMode::Repository, &[]);
        let key = derive_logical_project_key_from_settings(&ordinary, &repo_settings, false);
        assert_eq!(
            resolve_scratch_project_scope_key(Some(&key), &[scratch, ordinary], &repo_settings)
                .as_deref(),
            Some(key.as_str())
        );
    }

    #[test]
    fn coalesces_scratch_homes_without_merging_ordinary_same_title_projects() {
        for mode in MODES {
            let remote = env("remote");
            let titled = |p: Project| Project {
                title: "No project".into(),
                ..p
            };
            let projects = vec![
                titled(Project {
                    is_scratch: true,
                    identity: None,
                    ..make("scratch-local", "/local/scratch")
                }),
                titled(Project {
                    environment_id: remote.clone(),
                    is_scratch: true,
                    ..make("scratch-remote", "/remote/scratch")
                }),
                titled(Project {
                    environment_id: remote.clone(),
                    is_scratch: true,
                    identity: None,
                    updated_at: "2026-07-02T00:00:00.000Z".into(),
                    ..make("scratch-duplicate", "/remote/scratch/")
                }),
                titled(make("ordinary", "/ordinary")),
                titled(Project {
                    identity: identity(json!({ "canonicalKey": "\0no-project" })),
                    ..make("reserved-collision", "/other")
                }),
            ];
            let groups = build_project_groups(
                &projects,
                &BuildProjectGroupsInput {
                    settings: settings(
                        mode,
                        &[(
                            derive_physical_project_key(&projects[0]),
                            SidebarProjectGroupingMode::Separate,
                        )],
                    ),
                    preferred_environment_id: Some(&remote),
                    group_scratch_projects: true,
                },
            );
            assert_eq!(groups.len(), 3);
            let scratch = groups.iter().find(|g| g.representative.is_scratch).unwrap();
            assert_eq!(scratch.label, "No project");
            assert_eq!(scratch.representative.id, projects[2].id);
            assert_eq!(scratch.members.len(), 2);
            assert_eq!(
                scratch.member_project_refs,
                projects[..3].iter().map(ref_of).collect::<Vec<_>>()
            );
            assert_eq!(
                resolve_scratch_project_scope_key(
                    Some(&derive_physical_project_key(&projects[1])),
                    &projects,
                    &settings(SidebarProjectGroupingMode::Repository, &[])
                )
                .as_deref(),
                Some(scratch.key.as_str())
            );
            let ordinary_key = derive_physical_project_key(&projects[3]);
            assert_eq!(
                resolve_scratch_project_scope_key(
                    Some(&ordinary_key),
                    &projects,
                    &settings(SidebarProjectGroupingMode::Repository, &[])
                )
                .as_deref(),
                Some(ordinary_key.as_str())
            );
            assert_eq!(
                groups
                    .iter()
                    .filter(|g| !g.representative.is_scratch)
                    .count(),
                2
            );
        }
    }

    #[test]
    fn preserves_every_physical_clone_as_a_selectable_member_in_repository_modes() {
        let projects = vec![
            make("t3code", "/work/t3code"),
            make("t3code-2", "/work/t3code-2"),
            make("t3code-3", "/work/t3code-3"),
        ];
        for mode in [
            SidebarProjectGroupingMode::Repository,
            SidebarProjectGroupingMode::RepositoryPath,
        ] {
            let groups = group(&projects, settings(mode, &[]));
            assert_eq!(groups.len(), 1);
            assert_eq!(
                groups[0]
                    .members
                    .iter()
                    .map(|m| m.project.id.to_string())
                    .collect::<Vec<_>>(),
                vec!["t3code", "t3code-2", "t3code-3"]
            );
            assert_eq!(groups[0].member_project_refs.len(), 3);
        }
    }

    #[test]
    fn uses_a_shared_custom_title_as_the_repository_groups_label() {
        let projects = vec![
            Project {
                title: "Custom project".into(),
                ..make("first", "/work/t3code")
            },
            Project {
                title: "Custom project".into(),
                ..make("second", "/work/t3code-2")
            },
        ];
        assert_eq!(
            group(
                &projects,
                settings(SidebarProjectGroupingMode::Repository, &[])
            )[0]
            .label,
            "Custom project"
        );
    }

    #[test]
    fn keeps_the_repository_label_when_shared_titles_match_its_repository_name() {
        let projects = vec![
            Project {
                title: "t3code".into(),
                ..make("first", "/work/t3code")
            },
            Project {
                title: "t3code".into(),
                ..make("second", "/work/t3code-2")
            },
        ];
        assert_eq!(
            group(
                &projects,
                settings(SidebarProjectGroupingMode::Repository, &[])
            )[0]
            .label,
            "T3 Code"
        );
    }

    #[test]
    fn keeps_a_fork_apart_from_its_upstream_checkout_and_labels_it_by_its_own_remote() {
        let fork_identity = identity(
            json!({ "origin": { "canonicalKey": "github.com/julius/t3code-fork", "displayName": "julius/t3code-fork" } }),
        );
        let fork = Project {
            identity: fork_identity.clone(),
            ..make("fork", "/work/t3code-fork")
        };
        let fork_worktree = Project {
            identity: fork_identity,
            ..make("fork-2", "/work/t3code-fork-2")
        };
        let projects = vec![make("t3code", "/work/t3code"), fork, fork_worktree];
        let groups = group(
            &projects,
            settings(SidebarProjectGroupingMode::Repository, &[]),
        );
        assert_eq!(
            groups.iter().map(|g| g.key.clone()).collect::<Vec<_>>(),
            vec!["github.com/t3tools/t3code", "github.com/julius/t3code-fork"]
        );
        assert_eq!(
            groups[1]
                .members
                .iter()
                .map(|m| m.project.id.to_string())
                .collect::<Vec<_>>(),
            vec!["fork", "fork-2"]
        );
        assert_eq!(groups[1].label, "julius/t3code-fork");
    }

    #[test]
    fn labels_a_fork_by_its_canonical_key_when_its_origin_has_no_display_name() {
        let identity = identity(json!({ "origin": { "canonicalKey": "internal-host" } }));
        let projects = vec![
            Project {
                identity: identity.clone(),
                ..make("fork", "/work/fork")
            },
            Project {
                identity,
                ..make("fork-2", "/work/fork-2")
            },
        ];
        let groups = group(
            &projects,
            settings(SidebarProjectGroupingMode::Repository, &[]),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "internal-host");
    }

    #[test]
    fn keeps_physical_clones_in_separate_groups_when_requested() {
        let projects = vec![
            make("t3code", "/work/t3code"),
            make("t3code-2", "/work/t3code-2"),
            make("t3code-3", "/work/t3code-3"),
        ];
        let groups = group(
            &projects,
            settings(SidebarProjectGroupingMode::Separate, &[]),
        );
        assert_eq!(groups.len(), 3);
        assert_eq!(groups.iter().map(|g| g.members.len()).sum::<usize>(), 3);
        assert_eq!(
            groups.iter().map(|g| g.label.clone()).collect::<Vec<_>>(),
            vec!["t3code", "t3code-2", "t3code-3"]
        );
    }

    #[test]
    fn applies_a_physical_project_override_without_dropping_its_siblings() {
        let (first, second, third) = (
            make("t3code", "/work/t3code"),
            make("t3code-2", "/work/t3code-2"),
            make("t3code-3", "/work/t3code-3"),
        );
        let groups = group(
            &[first.clone(), second.clone(), third.clone()],
            settings(
                SidebarProjectGroupingMode::Repository,
                &[(
                    derive_physical_project_key(&second),
                    SidebarProjectGroupingMode::Separate,
                )],
            ),
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups
                .iter()
                .flat_map(|g| g.members.iter().map(|m| m.project.id.to_string()))
                .collect::<Vec<_>>(),
            vec!["t3code", "t3code-3", "t3code-2"]
        );
    }

    #[test]
    fn dedupes_stale_registrations_at_one_physical_path_using_the_freshest_project() {
        let stale = Project {
            identity: None,
            ..make("stale", "/work/t3code")
        };
        let fresh = Project {
            updated_at: "2026-07-02T00:00:00.000Z".into(),
            ..make("fresh", "/work/t3code/")
        };
        let groups = group(
            &[stale, fresh],
            settings(SidebarProjectGroupingMode::Repository, &[]),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 1);
        assert_eq!(groups[0].representative.id.as_str(), "fresh");
        assert_eq!(groups[0].member_project_refs.len(), 2);
    }

    #[test]
    fn uses_repository_identity_from_a_duplicate_registration_when_the_winner_lacks_it() {
        let identified = make("identified", "/work/t3code");
        let fresh_unidentified = Project {
            identity: None,
            updated_at: "2026-07-02T00:00:00.000Z".into(),
            ..make("fresh", "/work/t3code/")
        };
        let sibling = make("sibling", "/work/t3code-2");
        let groups = group(
            &[identified, fresh_unidentified, sibling],
            settings(SidebarProjectGroupingMode::Repository, &[]),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0]
                .members
                .iter()
                .map(|m| m.project.id.to_string())
                .collect::<Vec<_>>(),
            vec!["fresh", "sibling"]
        );
    }

    #[test]
    fn uses_the_freshest_winners_repository_identity_when_stale_duplicates_disagree() {
        let stale_identity = identity(
            json!({ "canonicalKey": "github.com/t3tools/old-repository", "name": "old-repository", "displayName": "Old Repository" }),
        );
        let stale = Project {
            identity: stale_identity,
            ..make("stale", "/work/t3code")
        };
        let fresh = Project {
            updated_at: "2026-07-02T00:00:00.000Z".into(),
            ..make("fresh", "/work/t3code/")
        };
        let sibling = make("sibling", "/work/t3code-2");
        let groups = group(
            &[stale, fresh, sibling],
            settings(SidebarProjectGroupingMode::Repository, &[]),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0]
                .members
                .iter()
                .map(|m| m.project.id.to_string())
                .collect::<Vec<_>>(),
            vec!["fresh", "sibling"]
        );
    }

    #[test]
    fn uses_the_freshest_identity_bearing_duplicate_when_the_winner_lacks_identity() {
        let stale_identity = identity(
            json!({ "canonicalKey": "github.com/t3tools/old-repository", "name": "old-repository", "displayName": "Old Repository" }),
        );
        let stale_identified = Project {
            identity: stale_identity,
            ..make("stale-identified", "/work/t3code")
        };
        let fresh_identified = Project {
            updated_at: "2026-07-02T00:00:00.000Z".into(),
            ..make("fresh-identified", "/work/t3code/")
        };
        let winner = Project {
            identity: None,
            updated_at: "2026-07-03T00:00:00.000Z".into(),
            ..make("winner", "/work/t3code")
        };
        let sibling = make("sibling", "/work/t3code-2");
        let groups = group(
            &[stale_identified, fresh_identified, winner, sibling],
            settings(SidebarProjectGroupingMode::Repository, &[]),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0]
                .members
                .iter()
                .map(|m| m.project.id.to_string())
                .collect::<Vec<_>>(),
            vec!["winner", "sibling"]
        );
    }

    fn pref(env_id: &str, project_id: &str) -> ScopedProjectRef {
        ScopedProjectRef {
            environment_id: env(env_id),
            project_id: ProjectId::new(project_id).unwrap(),
        }
    }

    #[test]
    fn sort_follows_the_group_order_keeping_ungrouped_projects_last_in_their_own_order() {
        let projects: Vec<(String, String)> = ["alpha", "zeta", "orphan-b", "beta", "orphan-a"]
            .iter()
            .map(|id| ("environment".to_owned(), (*id).to_owned()))
            .collect();
        let sorted = sort_projects_by_group_order(
            &projects,
            &[
                vec![pref("environment", "zeta")],
                vec![pref("environment", "beta"), pref("environment", "alpha")],
            ],
            Clone::clone,
        );
        assert_eq!(
            sorted.iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>(),
            vec!["zeta", "alpha", "beta", "orphan-b", "orphan-a"]
        );
    }

    #[test]
    fn sort_tells_the_same_project_id_on_two_environments_apart() {
        let projects = vec![
            ("environment".to_owned(), "shared".to_owned()),
            ("remote".to_owned(), "shared".to_owned()),
        ];
        let sorted = sort_projects_by_group_order(
            &projects,
            &[
                vec![pref("remote", "shared")],
                vec![pref("environment", "shared")],
            ],
            Clone::clone,
        );
        assert_eq!(
            sorted
                .iter()
                .map(|(env, _)| env.as_str())
                .collect::<Vec<_>>(),
            vec!["remote", "environment"]
        );
    }
}
