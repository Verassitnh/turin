//! Port of upstream/apps/web/src/onboarding/projectImport.logic.ts

use std::collections::HashMap;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use turin_contracts::agent_sessions::{AgentSessionProjectCandidate, AgentSessionProjectGit};
use turin_contracts::{EnvironmentId, ProjectId};
use turin_shared::js_string::locale_compare;

use crate::state::helpers::provider_types::parse_date_ms;
use crate::state::projects::find_project_by_path;

const RECENT_PROJECT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// One or two threads in a directory is usually a one-off question, not a project.
const DEFAULT_SELECTION_MIN_THREADS: u64 = 3;

/// Git identity as the scan reports it. Servers that predate the git scan omit
/// the field (`undefined` upstream), which is neither `null` nor an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateGit<'a> {
    /// `git` is absent.
    Missing,
    /// `git === null`: the directory is not a repository root.
    NotARepository,
    Repository(&'a AgentSessionProjectGit),
}

/// What the picker reads from a scan candidate (`AgentSessionProjectCandidate`
/// and anything that extends it).
pub trait OnboardingCandidate {
    fn path(&self) -> &str;
    fn title(&self) -> &str;
    fn git(&self) -> CandidateGit<'_>;
    fn thread_count(&self) -> u64;
    fn last_active_at(&self) -> Option<&str>;
    fn project_id(&self) -> Option<&ProjectId>;
}

impl OnboardingCandidate for AgentSessionProjectCandidate {
    fn path(&self) -> &str {
        self.path.as_str()
    }
    fn title(&self) -> &str {
        self.title.as_str()
    }
    fn git(&self) -> CandidateGit<'_> {
        match &self.git {
            None => CandidateGit::Missing,
            Some(None) => CandidateGit::NotARepository,
            Some(Some(git)) => CandidateGit::Repository(git),
        }
    }
    fn thread_count(&self) -> u64 {
        self.thread_count
    }
    fn last_active_at(&self) -> Option<&str> {
        self.last_active_at.as_deref()
    }
    fn project_id(&self) -> Option<&ProjectId> {
        self.project_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PartitionedProjects<T> {
    pub available: Vec<T>,
    pub recent: Vec<T>,
}

/// Existing projects still need their agent history imported, so every scan
/// candidate is offered. The default selection is narrower: git repositories
/// active in the last 30 days with enough threads to look like real work.
/// Servers that predate the git scan omit `git`; their candidates are treated
/// as repositories so old computers still get a useful default selection.
pub fn partition_onboarding_projects<T: OnboardingCandidate + Clone>(
    candidates: &[T],
    now_ms: i64,
) -> PartitionedProjects<T> {
    let cutoff = now_ms - RECENT_PROJECT_WINDOW_MS;
    PartitionedProjects {
        available: candidates.to_vec(),
        recent: candidates
            .iter()
            .filter(|candidate| {
                if candidate.git() == CandidateGit::NotARepository {
                    return false;
                }
                if candidate.thread_count() < DEFAULT_SELECTION_MIN_THREADS {
                    return false;
                }
                let Some(last_active_at) = candidate.last_active_at() else {
                    return false;
                };
                // An unparseable date is `NaN` upstream, which fails both comparisons.
                parse_date_ms(last_active_at)
                    .is_some_and(|last_active| last_active >= cutoff && last_active <= now_ms)
            })
            .cloned()
            .collect(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OnboardingProjectGroup<T> {
    /// Stable identity for collapse state and list keys.
    pub key: String,
    /// GitHub `owner/name`, or the checkout's folder name when the origin is elsewhere.
    pub label: String,
    pub repository: Option<String>,
    pub candidates: Vec<T>,
    pub thread_count: u64,
    pub last_active_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupedProjects<T> {
    pub repositories: Vec<OnboardingProjectGroup<T>>,
    pub other: Vec<T>,
}

fn latest_activity(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (None, right) => right.map(str::to_owned),
        (left, None) => left.map(str::to_owned),
        (Some(left), Some(right)) => Some(if left > right { left } else { right }.to_owned()),
    }
}

/// Group scan candidates for the onboarding picker. Clones of one repository
/// share a group keyed by their normalized origin URL. Repositories without an
/// origin get a group each, as do candidates from servers that do not report
/// git identity. Directories that are not git repositories are returned
/// separately so the UI can fold them away by default. Groups sort by most
/// recent activity, newest first.
pub fn group_onboarding_projects<T: OnboardingCandidate + Clone>(
    candidates: &[T],
) -> GroupedProjects<T> {
    let mut groups: IndexMap<String, OnboardingProjectGroup<T>> = IndexMap::new();
    let mut other = Vec::new();

    for candidate in candidates {
        let git = match candidate.git() {
            CandidateGit::NotARepository => {
                other.push(candidate.clone());
                continue;
            }
            CandidateGit::Missing => None,
            CandidateGit::Repository(git) => Some(git),
        };
        let remote_key = git.and_then(|git| git.remote_key.as_deref());
        let repository = git.and_then(|git| git.repository.clone());
        let key = match remote_key {
            None => format!("path:{}", candidate.path()),
            Some(remote_key) => format!("remote:{remote_key}"),
        };
        match groups.get_mut(&key) {
            None => {
                groups.insert(
                    key.clone(),
                    OnboardingProjectGroup {
                        key,
                        label: repository
                            .clone()
                            .unwrap_or_else(|| candidate.title().to_owned()),
                        repository,
                        candidates: vec![candidate.clone()],
                        thread_count: candidate.thread_count(),
                        last_active_at: candidate.last_active_at().map(str::to_owned),
                    },
                );
            }
            Some(existing) => {
                existing.candidates.push(candidate.clone());
                existing.thread_count += candidate.thread_count();
                existing.last_active_at = latest_activity(
                    existing.last_active_at.as_deref(),
                    candidate.last_active_at(),
                );
            }
        }
    }

    let mut repositories: Vec<_> = groups.into_values().collect();
    repositories.sort_by(|left, right| {
        if left.last_active_at == right.last_active_at {
            return locale_compare(&left.label, &right.label);
        }
        match (&left.last_active_at, &right.last_active_at) {
            (None, _) => std::cmp::Ordering::Greater,
            (_, None) => std::cmp::Ordering::Less,
            (Some(left), Some(right)) => right.cmp(left),
        }
    });

    GroupedProjects {
        repositories,
        other,
    }
}

/// The client's view of a project, as the id lookup reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnboardingKnownProject {
    pub id: ProjectId,
    pub environment_id: EnvironmentId,
    pub workspace_root: String,
}

/// Use the server's project match before the client snapshot, which can lag behind the scan.
pub fn resolve_onboarding_project_id(
    projects: &[OnboardingKnownProject],
    environment_id: &EnvironmentId,
    candidate: &impl OnboardingCandidate,
) -> Option<ProjectId> {
    if let Some(project_id) = candidate.project_id() {
        return Some(project_id.clone());
    }
    let environment_projects: Vec<&OnboardingKnownProject> = projects
        .iter()
        .filter(|project| &project.environment_id == environment_id)
        .collect();
    find_project_by_path(&environment_projects, candidate.path(), |project| {
        Some(project.workspace_root.clone())
    })
    .map(|project| project.id.clone())
}

/// Prefer a selected project with imported history, then a completed empty import.
pub fn resolve_onboarding_landing_project<T: Clone>(
    selection: &[String],
    projects_with_imported_history: &HashMap<String, T>,
    completed_projects: &HashMap<String, T>,
) -> Option<T> {
    for path in selection {
        if let Some(project) = projects_with_imported_history.get(path) {
            return Some(project.clone());
        }
    }
    for path in selection {
        if let Some(project) = completed_projects.get(path) {
            return Some(project.clone());
        }
    }
    None
}

static DRIVE_ROOT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]:[\\/]+$").expect("static pattern"));
static UNC_SHARE_ROOT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\\/]{2}[^\\/]+[\\/][^\\/]+[\\/]*$").expect("static pattern"));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitProjectPath {
    pub name: String,
    pub parent: String,
}

/// Splits a scan path into the folder name and the directory above it (with
/// its trailing separator), so a picker row can keep the name visible and let
/// the shared prefix truncate. Handles both separators because candidates
/// come from any environment.
pub fn split_onboarding_project_path(path: &str) -> SplitProjectPath {
    let whole = || SplitProjectPath {
        name: path.to_owned(),
        parent: String::new(),
    };
    if DRIVE_ROOT.is_match(path) || UNC_SHARE_ROOT.is_match(path) {
        return whole();
    }
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return whole();
    }
    // `lastIndexOf` of either separator; both are one byte.
    let Some(separator_index) = trimmed.rfind(['/', '\\']) else {
        return SplitProjectPath {
            name: trimmed.to_owned(),
            parent: String::new(),
        };
    };
    SplitProjectPath {
        name: trimmed[separator_index + 1..].to_owned(),
        parent: trimmed[..=separator_index].to_owned(),
    }
}

/// Paths identify projects only within the computer that owns them.
pub fn onboarding_project_key(environment_id: &EnvironmentId, path: &str) -> String {
    serde_json::to_string(&[environment_id.as_str(), path]).expect("strings serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::agent_sessions::AgentSessionSource;

    fn now() -> i64 {
        parse_date_ms("2026-08-22T12:00:00.000Z").unwrap()
    }

    fn name(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn candidate(path: &str) -> AgentSessionProjectCandidate {
        AgentSessionProjectCandidate {
            title: name(path.rsplit('/').next().unwrap_or(path)),
            path: name(path),
            project_id: None,
            sources: vec![AgentSessionSource::Codex],
            thread_count: 3,
            last_active_at: Some("2026-08-20T12:00:00.000Z".to_owned()),
            already_imported: false,
            git: Some(Some(AgentSessionProjectGit {
                remote_key: None,
                repository: None,
            })),
        }
    }

    fn github(repository: &str) -> Option<Option<AgentSessionProjectGit>> {
        Some(Some(AgentSessionProjectGit {
            remote_key: Some(format!("github.com/{}", repository.to_lowercase())),
            repository: Some(repository.to_owned()),
        }))
    }

    fn with(
        mut base: AgentSessionProjectCandidate,
        edit: impl FnOnce(&mut AgentSessionProjectCandidate),
    ) -> AgentSessionProjectCandidate {
        edit(&mut base);
        base
    }

    fn paths(candidates: &[AgentSessionProjectCandidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.path.as_str()).collect()
    }

    #[test]
    fn keeps_existing_projects_available_for_thread_history_import() {
        let imported = with(candidate("/projects/current"), |c| {
            c.already_imported = true
        });
        let available = candidate("/projects/other");
        let result = partition_onboarding_projects(&[imported.clone(), available.clone()], now());
        assert_eq!(result.available, vec![imported.clone(), available.clone()]);
        assert_eq!(result.recent, vec![imported, available]);
    }

    #[test]
    fn keeps_projects_older_than_30_days_out_of_the_default_selection() {
        let recent = candidate("/projects/recent");
        let older = with(candidate("/projects/older"), |c| {
            c.last_active_at = Some("2026-07-01T12:00:00.000Z".to_owned());
        });
        let result = partition_onboarding_projects(&[recent.clone(), older.clone()], now());
        assert_eq!(result.available, vec![recent.clone(), older]);
        assert_eq!(result.recent, vec![recent]);
    }

    #[test]
    fn keeps_future_activity_out_of_the_default_selection() {
        let recent = candidate("/projects/recent");
        let future = with(candidate("/projects/future"), |c| {
            c.last_active_at = Some("2026-08-23T12:00:00.000Z".to_owned());
        });
        let result = partition_onboarding_projects(&[recent.clone(), future.clone()], now());
        assert_eq!(result.available, vec![recent.clone(), future]);
        assert_eq!(result.recent, vec![recent]);
    }

    #[test]
    fn keeps_non_git_folders_and_thin_histories_out_of_the_default_selection() {
        let repo = candidate("/projects/repo");
        let folder = with(candidate("/projects/folder"), |c| c.git = Some(None));
        let thin = with(candidate("/projects/thin"), |c| c.thread_count = 2);
        let result = partition_onboarding_projects(&[repo.clone(), folder, thin], now());
        assert_eq!(result.recent, vec![repo]);
    }

    #[test]
    fn selects_candidates_from_servers_that_do_not_report_git_identity() {
        let legacy = with(candidate("/projects/legacy"), |c| c.git = None);
        let result = partition_onboarding_projects(std::slice::from_ref(&legacy), now());
        assert_eq!(result.recent, vec![legacy]);
    }

    #[test]
    fn groups_clones_by_origin_keeps_local_repos_separate_and_folds_non_git_folders_away() {
        let main = with(candidate("/code/t3code"), |c| {
            c.git = github("pingdotgg/t3code");
            c.thread_count = 79;
            c.last_active_at = Some("2026-08-21T12:00:00.000Z".to_owned());
        });
        let clone = with(candidate("/code/clones/t3code-2"), |c| {
            c.git = github("pingdotgg/t3code");
            c.thread_count = 13;
            c.last_active_at = Some("2026-08-10T12:00:00.000Z".to_owned());
        });
        let older = with(candidate("/code/fleet"), |c| {
            c.git = github("t3dotgg/fleet");
            c.thread_count = 295;
            c.last_active_at = Some("2026-08-22T00:00:00.000Z".to_owned());
        });
        let local = with(candidate("/code/scratch-repo"), |c| {
            c.title = name("scratch-repo");
        });
        let folder = with(candidate("/tmp/notes"), |c| c.git = Some(None));

        let grouped = group_onboarding_projects(&[main, clone, older, local, folder.clone()]);
        assert_eq!(grouped.other, vec![folder]);
        let summary: Vec<_> = grouped
            .repositories
            .iter()
            .map(|group| {
                (
                    group.label.as_str(),
                    paths(&group.candidates),
                    group.thread_count,
                    group.last_active_at.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "t3dotgg/fleet",
                    vec!["/code/fleet"],
                    295,
                    Some("2026-08-22T00:00:00.000Z")
                ),
                (
                    "pingdotgg/t3code",
                    vec!["/code/t3code", "/code/clones/t3code-2"],
                    92,
                    Some("2026-08-21T12:00:00.000Z")
                ),
                (
                    "scratch-repo",
                    vec!["/code/scratch-repo"],
                    3,
                    Some("2026-08-20T12:00:00.000Z")
                ),
            ]
        );
    }

    #[test]
    fn lists_candidates_without_git_identity_as_standalone_repositories() {
        let legacy = with(candidate("/code/legacy"), |c| {
            c.title = name("legacy");
            c.git = None;
        });
        let grouped = group_onboarding_projects(&[legacy]);
        assert!(grouped.other.is_empty());
        let labels: Vec<_> = grouped
            .repositories
            .iter()
            .map(|group| (group.label.as_str(), group.repository.clone()))
            .collect();
        assert_eq!(labels, vec![("legacy", None)]);
    }

    fn env(value: &str) -> EnvironmentId {
        EnvironmentId::new(value).unwrap()
    }

    fn project_id(value: &str) -> ProjectId {
        ProjectId::new(value).unwrap()
    }

    fn known(id: &str, environment: &str, root: &str) -> OnboardingKnownProject {
        OnboardingKnownProject {
            id: project_id(id),
            environment_id: env(environment),
            workspace_root: root.to_owned(),
        }
    }

    #[test]
    fn uses_the_scanned_project_id_before_the_project_reaches_the_client() {
        let scanned = with(candidate("/projects/repo"), |c| {
            c.project_id = Some(project_id("local-project"));
        });
        assert_eq!(
            resolve_onboarding_project_id(&[], &env("local"), &scanned),
            Some(project_id("local-project"))
        );
        // Even when the client still has an older project at that root.
        let stale = [known("stale-project", "local", "/projects/repo")];
        assert_eq!(
            resolve_onboarding_project_id(&stale, &env("local"), &scanned),
            Some(project_id("local-project"))
        );
    }

    #[test]
    fn returns_none_to_create_a_project_when_neither_scan_nor_client_has_an_id() {
        assert_eq!(
            resolve_onboarding_project_id(&[], &env("local"), &candidate("/projects/new")),
            None
        );
    }

    #[test]
    fn finds_an_existing_project_by_normalized_root_in_the_target_environment() {
        let projects = [
            known("remote-project", "remote", "C:\\Work\\Repo"),
            known("local-project", "local", "C:\\Work\\Repo\\"),
        ];
        assert_eq!(
            resolve_onboarding_project_id(&projects, &env("local"), &candidate("c:/work/repo")),
            Some(project_id("local-project"))
        );
    }

    #[test]
    fn does_not_reuse_a_project_from_another_environment() {
        let projects = [known("remote-project", "remote", "/projects/repo")];
        assert_eq!(
            resolve_onboarding_project_id(&projects, &env("local"), &candidate("/projects/repo")),
            None
        );
    }

    #[test]
    fn finds_the_current_root_owner_when_the_scan_has_no_project_id() {
        let projects = [
            known("local-project", "local", "/projects/other"),
            known("recreated-project", "local", "/projects/repo"),
        ];
        assert_eq!(
            resolve_onboarding_project_id(&projects, &env("local"), &candidate("/projects/repo")),
            Some(project_id("recreated-project"))
        );
        let moved = [known("local-project", "local", "/projects/moved")];
        assert_eq!(
            resolve_onboarding_project_id(&moved, &env("local"), &candidate("/projects/repo")),
            None
        );
    }

    fn map(entries: &[(&str, &'static str)]) -> HashMap<String, &'static str> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), *value))
            .collect()
    }

    fn selection(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn landing_project_skips_a_failed_first_project_for_a_later_one_with_history() {
        assert_eq!(
            resolve_onboarding_landing_project(
                &selection(&["/projects/failed", "/projects/imported"]),
                &map(&[("/projects/imported", "imported")]),
                &map(&[("/projects/imported", "imported")]),
            ),
            Some("imported")
        );
    }

    #[test]
    fn landing_project_prefers_a_partial_first_import_that_added_history() {
        assert_eq!(
            resolve_onboarding_landing_project(
                &selection(&["/projects/partial", "/projects/complete"]),
                &map(&[("/projects/partial", "partial")]),
                &map(&[("/projects/complete", "complete")]),
            ),
            Some("partial")
        );
    }

    #[test]
    fn landing_project_uses_a_completed_zero_history_project_when_nothing_added_history() {
        assert_eq!(
            resolve_onboarding_landing_project(
                &selection(&["/projects/empty", "/projects/failed"]),
                &map(&[]),
                &map(&[("/projects/empty", "empty")]),
            ),
            Some("empty")
        );
    }

    #[test]
    fn landing_project_ignores_cached_successes_outside_the_current_selection() {
        assert_eq!(
            resolve_onboarding_landing_project(
                &selection(&["/projects/current"]),
                &map(&[("/projects/previous", "previous")]),
                &map(&[
                    ("/projects/previous", "previous"),
                    ("/projects/current", "current")
                ]),
            ),
            Some("current")
        );
    }

    #[test]
    fn identical_paths_on_different_computers_stay_independent() {
        let first = onboarding_project_key(&env("first"), "/code/app");
        let second = onboarding_project_key(&env("second"), "/code/app");
        assert_ne!(first, second);
        assert_eq!(first, r#"["first","/code/app"]"#);
        let mut completed: HashMap<String, &'static str> =
            HashMap::from([(first.clone(), "first-project")]);
        assert!(!completed.contains_key(&second));
        assert_eq!(
            resolve_onboarding_landing_project(&[second.clone()], &completed, &completed),
            None
        );
        completed.insert(second.clone(), "second-project");
        assert_eq!(
            resolve_onboarding_landing_project(&[second, first], &completed, &completed),
            Some("second-project")
        );
    }

    fn split(path: &str) -> (String, String) {
        let split = split_onboarding_project_path(path);
        (split.name, split.parent)
    }

    fn pair(name: &str, parent: &str) -> (String, String) {
        (name.to_owned(), parent.to_owned())
    }

    #[test]
    fn splitting_keeps_sibling_checkouts_that_share_a_prefix_distinguishable_by_name() {
        let left = split("/Users/me/Projects/clients/acme/t3code");
        let right = split("/Users/me/Projects/clients/acme/t3code-worktree");
        assert_eq!(left, pair("t3code", "/Users/me/Projects/clients/acme/"));
        assert_eq!(
            right,
            pair("t3code-worktree", "/Users/me/Projects/clients/acme/")
        );
    }

    #[test]
    fn splitting_handles_windows_paths_and_ignores_a_trailing_separator() {
        assert_eq!(
            split("C:\\Users\\me\\t3code\\"),
            pair("t3code", "C:\\Users\\me\\")
        );
    }

    #[test]
    fn splitting_keeps_a_root_or_bare_name_whole() {
        assert_eq!(split("/"), pair("/", ""));
        assert_eq!(split("/t3code"), pair("t3code", "/"));
        assert_eq!(split("t3code"), pair("t3code", ""));
        assert_eq!(split("t3code/"), pair("t3code", ""));
    }

    #[test]
    fn splitting_keeps_filesystem_roots_whole() {
        for path in [
            "C:\\",
            "C:/",
            "\\\\server\\share",
            "\\\\server\\share\\",
            "//server/share/",
        ] {
            assert_eq!(split(path), pair(path, ""), "{path}");
        }
    }

    #[test]
    fn splitting_a_folder_below_a_network_share() {
        assert_eq!(
            split("\\\\server\\share\\project\\"),
            pair("project", "\\\\server\\share\\")
        );
    }
}
