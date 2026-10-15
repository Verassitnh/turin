//! Port of upstream/packages/client-runtime/src/state/models.ts (the shell
//! read model the sidebar consumes: `EnvironmentThreadShell`,
//! `EnvironmentProject` and the helpers on them) plus `presentThreadShell` /
//! `scopeProject`, which build them from the contract shells.
//!
//! The field set is the one T15 (sidebar) and T16 (thread view) read, with
//! upstream's names and nullability. Everything else upstream carries on the
//! shell (`providerInstanceId`, `runtimeMode`, `activeProviderThreadId`, …) is
//! reached through [`EnvironmentThreadShell::source`], the contract row the
//! shell was presented from (upstream's `source`). PORTING.md Deviations lists
//! the omitted fields.

use std::sync::Arc;

pub use crate::state::contracts::ProjectIconColor;
use crate::state::contracts::{
    EnvironmentId, OrchestrationProjectShell, OrchestrationV2ActivityRunStatus,
    OrchestrationV2ForkSource, OrchestrationV2LimitRecovery, OrchestrationV2LineageRelationship,
    OrchestrationV2PendingBackgroundTask, OrchestrationV2ProviderFailureClass,
    OrchestrationV2ProviderGoal, OrchestrationV2RunStatus, OrchestrationV2SettleOverride,
    OrchestrationV2ShellThreadStatus, OrchestrationV2ThreadShell, ProjectId,
    ProviderInteractionMode, PullRequestState as ContractPullRequestState, RunId, ThreadId,
    ThreadLinkedPullRequest, ThreadPullRequestLink as ContractLink, ThreadPullRequestLinkSource,
};
use crate::state::support::background_work::background_work_holds_completion as contract_background_work_holds_completion;
use crate::state::support::thread_pull_requests::thread_pull_requests_of;
use crate::state::time::{iso, nullable_iso, parse_ms};

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
    /// The provider-native goal (`goal: thread.goal ?? null`), in full.
    pub goal: Option<OrchestrationV2ProviderGoal>,
    /// Message counts for the thread (`itemCount`, `visibleItemCount`).
    pub item_count: u64,
    pub visible_item_count: u64,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<String>,
    pub unsettled_at: Option<String>,
    pub snoozed_until: Option<String>,
    pub snoozed_at: Option<String>,
    pub limit_recovery: Option<OrchestrationV2LimitRecovery>,
    pub pinned_at: Option<String>,
    pub auto_settle_disabled_at: Option<String>,
    pub pin_order_key: Option<String>,
    pub active_order_key: Option<String>,
    pub pull_requests: Vec<ThreadPullRequestLink>,
    /// `linkedPullRequest`. `pull_requests` only folds it in for threads
    /// without a `pullRequests` list, so status readers need this field too.
    pub linked_pull_request: Option<ThreadLinkedPullRequest>,
    /// `branchPullRequest`: the pull request found on the thread's branch.
    pub branch_pull_request: Option<ThreadLinkedPullRequest>,
    /// Server-tracked visited watermark; `None` = server predates tracking.
    pub last_visited_at: Option<Option<String>>,
    pub title_regeneration: Option<TitleRegeneration>,
    pub deleted_at: Option<String>,
    /// Insertions/deletions of the latest run. Upstream shells don't carry
    /// checkpoint summaries yet (`latestRunDiff` returns null); the row keeps
    /// the slot as a seam, and so does this field.
    pub latest_run_diff: Option<DiffStat>,
    /// The contract row this shell was presented from (upstream's `source`);
    /// `None` for fixtures built with [`EnvironmentThreadShell::new`].
    pub source: Option<SourceRef<OrchestrationV2ThreadShell>>,
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
            item_count: 0,
            visible_item_count: 0,
            updated_at: created_at.clone(),
            created_at,
            archived_at: None,
            settled_override: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            limit_recovery: None,
            pinned_at: None,
            auto_settle_disabled_at: None,
            pin_order_key: None,
            active_order_key: None,
            pull_requests: Vec::new(),
            linked_pull_request: None,
            branch_pull_request: None,
            last_visited_at: None,
            title_regeneration: None,
            deleted_at: None,
            latest_run_diff: None,
            source: None,
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
    /// Derived from the environment's configured scratch root, never persisted.
    pub is_scratch: bool,
    /// The contract row (repository identity, scripts, default model, …).
    pub source: Option<SourceRef<OrchestrationProjectShell>>,
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
    let valid = |value: Option<&String>| value.filter(|value| parse_ms(value).is_some()).cloned();
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

/// The contract row a presented value came from (upstream's `source`).
/// Equality is by identity first (the common case: a reused row), then by
/// value.
#[derive(Clone, Debug)]
pub struct SourceRef<T>(pub Arc<T>);

impl<T: PartialEq> PartialEq for SourceRef<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl<T: PartialEq> Eq for SourceRef<T> {}

impl<T> std::ops::Deref for SourceRef<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl From<OrchestrationV2RunStatus> for RunStatus {
    fn from(status: OrchestrationV2RunStatus) -> Self {
        match status {
            OrchestrationV2RunStatus::Preparing => Self::Preparing,
            OrchestrationV2RunStatus::Queued => Self::Queued,
            OrchestrationV2RunStatus::Starting => Self::Starting,
            OrchestrationV2RunStatus::Running => Self::Running,
            OrchestrationV2RunStatus::Waiting => Self::Waiting,
            OrchestrationV2RunStatus::Completed => Self::Completed,
            OrchestrationV2RunStatus::Interrupted => Self::Interrupted,
            OrchestrationV2RunStatus::Failed => Self::Failed,
            OrchestrationV2RunStatus::Cancelled => Self::Cancelled,
            OrchestrationV2RunStatus::RolledBack => Self::RolledBack,
        }
    }
}

impl From<OrchestrationV2ShellThreadStatus> for RunStatus {
    fn from(status: OrchestrationV2ShellThreadStatus) -> Self {
        match status {
            OrchestrationV2ShellThreadStatus::Idle => Self::Idle,
            OrchestrationV2ShellThreadStatus::Preparing => Self::Preparing,
            OrchestrationV2ShellThreadStatus::Queued => Self::Queued,
            OrchestrationV2ShellThreadStatus::Starting => Self::Starting,
            OrchestrationV2ShellThreadStatus::Running => Self::Running,
            OrchestrationV2ShellThreadStatus::Waiting => Self::Waiting,
            OrchestrationV2ShellThreadStatus::Completed => Self::Completed,
            OrchestrationV2ShellThreadStatus::Interrupted => Self::Interrupted,
            OrchestrationV2ShellThreadStatus::Failed => Self::Failed,
            OrchestrationV2ShellThreadStatus::Cancelled => Self::Cancelled,
            OrchestrationV2ShellThreadStatus::RolledBack => Self::RolledBack,
        }
    }
}

impl From<OrchestrationV2ActivityRunStatus> for RunStatus {
    fn from(status: OrchestrationV2ActivityRunStatus) -> Self {
        match status {
            OrchestrationV2ActivityRunStatus::Preparing => Self::Preparing,
            OrchestrationV2ActivityRunStatus::Starting => Self::Starting,
            OrchestrationV2ActivityRunStatus::Running => Self::Running,
            OrchestrationV2ActivityRunStatus::Waiting => Self::Waiting,
        }
    }
}

impl From<OrchestrationV2ProviderFailureClass> for ProviderFailureClass {
    fn from(class: OrchestrationV2ProviderFailureClass) -> Self {
        match class {
            OrchestrationV2ProviderFailureClass::UsageLimit => Self::UsageLimit,
            OrchestrationV2ProviderFailureClass::ProviderError => {
                Self::Other("provider_error".into())
            }
            OrchestrationV2ProviderFailureClass::TransportError => {
                Self::Other("transport_error".into())
            }
            OrchestrationV2ProviderFailureClass::PermissionError => {
                Self::Other("permission_error".into())
            }
            OrchestrationV2ProviderFailureClass::ValidationError => {
                Self::Other("validation_error".into())
            }
            OrchestrationV2ProviderFailureClass::Unknown => Self::Other("unknown".into()),
        }
    }
}

impl From<&OrchestrationV2PendingBackgroundTask> for PendingBackgroundTask {
    fn from(task: &OrchestrationV2PendingBackgroundTask) -> Self {
        let kind = match task {
            OrchestrationV2PendingBackgroundTask::Subagent { .. } => BackgroundTaskKind::Subagent,
            OrchestrationV2PendingBackgroundTask::Command { .. } => BackgroundTaskKind::Command,
            OrchestrationV2PendingBackgroundTask::Monitor { .. } => BackgroundTaskKind::Monitor,
            OrchestrationV2PendingBackgroundTask::BackgroundTask { .. } => {
                BackgroundTaskKind::BackgroundTask
            }
        };
        Self {
            task_id: task.task_id().as_str().to_owned(),
            description: task.description().map(|d| d.as_str().to_owned()),
            kind,
        }
    }
}

impl From<&ContractLink> for ThreadPullRequestLink {
    fn from(link: &ContractLink) -> Self {
        Self {
            repository: link.repository.as_str().to_owned(),
            number: link.number.get(),
            url: link.url.as_str().to_owned(),
            title: link
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.title.as_str().to_owned()),
            state: link.snapshot.as_ref().map(|snapshot| match snapshot.state {
                ContractPullRequestState::Open => PullRequestState::Open,
                ContractPullRequestState::Closed => PullRequestState::Closed,
                ContractPullRequestState::Merged => PullRequestState::Merged,
            }),
            is_draft: link.snapshot.as_ref().is_some_and(|s| s.is_draft),
            dismissed: link.source == ThreadPullRequestLinkSource::StackDismissed,
        }
    }
}

fn terminal_run_status(status: OrchestrationV2ShellThreadStatus) -> bool {
    matches!(
        status,
        OrchestrationV2ShellThreadStatus::Completed
            | OrchestrationV2ShellThreadStatus::Interrupted
            | OrchestrationV2ShellThreadStatus::Failed
            | OrchestrationV2ShellThreadStatus::Cancelled
            | OrchestrationV2ShellThreadStatus::RolledBack
    )
}

/// Park runtime at idle when the post-settlement background roster holds the
/// run's completion, so #4415 waiting-presentation Waiting (session.idle) can
/// consume CTM runtime. Only work that wakes the agent holds it: commands it
/// left running, such as a dev server, present the run's own status (#14872).
/// The server suppresses the roster while an interruptible activity run exists,
/// so a remaining roster is stronger than checkpoint-oriented waiting.
/// latestRun keeps the latest run's status for history presentation.
/// A failed latest run outranks the roster, so the failure stays visible.
fn shell_runtime(thread: &OrchestrationV2ThreadShell) -> Option<ThreadRuntimeSummary> {
    let park_at_idle = contract_background_work_holds_completion(&thread.pending_background_tasks)
        && thread.status != OrchestrationV2ShellThreadStatus::Failed;
    // A pull request watch can hold a thread that never ran.
    if thread.latest_run_id.is_none() && thread.active_provider_thread_id.is_none() && !park_at_idle
    {
        return None;
    }
    let status = if park_at_idle {
        RunStatus::Idle
    } else {
        // `thread.activityRunStatus ?? thread.status`
        thread
            .activity_run_status
            .flatten()
            .map_or_else(|| thread.status.into(), RunStatus::from)
    };
    Some(ThreadRuntimeSummary {
        status,
        active_run_id: thread.active_run_id.clone(),
        activity_started_at: thread
            .activity_run_started_at
            .as_ref()
            .map(|value| nullable_iso(value.as_ref())),
        provider_instance_id: thread.provider_instance_id.as_str().to_owned(),
        provider_name: None,
        last_error: thread.last_error.clone().flatten(),
        last_error_class: thread.last_error_class.flatten().map(Into::into),
        updated_at: iso(&thread.updated_at),
    })
}

/// `scopeProject`.
pub fn scope_project(
    environment_id: &EnvironmentId,
    project: &Arc<OrchestrationProjectShell>,
) -> EnvironmentProject {
    EnvironmentProject {
        environment_id: environment_id.clone(),
        id: project.id.clone(),
        title: project.title.as_str().to_owned(),
        workspace_root: project.workspace_root.as_str().to_owned(),
        favicon_path: project
            .favicon_path
            .clone()
            .flatten()
            .map(|path| path.as_str().to_owned()),
        project_icon: project
            .project_icon
            .as_ref()
            .and_then(|icon| icon.as_ref())
            .and_then(|icon| icon.0.as_ref())
            .map(ProjectIcon::from),
        created_at: Some(project.created_at.clone()),
        updated_at: Some(project.updated_at.clone()),
        is_scratch: false,
        source: Some(SourceRef(Arc::clone(project))),
    }
}

impl From<&crate::state::contracts::ProjectIconOverride> for ProjectIcon {
    fn from(icon: &crate::state::contracts::ProjectIconOverride) -> Self {
        use crate::state::contracts::ProjectIconOverride as Source;
        match icon {
            Source::Monogram { text, color } => Self::Monogram {
                text: text.as_str().to_owned(),
                color: *color,
            },
            Source::Emoji { emoji } => Self::Emoji(emoji.as_str().to_owned()),
            Source::Lucide { name, color } => Self::Lucide {
                name: name.as_str().to_owned(),
                color: *color,
            },
        }
    }
}

/// `presentThreadShell`.
pub fn present_thread_shell(
    environment_id: &EnvironmentId,
    thread: &Arc<OrchestrationV2ThreadShell>,
    pending_subagent_request: Option<PendingSubagentRequest>,
) -> EnvironmentThreadShell {
    let updated_at = iso(&thread.updated_at);
    let latest_run = thread
        .latest_run_id
        .as_ref()
        .map(|run_id| ThreadRunSummary {
            run_id: run_id.clone(),
            status: if thread.status == OrchestrationV2ShellThreadStatus::Idle {
                RunStatus::Completed
            } else {
                thread.status.into()
            },
            requested_at: nullable_iso(thread.latest_run_requested_at.flatten().as_ref()),
            started_at: nullable_iso(thread.latest_run_started_at.flatten().as_ref()),
            completed_at: match &thread.latest_run_completed_at {
                None => {
                    if thread.status == OrchestrationV2ShellThreadStatus::Idle
                        || terminal_run_status(thread.status)
                    {
                        Some(updated_at.clone())
                    } else {
                        None
                    }
                }
                Some(value) => nullable_iso(value.as_ref()),
            },
        });
    let relationship = thread.lineage.relationship_to_parent;
    let linked = thread
        .linked_pull_request
        .as_ref()
        .and_then(|linked| linked.as_ref());
    let pull_requests = thread_pull_requests_of(thread.pull_requests.as_deref(), linked)
        .iter()
        .map(ThreadPullRequestLink::from)
        .collect();
    let request_kind = thread.pending_runtime_request.as_ref().map(|r| r.kind);
    use crate::state::contracts::OrchestrationV2RuntimeRequestKind as RequestKind;
    EnvironmentThreadShell {
        environment_id: environment_id.clone(),
        id: thread.id.clone(),
        project_id: thread.project_id.clone(),
        title: if relationship == Some(OrchestrationV2LineageRelationship::Subagent) {
            crate::state::subagent_display::format_subagent_display_title(&thread.title)
        } else {
            thread.title.clone()
        },
        model_selection: ModelSelection {
            instance_id: thread.model_selection.instance_id.as_str().to_owned(),
            model: thread.model_selection.model.as_str().to_owned(),
        },
        interaction_mode: match thread.interaction_mode {
            ProviderInteractionMode::Default => InteractionMode::Default,
            ProviderInteractionMode::Plan => InteractionMode::Plan,
        },
        branch: thread.branch.as_ref().map(|b| b.as_str().to_owned()),
        worktree_path: thread.worktree_path.as_ref().map(|p| p.as_str().to_owned()),
        lineage: ThreadLineage {
            relationship_to_parent: relationship.map(|relationship| match relationship {
                OrchestrationV2LineageRelationship::Fork => ThreadRelationship::Fork,
                OrchestrationV2LineageRelationship::Subagent => ThreadRelationship::Subagent,
            }),
            parent_thread_id: thread.lineage.parent_thread_id.clone(),
        },
        forked_from: thread.forked_from.as_ref().map(|source| match source {
            OrchestrationV2ForkSource::Run { thread_id, .. } => ForkedFrom::Run {
                thread_id: thread_id.clone(),
            },
            _ => ForkedFrom::Other,
        }),
        latest_run,
        runtime: shell_runtime(thread),
        latest_user_message_at: nullable_iso(thread.latest_user_message_at.as_ref()),
        latest_user_authored_message_at: thread
            .latest_user_authored_message_at
            .as_ref()
            .map(|value| nullable_iso(value.as_ref())),
        has_pending_approvals: request_kind
            .is_some_and(|kind| kind != RequestKind::UserInput && kind != RequestKind::AuthRefresh),
        has_pending_user_input: request_kind == Some(RequestKind::UserInput),
        pending_subagent_request,
        has_actionable_proposed_plan: thread.has_actionable_proposed_plan,
        pending_background_tasks: thread
            .pending_background_tasks
            .iter()
            .map(PendingBackgroundTask::from)
            .collect(),
        provider_instance_history: thread
            .provider_instance_history
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
        goal: thread.goal.clone().flatten(),
        item_count: thread.item_count,
        visible_item_count: thread.visible_item_count,
        created_at: iso(&thread.created_at),
        updated_at,
        archived_at: nullable_iso(thread.archived_at.as_ref()),
        settled_override: thread.settled_override.map(|value| match value {
            OrchestrationV2SettleOverride::Settled => SettledOverride::Settled,
            OrchestrationV2SettleOverride::Active => SettledOverride::Active,
        }),
        settled_at: nullable_iso(thread.settled_at.as_ref()),
        unsettled_at: nullable_iso(thread.unsettled_at.flatten().as_ref()),
        snoozed_until: nullable_iso(thread.snoozed_until.flatten().as_ref()),
        snoozed_at: nullable_iso(thread.snoozed_at.flatten().as_ref()),
        limit_recovery: thread.limit_recovery.clone().flatten(),
        pinned_at: nullable_iso(thread.pinned_at.flatten().as_ref()),
        auto_settle_disabled_at: nullable_iso(thread.auto_settle_disabled_at.flatten().as_ref()),
        pin_order_key: thread
            .pin_order_key
            .clone()
            .flatten()
            .map(|key| key.as_str().to_owned()),
        active_order_key: thread
            .active_order_key
            .clone()
            .flatten()
            .map(|key| key.as_str().to_owned()),
        pull_requests,
        linked_pull_request: linked.cloned(),
        branch_pull_request: thread.branch_pull_request.clone().flatten(),
        last_visited_at: thread
            .last_visited_at
            .as_ref()
            .map(|value| nullable_iso(value.as_ref())),
        title_regeneration: thread
            .title_regeneration
            .as_ref()
            .and_then(|value| value.as_ref())
            .map(|regeneration| TitleRegeneration {
                request_id: regeneration.request_id.as_str().to_owned(),
                started_at: iso(&regeneration.started_at),
            }),
        deleted_at: nullable_iso(thread.deleted_at.as_ref()),
        source: Some(SourceRef(Arc::clone(thread))),
        latest_run_diff: None,
    }
}

/// `scopeThreadShell`.
pub fn scope_thread_shell(
    environment_id: &EnvironmentId,
    thread: &Arc<OrchestrationV2ThreadShell>,
) -> EnvironmentThreadShell {
    present_thread_shell(environment_id, thread, None)
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

    #[test]
    fn presented_row_carries_the_contract_fields_the_sidebar_reads() {
        use crate::state::test_fixtures::{V2_NOW, from_json, v2_thread_shell};
        use serde_json::json;

        let linked = json!({
            "projectId": "project-v2",
            "repository": "o/r",
            "number": 3,
            "url": "https://github.com/o/r/pull/3",
        });
        let branch = json!({
            "projectId": "project-v2",
            "repository": "o/r",
            "number": 7,
            "url": "https://github.com/o/r/pull/7",
        });
        let goal = json!({
            "objective": "Ship the sidebar",
            "status": "paused",
            "tokensUsed": 42,
            "tokenBudget": 1000,
            "timeUsedSeconds": 7,
            "checks": 2,
            "lastCheck": "not done",
        });
        let mut value = serde_json::to_value(v2_thread_shell()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("linkedPullRequest".into(), linked.clone());
        object.insert("branchPullRequest".into(), branch.clone());
        object.insert("goal".into(), goal.clone());
        object.insert("itemCount".into(), json!(5));
        object.insert("visibleItemCount".into(), json!(3));
        object.insert(
            "limitRecovery".into(),
            json!({ "runId": "run-1", "resetAt": V2_NOW, "autoResume": true }),
        );
        object.insert("deletedAt".into(), json!(V2_NOW));

        let thread: OrchestrationV2ThreadShell = from_json(value);
        let row = present_thread_shell(&env("environment-local"), &Arc::new(thread), None);

        assert_eq!(row.linked_pull_request, Some(from_json(linked)));
        assert_eq!(row.branch_pull_request, Some(from_json(branch)));
        assert_eq!(row.goal, Some(from_json(goal)));
        assert_eq!(row.item_count, 5);
        assert_eq!(row.visible_item_count, 3);
        assert_eq!(
            row.limit_recovery
                .as_ref()
                .map(|recovery| recovery.auto_resume),
            Some(true)
        );
        assert_eq!(row.deleted_at.as_deref(), Some(V2_NOW));
    }

    #[test]
    fn presented_row_leaves_the_pull_request_fields_empty_when_the_contract_has_none() {
        use crate::state::test_fixtures::v2_thread_shell;

        let thread = Arc::new(v2_thread_shell());
        let row = present_thread_shell(&env("environment-local"), &thread, None);

        assert_eq!(row.linked_pull_request, None);
        assert_eq!(row.branch_pull_request, None);
        assert_eq!(row.goal, None);
        assert_eq!(row.limit_recovery, None);
        assert_eq!(row.deleted_at, None);
    }
}
