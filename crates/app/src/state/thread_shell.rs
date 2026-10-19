//! Port of upstream/packages/client-runtime/src/state/threadShell.ts
//!
//! The atom families become [`ShellView`]: everything the sidebar and the
//! navigation read from one shell snapshot, built once per snapshot with
//! structural sharing (a presented row is reused while its source row and its
//! pending-subagent request are unchanged).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::state::contracts::{
    EnvironmentId, OrchestrationV2CreationSource, OrchestrationV2LineageRelationship,
    OrchestrationV2RuntimeRequestKind, OrchestrationV2ThreadShell, ProjectId, ThreadId,
};
use crate::state::models::{
    EnvironmentProject, EnvironmentThreadShell, PendingSubagentRequest, present_thread_shell,
    scope_project,
};
use crate::state::shell_reducer::{ProjectShellRow, ShellSnapshot, ThreadShellRow};

/// An async question outlives the subagent's run, but once that run ends the
/// parent is no longer waiting on the answer, so it stops counting as input.
/// The activity run stays set when a follow-up is queued behind a blocked run;
/// older servers omit it, so fall back to the latest run's status.
fn is_child_run_live(thread: &OrchestrationV2ThreadShell) -> bool {
    use crate::state::contracts::OrchestrationV2ShellThreadStatus as Status;
    match &thread.activity_run_status {
        Some(activity) => activity.is_some(),
        None => matches!(
            thread.status,
            Status::Preparing | Status::Starting | Status::Running | Status::Waiting
        ),
    }
}

/// `isProviderNativeSubagentThread`, for a shell row: a subagent the provider
/// spawned on its own, which owns its conversation.
fn is_provider_native_subagent_thread_shell(thread: &OrchestrationV2ThreadShell) -> bool {
    thread.lineage.relationship_to_parent == Some(OrchestrationV2LineageRelationship::Subagent)
        && thread.creation_source == OrchestrationV2CreationSource::Provider
}

/// What a subagent's live run waits on the user for. Provider-native
/// subagents ask on their parent, so they are skipped.
pub fn subagent_request(thread: &OrchestrationV2ThreadShell) -> Option<PendingSubagentRequest> {
    let kind = thread.pending_runtime_request.as_ref()?.kind;
    if kind == OrchestrationV2RuntimeRequestKind::AuthRefresh
        || thread.lineage.relationship_to_parent
            != Some(OrchestrationV2LineageRelationship::Subagent)
        || is_provider_native_subagent_thread_shell(thread)
        || !is_child_run_live(thread)
    {
        return None;
    }
    Some(if kind == OrchestrationV2RuntimeRequestKind::UserInput {
        PendingSubagentRequest::UserInput
    } else {
        PendingSubagentRequest::Approval
    })
}

/// Threads above a subagent that waits on the user, by what it waits on. Every
/// thread up to the visible one carries it, because subagent rows are hidden.
/// An approval outranks a question.
pub fn pending_subagent_requests_by_thread(
    threads: &[ThreadShellRow],
) -> HashMap<ThreadId, PendingSubagentRequest> {
    let mut requests: HashMap<ThreadId, PendingSubagentRequest> = HashMap::new();
    let mut by_id: Option<HashMap<&ThreadId, &ThreadShellRow>> = None;
    for child in threads {
        let Some(request) = subagent_request(child) else {
            continue;
        };
        let by_id = by_id
            .get_or_insert_with(|| threads.iter().map(|thread| (&thread.id, thread)).collect());
        let mut seen: HashSet<ThreadId> = HashSet::from([child.id.clone()]);
        let mut current: Option<&ThreadShellRow> = Some(child);
        while let Some(thread) = current {
            if thread.lineage.relationship_to_parent
                != Some(OrchestrationV2LineageRelationship::Subagent)
            {
                break;
            }
            let Some(parent_id) = thread.lineage.parent_thread_id.clone() else {
                break;
            };
            if seen.contains(&parent_id) {
                break;
            }
            seen.insert(parent_id.clone());
            if requests.get(&parent_id) != Some(&PendingSubagentRequest::Approval) {
                requests.insert(parent_id.clone(), request);
            }
            current = by_id.get(&parent_id).copied();
        }
    }
    requests
}

/// Banner title for the subagents `child_thread_inputs` lists.
pub fn child_thread_inputs_title(children: &[Arc<EnvironmentThreadShell>]) -> String {
    let approvals = children
        .iter()
        .filter(|child| child.has_pending_approvals)
        .count();
    let need = if approvals == 0 {
        "input"
    } else if approvals == children.len() {
        "approval"
    } else {
        "you"
    };
    if children.len() == 1 {
        format!("Subagent needs {need}")
    } else {
        format!("{} subagents need {need}", children.len())
    }
}

/// Reuses the presented value of a shell row while the row and its request are unchanged.
#[derive(Default)]
struct PresentationCache {
    threads: HashMap<ThreadId, CachedThread>,
}

struct CachedThread {
    source: ThreadShellRow,
    request: Option<PendingSubagentRequest>,
    value: Arc<EnvironmentThreadShell>,
}

/// Everything read from one shell snapshot, with presented rows shared across
/// snapshots that did not change them.
#[derive(Clone, Debug)]
pub struct ShellView {
    pub environment_id: EnvironmentId,
    pub snapshot: Arc<ShellSnapshot>,
    /// `environmentProjectsAtom` presented as `EnvironmentProject`.
    pub projects: Arc<Vec<Arc<EnvironmentProject>>>,
    /// `threadShellsAtom`: every active thread, subagents included.
    pub threads: Arc<Vec<Arc<EnvironmentThreadShell>>>,
    /// `navigationThreadShellsAtom`: unarchived, non-subagent threads.
    pub navigation_threads: Arc<Vec<Arc<EnvironmentThreadShell>>>,
    pub archived_threads: Arc<Vec<Arc<EnvironmentThreadShell>>>,
    pub pending_subagent_requests: Arc<HashMap<ThreadId, PendingSubagentRequest>>,
    thread_index: Arc<HashMap<ThreadId, usize>>,
    project_index: Arc<HashMap<ProjectId, usize>>,
}

impl ShellView {
    pub fn thread(&self, thread_id: &ThreadId) -> Option<&Arc<EnvironmentThreadShell>> {
        self.thread_index
            .get(thread_id)
            .map(|index| &self.threads[*index])
    }

    pub fn project(&self, project_id: &ProjectId) -> Option<&Arc<EnvironmentProject>> {
        self.project_index
            .get(project_id)
            .map(|index| &self.projects[*index])
    }

    /// The project a thread belongs to (its icon heads the chat header).
    pub fn project_of_thread(&self, thread_id: &ThreadId) -> Option<&Arc<EnvironmentProject>> {
        let thread = self.thread(thread_id).or_else(|| {
            self.archived_threads
                .iter()
                .find(|row| &row.id == thread_id)
        })?;
        self.project(&thread.project_id)
    }

    /// `environmentThreadRefsByProjectAtom` values, as shells in list order.
    pub fn threads_for_project(&self, project_id: &ProjectId) -> Vec<Arc<EnvironmentThreadShell>> {
        self.threads
            .iter()
            .filter(|thread| thread.project_id == *project_id)
            .cloned()
            .collect()
    }

    /// `threadShellsForProjectRefsAtom`: shells of the listed projects, deduplicated.
    pub fn threads_for_projects(
        &self,
        project_ids: &[ProjectId],
    ) -> Vec<Arc<EnvironmentThreadShell>> {
        let mut seen: HashSet<&ThreadId> = HashSet::new();
        let mut out = Vec::new();
        for project_id in project_ids {
            for thread in self.threads.iter().filter(|t| t.project_id == *project_id) {
                if seen.insert(&thread.id) {
                    out.push(Arc::clone(thread));
                }
            }
        }
        out
    }

    /// `childThreadInputsAtom`: subagents below `root` (breadth first) that wait on the user.
    pub fn child_thread_inputs(&self, root: &ThreadId) -> Vec<Arc<EnvironmentThreadShell>> {
        let mut children: HashMap<&ThreadId, Vec<&ThreadShellRow>> = HashMap::new();
        for thread in self.snapshot.threads.iter() {
            let Some(parent) = &thread.lineage.parent_thread_id else {
                continue;
            };
            if thread.lineage.relationship_to_parent
                != Some(OrchestrationV2LineageRelationship::Subagent)
            {
                continue;
            }
            children.entry(parent).or_default().push(thread);
        }
        let mut seen: HashSet<&ThreadId> = HashSet::from([root]);
        let mut pending: Vec<&ThreadId> = vec![root];
        let mut out = Vec::new();
        let mut index = 0;
        while index < pending.len() {
            let parent = pending[index];
            index += 1;
            for child in children.get(parent).map(Vec::as_slice).unwrap_or(&[]) {
                if !seen.insert(&child.id) {
                    continue;
                }
                pending.push(&child.id);
                if subagent_request(child).is_some()
                    && let Some(presented) = self.thread(&child.id)
                {
                    out.push(Arc::clone(presented));
                }
            }
        }
        out
    }
}

/// Builds [`ShellView`]s, remembering the previous one so unchanged rows keep
/// their presented `Arc`.
#[derive(Default)]
pub struct ShellViewBuilder {
    cache: PresentationCache,
    projects: HashMap<ProjectId, (ProjectShellRow, bool, Arc<EnvironmentProject>)>,
    previous: Option<ShellView>,
    previous_scratch_workspace_root: Option<String>,
}

impl ShellViewBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// `scratch_workspace_root` is the environment's Scratch project folder, if any.
    pub fn build(
        &mut self,
        environment_id: &EnvironmentId,
        snapshot: &Arc<ShellSnapshot>,
        scratch_workspace_root: Option<&str>,
    ) -> ShellView {
        if let Some(previous) = &self.previous
            && Arc::ptr_eq(&previous.snapshot, snapshot)
            && previous.environment_id == *environment_id
            && self.previous_scratch_workspace_root.as_deref() == scratch_workspace_root
        {
            return previous.clone();
        }
        let requests = pending_subagent_requests_by_thread(&snapshot.threads);

        let present = |row: &ThreadShellRow, next: &mut HashMap<ThreadId, CachedThread>| {
            let request = requests.get(&row.id).copied();
            if let Some(cached) = self.cache.threads.get(&row.id)
                && Arc::ptr_eq(&cached.source, row)
                && cached.request == request
            {
                next.insert(
                    row.id.clone(),
                    CachedThread {
                        source: Arc::clone(&cached.source),
                        request,
                        value: Arc::clone(&cached.value),
                    },
                );
                return Arc::clone(&cached.value);
            }
            let value = Arc::new(present_thread_shell(environment_id, row, request));
            next.insert(
                row.id.clone(),
                CachedThread {
                    source: Arc::clone(row),
                    request,
                    value: Arc::clone(&value),
                },
            );
            value
        };

        let mut next_cache: HashMap<ThreadId, CachedThread> = HashMap::new();
        let threads: Vec<Arc<EnvironmentThreadShell>> = snapshot
            .threads
            .iter()
            .map(|row| present(row, &mut next_cache))
            .collect();
        let archived_threads: Vec<Arc<EnvironmentThreadShell>> = snapshot
            .archived_threads
            .iter()
            .map(|row| present(row, &mut next_cache))
            .collect();
        self.cache.threads = next_cache;

        let navigation_threads = snapshot
            .threads
            .iter()
            .zip(&threads)
            .filter(|(row, _)| {
                row.archived_at.is_none()
                    && row.lineage.relationship_to_parent
                        != Some(OrchestrationV2LineageRelationship::Subagent)
            })
            .map(|(_, presented)| Arc::clone(presented))
            .collect::<Vec<_>>();

        let mut next_projects = HashMap::new();
        let projects: Vec<Arc<EnvironmentProject>> = snapshot
            .projects
            .iter()
            .map(|row| {
                let scratch = crate::state::projects::is_scratch_project(
                    row.workspace_root.as_str(),
                    scratch_workspace_root,
                );
                let value = match self.projects.get(&row.id) {
                    Some((source, was_scratch, value))
                        if Arc::ptr_eq(source, row) && *was_scratch == scratch =>
                    {
                        Arc::clone(value)
                    }
                    _ => {
                        let mut project = scope_project(environment_id, row);
                        project.is_scratch = scratch;
                        Arc::new(project)
                    }
                };
                next_projects.insert(
                    row.id.clone(),
                    (Arc::clone(row), scratch, Arc::clone(&value)),
                );
                value
            })
            .collect();
        self.projects = next_projects;

        let thread_index = snapshot
            .threads
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id.clone(), index))
            .collect();
        let project_index = snapshot
            .projects
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id.clone(), index))
            .collect();

        // Keep the previous lists when every element is the same object.
        let reuse = |previous: Option<&Arc<Vec<Arc<EnvironmentThreadShell>>>>,
                     next: Vec<Arc<EnvironmentThreadShell>>| {
            match previous {
                Some(previous)
                    if previous.len() == next.len()
                        && previous.iter().zip(&next).all(|(a, b)| Arc::ptr_eq(a, b)) =>
                {
                    Arc::clone(previous)
                }
                _ => Arc::new(next),
            }
        };
        let previous = self.previous.as_ref();
        let view = ShellView {
            environment_id: environment_id.clone(),
            snapshot: Arc::clone(snapshot),
            projects: match previous {
                Some(previous)
                    if previous.projects.len() == projects.len()
                        && previous
                            .projects
                            .iter()
                            .zip(&projects)
                            .all(|(a, b)| Arc::ptr_eq(a, b)) =>
                {
                    Arc::clone(&previous.projects)
                }
                _ => Arc::new(projects),
            },
            threads: reuse(previous.map(|view| &view.threads), threads),
            navigation_threads: reuse(
                previous.map(|view| &view.navigation_threads),
                navigation_threads,
            ),
            archived_threads: reuse(
                previous.map(|view| &view.archived_threads),
                archived_threads,
            ),
            pending_subagent_requests: Arc::new(requests),
            thread_index: Arc::new(thread_index),
            project_index: Arc::new(project_index),
        };
        self.previous = Some(view.clone());
        self.previous_scratch_workspace_root = scratch_workspace_root.map(str::to_owned);
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2ThreadShell;
    use crate::state::test_fixtures::{from_json, v2_project, v2_thread_shell};
    use serde_json::{Value, json};

    fn shell(id: &str, patch: Value) -> ThreadShellRow {
        let mut value = serde_json::to_value(v2_thread_shell()).unwrap();
        value["id"] = json!(id);
        value["title"] = json!(id);
        value["lineage"] =
            json!({ "rootThreadId": id, "parentThreadId": null, "relationshipToParent": null });
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        Arc::new(from_json::<OrchestrationV2ThreadShell>(value))
    }

    fn subagent_of(id: &str, parent: &str, request_kind: Option<&str>) -> ThreadShellRow {
        shell(
            id,
            json!({
                "lineage": { "rootThreadId": parent, "parentThreadId": parent, "relationshipToParent": "subagent" },
                "creationSource": "mcp",
                "status": "running",
                "pendingRuntimeRequest": request_kind.map(|kind| json!({
                    "id": format!("request-{id}"), "kind": kind, "createdAt": "2026-06-20T00:00:00.000Z",
                })),
            }),
        )
    }

    fn snapshot(threads: Vec<ThreadShellRow>) -> Arc<ShellSnapshot> {
        Arc::new(ShellSnapshot {
            schema_version: crate::state::contracts::PositiveInt::new(1).unwrap(),
            snapshot_sequence: 1,
            threads: Arc::new(threads),
            archived_threads: Arc::new(vec![]),
            projects: Arc::new(vec![Arc::new(v2_project())]),
        })
    }

    #[test]
    fn carries_a_subagents_request_up_every_hidden_ancestor_with_approvals_outranking_questions() {
        let threads = vec![
            shell("root", json!({})),
            subagent_of("middle", "root", None),
            subagent_of("asks", "middle", Some("user_input")),
            subagent_of("approves", "middle", Some("command")),
        ];
        let requests = pending_subagent_requests_by_thread(&threads);
        assert_eq!(
            requests.get(&ThreadId::new("middle").unwrap()),
            Some(&PendingSubagentRequest::Approval)
        );
        assert_eq!(
            requests.get(&ThreadId::new("root").unwrap()),
            Some(&PendingSubagentRequest::Approval)
        );
        assert_eq!(requests.get(&ThreadId::new("asks").unwrap()), None);
    }

    #[test]
    fn a_question_alone_reaches_the_root_and_a_finished_child_run_stops_counting() {
        let threads = vec![
            shell("root", json!({})),
            subagent_of("asks", "root", Some("user_input")),
        ];
        let requests = pending_subagent_requests_by_thread(&threads);
        assert_eq!(
            requests.get(&ThreadId::new("root").unwrap()),
            Some(&PendingSubagentRequest::UserInput)
        );

        let finished = shell(
            "done",
            json!({
                "lineage": { "rootThreadId": "root", "parentThreadId": "root", "relationshipToParent": "subagent" },
                "creationSource": "mcp", "status": "completed", "activityRunStatus": null,
                "pendingRuntimeRequest": { "id": "r", "kind": "user_input", "createdAt": "2026-06-20T00:00:00.000Z" },
            }),
        );
        assert!(
            pending_subagent_requests_by_thread(&[shell("root", json!({})), finished]).is_empty()
        );
    }

    #[test]
    fn provider_native_subagents_and_auth_refresh_do_not_surface() {
        let native = shell(
            "native",
            json!({
                "lineage": { "rootThreadId": "root", "parentThreadId": "root", "relationshipToParent": "subagent" },
                "creationSource": "provider", "status": "running",
                "pendingRuntimeRequest": { "id": "r", "kind": "command", "createdAt": "2026-06-20T00:00:00.000Z" },
            }),
        );
        assert!(
            pending_subagent_requests_by_thread(&[shell("root", json!({})), native]).is_empty()
        );
        let auth = subagent_of("auth", "root", Some("auth_refresh"));
        assert!(pending_subagent_requests_by_thread(&[shell("root", json!({})), auth]).is_empty());
    }

    #[test]
    fn survives_parent_cycles() {
        let a = subagent_of("a", "b", Some("user_input"));
        let b = subagent_of("b", "a", None);
        // Terminates; both end up carrying the request once.
        let requests = pending_subagent_requests_by_thread(&[a, b]);
        assert_eq!(
            requests.get(&ThreadId::new("b").unwrap()),
            Some(&PendingSubagentRequest::UserInput)
        );
    }

    #[test]
    fn titles_the_child_inputs_banner() {
        let child = |approval: bool| {
            let mut thread = EnvironmentThreadShell::new(
                EnvironmentId::new("local").unwrap(),
                ThreadId::new("t").unwrap(),
                ProjectId::new("p").unwrap(),
                "t",
                "2026-06-20T00:00:00.000Z",
            );
            thread.has_pending_approvals = approval;
            Arc::new(thread)
        };
        assert_eq!(
            child_thread_inputs_title(&[child(false)]),
            "Subagent needs input"
        );
        assert_eq!(
            child_thread_inputs_title(&[child(true)]),
            "Subagent needs approval"
        );
        assert_eq!(
            child_thread_inputs_title(&[child(true), child(true)]),
            "2 subagents need approval"
        );
        assert_eq!(
            child_thread_inputs_title(&[child(true), child(false)]),
            "2 subagents need you"
        );
        assert_eq!(
            child_thread_inputs_title(&[child(false), child(false)]),
            "2 subagents need input"
        );
    }

    #[test]
    fn a_thread_finds_its_own_project() {
        let environment = EnvironmentId::new("local").unwrap();
        let root = shell("root", json!({}));
        let archived = shell("old", json!({ "archivedAt": "2026-06-21T00:00:00.000Z" }));
        let orphan = shell("orphan", json!({ "projectId": "gone" }));
        let view = ShellViewBuilder::new().build(
            &environment,
            &snapshot(vec![root, archived, orphan]),
            None,
        );
        let project_of = |id: &str| {
            view.project_of_thread(&ThreadId::new(id).unwrap())
                .map(|project| project.id.to_string())
        };
        assert_eq!(project_of("root").as_deref(), Some("project-v2"));
        assert_eq!(project_of("old").as_deref(), Some("project-v2"));
        assert_eq!(project_of("orphan"), None);
        assert_eq!(project_of("missing"), None);
    }

    #[test]
    fn shares_presented_rows_between_snapshots_and_splits_navigation() {
        let environment = EnvironmentId::new("local").unwrap();
        let root = shell("root", json!({}));
        let child = subagent_of("child", "root", Some("user_input"));
        let archived = shell("old", json!({ "archivedAt": "2026-06-21T00:00:00.000Z" }));
        let mut builder = ShellViewBuilder::new();
        let first = builder.build(
            &environment,
            &snapshot(vec![root.clone(), child.clone(), archived.clone()]),
            None,
        );

        assert_eq!(first.threads.len(), 3);
        // Navigation hides subagents and archived rows.
        assert_eq!(
            first
                .navigation_threads
                .iter()
                .map(|t| t.id.to_string())
                .collect::<Vec<_>>(),
            vec!["root"]
        );
        // The root carries its subagent's question.
        assert_eq!(
            first
                .thread(&ThreadId::new("root").unwrap())
                .unwrap()
                .pending_subagent_request,
            Some(PendingSubagentRequest::UserInput)
        );
        assert_eq!(
            first
                .child_thread_inputs(&ThreadId::new("root").unwrap())
                .len(),
            1
        );
        assert!(first.project(&v2_project().id).is_some());

        // A snapshot that only retitles `child` reuses the other presented rows.
        let retitled = Arc::new(OrchestrationV2ThreadShell {
            title: "renamed".into(),
            ..(*child).clone()
        });
        let second = builder.build(
            &environment,
            &snapshot(vec![root.clone(), retitled, archived]),
            None,
        );
        assert!(Arc::ptr_eq(&second.threads[0], &first.threads[0]));
        assert!(!Arc::ptr_eq(&second.threads[1], &first.threads[1]));
        // Same snapshot, same view.
        let again = builder.build(&environment, &second.snapshot.clone(), None);
        assert!(Arc::ptr_eq(&again.threads, &second.threads));
    }
}
