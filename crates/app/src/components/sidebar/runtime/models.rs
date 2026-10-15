//! Port of upstream/packages/client-runtime/src/state/models.ts (the shell
//! read model the sidebar consumes: `EnvironmentThreadShell`,
//! `EnvironmentProject` and the small helpers on them).
//!
//! This is the typed seam between the sidebar and the client view-model
//! reducers (T14). Field names and nullability follow upstream; types that
//! upstream borrows from `OrchestrationV2ThreadShell` are narrowed to what
//! the sidebar reads, because the contracts port is not on this branch yet.
//! T14 should move this module to `crates/app/src/state` and build these
//! values with `presentThreadShell`.

use turin_contracts::{EnvironmentId, ProjectId, RunId, ThreadId};

use crate::timestamp_format::parse_timestamp_ms;

/// `OrchestrationV2RunStatus`, plus the runtime-only `"idle"` (background
/// work parked the run; see `shellRuntime`). `latestRun.status` never holds
/// `Idle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RunStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
    Idle,
}

/// `OrchestrationV2ProviderFailureClass`; the sidebar only distinguishes
/// usage limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderFailureClass {
    UsageLimit,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRunSummary {
    pub run_id: RunId,
    pub status: RunStatus,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRuntimeSummary {
    pub status: RunStatus,
    pub active_run_id: Option<RunId>,
    /// `undefined` (older servers) is `None`; `null` is `Some(None)`.
    pub activity_started_at: Option<Option<String>>,
    pub provider_instance_id: String,
    pub provider_name: Option<String>,
    pub last_error: Option<String>,
    pub last_error_class: Option<ProviderFailureClass>,
    pub updated_at: String,
}

/// `PendingSubagentRequest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingSubagentRequest {
    Approval,
    UserInput,
}

/// `OrchestrationV2PendingBackgroundTask["kind"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundTaskKind {
    Command,
    Subagent,
    Monitor,
    BackgroundTask,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingBackgroundTask {
    pub task_id: String,
    pub description: Option<String>,
    pub kind: BackgroundTaskKind,
}

/// Port of `backgroundWorkHoldsCompletion`
/// (packages/shared/src/orchestrationV2PendingBackgroundWork.ts): commands a
/// finished agent leaves running do not hold its completion; subagents,
/// monitors and unnamed background work do.
pub fn background_work_holds_completion(tasks: &[PendingBackgroundTask]) -> bool {
    tasks.iter().any(|task| match task.kind {
        BackgroundTaskKind::Command => false,
        BackgroundTaskKind::Subagent
        | BackgroundTaskKind::Monitor
        | BackgroundTaskKind::BackgroundTask => true,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InteractionMode {
    #[default]
    Default,
    Plan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRelationship {
    Fork,
    Subagent,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadLineage {
    pub relationship_to_parent: Option<ThreadRelationship>,
    pub parent_thread_id: Option<ThreadId>,
}

/// `forkedFrom`, narrowed to the run-fork case `getSidebarForkParentThreadId` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForkedFrom {
    Run { thread_id: ThreadId },
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSelection {
    pub instance_id: String,
    pub model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalStatus {
    Active,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettledOverride {
    Settled,
    Active,
}

/// Change request state, as the PR badge colors it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

/// `ThreadPullRequestLink` with its snapshot flattened to the fields the
/// sidebar reads (number, title, state). Stacks and watches are the PR
/// task's (T19).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadPullRequestLink {
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub title: Option<String>,
    pub state: Option<PullRequestState>,
    pub is_draft: bool,
    /// `source === "stack-dismissed"`: hidden from every list.
    pub dismissed: bool,
}

/// Port of `threadPullRequestSearchTerms` (packages/shared/src/threadPullRequests.ts)
/// for the link shape above.
pub fn thread_pull_request_search_terms(pull_requests: &[ThreadPullRequestLink]) -> Vec<String> {
    pull_requests
        .iter()
        .filter(|link| !link.dismissed)
        .flat_map(|link| {
            [
                format!("#{}", link.number),
                format!("{}#{}", link.repository, link.number),
                link.url.clone(),
                link.title.clone().unwrap_or_default(),
            ]
        })
        .collect()
}

/// Port of `resolveThreadCurrentPullRequestLink`, without stack chains
/// (T19): the only open link, else the newest visible one. Unsynced links
/// (no state yet) count as open.
pub fn resolve_thread_current_pull_request_link(
    pull_requests: &[ThreadPullRequestLink],
) -> Option<&ThreadPullRequestLink> {
    let visible: Vec<&ThreadPullRequestLink> = pull_requests
        .iter()
        .filter(|link| !link.dismissed)
        .collect();
    let open: Vec<&&ThreadPullRequestLink> = visible
        .iter()
        .filter(|link| matches!(link.state, None | Some(PullRequestState::Open)))
        .collect();
    if open.len() == 1 {
        return Some(open[0]);
    }
    open.last().map(|link| **link).or(visible.last().copied())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleRegeneration {
    pub request_id: String,
    pub started_at: String,
}

/// `EnvironmentThreadShell` (alias `SidebarThreadSummary` in apps/web/src/types.ts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentThreadShell {
    pub environment_id: EnvironmentId,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub model_selection: ModelSelection,
    pub interaction_mode: InteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub lineage: ThreadLineage,
    pub forked_from: Option<ForkedFrom>,
    pub latest_run: Option<ThreadRunSummary>,
    pub runtime: Option<ThreadRuntimeSummary>,
    pub latest_user_message_at: Option<String>,
    /// `undefined` means the server predates it: `None`.
    pub latest_user_authored_message_at: Option<Option<String>>,
    pub has_pending_approvals: bool,
    pub has_pending_user_input: bool,
    pub pending_subagent_request: Option<PendingSubagentRequest>,
    pub has_actionable_proposed_plan: bool,
    pub pending_background_tasks: Vec<PendingBackgroundTask>,
    /// Provider instances that have owned the root conversation, oldest first.
    pub provider_instance_history: Vec<String>,
    pub goal: Option<GoalStatus>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<String>,
    pub unsettled_at: Option<String>,
    pub snoozed_until: Option<String>,
    pub snoozed_at: Option<String>,
    pub pinned_at: Option<String>,
    pub auto_settle_disabled_at: Option<String>,
    pub pin_order_key: Option<String>,
    pub active_order_key: Option<String>,
    pub pull_requests: Vec<ThreadPullRequestLink>,
    /// Server-tracked visited watermark; `None` = server predates tracking.
    pub last_visited_at: Option<Option<String>>,
    pub title_regeneration: Option<TitleRegeneration>,
    /// Insertions/deletions of the latest run. Upstream shells don't carry
    /// checkpoint summaries yet (`latestRunDiff` returns null); the row keeps
    /// the slot as a seam, and so does this field.
    pub latest_run_diff: Option<DiffStat>,
}

/// `{ insertions, deletions }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffStat {
    pub insertions: u64,
    pub deletions: u64,
}

impl EnvironmentThreadShell {
    /// A blank shell for tests and fixtures; every lifecycle field empty.
    pub fn new(
        environment_id: EnvironmentId,
        id: ThreadId,
        project_id: ProjectId,
        title: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            environment_id,
            id,
            project_id,
            title: title.into(),
            model_selection: ModelSelection::default(),
            interaction_mode: InteractionMode::Default,
            branch: None,
            worktree_path: None,
            lineage: ThreadLineage::default(),
            forked_from: None,
            latest_run: None,
            runtime: None,
            latest_user_message_at: None,
            latest_user_authored_message_at: None,
            has_pending_approvals: false,
            has_pending_user_input: false,
            pending_subagent_request: None,
            has_actionable_proposed_plan: false,
            pending_background_tasks: Vec::new(),
            provider_instance_history: Vec::new(),
            goal: None,
            updated_at: created_at.clone(),
            created_at,
            archived_at: None,
            settled_override: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            pinned_at: None,
            auto_settle_disabled_at: None,
            pin_order_key: None,
            active_order_key: None,
            pull_requests: Vec::new(),
            last_visited_at: None,
            title_regeneration: None,
            latest_run_diff: None,
        }
    }

    /// `scopedThreadKey(scopeThreadRef(environmentId, id))`.
    pub fn key(&self) -> String {
        scoped_thread_key(&self.environment_id, &self.id)
    }

    /// `${environmentId}:${projectId}`: the sidebar's project lookup key.
    pub fn project_key(&self) -> String {
        format!("{}:{}", self.environment_id, self.project_id)
    }
}

/// `scopedThreadKey`: always contains a colon, which the sortable marker
/// ids rely on.
pub fn scoped_thread_key(environment_id: &EnvironmentId, thread_id: &ThreadId) -> String {
    format!("{environment_id}:{thread_id}")
}

/// `parseScopedThreadKey`.
pub fn parse_scoped_thread_key(key: &str) -> Option<(EnvironmentId, ThreadId)> {
    let (environment, thread) = key.split_once(':')?;
    Some((
        EnvironmentId::new(environment).ok()?,
        ThreadId::new(thread).ok()?,
    ))
}

/// `ProjectIconColor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectIconColor {
    Gray,
    Red,
    Orange,
    Amber,
    Yellow,
    Lime,
    Green,
    Emerald,
    Teal,
    Cyan,
    Sky,
    Blue,
    Indigo,
    Violet,
    Purple,
    Fuchsia,
    Pink,
    Rose,
}

/// `ProjectIcon` (contracts project.ts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectIcon {
    Monogram {
        text: String,
        color: ProjectIconColor,
    },
    Emoji(String),
    Lucide {
        name: String,
        color: ProjectIconColor,
    },
}

/// `EnvironmentProject`: `OrchestrationProjectShell` + its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProject {
    pub environment_id: EnvironmentId,
    pub id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    pub favicon_path: Option<String>,
    pub project_icon: Option<ProjectIcon>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl EnvironmentProject {
    pub fn key(&self) -> String {
        format!("{}:{}", self.environment_id, self.id)
    }
}

/// `threadRuntimeIsActive`.
pub fn thread_runtime_is_active(runtime: Option<&ThreadRuntimeSummary>) -> bool {
    runtime.is_some_and(|runtime| thread_run_status_is_active(runtime.status))
}

/// `threadRuntimeCanArchive`: archiving may discard queued work, but must
/// not detach a provider that is preparing, starting, or running a turn.
pub fn thread_runtime_can_archive(runtime: Option<&ThreadRuntimeSummary>) -> bool {
    match runtime {
        Some(runtime) if runtime.status == RunStatus::Queued => runtime.active_run_id.is_none(),
        Some(runtime) => !matches!(
            runtime.status,
            RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
        ),
        None => true,
    }
}

fn thread_run_status_is_active(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Preparing
            | RunStatus::Queued
            | RunStatus::Starting
            | RunStatus::Running
            | RunStatus::Waiting
    )
}

const THREAD_PROVIDER_STACK_LIMIT: usize = 3;

/// `resolveThreadProviderStack`: provider instances for a row's trailing
/// stack, back to front; the current one is last.
pub fn resolve_thread_provider_stack(thread: &EnvironmentThreadShell) -> Vec<String> {
    let current = thread
        .runtime
        .as_ref()
        .map(|runtime| runtime.provider_instance_id.clone())
        .unwrap_or_else(|| thread.model_selection.instance_id.clone());
    let previous: Vec<String> = thread
        .provider_instance_history
        .iter()
        .filter(|id| **id != current)
        .cloned()
        .collect();
    let keep = THREAD_PROVIDER_STACK_LIMIT - 1;
    let mut stack: Vec<String> = previous[previous.len().saturating_sub(keep)..].to_vec();
    stack.push(current);
    stack
}

/// `resolveThreadWorkingStartedAt`: both timers count from the activity
/// run's work start, never last activity.
pub fn resolve_thread_working_started_at(thread: &EnvironmentThreadShell) -> Option<String> {
    let valid = |value: Option<&String>| {
        value
            .filter(|value| parse_timestamp_ms(value).is_some())
            .cloned()
    };
    if let Some(runtime) = &thread.runtime
        && let Some(activity_started_at) = &runtime.activity_started_at
    {
        return valid(activity_started_at.as_ref());
    }
    // Older servers can supply a timestamp only if the newest run owns the work.
    let run = thread.latest_run.as_ref()?;
    let active_run_id = thread
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.active_run_id.as_ref());
    if run.completed_at.is_none() && Some(&run.run_id) == active_run_id {
        return valid(run.started_at.as_ref()).or_else(|| valid(run.requested_at.as_ref()));
    }
    None
}

/// `isLatestRunSettled` (apps/web/src/session-logic.ts).
pub fn is_latest_run_settled(
    latest_run: Option<&ThreadRunSummary>,
    runtime: Option<&ThreadRuntimeSummary>,
) -> bool {
    let Some(run) = latest_run else {
        return false;
    };
    if thread_run_status_is_active(run.status) {
        return false;
    }
    runtime.and_then(|runtime| runtime.active_run_id.as_ref()) != Some(&run.run_id)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).unwrap()
    }

    pub fn thread_id(id: &str) -> ThreadId {
        ThreadId::new(id).unwrap()
    }

    pub fn project_id(id: &str) -> ProjectId {
        ProjectId::new(id).unwrap()
    }

    pub fn run_id(id: &str) -> RunId {
        RunId::new(id).unwrap()
    }

    pub fn shell(id: &str) -> EnvironmentThreadShell {
        EnvironmentThreadShell::new(
            env("environment-local"),
            thread_id(id),
            project_id("project-1"),
            id,
            "2026-03-09T10:00:00.000Z",
        )
    }

    pub fn runtime(status: RunStatus) -> ThreadRuntimeSummary {
        ThreadRuntimeSummary {
            status,
            active_run_id: None,
            activity_started_at: None,
            provider_instance_id: "codex".into(),
            provider_name: Some("Codex".into()),
            last_error: None,
            last_error_class: None,
            updated_at: "2026-03-09T10:00:00.000Z".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn provider_stack_keeps_newest_history_and_current_last() {
        let mut thread = shell("t");
        thread.model_selection.instance_id = "claude".into();
        thread.provider_instance_history = vec![
            "codex".into(),
            "cursor".into(),
            "opencode".into(),
            "claude".into(),
        ];
        assert_eq!(
            resolve_thread_provider_stack(&thread),
            vec!["cursor", "opencode", "claude"]
        );
    }

    #[test]
    fn working_started_at_prefers_activity_start() {
        let mut thread = shell("t");
        let mut rt = runtime(RunStatus::Running);
        rt.activity_started_at = Some(Some("2026-03-09T10:05:00.000Z".into()));
        thread.runtime = Some(rt);
        assert_eq!(
            resolve_thread_working_started_at(&thread).as_deref(),
            Some("2026-03-09T10:05:00.000Z")
        );
        thread.runtime.as_mut().unwrap().activity_started_at = Some(Some("bad".into()));
        assert_eq!(resolve_thread_working_started_at(&thread), None);
    }

    #[test]
    fn working_started_at_falls_back_to_the_owning_run() {
        let mut thread = shell("t");
        let mut rt = runtime(RunStatus::Running);
        rt.active_run_id = Some(run_id("run-1"));
        thread.runtime = Some(rt);
        thread.latest_run = Some(ThreadRunSummary {
            run_id: run_id("run-1"),
            status: RunStatus::Running,
            requested_at: Some("2026-03-09T10:01:00.000Z".into()),
            started_at: None,
            completed_at: None,
        });
        assert_eq!(
            resolve_thread_working_started_at(&thread).as_deref(),
            Some("2026-03-09T10:01:00.000Z")
        );
    }

    #[test]
    fn archive_guard() {
        assert!(thread_runtime_can_archive(None));
        assert!(!thread_runtime_can_archive(Some(&runtime(
            RunStatus::Running
        ))));
        assert!(thread_runtime_can_archive(Some(&runtime(
            RunStatus::Queued
        ))));
        let mut queued = runtime(RunStatus::Queued);
        queued.active_run_id = Some(run_id("r"));
        assert!(!thread_runtime_can_archive(Some(&queued)));
    }
}
