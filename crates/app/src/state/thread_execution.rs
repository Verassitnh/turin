//! Port of upstream/packages/client-runtime/src/state/threadExecution.ts
//!
//! Not ported: `formatModelSelectionEffort` (needs the provider option
//! descriptor helpers of packages/shared/src/model.ts, which belong to the
//! composer's model picker). See PORTING.md deviations.

use std::collections::HashMap;
use std::sync::Arc;

use crate::state::contracts::{
    MessageId, ModelSelection, OrchestrationV2ConversationMessageRole,
    OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
    OrchestrationV2ExecutionNodeStatus, OrchestrationV2NotificationSource,
    OrchestrationV2PendingBackgroundTask, OrchestrationV2ProviderGoal,
    OrchestrationV2ProviderGoalStatus, OrchestrationV2Run, OrchestrationV2RunStatus,
    OrchestrationV2SourcePlanRef, ThreadId, is_provider_native_subagent_thread, js_trim,
    orchestration_v2_run_work_started_at,
};
use crate::state::models::{
    ProviderFailureClass, RunStatus, ThreadRunSummary, ThreadRuntimeSummary,
    thread_runtime_is_active,
};
use crate::state::subagent_display::format_subagent_display_title;
use crate::state::support::background_work::{
    DerivePendingBackgroundWorkInput, background_work_holds_completion,
    derive_pending_background_work,
};
use crate::state::support::thread_error::{
    latest_root_provider_failure, latest_unheld_run, thread_error_summary,
    usage_limit_run_presented_as_latest,
};
use crate::state::support::timing::format_duration;
use crate::state::thread_projection::ThreadProjection;
use crate::state::time::{iso, nullable_iso, parse_ms};

fn is_activity_run_status(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

fn is_interruptible_run_status(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
    )
}

fn latest_matching_run(
    projection: &ThreadProjection,
    predicate: impl Fn(&OrchestrationV2Run) -> bool,
) -> Option<&Arc<OrchestrationV2Run>> {
    projection.runs.iter().fold(None, |latest, candidate| {
        if predicate(candidate)
            && latest
                .is_none_or(|latest: &Arc<OrchestrationV2Run>| candidate.ordinal > latest.ordinal)
        {
            Some(candidate)
        } else {
            latest
        }
    })
}

/// `ThreadRunSummary` as upstream builds it, with the fields the sidebar's
/// narrower [`ThreadRunSummary`] leaves out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRunDetail {
    pub run_id: crate::state::contracts::RunId,
    pub status: RunStatus,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub assistant_message_id: Option<MessageId>,
    pub source_plan_ref: Option<OrchestrationV2SourcePlanRef>,
}

impl ThreadRunDetail {
    /// The shell-level summary the sidebar reads.
    pub fn to_summary(&self) -> ThreadRunSummary {
        ThreadRunSummary {
            run_id: self.run_id.clone(),
            status: self.status,
            requested_at: self.requested_at.clone(),
            started_at: self.started_at.clone(),
            completed_at: self.completed_at.clone(),
        }
    }
}

fn summarize_thread_run(
    projection: &ThreadProjection,
    run: &OrchestrationV2Run,
) -> ThreadRunDetail {
    ThreadRunDetail {
        run_id: run.id.clone(),
        status: run.status.into(),
        requested_at: Some(iso(&run.requested_at)),
        started_at: nullable_iso(run.started_at.as_ref()),
        completed_at: nullable_iso(run.completed_at.as_ref()),
        assistant_message_id: projection
            .messages
            .iter()
            .rfind(|message| {
                message.run_id.as_ref() == Some(&run.id)
                    && message.role == OrchestrationV2ConversationMessageRole::Assistant
            })
            .map(|message| message.id.clone()),
        source_plan_ref: run.source_plan_ref.clone(),
    }
}

fn session_error(projection: &ThreadProjection) -> Option<String> {
    projection
        .provider_sessions
        .iter()
        .rfind(|session| session.provider_instance_id == projection.thread.provider_instance_id)
        .and_then(|session| session.last_error.clone())
}

fn presented_usage_limit_run(projection: &ThreadProjection) -> Option<&Arc<OrchestrationV2Run>> {
    usage_limit_run_presented_as_latest(
        &projection.runs,
        &projection.turn_items,
        session_error(projection).as_deref(),
    )
}

/// The run that stands for the thread's outcome when no run is executing.
fn presented_latest_run(projection: &ThreadProjection) -> Option<&Arc<OrchestrationV2Run>> {
    presented_usage_limit_run(projection).or_else(|| latest_unheld_run(&projection.runs))
}

pub fn derive_latest_thread_run(projection: &ThreadProjection) -> Option<ThreadRunDetail> {
    presented_latest_run(projection).map(|run| summarize_thread_run(projection, run))
}

/// Returns the run that owns live provider work, falling back to the newest run
/// once the thread is idle. A newer queued run must not make an older executing
/// run look settled in clients that render per-run activity.
pub fn derive_thread_activity_run(projection: &ThreadProjection) -> Option<ThreadRunDetail> {
    latest_matching_run(projection, |candidate| {
        is_activity_run_status(candidate.status)
    })
    .or_else(|| presented_latest_run(projection))
    .map(|run| summarize_thread_run(projection, run))
}

fn is_execution_node_active(status: OrchestrationV2ExecutionNodeStatus) -> bool {
    matches!(
        status,
        OrchestrationV2ExecutionNodeStatus::Pending
            | OrchestrationV2ExecutionNodeStatus::Running
            | OrchestrationV2ExecutionNodeStatus::Waiting
    )
}

/// Provider-native subagent threads never get app runs: their work is a runless
/// root turn whose status follows the subagent. Returns when that work started
/// while it is still active, so clients can show the same working state (and
/// timer) as a run. Stop, queue, and steer stay run-only.
pub fn derive_runless_work_started_at(projection: &ThreadProjection) -> Option<String> {
    let status = derive_provider_subagent_status(projection)?;
    is_execution_node_active(status.status)
        .then_some(status.started_at)
        .flatten()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSubagentStatus {
    pub status: OrchestrationV2ExecutionNodeStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Status of a provider-native subagent thread (see
/// `is_provider_native_subagent_thread`), read from its runless root turn. `None`
/// until that root turn arrives, and for every other thread.
pub fn derive_provider_subagent_status(
    projection: &ThreadProjection,
) -> Option<ProviderSubagentStatus> {
    if !is_provider_native_subagent_thread(&projection.thread) {
        return None;
    }
    let node: &OrchestrationV2ExecutionNode = projection.nodes.iter().rfind(|candidate| {
        candidate.kind == OrchestrationV2ExecutionNodeKind::RootTurn && candidate.run_id.is_none()
    })?;
    Some(ProviderSubagentStatus {
        status: node.status,
        started_at: nullable_iso(node.started_at.as_ref()),
        completed_at: nullable_iso(node.completed_at.as_ref()),
    })
}

/// The observed selection belongs to the active provider thread, never a previous handoff.
pub fn derive_reported_model_selection(projection: &ThreadProjection) -> Option<&ModelSelection> {
    projection
        .provider_threads
        .iter()
        .find(|candidate| {
            Some(&candidate.id) == projection.thread.active_provider_thread_id.as_ref()
                && candidate.provider_instance_id == projection.thread.model_selection.instance_id
        })
        .and_then(|thread| thread.native_metadata.as_ref())
        .and_then(|metadata| metadata.model_selection.as_ref())
}

fn subagent_status_label(status: OrchestrationV2ExecutionNodeStatus) -> &'static str {
    match status {
        OrchestrationV2ExecutionNodeStatus::Idle => "Idle",
        OrchestrationV2ExecutionNodeStatus::Pending
        | OrchestrationV2ExecutionNodeStatus::Running => "Working",
        OrchestrationV2ExecutionNodeStatus::Waiting => "Waiting",
        OrchestrationV2ExecutionNodeStatus::Completed => "Completed",
        OrchestrationV2ExecutionNodeStatus::Interrupted => "Interrupted",
        OrchestrationV2ExecutionNodeStatus::Failed => "Failed",
        OrchestrationV2ExecutionNodeStatus::Cancelled
        | OrchestrationV2ExecutionNodeStatus::RolledBack => "Cancelled",
    }
}

/// One line for the read-only subagent bar: "Working 12s", "Completed in 34s",
/// or just the status when no duration is known.
pub fn format_provider_subagent_status(
    status: Option<&ProviderSubagentStatus>,
    now_ms: f64,
) -> String {
    let Some(status) = status else {
        return "Starting".to_owned();
    };
    let label = subagent_status_label(status.status);
    let live = is_execution_node_active(status.status);
    if !live && status.status != OrchestrationV2ExecutionNodeStatus::Completed {
        return label.to_owned();
    }
    let start = status
        .started_at
        .as_deref()
        .and_then(parse_ms)
        .map(|ms| ms as f64);
    let end = if live {
        Some(now_ms)
    } else {
        status
            .completed_at
            .as_deref()
            .and_then(parse_ms)
            .map(|ms| ms as f64)
    };
    let (Some(start), Some(end)) = (start, end) else {
        return label.to_owned();
    };
    // Whole seconds: a ticking label must not flicker through tenths.
    let elapsed = format_duration(1_000_f64.max(((end - start) / 1_000.0).floor() * 1_000.0));
    if live {
        format!("{label} {elapsed}")
    } else {
        format!("{label} in {elapsed}")
    }
}

/// `ThreadRuntimeSummary` with the fields the sidebar's narrower summary
/// leaves out (`usageLimitResetAt`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRuntimeDetail {
    pub status: RunStatus,
    pub active_run_id: Option<crate::state::contracts::RunId>,
    /// `None` is `undefined` upstream; `Some(None)` is `null`.
    pub activity_started_at: Option<Option<String>>,
    pub provider_instance_id: String,
    pub provider_name: Option<String>,
    pub last_error: Option<String>,
    pub last_error_class: Option<ProviderFailureClass>,
    pub usage_limit_reset_at: Option<String>,
    pub updated_at: String,
}

impl ThreadRuntimeDetail {
    /// The shell-level summary the sidebar reads.
    pub fn to_summary(&self) -> ThreadRuntimeSummary {
        ThreadRuntimeSummary {
            status: self.status,
            active_run_id: self.active_run_id.clone(),
            activity_started_at: self.activity_started_at.clone(),
            provider_instance_id: self.provider_instance_id.clone(),
            provider_name: self.provider_name.clone(),
            last_error: self.last_error.clone(),
            last_error_class: self.last_error_class.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

pub fn derive_thread_runtime(projection: &ThreadProjection) -> Option<ThreadRuntimeDetail> {
    let latest_run = derive_latest_thread_run(projection);
    let provider_session = projection
        .provider_sessions
        .iter()
        .rfind(|session| session.provider_instance_id == projection.thread.provider_instance_id);
    let usage_limited_run = presented_usage_limit_run(projection);
    let latest_run_projection = presented_latest_run(projection);
    let activity_run = derive_thread_activity_run(projection);
    let live_activity_run =
        latest_matching_run(projection, |run| is_activity_run_status(run.status));
    // Same rule as the shell runtime: only background work that holds the
    // completion parks the thread at idle; a dev server left running does not.
    let linked = projection
        .thread
        .linked_pull_request
        .as_ref()
        .and_then(|linked| linked.as_ref());
    let pull_requests = projection.thread.pull_requests.clone().or_else(|| {
        let legacy =
            crate::state::support::thread_pull_requests::thread_pull_requests_of(None, linked);
        (!legacy.is_empty()).then_some(legacy)
    });
    let pending: Vec<OrchestrationV2PendingBackgroundTask> =
        derive_pending_background_work(&DerivePendingBackgroundWorkInput {
            latest_run: latest_run_projection.map(|run| &**run),
            provider_threads: &projection.provider_threads,
            turn_items: &projection.turn_items,
            active_provider_thread_id: projection.thread.active_provider_thread_id.as_ref(),
            has_active_run: None,
            runs: Some(&projection.runs),
            pull_requests: projection
                .thread
                .pull_requests
                .as_deref()
                .or(pull_requests.as_deref()),
        });
    let background_work_holds_run = background_work_holds_completion(&pending);
    // A pull request watch can hold a thread that never ran.
    if latest_run.is_none()
        && projection.thread.active_provider_thread_id.is_none()
        && !background_work_holds_run
    {
        return None;
    }
    let active_run_id =
        latest_matching_run(projection, |run| is_interruptible_run_status(run.status))
            .map(|run| run.id.clone());
    let status = if usage_limited_run.is_some() {
        RunStatus::Failed
    } else if background_work_holds_run
        && latest_run_projection.map(|run| run.status) != Some(OrchestrationV2RunStatus::Failed)
    {
        RunStatus::Idle
    } else {
        activity_run
            .as_ref()
            .map_or(RunStatus::Idle, |run| run.status)
    };
    let summary = thread_error_summary(
        latest_root_provider_failure(
            latest_run_projection.map(|run| &**run),
            &projection.turn_items,
        ),
        provider_session.and_then(|session| session.last_error.as_deref()),
    );
    Some(ThreadRuntimeDetail {
        status,
        active_run_id,
        activity_started_at: Some(
            live_activity_run.map(|run| iso(orchestration_v2_run_work_started_at(run))),
        ),
        provider_instance_id: projection.thread.provider_instance_id.as_str().to_owned(),
        provider_name: provider_session.map(|session| session.driver.as_str().to_owned()),
        last_error: summary.last_error,
        last_error_class: summary.last_error_class.map(Into::into),
        usage_limit_reset_at: summary.usage_limit_reset_at,
        updated_at: iso(&projection.updated_at),
    })
}

pub fn thread_runtime_has_interruptible_run(runtime: Option<&ThreadRuntimeSummary>) -> bool {
    thread_runtime_is_active(runtime)
        && runtime.is_some_and(|runtime| runtime.active_run_id.is_some())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundWorkKind {
    Subagent,
    Command,
    Monitor,
    BackgroundTask,
}

impl BackgroundWorkKind {
    /// Groups work the way a reader thinks about it: agents first, loose tasks last.
    fn order(self) -> u8 {
        match self {
            Self::Subagent => 0,
            Self::Command => 1,
            Self::Monitor => 2,
            Self::BackgroundTask => 3,
        }
    }

    fn singular(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
            Self::Command => "command",
            Self::Monitor => "monitor",
            Self::BackgroundTask => "background task",
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Self::Subagent => "subagents",
            Self::Command => "commands",
            Self::Monitor => "monitors",
            Self::BackgroundTask => "background tasks",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingBackgroundWorkItem {
    pub task_id: String,
    pub kind: BackgroundWorkKind,
    /// The work's name, or its noun when the provider gave none.
    pub label: String,
    /// A subagent's own thread, when it has one.
    pub child_thread_id: Option<ThreadId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingBackgroundWorkPresentation {
    /// "Waiting on subagent Review src/math.ts", "Waiting on 2 subagents and 1 command",
    /// or "Running: Start the dev server" when only commands remain.
    pub title: String,
    pub items: Vec<PendingBackgroundWorkItem>,
    /// True when the work will wake the agent (subagents, monitors). False when
    /// only commands remain, such as a dev server: the agent is done.
    pub waiting: bool,
}

fn join_with_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Names what a settled thread still runs, grouped by kind, for the composer strip.
pub fn present_pending_background_work(
    tasks: &[OrchestrationV2PendingBackgroundTask],
) -> Option<PendingBackgroundWorkPresentation> {
    if tasks.is_empty() {
        return None;
    }
    let waiting = background_work_holds_completion(tasks);
    let mut items: Vec<PendingBackgroundWorkItem> = tasks
        .iter()
        .map(|task| {
            let kind = match task {
                OrchestrationV2PendingBackgroundTask::Subagent { .. } => {
                    BackgroundWorkKind::Subagent
                }
                OrchestrationV2PendingBackgroundTask::Command { .. } => BackgroundWorkKind::Command,
                OrchestrationV2PendingBackgroundTask::Monitor { .. } => BackgroundWorkKind::Monitor,
                OrchestrationV2PendingBackgroundTask::BackgroundTask { .. } => {
                    BackgroundWorkKind::BackgroundTask
                }
            };
            let description = task.description().map(|d| js_trim(d.as_str()).to_owned());
            let label = match (&description, kind) {
                (Some(description), BackgroundWorkKind::Subagent) => {
                    Some(js_trim(&format_subagent_display_title(description)).to_owned())
                }
                (description, _) => description.clone(),
            };
            PendingBackgroundWorkItem {
                task_id: task.task_id().as_str().to_owned(),
                kind,
                label: match label {
                    Some(label) if !label.is_empty() => label,
                    _ => kind.singular().to_owned(),
                },
                child_thread_id: match task {
                    OrchestrationV2PendingBackgroundTask::Subagent {
                        child_thread_id, ..
                    } => child_thread_id.clone(),
                    _ => None,
                },
            }
        })
        .collect();
    // Stable, like `Array#sort`.
    items.sort_by_key(|item| item.kind.order());
    if let [only] = items.as_slice() {
        let noun = only.kind.singular();
        let named = only.label != noun;
        let title = if waiting {
            if named {
                format!(
                    "{}{}",
                    if only.kind == BackgroundWorkKind::Monitor {
                        String::new()
                    } else {
                        format!("Waiting on {noun} ")
                    },
                    only.label
                )
            } else {
                format!("Waiting on a {noun}")
            }
        } else if named {
            format!("Running: {}", only.label)
        } else {
            format!("Running a {noun}")
        };
        return Some(PendingBackgroundWorkPresentation {
            title,
            items,
            waiting,
        });
    }
    // Counts in first-seen kind order, like a JS `Map`.
    let mut order: Vec<BackgroundWorkKind> = Vec::new();
    let mut counts: HashMap<BackgroundWorkKind, usize> = HashMap::new();
    for item in &items {
        if !counts.contains_key(&item.kind) {
            order.push(item.kind);
        }
        *counts.entry(item.kind).or_insert(0) += 1;
    }
    let groups: Vec<String> = order
        .into_iter()
        .map(|kind| {
            let count = counts[&kind];
            format!(
                "{count} {}",
                if count == 1 {
                    kind.singular()
                } else {
                    kind.plural()
                }
            )
        })
        .collect();
    Some(PendingBackgroundWorkPresentation {
        title: format!(
            "{} {}",
            if waiting { "Waiting on" } else { "Running" },
            join_with_and(&groups)
        ),
        items,
        waiting,
    })
}

impl std::hash::Hash for BackgroundWorkKind {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.order().hash(state);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderGoalPresentation {
    /// "Pursuing goal", "Goal paused", "Goal complete"...
    pub title: String,
    pub objective: String,
    /// "12k / 50k tokens · 4m" for Codex, "2 checks" for Claude; `None` when nothing is reported.
    pub usage: Option<String>,
    /// Only Codex stops a goal short of done, and only a stopped goal can resume.
    pub can_resume: bool,
}

fn provider_goal_title(status: OrchestrationV2ProviderGoalStatus) -> &'static str {
    match status {
        OrchestrationV2ProviderGoalStatus::Active => "Pursuing goal",
        OrchestrationV2ProviderGoalStatus::Paused => "Goal paused",
        OrchestrationV2ProviderGoalStatus::Blocked => "Goal blocked",
        OrchestrationV2ProviderGoalStatus::UsageLimited => "Goal hit a usage limit",
        OrchestrationV2ProviderGoalStatus::BudgetLimited => "Goal reached its token budget",
        OrchestrationV2ProviderGoalStatus::Complete => "Goal complete",
    }
}

fn format_goal_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    if tokens < 1_000_000 {
        return format!("{}k", (tokens as f64 / 1_000.0).round());
    }
    let formatted = format!("{:.1}", tokens as f64 / 1_000_000.0);
    format!("{}m", formatted.strip_suffix(".0").unwrap_or(&formatted))
}

/// Status line for a native `/goal`, shared by the web composer row and mobile.
/// An active goal on an idle thread (Claude after Stop) is set, not pursued.
pub fn present_provider_goal(
    goal: &OrchestrationV2ProviderGoal,
    working: bool,
) -> ProviderGoalPresentation {
    let mut usage: Vec<String> = Vec::new();
    if let Some(tokens_used) = goal.tokens_used.filter(|used| *used > 0) {
        usage.push(match goal.token_budget.flatten() {
            None => format!("{} tokens", format_goal_tokens(tokens_used)),
            Some(budget) => format!(
                "{} / {} tokens",
                format_goal_tokens(tokens_used),
                format_goal_tokens(budget)
            ),
        });
    }
    if let Some(seconds) = goal.time_used_seconds.filter(|seconds| *seconds >= 60) {
        usage.push(format_duration(seconds as f64 * 1_000.0));
    }
    if let Some(checks) = goal.checks.filter(|checks| *checks > 0) {
        usage.push(format!(
            "{checks} {}",
            if checks == 1 { "check" } else { "checks" }
        ));
    }
    ProviderGoalPresentation {
        title: if goal.status == OrchestrationV2ProviderGoalStatus::Active && !working {
            "Goal set".to_owned()
        } else {
            provider_goal_title(goal.status).to_owned()
        },
        objective: goal.objective.as_str().to_owned(),
        usage: (!usage.is_empty()).then(|| usage.join(" · ")),
        can_resume: goal.status != OrchestrationV2ProviderGoalStatus::Active
            && goal.status != OrchestrationV2ProviderGoalStatus::Complete,
    }
}

/// The thread a notification row opens: that of the one subagent or delegated task it reports.
pub fn notification_child_thread_id(
    source: &OrchestrationV2NotificationSource,
) -> Option<&ThreadId> {
    match source {
        OrchestrationV2NotificationSource::Subagent {
            child_thread_id, ..
        }
        | OrchestrationV2NotificationSource::DelegatedTask {
            child_thread_id, ..
        } => child_thread_id.as_ref(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::RunId;
    use crate::state::models::{ThreadRuntimeSummary, thread_runtime_can_archive};
    use crate::state::support::thread_error::usage_limit_blocked_run;
    use crate::state::test_fixtures::{from_json, v2_projection, v2_run};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    const NOW: &str = "2026-07-28T10:00:00.000Z";

    fn run(id: &str, ordinal: u64, status: &str) -> Value {
        let mut value = serde_json::to_value(v2_run(id, ordinal, status)).unwrap();
        value["requestedAt"] = json!(NOW);
        value["startedAt"] = if status == "queued" {
            json!(null)
        } else {
            json!(NOW)
        };
        value["completedAt"] = json!(null);
        value["userMessageId"] = json!(format!("message-{id}"));
        value
    }

    fn with(mut value: Value, patch: Value) -> Value {
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        value
    }

    fn projection_of(runs: Vec<Value>, items: Vec<Value>) -> ThreadProjection {
        let mut contract = v2_projection();
        contract.runs = runs.into_iter().map(from_json).collect();
        contract.turn_items = items.into_iter().map(from_json).collect();
        contract.updated_at = NOW.parse().unwrap();
        contract.into()
    }

    fn limit_error(extra: Value) -> Value {
        with(
            json!({
                "id": "limit-error", "threadId": "thread-v2", "runId": "limited", "nodeId": "root",
                "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
                "parentItemId": null, "ordinal": 1, "type": "error", "status": "failed",
                "title": "Usage limit reached", "startedAt": NOW, "completedAt": NOW, "updatedAt": NOW,
                "failure": { "class": "usage_limit", "message": "Plan limit reached", "code": "usageLimitExceeded", "retryable": null },
            }),
            extra,
        )
    }

    fn failed_run() -> Value {
        with(
            run("limited", 1, "failed"),
            json!({ "rootNodeId": "root", "completedAt": NOW }),
        )
    }

    #[test]
    fn derives_the_current_root_failure_without_inheriting_errors_from_children_or_previous_runs() {
        let failed = failed_run();
        let projection = projection_of(vec![failed.clone()], vec![limit_error(json!({}))]);
        let runtime = derive_thread_runtime(&projection).unwrap();
        assert_eq!(runtime.last_error.as_deref(), Some("Plan limit reached"));
        assert_eq!(
            runtime.last_error_class,
            Some(ProviderFailureClass::UsageLimit)
        );

        let child = projection_of(
            vec![failed.clone()],
            vec![limit_error(json!({ "nodeId": "child" }))],
        );
        let runtime = derive_thread_runtime(&child).unwrap();
        assert_eq!((runtime.last_error, runtime.last_error_class), (None, None));

        let new_root = projection_of(
            vec![with(failed.clone(), json!({ "rootNodeId": "new-root" }))],
            vec![limit_error(json!({}))],
        );
        let runtime = derive_thread_runtime(&new_root).unwrap();
        assert_eq!((runtime.last_error, runtime.last_error_class), (None, None));

        let newer = projection_of(
            vec![failed, run("new", 2, "running")],
            vec![limit_error(json!({}))],
        );
        let runtime = derive_thread_runtime(&newer).unwrap();
        assert_eq!(runtime.status, RunStatus::Running);
        assert_eq!((runtime.last_error, runtime.last_error_class), (None, None));
    }

    #[test]
    fn counts_a_wake_runs_activity_from_the_start_of_the_work_it_continues() {
        let wake = with(
            run("wake", 2, "running"),
            json!({ "workStartedAt": "2026-07-28T09:20:00.000Z" }),
        );
        let projection = projection_of(vec![run("prompt", 1, "completed"), wake], vec![]);
        assert_eq!(
            derive_thread_runtime(&projection)
                .unwrap()
                .activity_started_at,
            Some(Some("2026-07-28T09:20:00.000Z".to_owned()))
        );
        let plain = projection_of(vec![run("prompt", 1, "running")], vec![]);
        assert_eq!(
            derive_thread_runtime(&plain).unwrap().activity_started_at,
            Some(Some("2026-07-28T10:00:00.000Z".to_owned()))
        );
    }

    #[test]
    fn keeps_a_subscription_limit_visible_while_later_messages_stay_queued() {
        let failed = failed_run();
        let queued = run("queued", 2, "queued");
        let projection = projection_of(
            vec![failed.clone(), queued.clone()],
            vec![limit_error(json!({}))],
        );
        assert_eq!(
            derive_latest_thread_run(&projection)
                .unwrap()
                .run_id
                .as_str(),
            "limited"
        );
        assert_eq!(
            derive_thread_activity_run(&projection)
                .unwrap()
                .run_id
                .as_str(),
            "limited"
        );
        let runtime = derive_thread_runtime(&projection).unwrap();
        assert_eq!(runtime.status, RunStatus::Failed);
        assert_eq!(runtime.last_error.as_deref(), Some("Plan limit reached"));
        assert_eq!(
            runtime.last_error_class,
            Some(ProviderFailureClass::UsageLimit)
        );

        let cancelled_queued = with(
            run("cancelled-queued", 3, "cancelled"),
            json!({ "startedAt": null, "completedAt": NOW }),
        );
        let three = projection_of(
            vec![failed.clone(), queued.clone(), cancelled_queued],
            vec![limit_error(json!({}))],
        );
        assert_eq!(
            usage_limit_blocked_run(&three.runs, &three.turn_items, None)
                .unwrap()
                .id
                .as_str(),
            "limited"
        );
        let runtime = derive_thread_runtime(&three).unwrap();
        assert_eq!(runtime.status, RunStatus::Failed);
        assert_eq!(
            runtime.last_error_class,
            Some(ProviderFailureClass::UsageLimit)
        );

        let provider_error = projection_of(
            vec![failed, queued],
            vec![limit_error(
                json!({ "failure": { "class": "provider_error", "message": "Plan limit reached", "code": "usageLimitExceeded", "retryable": null } }),
            )],
        );
        let runtime = derive_thread_runtime(&provider_error).unwrap();
        assert_eq!(runtime.status, RunStatus::Queued);
        assert_eq!(runtime.last_error_class, None);
    }

    #[test]
    fn keeps_live_activity_attached_to_an_executing_run_when_a_newer_run_is_queued() {
        let projection = projection_of(
            vec![
                run("run-queued", 2, "queued"),
                run("run-running", 1, "running"),
            ],
            vec![],
        );
        assert_eq!(
            derive_latest_thread_run(&projection)
                .unwrap()
                .run_id
                .as_str(),
            "run-queued"
        );
        let activity = derive_thread_activity_run(&projection).unwrap();
        assert_eq!(
            (activity.run_id.as_str(), activity.status),
            ("run-running", RunStatus::Running)
        );
        let runtime = derive_thread_runtime(&projection).unwrap();
        assert_eq!(runtime.status, RunStatus::Running);
        assert_eq!(
            runtime.active_run_id.as_ref().map(RunId::as_str),
            Some("run-running")
        );
        assert!(thread_runtime_has_interruptible_run(Some(
            &runtime.to_summary()
        )));
    }

    #[test]
    fn presents_a_held_queue_as_the_stopped_run_instead_of_queued_work() {
        let interrupted = with(
            run("run-interrupted", 1, "interrupted"),
            json!({ "completedAt": NOW }),
        );
        let held = with(run("run-held", 2, "queued"), json!({ "queueHeld": true }));
        let projection = projection_of(vec![interrupted.clone(), held.clone()], vec![]);
        assert_eq!(
            derive_latest_thread_run(&projection)
                .unwrap()
                .run_id
                .as_str(),
            "run-interrupted"
        );
        assert_eq!(
            derive_thread_activity_run(&projection)
                .unwrap()
                .run_id
                .as_str(),
            "run-interrupted"
        );
        let runtime = derive_thread_runtime(&projection).unwrap();
        assert_eq!(
            (runtime.status, runtime.active_run_id),
            (RunStatus::Interrupted, None)
        );

        // Resuming clears the hold, and the run reads as queued until it starts.
        let resumed = projection_of(
            vec![
                interrupted,
                with(held.clone(), json!({ "queueHeld": false })),
            ],
            vec![],
        );
        assert_eq!(
            derive_thread_runtime(&resumed).unwrap().status,
            RunStatus::Queued
        );

        // Recovery can hold a first message before any run executed; it is not work.
        let only_held = projection_of(vec![held], vec![]);
        assert_eq!(derive_latest_thread_run(&only_held), None);
        assert_eq!(derive_thread_runtime(&only_held), None);
    }

    #[test]
    fn does_not_expose_a_queued_only_run_as_interruptible() {
        let projection = projection_of(vec![run("run-queued", 1, "queued")], vec![]);
        let activity = derive_thread_activity_run(&projection).unwrap();
        assert_eq!(
            (activity.run_id.as_str(), activity.status),
            ("run-queued", RunStatus::Queued)
        );
        let runtime = derive_thread_runtime(&projection).unwrap();
        assert_eq!(
            (runtime.status, runtime.active_run_id.clone()),
            (RunStatus::Queued, None)
        );
        assert!(!thread_runtime_has_interruptible_run(Some(
            &runtime.to_summary()
        )));
    }

    #[test]
    fn keeps_checkpoint_wait_activity_visible_without_exposing_a_non_functional_interrupt() {
        let projection = projection_of(vec![run("run-waiting", 1, "waiting")], vec![]);
        let activity = derive_thread_activity_run(&projection).unwrap();
        assert_eq!(
            (activity.run_id.as_str(), activity.status),
            ("run-waiting", RunStatus::Waiting)
        );
        let runtime = derive_thread_runtime(&projection).unwrap();
        assert_eq!(
            (runtime.status, runtime.active_run_id.clone()),
            (RunStatus::Waiting, None)
        );
        assert!(!thread_runtime_has_interruptible_run(Some(
            &runtime.to_summary()
        )));
    }

    fn runtime_summary(status: RunStatus, active: Option<&str>) -> ThreadRuntimeSummary {
        ThreadRuntimeSummary {
            status,
            active_run_id: active.map(|id| RunId::new(id).unwrap()),
            activity_started_at: None,
            provider_instance_id: "codex".into(),
            provider_name: None,
            last_error: None,
            last_error_class: None,
            updated_at: NOW.into(),
        }
    }

    #[test]
    fn does_not_expose_a_stale_active_run_after_the_runtime_parks_at_idle() {
        assert!(!thread_runtime_has_interruptible_run(Some(
            &runtime_summary(RunStatus::Idle, Some("run-stale"))
        )));
    }

    #[test]
    fn keeps_an_active_preparing_or_starting_run_interruptible() {
        for status in [RunStatus::Preparing, RunStatus::Starting] {
            assert!(thread_runtime_has_interruptible_run(Some(
                &runtime_summary(status, Some("run-x"))
            )));
        }
    }

    fn root_turn(status: &str, started_at: &str, run_id: Value) -> Value {
        json!({
            "id": "child-root", "threadId": "thread-v2", "runId": run_id, "parentNodeId": null,
            "rootNodeId": "child-root", "kind": "root_turn", "status": status, "countsForRun": false,
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "runtimeRequestId": null, "checkpointScopeId": null, "startedAt": started_at, "completedAt": null,
        })
    }

    fn native_child(creation_source: &str, nodes: Vec<Value>) -> ThreadProjection {
        let mut contract = v2_projection();
        contract.thread.creation_source = from_json(json!(creation_source));
        contract.thread.lineage = from_json(json!({
            "parentThreadId": "parent", "relationshipToParent": "subagent", "rootThreadId": "parent",
        }));
        contract.nodes = nodes.into_iter().map(from_json).collect();
        contract.into()
    }

    #[test]
    fn times_a_provider_native_subagent_from_its_runless_root_turn_while_it_works() {
        let projection = native_child(
            "provider",
            vec![root_turn(
                "running",
                "2026-07-28T10:05:00.000Z",
                json!(null),
            )],
        );
        assert_eq!(
            derive_runless_work_started_at(&projection).as_deref(),
            Some("2026-07-28T10:05:00.000Z")
        );
        // The subagent has no run, so it stays unstoppable and unqueueable.
        assert_eq!(derive_thread_runtime(&projection), None);
    }

    #[test]
    fn is_idle_once_the_subagent_is_settled() {
        for status in ["completed", "cancelled", "failed", "interrupted", "idle"] {
            let projection = native_child("provider", vec![root_turn(status, NOW, json!(null))]);
            assert_eq!(
                derive_runless_work_started_at(&projection),
                None,
                "{status}"
            );
        }
    }

    #[test]
    fn ignores_root_turns_that_belong_to_a_run_and_threads_the_provider_does_not_run() {
        let owned = native_child("provider", vec![root_turn("running", NOW, json!("run-1"))]);
        assert_eq!(derive_runless_work_started_at(&owned), None);
        let mut ordinary = v2_projection();
        ordinary.nodes = vec![from_json(root_turn("running", NOW, json!(null)))];
        assert_eq!(derive_runless_work_started_at(&ordinary.into()), None);
    }

    #[test]
    fn reports_the_providers_own_subagent_from_its_runless_root_turn() {
        let mut node = root_turn("completed", NOW, json!(null));
        node["completedAt"] = json!(NOW);
        assert_eq!(
            derive_provider_subagent_status(&native_child("provider", vec![node.clone()])),
            Some(ProviderSubagentStatus {
                status: OrchestrationV2ExecutionNodeStatus::Completed,
                started_at: Some(NOW.into()),
                completed_at: Some(NOW.into()),
            })
        );
        // T3 delegated tasks and ordinary threads are left alone.
        assert_eq!(
            derive_provider_subagent_status(&native_child("mcp", vec![node.clone()])),
            None
        );
        let mut ordinary = v2_projection();
        ordinary.nodes = vec![from_json(node)];
        assert_eq!(derive_provider_subagent_status(&ordinary.into()), None);
    }

    #[test]
    fn says_how_long_the_subagent_has_worked_or_took() {
        let started_at = Some("2026-07-28T10:00:00.000Z".to_owned());
        let at = |iso: &str| parse_ms(iso).unwrap() as f64;
        let status = |status, completed_at: Option<&str>| ProviderSubagentStatus {
            status,
            started_at: started_at.clone(),
            completed_at: completed_at.map(str::to_owned),
        };
        assert_eq!(
            format_provider_subagent_status(
                Some(&status(OrchestrationV2ExecutionNodeStatus::Running, None)),
                at("2026-07-28T10:01:05.400Z")
            ),
            "Working 1m 5s"
        );
        assert_eq!(
            format_provider_subagent_status(
                Some(&status(
                    OrchestrationV2ExecutionNodeStatus::Completed,
                    Some("2026-07-28T10:00:34.000Z")
                )),
                at("2026-07-28T11:00:00.000Z")
            ),
            "Completed in 34s"
        );
        assert_eq!(
            format_provider_subagent_status(
                Some(&status(
                    OrchestrationV2ExecutionNodeStatus::Cancelled,
                    Some("2026-07-28T10:00:34.000Z")
                )),
                0.0
            ),
            "Cancelled"
        );
        assert_eq!(format_provider_subagent_status(None, 0.0), "Starting");
    }

    #[test]
    fn thread_runtime_can_archive_blocks_only_active_provider_work() {
        for status in [
            RunStatus::Preparing,
            RunStatus::Starting,
            RunStatus::Running,
        ] {
            assert!(
                !thread_runtime_can_archive(Some(&runtime_summary(status, Some("run-x")))),
                "{status:?}"
            );
        }
        assert!(!thread_runtime_can_archive(Some(&runtime_summary(
            RunStatus::Queued,
            Some("run-queued")
        ))));
        assert!(thread_runtime_can_archive(Some(&runtime_summary(
            RunStatus::Queued,
            None
        ))));
        assert!(thread_runtime_can_archive(Some(&runtime_summary(
            RunStatus::Waiting,
            Some("run-finished")
        ))));
        assert!(thread_runtime_can_archive(Some(&runtime_summary(
            RunStatus::Idle,
            None
        ))));
    }

    fn task(
        kind: &str,
        id: &str,
        description: Option<&str>,
        child: Option<&str>,
    ) -> OrchestrationV2PendingBackgroundTask {
        let mut value = json!({ "taskId": id, "kind": kind });
        if let Some(description) = description {
            value["description"] = json!(description);
        }
        if let Some(child) = child {
            value["childThreadId"] = json!(child);
        }
        from_json(value)
    }

    #[test]
    fn falls_back_to_the_subagent_noun_when_the_description_has_no_display_name() {
        for description in ["Subagent:", "Subagent:   "] {
            // The contract trims descriptions; `Subagent:` is what remains.
            let presentation = present_pending_background_work(&[task(
                "subagent",
                "unnamed",
                Some(description),
                None,
            )])
            .unwrap();
            assert_eq!(presentation.title, "Waiting on a subagent");
            assert_eq!(presentation.items[0].label, "subagent");
        }
    }

    #[test]
    fn uses_the_subagent_display_name() {
        for description in [
            "/root/luna_window_properties",
            "Subagent: /root/luna_window_properties",
            "/root/parent/luna_window_properties",
        ] {
            let presentation = present_pending_background_work(&[task(
                "subagent",
                "luna",
                Some(description),
                Some("thread:luna"),
            )])
            .unwrap();
            assert_eq!(
                presentation.title,
                "Waiting on subagent Luna Window Properties"
            );
            assert_eq!(presentation.items[0].label, "Luna Window Properties");
            assert_eq!(
                presentation.items[0]
                    .child_thread_id
                    .as_ref()
                    .map(|id| id.as_str()),
                Some("thread:luna")
            );
            assert!(presentation.waiting);
        }
    }

    #[test]
    fn formats_subagent_names_in_a_mixed_roster_and_preserves_command_descriptions() {
        let presentation = present_pending_background_work(&[
            task("command", "cmd", Some("/root/run_tests"), None),
            task(
                "subagent",
                "luna",
                Some("/root/luna_window_properties"),
                None,
            ),
            task("subagent", "review", Some("Review src/math.ts"), None),
        ])
        .unwrap();
        assert_eq!(presentation.title, "Waiting on 2 subagents and 1 command");
        assert_eq!(
            presentation
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Luna Window Properties",
                "Review src/math.ts",
                "/root/run_tests"
            ]
        );
    }

    #[test]
    fn names_a_single_piece_of_work_by_kind() {
        let title = |tasks: &[OrchestrationV2PendingBackgroundTask]| {
            present_pending_background_work(tasks).map(|p| p.title)
        };
        assert_eq!(
            title(&[task("subagent", "a", Some("Review src/math.ts"), None)]).as_deref(),
            Some("Waiting on subagent Review src/math.ts")
        );
        assert_eq!(
            title(&[task("monitor", "a", None, None)]).as_deref(),
            Some("Waiting on a monitor")
        );
        assert_eq!(title(&[]), None);
    }

    #[test]
    fn says_only_commands_are_running_not_waited_on() {
        let present = |tasks: &[OrchestrationV2PendingBackgroundTask]| {
            present_pending_background_work(tasks).unwrap()
        };
        let one = present(&[task(
            "command",
            "dev",
            Some("Start the shared dev server"),
            None,
        )]);
        assert_eq!(
            (one.title.as_str(), one.waiting),
            ("Running: Start the shared dev server", false)
        );
        let nameless = present(&[task("command", "a", None, None)]);
        assert_eq!(
            (nameless.title.as_str(), nameless.waiting),
            ("Running a command", false)
        );
        let two = present(&[
            task("command", "a", Some("vp run dev"), None),
            task("command", "b", Some("tailscale serve"), None),
        ]);
        assert_eq!(
            (two.title.as_str(), two.waiting),
            ("Running 2 commands", false)
        );
        let mixed = present(&[
            task("command", "a", Some("vp run dev"), None),
            task("monitor", "b", Some("Watch PR checks"), None),
        ]);
        assert_eq!(
            (mixed.title.as_str(), mixed.waiting),
            ("Waiting on 1 command and 1 monitor", true)
        );
    }

    #[test]
    fn groups_work_by_kind_subagents_first_and_keeps_each_name() {
        let presentation = present_pending_background_work(&[
            task("command", "cmd", Some("npm test"), None),
            task("subagent", "b", Some("Write tests"), Some("thread:b")),
            task("subagent", "a", Some("Review src/math.ts"), None),
        ])
        .unwrap();
        assert_eq!(presentation.title, "Waiting on 2 subagents and 1 command");
        assert_eq!(
            presentation
                .items
                .iter()
                .map(|item| (
                    item.kind,
                    item.label.as_str(),
                    item.child_thread_id.as_ref().map(|id| id.as_str())
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    BackgroundWorkKind::Subagent,
                    "Write tests",
                    Some("thread:b")
                ),
                (BackgroundWorkKind::Subagent, "Review src/math.ts", None),
                (BackgroundWorkKind::Command, "npm test", None),
            ]
        );
    }

    #[test]
    fn names_generic_work_including_rosters_from_servers_that_predate_kinds() {
        let title = |tasks: &[OrchestrationV2PendingBackgroundTask]| {
            present_pending_background_work(tasks).unwrap().title
        };
        assert_eq!(
            title(&[
                task("command", "bash", Some("Background sleep"), None),
                task("monitor", "watch", None, None),
                task("background_task", "other", None, None),
            ]),
            "Waiting on 1 command, 1 monitor and 1 background task"
        );
        assert_eq!(
            title(&[task("background_task", "old", None, None)]),
            "Waiting on a background task"
        );
    }

    #[test]
    fn reads_only_the_active_provider_threads_reported_selection() {
        let reported = json!({ "instanceId": "codex", "model": "gpt-5.4", "options": [{ "id": "reasoningEffort", "value": "default" }] });
        let provider_thread = json!({
            "id": "active", "driver": "codex", "providerInstanceId": "codex", "providerSessionId": "session",
            "appThreadId": "thread-v2", "ownerNodeId": null, "nativeThreadRef": null,
            "nativeConversationHeadRef": null, "status": "idle", "firstRunOrdinal": null,
            "lastRunOrdinal": null, "handoffIds": [], "forkedFrom": null, "createdAt": NOW, "updatedAt": NOW,
            "nativeMetadata": { "modelSelection": reported },
        });
        let build = |active: Option<&str>, instance: &str| {
            let mut contract = v2_projection();
            contract.thread.active_provider_thread_id =
                active.map(|id| crate::state::contracts::ProviderThreadId::new(id).unwrap());
            contract.provider_threads = vec![from_json(with(
                provider_thread.clone(),
                json!({ "providerInstanceId": instance }),
            ))];
            ThreadProjection::from(contract)
        };
        let expected: ModelSelection = from_json(reported);
        assert_eq!(
            derive_reported_model_selection(&build(Some("active"), "codex")),
            Some(&expected)
        );
        assert_eq!(derive_reported_model_selection(&build(None, "codex")), None);
        assert_eq!(
            derive_reported_model_selection(&build(Some("active"), "other")),
            None
        );
    }

    fn goal(value: Value) -> OrchestrationV2ProviderGoal {
        from_json(value)
    }

    #[test]
    fn summarizes_codex_accounting_and_offers_resume_once_the_goal_stops_short() {
        assert_eq!(
            present_provider_goal(
                &goal(
                    json!({ "objective": "Ship the feature", "status": "paused", "tokensUsed": 12400, "tokenBudget": 50000, "timeUsedSeconds": 245 })
                ),
                false
            ),
            ProviderGoalPresentation {
                title: "Goal paused".into(),
                objective: "Ship the feature".into(),
                usage: Some("12k / 50k tokens · 4m 5s".into()),
                can_resume: true,
            }
        );
    }

    #[test]
    fn counts_claude_evaluator_checks_and_never_offers_resume() {
        assert_eq!(
            present_provider_goal(
                &goal(json!({ "objective": "All tests pass", "status": "active", "checks": 1 })),
                true
            ),
            ProviderGoalPresentation {
                title: "Pursuing goal".into(),
                objective: "All tests pass".into(),
                usage: Some("1 check".into()),
                can_resume: false,
            }
        );
        assert_eq!(
            present_provider_goal(
                &goal(json!({ "objective": "All tests pass", "status": "complete", "checks": 0 })),
                false
            )
            .usage,
            None
        );
        assert_eq!(
            present_provider_goal(
                &goal(json!({ "objective": "All tests pass", "status": "active" })),
                false
            )
            .title,
            "Goal set"
        );
    }
}
