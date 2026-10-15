//! The shell (sidebar row) side of ProjectionStore.ts: `threadShellFromProjection`,
//! the SQL shell snapshot, per-thread shell, settlement candidates and the
//! other list-shaped reads that avoid loading transcripts.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, Row, params};
use serde_json::Value;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ActivityRunStatus, OrchestrationV2AppThread, OrchestrationV2ForkSource,
    OrchestrationV2LatestVisibleMessageSummary, OrchestrationV2PendingBackgroundTask,
    OrchestrationV2PendingRuntimeRequestSummary, OrchestrationV2ProviderFailureClass,
    OrchestrationV2ProviderGoal, OrchestrationV2ProviderThread, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2RuntimeRequest, OrchestrationV2SettleOverride,
    OrchestrationV2ShellThreadStatus, OrchestrationV2ThreadHistoryOrigin,
    OrchestrationV2ThreadProjection, OrchestrationV2ThreadShell,
    OrchestrationV2ThreadShellSnapshot, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    orchestration_v2_run_work_started_at,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::thread_pull_request::ThreadLinkedPullRequest;
use turin_contracts::{PositiveInt, ProjectId, RunId, ThreadId, UtcDateTime};

use crate::projection::error::ProjectionStoreError;
use crate::projection::shared::pending_background_work::{
    DerivePendingBackgroundWorkInput, PendingBackgroundWorkItem,
    PendingBackgroundWorkProviderThread, PendingBackgroundWorkRun, derive_pending_background_work,
};
use crate::projection::shared::thread_error::{
    ThreadErrorSummary, latest_root_provider_failure, latest_unheld_run, thread_error_summary,
    usage_limit_run_presented_as_latest,
};
use crate::projection::shared::thread_pull_requests::thread_pull_requests_of;
use crate::projection::sql::{CauseResult, decode, id_list, query_map};
use crate::projection::store::ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION;
use crate::projection::store::canonical::active_local_turn_items;
use crate::projection::turn_item;

/// `ShellSnapshotOptions.location`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellSnapshotLocation {
    Active,
    Archive,
}

/// `ShellSnapshotOptions`.
#[derive(Debug, Clone, Default)]
pub struct ShellSnapshotOptions {
    pub location: Option<ShellSnapshotLocation>,
    /// Reads only this project's threads. Fork sources in other projects still
    /// load, so visible item counts match the unscoped snapshot.
    pub project_id: Option<ProjectId>,
    /// For background sweeps, not clients: skips settled threads before any of
    /// their run, item or session rows are read.
    pub unsettled_only: bool,
}

// ---------------------------------------------------------------------------
// In-memory twin: threadShellFromProjection
// ---------------------------------------------------------------------------

/// An agent waiting on a secret is waiting on the user just like a question,
/// so the shell reports it as pending user input. Secret requests have no
/// runtime request of their own; this stands one in for the shell summary
/// only, keyed by the card's turn item.
fn secret_request_as_pending_input(
    item: Option<&OrchestrationV2TurnItem>,
) -> Option<OrchestrationV2RuntimeRequest> {
    let item = item?;
    if !matches!(item, OrchestrationV2TurnItem::SecretRequest { .. }) {
        return None;
    }
    let common = turn_item::common(item);
    let node_id = common.node_id?;
    serde_json::from_value(serde_json::json!({
        "id": common.id,
        "nodeId": node_id,
        "providerTurnId": common.provider_turn_id,
        "nativeRequestRef": null,
        "kind": "user_input",
        "status": "pending",
        "responseCapability": {"type": "message"},
        "createdAt": common.started_at.unwrap_or(common.updated_at),
        "resolvedAt": null
    }))
    .ok()
}

/// Provider instances that have owned this thread's root conversation, oldest
/// first. Subagent provider threads carry an owner node and are excluded so a
/// delegated Codex child does not make a Claude thread look handed off.
fn provider_instance_history_for_shell(
    thread_id: &ThreadId,
    provider_threads: &[OrchestrationV2ProviderThread],
) -> Vec<ProviderInstanceId> {
    let mut roots = provider_threads
        .iter()
        .filter(|thread| {
            thread.app_thread_id.as_ref() == Some(thread_id) && thread.owner_node_id.is_none()
        })
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut history: Vec<ProviderInstanceId> = Vec::new();
    for provider_thread in roots {
        if !history.contains(&provider_thread.provider_instance_id) {
            history.push(provider_thread.provider_instance_id.clone());
        }
    }
    history
}

/// The native goal lives on the provider thread that currently owns the conversation.
fn active_provider_goal_for_shell(
    provider_threads: &[OrchestrationV2ProviderThread],
    active_provider_thread_id: Option<&turin_contracts::ProviderThreadId>,
) -> Option<OrchestrationV2ProviderGoal> {
    let active = active_provider_thread_id?;
    provider_threads
        .iter()
        .find(|thread| &thread.id == active)
        .and_then(|thread| thread.goal.clone())
}

fn is_interruptible_run_for_shell(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
    )
}

fn activity_status(status: OrchestrationV2RunStatus) -> Option<OrchestrationV2ActivityRunStatus> {
    match status {
        OrchestrationV2RunStatus::Preparing => Some(OrchestrationV2ActivityRunStatus::Preparing),
        OrchestrationV2RunStatus::Starting => Some(OrchestrationV2ActivityRunStatus::Starting),
        OrchestrationV2RunStatus::Running => Some(OrchestrationV2ActivityRunStatus::Running),
        OrchestrationV2RunStatus::Waiting => Some(OrchestrationV2ActivityRunStatus::Waiting),
        _ => None,
    }
}

fn is_activity_run_for_shell(run: &OrchestrationV2Run) -> bool {
    activity_status(run.status).is_some()
}

fn shell_status_from_run_status(
    status: OrchestrationV2RunStatus,
) -> OrchestrationV2ShellThreadStatus {
    match status {
        OrchestrationV2RunStatus::Preparing => OrchestrationV2ShellThreadStatus::Preparing,
        OrchestrationV2RunStatus::Queued => OrchestrationV2ShellThreadStatus::Queued,
        OrchestrationV2RunStatus::Starting => OrchestrationV2ShellThreadStatus::Starting,
        OrchestrationV2RunStatus::Running => OrchestrationV2ShellThreadStatus::Running,
        OrchestrationV2RunStatus::Waiting => OrchestrationV2ShellThreadStatus::Waiting,
        OrchestrationV2RunStatus::Completed => OrchestrationV2ShellThreadStatus::Completed,
        OrchestrationV2RunStatus::Interrupted => OrchestrationV2ShellThreadStatus::Interrupted,
        OrchestrationV2RunStatus::Failed => OrchestrationV2ShellThreadStatus::Failed,
        OrchestrationV2RunStatus::Cancelled => OrchestrationV2ShellThreadStatus::Cancelled,
        OrchestrationV2RunStatus::RolledBack => OrchestrationV2ShellThreadStatus::RolledBack,
    }
}

/// `shellStatusFromStoredRunStatus`.
fn shell_status_from_stored(status: Option<&str>) -> OrchestrationV2ShellThreadStatus {
    match status {
        None => OrchestrationV2ShellThreadStatus::Idle,
        Some(status) => {
            serde_json::from_value::<OrchestrationV2RunStatus>(Value::String(status.to_owned()))
                .map(shell_status_from_run_status)
                .unwrap_or(OrchestrationV2ShellThreadStatus::Failed)
        }
    }
}

/// Everything a shell row is made of, whichever read produced it.
struct ShellThreadState {
    thread: OrchestrationV2AppThread,
    latest_run_id: Option<RunId>,
    latest_run_status: OrchestrationV2ShellThreadStatus,
    latest_run_requested_at: Option<UtcDateTime>,
    latest_run_started_at: Option<UtcDateTime>,
    latest_run_completed_at: Option<UtcDateTime>,
    active_run_id: Option<RunId>,
    activity_run_status: Option<OrchestrationV2ActivityRunStatus>,
    activity_run_started_at: Option<UtcDateTime>,
    error: ThreadErrorSummary,
    pending_runtime_request: Option<OrchestrationV2RuntimeRequest>,
    latest_user_message_at: Option<UtcDateTime>,
    latest_user_authored_message_at: Option<UtcDateTime>,
    has_actionable_proposed_plan: bool,
    pending_background_tasks: Vec<OrchestrationV2PendingBackgroundTask>,
    provider_instance_history: Vec<ProviderInstanceId>,
    goal: Option<OrchestrationV2ProviderGoal>,
    item_count: u64,
    runless_item_count: u64,
    updated_at: UtcDateTime,
    run_ordinal_by_id: HashMap<RunId, u64>,
    item_count_by_run_id: HashMap<RunId, u64>,
}

fn shell_from_state(
    state: &ShellThreadState,
    visible_item_count: u64,
) -> OrchestrationV2ThreadShell {
    let thread = &state.thread;
    OrchestrationV2ThreadShell {
        created_by: thread.created_by,
        creation_source: thread.creation_source,
        id: thread.id.clone(),
        project_id: thread.project_id.clone(),
        title: thread.title.as_str().to_owned(),
        provider_instance_id: thread.provider_instance_id.clone(),
        model_selection: thread.model_selection.clone(),
        runtime_mode: thread.runtime_mode,
        interaction_mode: thread.interaction_mode,
        branch: thread.branch.clone(),
        worktree_path: thread.worktree_path.clone(),
        linked_pull_request: thread.linked_pull_request.clone(),
        pull_requests: Some(thread_pull_requests_of(thread)),
        branch_pull_request: thread.branch_pull_request.clone(),
        lineage: thread.lineage.clone(),
        forked_from: thread.forked_from.clone(),
        active_provider_thread_id: thread.active_provider_thread_id.clone(),
        history_origin: thread.history_origin,
        latest_run_id: state.latest_run_id.clone(),
        latest_run_requested_at: Some(state.latest_run_requested_at),
        latest_run_started_at: Some(state.latest_run_started_at),
        latest_run_completed_at: Some(state.latest_run_completed_at),
        active_run_id: state.active_run_id.clone(),
        activity_run_started_at: Some(state.activity_run_started_at),
        activity_run_status: Some(state.activity_run_status),
        status: state.latest_run_status,
        last_error: Some(state.error.last_error.clone()),
        last_error_class: Some(state.error.last_error_class),
        usage_limit_reset_at: Some(state.error.usage_limit_reset_at.clone()),
        pending_runtime_request: state.pending_runtime_request.as_ref().map(|request| {
            OrchestrationV2PendingRuntimeRequestSummary {
                id: request.id.clone(),
                kind: request.kind,
                created_at: request.created_at,
            }
        }),
        // Thread detail owns message bodies. Keeping them out of shell rows makes
        // initial hydration and streaming updates independent of transcript size.
        latest_visible_message: None::<OrchestrationV2LatestVisibleMessageSummary>,
        latest_user_message_at: state.latest_user_message_at,
        latest_user_authored_message_at: Some(state.latest_user_authored_message_at),
        has_actionable_proposed_plan: state.has_actionable_proposed_plan,
        pending_background_tasks: state.pending_background_tasks.clone(),
        provider_instance_history: state.provider_instance_history.clone(),
        goal: Some(state.goal.clone()),
        item_count: state.item_count,
        visible_item_count,
        created_at: thread.created_at,
        updated_at: state.updated_at,
        archived_at: thread.archived_at,
        settled_override: thread.settled_override,
        settled_at: thread.settled_at,
        unsettled_at: Some(thread.unsettled_at.flatten()),
        snoozed_until: Some(thread.snoozed_until.flatten()),
        snoozed_at: Some(thread.snoozed_at.flatten()),
        limit_recovery: Some(thread.limit_recovery.clone().flatten()),
        pinned_at: Some(thread.pinned_at.flatten()),
        auto_settle_disabled_at: Some(thread.auto_settle_disabled_at.flatten()),
        pin_order_key: Some(thread.pin_order_key.clone().flatten()),
        active_order_key: thread.active_order_key.clone(),
        last_visited_at: Some(thread.last_visited_at),
        title_regeneration: Some(thread.title_regeneration.clone().flatten()),
        deleted_at: thread.deleted_at,
    }
}

/// `threadShellFromProjection`: the shell of an already-loaded projection.
pub fn thread_shell_from_projection(
    projection: &OrchestrationV2ThreadProjection,
) -> OrchestrationV2ThreadShell {
    let thread = &projection.thread;
    let provider_session = projection
        .provider_sessions
        .iter()
        .filter(|session| session.provider_instance_id == thread.provider_instance_id)
        .max_by_key(|session| session.updated_at);
    let session_error = provider_session.and_then(|session| session.last_error.as_deref());
    let latest_run = usage_limit_run_presented_as_latest(
        &projection.runs,
        &projection.turn_items,
        session_error,
    )
    .or_else(|| latest_unheld_run(&projection.runs));
    let active_run = projection
        .runs
        .iter()
        .filter(|run| is_interruptible_run_for_shell(run))
        .max_by_key(|run| run.ordinal);
    let activity_run = projection
        .runs
        .iter()
        .filter(|run| is_activity_run_for_shell(run))
        .max_by_key(|run| run.ordinal);
    let live_run_ids = projection
        .runs
        .iter()
        .filter(|run| is_activity_run_for_shell(run))
        .map(|run| &run.id)
        .collect::<HashSet<_>>();
    let pending_runtime_request = projection
        .runtime_requests
        .iter()
        .filter(|request| {
            request.status
                == turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequestStatus::Pending
        })
        .max_by_key(|request| request.created_at)
        .cloned()
        .or_else(|| {
            let newest_secret = projection
                .turn_items
                .iter()
                .filter(|item| {
                    let common = turn_item::common(item);
                    matches!(item, OrchestrationV2TurnItem::SecretRequest { .. })
                        && common.status == OrchestrationV2TurnItemStatus::Waiting
                        && common
                            .run_id
                            .is_some_and(|run_id| live_run_ids.contains(run_id))
                })
                .max_by_key(|item| turn_item::common(item).updated_at);
            secret_request_as_pending_input(newest_secret)
        });
    let mut user_messages = projection
        .messages
        .iter()
        .filter(|message| {
            message.role
                == turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::User
        })
        .collect::<Vec<_>>();
    user_messages.sort_by_key(|message| std::cmp::Reverse(message.updated_at));
    let latest_user_message = user_messages.first();
    let runs_for_work = projection
        .runs
        .iter()
        .map(|run| PendingBackgroundWorkRun {
            id: run.id.as_str(),
            ordinal: run.ordinal.get(),
            status: run.status,
        })
        .collect::<Vec<_>>();
    let latest_pending_run = latest_run.map(|run| PendingBackgroundWorkRun {
        id: run.id.as_str(),
        ordinal: run.ordinal.get(),
        status: run.status,
    });
    let provider_thread_views = projection
        .provider_threads
        .iter()
        .map(|provider_thread| PendingBackgroundWorkProviderThread {
            id: provider_thread.id.as_str(),
            pending_background_tasks: &provider_thread.pending_background_tasks,
        })
        .collect::<Vec<_>>();
    let item_views = projection
        .turn_items
        .iter()
        .map(PendingBackgroundWorkItem::from_turn_item)
        .collect::<Vec<_>>();
    let pull_requests = thread_pull_requests_of(thread);
    let pending_background_tasks =
        derive_pending_background_work(&DerivePendingBackgroundWorkInput {
            latest_run: latest_pending_run,
            provider_threads: provider_thread_views,
            turn_items: item_views,
            active_provider_thread_id: thread
                .active_provider_thread_id
                .as_ref()
                .map(|id| id.as_str()),
            has_active_run: None,
            runs: Some(runs_for_work),
            pull_requests: Some(&pull_requests),
        });
    let failure = latest_root_provider_failure(latest_run, &projection.turn_items);
    let state = ShellThreadState {
        thread: thread.clone(),
        latest_run_id: latest_run.map(|run| run.id.clone()),
        latest_run_status: latest_run
            .map(|run| shell_status_from_run_status(run.status))
            .unwrap_or(OrchestrationV2ShellThreadStatus::Idle),
        latest_run_requested_at: latest_run.map(|run| run.requested_at),
        latest_run_started_at: latest_run.and_then(|run| run.started_at),
        latest_run_completed_at: latest_run.and_then(|run| run.completed_at),
        active_run_id: active_run.map(|run| run.id.clone()),
        activity_run_status: activity_run.and_then(|run| activity_status(run.status)),
        activity_run_started_at: activity_run.map(|run| *orchestration_v2_run_work_started_at(run)),
        error: thread_error_summary(failure, session_error),
        pending_runtime_request,
        latest_user_message_at: latest_user_message.map(|message| message.updated_at),
        latest_user_authored_message_at: user_messages
            .iter()
            .find(|message| message.created_by == turin_contracts::orchestration_v2::OrchestrationV2Actor::User)
            .map(|message| message.updated_at),
        has_actionable_proposed_plan: projection.plans.iter().any(|plan| {
            matches!(
                plan,
                turin_contracts::orchestration_v2::OrchestrationV2PlanArtifact::ProposedPlan {
                    status: turin_contracts::orchestration_v2::OrchestrationV2PlanArtifactStatus::Active,
                    ..
                }
            )
        }),
        pending_background_tasks,
        provider_instance_history: provider_instance_history_for_shell(&thread.id, &projection.provider_threads),
        goal: active_provider_goal_for_shell(&projection.provider_threads, thread.active_provider_thread_id.as_ref()),
        item_count: active_local_turn_items(projection).len() as u64,
        runless_item_count: 0,
        updated_at: projection.updated_at,
        run_ordinal_by_id: HashMap::new(),
        item_count_by_run_id: HashMap::new(),
    };
    shell_from_state(&state, projection.visible_turn_items.len() as u64)
}

// ---------------------------------------------------------------------------
// SQL shell snapshot
// ---------------------------------------------------------------------------

/// One thread's row of the shell query: the thread payload plus the latest-run
/// and activity facts SQL computes, so no run or item history is read.
struct ShellThreadRow {
    thread_id: String,
    payload_json: String,
    forked_from_run_source_thread_id: Option<String>,
    latest_run_id: Option<String>,
    latest_run_status: Option<String>,
    latest_run_requested_at: Option<String>,
    latest_run_started_at: Option<String>,
    latest_run_completed_at: Option<String>,
    active_run_id: Option<String>,
    activity_run_status: Option<String>,
    activity_run_started_at: Option<String>,
    last_error: Option<String>,
    terminal_failure_payload_json: Option<String>,
    blocking_run_id: Option<String>,
    blocking_run_requested_at: Option<String>,
    blocking_run_started_at: Option<String>,
    blocking_run_completed_at: Option<String>,
    blocking_failure_payload_json: Option<String>,
    pending_request_payload_json: Option<String>,
    pending_secret_request_payload_json: Option<String>,
    latest_user_message_at: Option<String>,
    latest_user_authored_message_at: Option<String>,
    has_actionable_proposed_plan: bool,
    item_count: u64,
    runless_item_count: u64,
}

impl ShellThreadRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            thread_id: row.get("thread_id")?,
            payload_json: row.get("payload_json")?,
            forked_from_run_source_thread_id: row.get("forked_from_run_source_thread_id")?,
            latest_run_id: row.get("latest_run_id")?,
            latest_run_status: row.get("latest_run_status")?,
            latest_run_requested_at: row.get("latest_run_requested_at")?,
            latest_run_started_at: row.get("latest_run_started_at")?,
            latest_run_completed_at: row.get("latest_run_completed_at")?,
            active_run_id: row.get("active_run_id")?,
            activity_run_status: row.get("activity_run_status")?,
            activity_run_started_at: row.get("activity_run_started_at")?,
            last_error: row.get("last_error")?,
            terminal_failure_payload_json: row.get("terminal_failure_payload_json")?,
            blocking_run_id: row.get("blocking_run_id")?,
            blocking_run_requested_at: row.get("blocking_run_requested_at")?,
            blocking_run_started_at: row.get("blocking_run_started_at")?,
            blocking_run_completed_at: row.get("blocking_run_completed_at")?,
            blocking_failure_payload_json: row.get("blocking_failure_payload_json")?,
            pending_request_payload_json: row.get("pending_request_payload_json")?,
            pending_secret_request_payload_json: row.get("pending_secret_request_payload_json")?,
            latest_user_message_at: row.get("latest_user_message_at")?,
            latest_user_authored_message_at: row.get("latest_user_authored_message_at")?,
            has_actionable_proposed_plan: row.get::<_, i64>("has_actionable_proposed_plan")? == 1,
            item_count: row.get::<_, i64>("item_count")?.max(0) as u64,
            runless_item_count: row.get::<_, i64>("runless_item_count")?.max(0) as u64,
        })
    }
}

/// Shell query up to (not including) its WHERE clause.
const SHELL_THREAD_ROWS_SELECT: &str = "
SELECT
  t.thread_id,
  t.payload_json,
  CASE
    WHEN json_extract(t.payload_json, '$.forkedFrom.type') = 'run'
      THEN json_extract(t.payload_json, '$.forkedFrom.threadId')
    ELSE NULL
  END AS forked_from_run_source_thread_id,
  presented.run_id AS latest_run_id,
  presented.status AS latest_run_status,
  presented.requested_at AS latest_run_requested_at,
  json_extract(presented.payload_json, '$.startedAt') AS latest_run_started_at,
  presented.completed_at AS latest_run_completed_at,
  (
    SELECT r.run_id
    FROM orchestration_v2_projection_runs r
    WHERE r.thread_id = t.thread_id
      AND r.status IN ('preparing', 'starting', 'running')
    ORDER BY r.ordinal DESC, r.run_id DESC
    LIMIT 1
  ) AS active_run_id,
  (
    SELECT r.status
    FROM orchestration_v2_projection_runs r
    WHERE r.thread_id = t.thread_id
      AND r.status IN ('preparing', 'starting', 'running', 'waiting')
    ORDER BY r.ordinal DESC, r.run_id DESC
    LIMIT 1
  ) AS activity_run_status,
  (
    -- Mirrors orchestrationV2RunWorkStartedAt.
    SELECT COALESCE(
      json_extract(r.payload_json, '$.workStartedAt'),
      json_extract(r.payload_json, '$.startedAt'),
      r.requested_at
    )
    FROM orchestration_v2_projection_runs r
    WHERE r.thread_id = t.thread_id
      AND r.status IN ('preparing', 'starting', 'running', 'waiting')
    ORDER BY r.ordinal DESC, r.run_id DESC
    LIMIT 1
  ) AS activity_run_started_at,
  (
    SELECT json_extract(session.payload_json, '$.lastError')
    -- Keep bindings outermost: one provider instance can hold every session.
    FROM orchestration_v2_projection_provider_session_bindings binding
    CROSS JOIN orchestration_v2_projection_provider_sessions session
      ON session.provider_session_id = binding.provider_session_id
    WHERE binding.thread_id = t.thread_id
      AND session.provider_instance_id = t.provider_instance_id
    ORDER BY session.updated_at DESC, session.provider_session_id DESC
    LIMIT 1
  ) AS last_error,
  (
    SELECT item.payload_json
    FROM orchestration_v2_projection_turn_items item
    INNER JOIN orchestration_v2_projection_runs r ON r.run_id = item.run_id
    WHERE r.run_id = presented.run_id
      AND r.status = 'failed'
      AND item.thread_id = t.thread_id
      AND item.type = 'error' AND item.status = 'failed'
      AND item.node_id IS json_extract(r.payload_json, '$.rootNodeId')
    ORDER BY item.updated_at DESC, item.ordinal DESC, item.turn_item_id DESC
    LIMIT 1
  ) AS terminal_failure_payload_json,
  blocked.run_id AS blocking_run_id,
  blocked.requested_at AS blocking_run_requested_at,
  json_extract(blocked.payload_json, '$.startedAt') AS blocking_run_started_at,
  blocked.completed_at AS blocking_run_completed_at,
  (
    SELECT item.payload_json
    FROM orchestration_v2_projection_turn_items item
    WHERE item.thread_id = t.thread_id
      AND item.run_id = blocked.run_id
      AND item.type = 'error' AND item.status = 'failed'
      AND item.node_id IS json_extract(blocked.payload_json, '$.rootNodeId')
    ORDER BY item.updated_at DESC, item.ordinal DESC, item.turn_item_id DESC
    LIMIT 1
  ) AS blocking_failure_payload_json,
  (
    SELECT request.payload_json
    FROM orchestration_v2_projection_runtime_requests request
    WHERE request.thread_id = t.thread_id
      AND request.status = 'pending'
    ORDER BY request.created_at DESC, request.runtime_request_id DESC
    LIMIT 1
  ) AS pending_request_payload_json,
  (
    SELECT secret.payload_json
    -- Keep runs outermost so completed history is never scanned for a pending secret.
    FROM orchestration_v2_projection_runs r
    CROSS JOIN orchestration_v2_projection_turn_items secret
      INDEXED BY orchestration_v2_projection_turn_items_thread_run_idx
    WHERE r.thread_id = t.thread_id
      AND r.status IN ('preparing', 'starting', 'running', 'waiting')
      AND secret.thread_id = t.thread_id AND secret.run_id = r.run_id
      AND secret.type = 'secret_request' AND secret.status = 'waiting'
    ORDER BY secret.updated_at DESC, secret.turn_item_id DESC
    LIMIT 1
  ) AS pending_secret_request_payload_json,
  (
    SELECT message.updated_at
    FROM orchestration_v2_projection_messages message
    WHERE message.thread_id = t.thread_id
      AND message.role = 'user'
    ORDER BY message.updated_at DESC, message.message_id DESC
    LIMIT 1
  ) AS latest_user_message_at,
  (
    SELECT message.updated_at
    FROM orchestration_v2_projection_messages message
    WHERE message.thread_id = t.thread_id
      AND message.role = 'user'
      AND json_extract(message.payload_json, '$.createdBy') = 'user'
    ORDER BY message.updated_at DESC, message.message_id DESC
    LIMIT 1
  ) AS latest_user_authored_message_at,
  EXISTS (
    SELECT 1
    FROM orchestration_v2_projection_plans plan
    WHERE plan.thread_id = t.thread_id
      AND plan.kind = 'proposed_plan'
      AND plan.status = 'active'
  ) AS has_actionable_proposed_plan,
  -- Count per run on the covering (thread_id, run_id) index, then look up each
  -- run once, instead of one run lookup per item.
  (
    SELECT COALESCE(SUM(per_run.item_count), 0)
    FROM (
      SELECT i.run_id, COUNT(*) AS item_count
      FROM orchestration_v2_projection_turn_items i
      WHERE i.thread_id = t.thread_id
      GROUP BY i.run_id
    ) per_run
    LEFT JOIN orchestration_v2_projection_runs r
      ON r.run_id = per_run.run_id
    WHERE per_run.run_id IS NULL OR r.status <> 'rolled_back'
  ) AS item_count,
  (
    SELECT COUNT(*)
    FROM orchestration_v2_projection_turn_items i
    WHERE i.thread_id = t.thread_id
      AND i.run_id IS NULL
  ) AS runless_item_count
FROM orchestration_v2_projection_threads t
-- The newest run not waiting in a held queue, matching latestUnheldRun.
LEFT JOIN orchestration_v2_projection_runs presented ON presented.run_id = (
  SELECT candidate.run_id
  FROM orchestration_v2_projection_runs candidate
  WHERE candidate.thread_id = t.thread_id
    AND NOT (
      candidate.status = 'queued'
      AND json_extract(candidate.payload_json, '$.queueHeld') IS 1
    )
  ORDER BY candidate.ordinal DESC, candidate.run_id DESC
  LIMIT 1
)
LEFT JOIN orchestration_v2_projection_runs blocked ON blocked.run_id = (
  SELECT candidate.run_id
  FROM orchestration_v2_projection_runs candidate
  WHERE candidate.thread_id = t.thread_id
    AND candidate.status <> 'queued'
    AND NOT (
      candidate.status = 'cancelled'
      AND json_extract(candidate.payload_json, '$.startedAt') IS NULL
    )
  -- latestExecutedRun: the run that ended last (runRanAfter).
  ORDER BY candidate.completed_at IS NULL DESC, candidate.completed_at DESC,
    candidate.ordinal DESC, candidate.run_id DESC
  LIMIT 1
) AND blocked.status = 'failed'
WHERE t.deleted_at IS NULL";

fn select_shell_thread_rows(
    conn: &Connection,
    thread_id: Option<&ThreadId>,
    options: &ShellSnapshotOptions,
) -> CauseResult<Vec<ShellThreadRow>> {
    let mut sql = crate::projection::sql::SqlBuilder::new(SHELL_THREAD_ROWS_SELECT);
    if let Some(thread_id) = thread_id {
        sql.push(" AND t.thread_id = ").bind(thread_id.to_string());
    }
    if let Some(project_id) = &options.project_id {
        sql.push(" AND t.project_id = ")
            .bind(project_id.to_string());
    }
    match options.location {
        Some(ShellSnapshotLocation::Active) => {
            sql.push(" AND json_extract(t.payload_json, '$.archivedAt') IS NULL");
        }
        Some(ShellSnapshotLocation::Archive) => {
            sql.push(" AND json_extract(t.payload_json, '$.archivedAt') IS NOT NULL");
        }
        None => {}
    }
    if options.unsettled_only {
        sql.push(
            " AND json_extract(t.payload_json, '$.settledAt') IS NULL AND json_extract(t.payload_json, '$.settledOverride') IS NOT 'settled'",
        );
    }
    sql.push(" ORDER BY t.updated_at ASC, t.thread_id ASC");
    let mut statement = conn.prepare_cached(&sql.sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(sql.params.iter()),
        ShellThreadRow::from_row,
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

struct ShellRunRow {
    thread_id: String,
    run_id: String,
    ordinal: u64,
}

struct ShellRunItemCountRow {
    thread_id: String,
    run_id: String,
    item_count: u64,
}

fn thread_id_list(thread_ids: &[ThreadId]) -> String {
    id_list(thread_ids.iter().map(ThreadId::as_str))
}

fn select_shell_run_rows(
    conn: &Connection,
    thread_ids: &[ThreadId],
) -> CauseResult<Vec<ShellRunRow>> {
    query_map(
        conn,
        "SELECT thread_id, run_id, ordinal
         FROM orchestration_v2_projection_runs
         WHERE thread_id IN (SELECT value FROM json_each(?1))",
        [thread_id_list(thread_ids)],
        |row| {
            Ok(ShellRunRow {
                thread_id: row.get(0)?,
                run_id: row.get(1)?,
                ordinal: row.get::<_, i64>(2)?.max(0) as u64,
            })
        },
    )
}

fn select_shell_run_item_counts(
    conn: &Connection,
    thread_ids: &[ThreadId],
) -> CauseResult<Vec<ShellRunItemCountRow>> {
    query_map(
        conn,
        "SELECT thread_id, run_id, COUNT(*) AS item_count
         FROM orchestration_v2_projection_turn_items
         WHERE run_id IS NOT NULL
           AND thread_id IN (SELECT value FROM json_each(?1))
         GROUP BY thread_id, run_id",
        [thread_id_list(thread_ids)],
        |row| {
            Ok(ShellRunItemCountRow {
                thread_id: row.get(0)?,
                run_id: row.get(1)?,
                item_count: row.get::<_, i64>(2)?.max(0) as u64,
            })
        },
    )
}

fn select_shell_provider_thread_rows(
    conn: &Connection,
    thread_ids: &[ThreadId],
) -> CauseResult<Vec<(String, String)>> {
    query_map(
        conn,
        "SELECT thread_id, payload_json
         FROM orchestration_v2_projection_provider_threads
         WHERE thread_id IN (SELECT value FROM json_each(?1))",
        [thread_id_list(thread_ids)],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
}

fn select_shell_pending_turn_item_rows(
    conn: &Connection,
    thread_ids: &[ThreadId],
) -> CauseResult<Vec<(String, String)>> {
    query_map(
        conn,
        "SELECT i.thread_id, i.payload_json
         FROM orchestration_v2_projection_turn_items i
         LEFT JOIN orchestration_v2_projection_runs r
           ON r.run_id = i.run_id
         WHERE i.type IN ('command_execution', 'dynamic_tool', 'subagent')
           AND i.status NOT IN ('completed', 'interrupted', 'failed', 'cancelled')
           -- A rolled-back run's items are abandoned, not pending. Without this the
           -- shell reports Waiting for work no one will finish, matching the
           -- item_count query's exclusion above.
           AND (i.run_id IS NULL OR r.status <> 'rolled_back')
           AND i.thread_id IN (SELECT value FROM json_each(?1))",
        [thread_id_list(thread_ids)],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
}

/// Threads that some shell row forks from at a run. Only these need run
/// ordinals and per-run item counts, to count the inherited prefix, so a thread
/// nobody forked from never pays for a scan of its run history.
fn shell_fork_source_ids(rows: &[ShellThreadRow]) -> Vec<ThreadId> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter_map(|row| row.forked_from_run_source_thread_id.as_deref())
        .filter(|id| seen.insert(id.to_owned()))
        .filter_map(|id| ThreadId::new(id).ok())
        .collect()
}

/// Provider-thread and pending-item rows grouped by thread.
struct PendingBackgroundData {
    provider_threads_by_thread_id: HashMap<ThreadId, Vec<OrchestrationV2ProviderThread>>,
    pending_turn_items_by_thread_id: HashMap<ThreadId, Vec<OrchestrationV2TurnItem>>,
}

fn pending_background_data_by_thread_id(
    provider_thread_rows: &[(String, String)],
    pending_turn_item_rows: &[(String, String)],
) -> CauseResult<PendingBackgroundData> {
    let mut provider_threads_by_thread_id: HashMap<ThreadId, Vec<OrchestrationV2ProviderThread>> =
        HashMap::new();
    for (thread_id, payload) in provider_thread_rows {
        let provider_thread: OrchestrationV2ProviderThread = decode(payload)?;
        let thread_id = if thread_id.is_empty() {
            provider_thread.app_thread_id.clone()
        } else {
            ThreadId::new(thread_id).ok()
        };
        let Some(thread_id) = thread_id else { continue };
        provider_threads_by_thread_id
            .entry(thread_id)
            .or_default()
            .push(provider_thread);
    }
    let mut pending_turn_items_by_thread_id: HashMap<ThreadId, Vec<OrchestrationV2TurnItem>> =
        HashMap::new();
    for (thread_id, payload) in pending_turn_item_rows {
        let item: OrchestrationV2TurnItem = decode(payload)?;
        if let Ok(thread_id) = ThreadId::new(thread_id) {
            pending_turn_items_by_thread_id
                .entry(thread_id)
                .or_default()
                .push(item);
        }
    }
    Ok(PendingBackgroundData {
        provider_threads_by_thread_id,
        pending_turn_items_by_thread_id,
    })
}

fn optional_time(value: Option<&str>) -> CauseResult<Option<UtcDateTime>> {
    value
        .map(|text| {
            text.parse::<UtcDateTime>()
                .map_err(|error| error.to_string().into())
        })
        .transpose()
}

fn pending_background_tasks_for(
    latest_run: Option<(&RunId, OrchestrationV2ShellThreadStatus)>,
    provider_threads: &[OrchestrationV2ProviderThread],
    pending_items: &[OrchestrationV2TurnItem],
    thread: &OrchestrationV2AppThread,
    has_active_run: bool,
) -> Vec<OrchestrationV2PendingBackgroundTask> {
    let run_status =
        |status: OrchestrationV2ShellThreadStatus| -> Option<OrchestrationV2RunStatus> {
            serde_json::from_value(serde_json::to_value(status).ok()?).ok()
        };
    let latest = latest_run.and_then(|(id, status)| {
        if status == OrchestrationV2ShellThreadStatus::Idle {
            return None;
        }
        Some(PendingBackgroundWorkRun {
            id: id.as_str(),
            ordinal: 0,
            status: run_status(status)?,
        })
    });
    let pull_requests = thread_pull_requests_of(thread);
    derive_pending_background_work(&DerivePendingBackgroundWorkInput {
        latest_run: latest,
        provider_threads: provider_threads
            .iter()
            .map(|provider_thread| PendingBackgroundWorkProviderThread {
                id: provider_thread.id.as_str(),
                pending_background_tasks: &provider_thread.pending_background_tasks,
            })
            .collect(),
        turn_items: pending_items
            .iter()
            .map(PendingBackgroundWorkItem::from_turn_item)
            .collect(),
        active_provider_thread_id: thread
            .active_provider_thread_id
            .as_ref()
            .map(|id| id.as_str()),
        has_active_run: Some(has_active_run),
        runs: None,
        pull_requests: Some(&pull_requests),
    })
}

fn shell_thread_state_from_row(
    row: &ShellThreadRow,
    run_ordinals: &HashMap<ThreadId, HashMap<RunId, u64>>,
    item_counts: &HashMap<ThreadId, HashMap<RunId, u64>>,
    background: &PendingBackgroundData,
) -> CauseResult<ShellThreadState> {
    let thread: OrchestrationV2AppThread = decode(&row.payload_json)?;
    let pending_runtime_request = if let Some(payload) = &row.pending_request_payload_json {
        Some(decode::<OrchestrationV2RuntimeRequest>(payload)?)
    } else if let Some(payload) = &row.pending_secret_request_payload_json {
        secret_request_as_pending_input(Some(&decode::<OrchestrationV2TurnItem>(payload)?))
    } else {
        None
    };
    let mut terminal_failure_item = row
        .terminal_failure_payload_json
        .as_deref()
        .map(decode::<OrchestrationV2TurnItem>)
        .transpose()?;
    let mut latest_run_id = row.latest_run_id.as_deref().map(RunId::new).transpose()?;
    let mut latest_run_status = shell_status_from_stored(row.latest_run_status.as_deref());
    let mut latest_run_requested_at = optional_time(row.latest_run_requested_at.as_deref())?;
    let mut latest_run_started_at = optional_time(row.latest_run_started_at.as_deref())?;
    let mut latest_run_completed_at = optional_time(row.latest_run_completed_at.as_deref())?;
    if let (Some(blocking_run_id), Some(failure_payload)) =
        (&row.blocking_run_id, &row.blocking_failure_payload_json)
    {
        // A failure that cannot decode is ignored, like upstream's `orElseSucceed(() => null)`.
        let blocking_failure = decode::<OrchestrationV2TurnItem>(failure_payload).ok();
        let blocking_failure_value = match &blocking_failure {
            Some(OrchestrationV2TurnItem::Error { failure, .. }) => Some(failure),
            _ => None,
        };
        let blocks_queue = thread_error_summary(blocking_failure_value, row.last_error.as_deref())
            .last_error_class
            == Some(OrchestrationV2ProviderFailureClass::UsageLimit);
        if blocks_queue && blocking_failure.is_some() {
            terminal_failure_item = blocking_failure;
            latest_run_id = Some(RunId::new(blocking_run_id)?);
            latest_run_status = OrchestrationV2ShellThreadStatus::Failed;
            latest_run_requested_at = optional_time(row.blocking_run_requested_at.as_deref())?;
            latest_run_started_at = optional_time(row.blocking_run_started_at.as_deref())?;
            latest_run_completed_at = optional_time(row.blocking_run_completed_at.as_deref())?;
        }
    }
    let empty_threads = Vec::new();
    let empty_items = Vec::new();
    let provider_threads = background
        .provider_threads_by_thread_id
        .get(&thread.id)
        .unwrap_or(&empty_threads);
    let pending_items = background
        .pending_turn_items_by_thread_id
        .get(&thread.id)
        .unwrap_or(&empty_items);
    let pending_background_tasks = pending_background_tasks_for(
        latest_run_id.as_ref().map(|id| (id, latest_run_status)),
        provider_threads,
        pending_items,
        &thread,
        row.active_run_id.is_some(),
    );
    let failure = match &terminal_failure_item {
        Some(OrchestrationV2TurnItem::Error { failure, .. }) => Some(failure),
        _ => None,
    };
    let error = thread_error_summary(failure, row.last_error.as_deref());
    let thread_key = ThreadId::new(&row.thread_id)?;
    let updated_at = thread.updated_at;
    Ok(ShellThreadState {
        provider_instance_history: provider_instance_history_for_shell(
            &thread.id,
            provider_threads,
        ),
        goal: active_provider_goal_for_shell(
            provider_threads,
            thread.active_provider_thread_id.as_ref(),
        ),
        thread,
        latest_run_id,
        latest_run_status,
        latest_run_requested_at,
        latest_run_started_at,
        latest_run_completed_at,
        active_run_id: row.active_run_id.as_deref().map(RunId::new).transpose()?,
        activity_run_started_at: optional_time(row.activity_run_started_at.as_deref())?,
        activity_run_status: row
            .activity_run_status
            .as_deref()
            .and_then(|status| serde_json::from_value(Value::String(status.to_owned())).ok()),
        error,
        pending_runtime_request,
        latest_user_message_at: optional_time(row.latest_user_message_at.as_deref())?,
        latest_user_authored_message_at: optional_time(
            row.latest_user_authored_message_at.as_deref(),
        )?,
        has_actionable_proposed_plan: row.has_actionable_proposed_plan,
        pending_background_tasks,
        item_count: row.item_count,
        runless_item_count: row.runless_item_count,
        updated_at,
        run_ordinal_by_id: run_ordinals.get(&thread_key).cloned().unwrap_or_default(),
        item_count_by_run_id: item_counts.get(&thread_key).cloned().unwrap_or_default(),
    })
}

fn item_count_through_run(state: &ShellThreadState, run_id: &RunId) -> u64 {
    let Some(run_ordinal) = state.run_ordinal_by_id.get(run_id) else {
        return 0;
    };
    let mut count =
        if state.thread.history_origin == Some(OrchestrationV2ThreadHistoryOrigin::V1Import) {
            state.runless_item_count
        } else {
            0
        };
    for (run_id, item_count) in &state.item_count_by_run_id {
        if state
            .run_ordinal_by_id
            .get(run_id)
            .is_some_and(|ordinal| ordinal <= run_ordinal)
        {
            count += item_count;
        }
    }
    count
}

fn visible_item_count_for_shell(
    thread_id: &ThreadId,
    states: &HashMap<ThreadId, ShellThreadState>,
    seen: &HashSet<ThreadId>,
) -> u64 {
    let Some(state) = states.get(thread_id) else {
        return 0;
    };
    let Some(OrchestrationV2ForkSource::Run {
        thread_id: source_id,
        run_id,
    }) = &state.thread.forked_from
    else {
        return state.item_count;
    };
    if seen.contains(&state.thread.id) {
        return state.item_count;
    }
    let Some(source_state) = states.get(source_id) else {
        return state.item_count;
    };
    let inherited_prefix_count = if matches!(
        source_state.thread.forked_from,
        Some(OrchestrationV2ForkSource::Run { .. })
    ) {
        let mut next_seen = seen.clone();
        next_seen.insert(state.thread.id.clone());
        visible_item_count_for_shell(&source_state.thread.id, states, &next_seen)
            .saturating_sub(source_state.item_count)
    } else {
        0
    };
    inherited_prefix_count + item_count_through_run(source_state, run_id) + 1 + state.item_count
}

type RunCountsByThread = HashMap<ThreadId, HashMap<RunId, u64>>;

fn run_maps_by_thread_id(
    run_rows: &[ShellRunRow],
    item_count_rows: &[ShellRunItemCountRow],
) -> (RunCountsByThread, RunCountsByThread) {
    let mut run_ordinals: HashMap<ThreadId, HashMap<RunId, u64>> = HashMap::new();
    for row in run_rows {
        if let (Ok(thread_id), Ok(run_id)) =
            (ThreadId::new(&row.thread_id), RunId::new(&row.run_id))
        {
            run_ordinals
                .entry(thread_id)
                .or_default()
                .insert(run_id, row.ordinal);
        }
    }
    let mut item_counts: HashMap<ThreadId, HashMap<RunId, u64>> = HashMap::new();
    for row in item_count_rows {
        if let (Ok(thread_id), Ok(run_id)) =
            (ThreadId::new(&row.thread_id), RunId::new(&row.run_id))
        {
            item_counts
                .entry(thread_id)
                .or_default()
                .insert(run_id, row.item_count);
        }
    }
    (run_ordinals, item_counts)
}

/// The raw rows of a shell snapshot, read in one transaction. Decoding them is
/// a separate step ([`ShellSnapshotRows::decode`]) so the shared connection is
/// not held while thousands of rows are converted (`readShellSnapshot`).
pub struct ShellSnapshotRows {
    target_thread_ids: HashSet<ThreadId>,
    thread_rows: Vec<ShellThreadRow>,
    run_rows: Vec<ShellRunRow>,
    item_count_rows: Vec<ShellRunItemCountRow>,
    snapshot_sequence: u64,
    provider_thread_rows: Vec<(String, String)>,
    pending_turn_item_rows: Vec<(String, String)>,
}

/// `readShellSnapshotRows`. Run inside the caller's read transaction.
pub fn read_shell_snapshot_rows(
    conn: &Connection,
    options: &ShellSnapshotOptions,
) -> Result<ShellSnapshotRows, ProjectionStoreError> {
    read_shell_snapshot_rows_inner(conn, options).map_err(shell_read_error)
}

fn shell_read_error(cause: crate::projection::error::Cause) -> ProjectionStoreError {
    ProjectionStoreError::read(&ThreadId::new("thread:shell").expect("literal id"), cause)
}

fn read_shell_snapshot_rows_inner(
    conn: &Connection,
    options: &ShellSnapshotOptions,
) -> CauseResult<ShellSnapshotRows> {
    let target_rows = select_shell_thread_rows(conn, None, options)?;
    let target_thread_ids = target_rows
        .iter()
        .filter_map(|row| ThreadId::new(&row.thread_id).ok())
        .collect::<HashSet<_>>();
    let mut rows_by_thread_id: HashMap<String, ShellThreadRow> = HashMap::new();
    let mut pending_source_ids = target_rows
        .iter()
        .filter_map(|row| row.forked_from_run_source_thread_id.clone())
        .collect::<Vec<_>>();
    let mut order = Vec::new();
    for row in target_rows {
        order.push(row.thread_id.clone());
        rows_by_thread_id.insert(row.thread_id.clone(), row);
    }
    while let Some(source_id) = pending_source_ids.pop() {
        if rows_by_thread_id.contains_key(&source_id) {
            continue;
        }
        let Ok(source_thread_id) = ThreadId::new(&source_id) else {
            continue;
        };
        let Some(source) = select_shell_thread_rows(
            conn,
            Some(&source_thread_id),
            &ShellSnapshotOptions::default(),
        )?
        .into_iter()
        .next() else {
            continue;
        };
        if let Some(next) = &source.forked_from_run_source_thread_id {
            pending_source_ids.push(next.clone());
        }
        order.push(source.thread_id.clone());
        rows_by_thread_id.insert(source_id, source);
    }
    let thread_rows = order
        .into_iter()
        .filter_map(|id| rows_by_thread_id.remove(&id))
        .collect::<Vec<_>>();
    let thread_ids = thread_rows
        .iter()
        .filter_map(|row| ThreadId::new(&row.thread_id).ok())
        .collect::<Vec<_>>();
    let fork_source_ids = shell_fork_source_ids(&thread_rows);
    let run_rows = if fork_source_ids.is_empty() {
        Vec::new()
    } else {
        select_shell_run_rows(conn, &fork_source_ids)?
    };
    let item_count_rows = if fork_source_ids.is_empty() {
        Vec::new()
    } else {
        select_shell_run_item_counts(conn, &fork_source_ids)?
    };
    let sequence: Option<i64> = conn.query_row(
        "SELECT MAX(sequence) AS snapshot_sequence
         FROM orchestration_events
         WHERE application_event_version = 2
           AND aggregate_kind = 'thread'",
        [],
        |row| row.get(0),
    )?;
    let provider_thread_rows = if thread_ids.is_empty() {
        Vec::new()
    } else {
        select_shell_provider_thread_rows(conn, &thread_ids)?
    };
    let pending_turn_item_rows = if thread_ids.is_empty() {
        Vec::new()
    } else {
        select_shell_pending_turn_item_rows(conn, &thread_ids)?
    };
    Ok(ShellSnapshotRows {
        target_thread_ids,
        thread_rows,
        run_rows,
        item_count_rows,
        snapshot_sequence: sequence.unwrap_or(0).max(0) as u64,
        provider_thread_rows,
        pending_turn_item_rows,
    })
}

impl ShellSnapshotRows {
    /// `decodeShellSnapshot`: no database access.
    pub fn decode(&self) -> Result<OrchestrationV2ThreadShellSnapshot, ProjectionStoreError> {
        self.decode_inner().map_err(shell_read_error)
    }

    fn decode_inner(&self) -> CauseResult<OrchestrationV2ThreadShellSnapshot> {
        let (run_ordinals, item_counts) =
            run_maps_by_thread_id(&self.run_rows, &self.item_count_rows);
        let background = pending_background_data_by_thread_id(
            &self.provider_thread_rows,
            &self.pending_turn_item_rows,
        )?;
        let mut states = Vec::with_capacity(self.thread_rows.len());
        for row in &self.thread_rows {
            states.push(shell_thread_state_from_row(
                row,
                &run_ordinals,
                &item_counts,
                &background,
            )?);
        }
        let ordered_ids = states
            .iter()
            .map(|state| state.thread.id.clone())
            .collect::<Vec<_>>();
        let states_by_thread_id = states
            .into_iter()
            .map(|state| (state.thread.id.clone(), state))
            .collect::<HashMap<_, _>>();
        let mut shells = Vec::new();
        for id in ordered_ids {
            if !self.target_thread_ids.contains(&id) {
                continue;
            }
            let state = &states_by_thread_id[&id];
            let visible = visible_item_count_for_shell(&id, &states_by_thread_id, &HashSet::new());
            shells.push(shell_from_state(state, visible));
        }
        let (archived, active): (Vec<_>, Vec<_>) = shells
            .into_iter()
            .partition(|shell| shell.archived_at.is_some());
        Ok(OrchestrationV2ThreadShellSnapshot {
            schema_version: PositiveInt::new(ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION)
                .expect("non-zero"),
            snapshot_sequence: self.snapshot_sequence,
            threads: active,
            archived_threads: archived,
        })
    }
}

/// `getThreadShell` body: only the target thread and its fork-source chain.
pub(crate) fn read_thread_shell(
    conn: &Connection,
    thread_id: &ThreadId,
) -> CauseResult<Option<OrchestrationV2ThreadShell>> {
    let mut rows_by_thread_id: HashMap<String, ShellThreadRow> = HashMap::new();
    let mut pending = vec![thread_id.to_string()];
    while let Some(next) = pending.pop() {
        if rows_by_thread_id.contains_key(&next) {
            continue;
        }
        let Ok(next_id) = ThreadId::new(&next) else {
            continue;
        };
        let Some(row) =
            select_shell_thread_rows(conn, Some(&next_id), &ShellSnapshotOptions::default())?
                .into_iter()
                .next()
        else {
            continue;
        };
        if let Some(source) = &row.forked_from_run_source_thread_id {
            pending.push(source.clone());
        }
        rows_by_thread_id.insert(next, row);
    }
    if !rows_by_thread_id.contains_key(thread_id.as_str()) {
        return Ok(None);
    }
    let rows = rows_by_thread_id.into_values().collect::<Vec<_>>();
    let thread_ids = rows
        .iter()
        .filter_map(|row| ThreadId::new(&row.thread_id).ok())
        .collect::<Vec<_>>();
    let fork_source_ids = shell_fork_source_ids(&rows);
    let run_rows = if fork_source_ids.is_empty() {
        Vec::new()
    } else {
        select_shell_run_rows(conn, &fork_source_ids)?
    };
    let item_count_rows = if fork_source_ids.is_empty() {
        Vec::new()
    } else {
        select_shell_run_item_counts(conn, &fork_source_ids)?
    };
    let provider_thread_rows = select_shell_provider_thread_rows(conn, &thread_ids)?;
    let pending_turn_item_rows = select_shell_pending_turn_item_rows(conn, &thread_ids)?;
    let (run_ordinals, item_counts) = run_maps_by_thread_id(&run_rows, &item_count_rows);
    let background =
        pending_background_data_by_thread_id(&provider_thread_rows, &pending_turn_item_rows)?;
    let mut states = HashMap::new();
    for row in &rows {
        let state = shell_thread_state_from_row(row, &run_ordinals, &item_counts, &background)?;
        states.insert(state.thread.id.clone(), state);
    }
    let Some(state) = states.get(thread_id) else {
        return Ok(None);
    };
    let visible = visible_item_count_for_shell(thread_id, &states, &HashSet::new());
    Ok(Some(shell_from_state(state, visible)))
}

// ---------------------------------------------------------------------------
// Candidates and small list reads
// ---------------------------------------------------------------------------

/// Thread activity needed by settlement, without transcript or fork history
/// (`ProjectionSettlementCandidate`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionSettlementCandidate {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub branch: Option<turin_contracts::TrimmedNonEmptyString>,
    pub worktree_path: Option<turin_contracts::TrimmedNonEmptyString>,
    /// The thread's own `pullRequests` (absent on threads that predate links).
    pub pull_requests: Option<Vec<turin_contracts::thread_pull_request::ThreadPullRequestLink>>,
    pub linked_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    pub branch_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub archived_at: Option<UtcDateTime>,
    pub settled_override: Option<OrchestrationV2SettleOverride>,
    pub pinned_at: Option<UtcDateTime>,
    pub auto_settle_disabled_at: Option<UtcDateTime>,
    pub snoozed_until: Option<UtcDateTime>,
    pub snoozed_at: Option<UtcDateTime>,
    pub last_snooze_wake_at: Option<Option<UtcDateTime>>,
    pub latest_run_id: Option<RunId>,
    pub latest_run_requested_at: Option<UtcDateTime>,
    pub latest_run_started_at: Option<UtcDateTime>,
    pub latest_run_completed_at: Option<UtcDateTime>,
    pub latest_user_message_at: Option<UtcDateTime>,
    pub latest_user_authored_message_at: Option<UtcDateTime>,
    pub status: OrchestrationV2ShellThreadStatus,
    pub activity_run_started_at: Option<UtcDateTime>,
    pub activity_run_status: Option<OrchestrationV2ActivityRunStatus>,
    pub pending_runtime_request: Option<OrchestrationV2PendingRuntimeRequestSummary>,
    pub pending_background_tasks: Vec<OrchestrationV2PendingBackgroundTask>,
}

/// Persisted state needed for limit recovery, without transcript or fork history
/// (`ProjectionLimitRecoveryCandidate`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionLimitRecoveryCandidate {
    pub id: ThreadId,
    pub status: OrchestrationV2ShellThreadStatus,
    pub last_error_class: Option<OrchestrationV2ProviderFailureClass>,
    pub latest_run_id: Option<RunId>,
    pub usage_limit_reset_at: Option<turin_contracts::IsoDateTime>,
    pub archived_at: Option<UtcDateTime>,
    pub settled_override: Option<OrchestrationV2SettleOverride>,
    pub pending_runtime_request: Option<OrchestrationV2PendingRuntimeRequestSummary>,
    pub latest_run_completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
    pub limit_recovery: Option<turin_contracts::orchestration_v2::OrchestrationV2LimitRecovery>,
    pub snoozed_until: Option<UtcDateTime>,
}

/// The thread fields pull request sync reads, for a thread with at least one
/// link (`ProjectionThreadPullRequests`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionThreadPullRequests {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub lineage: turin_contracts::orchestration_v2::OrchestrationV2AppThreadLineage,
    pub settled_override: Option<OrchestrationV2SettleOverride>,
    pub settled_at: Option<UtcDateTime>,
    pub pull_requests: Vec<turin_contracts::thread_pull_request::ThreadPullRequestLink>,
}

/// `getSettlementCandidates` body. Settlement needs activity timestamps, not
/// item counts, run history, provider sessions or inherited fork rows. Parked
/// and busy threads are rejected before the remaining candidates' background
/// work is loaded.
pub(crate) fn read_settlement_candidates(
    conn: &Connection,
    thread_id: Option<&ThreadId>,
) -> CauseResult<Vec<ProjectionSettlementCandidate>> {
    let mut sql = crate::projection::sql::SqlBuilder::new(
        "SELECT t.thread_id, t.payload_json,
           r.run_id AS latest_run_id,
           r.status AS latest_run_status,
           r.requested_at AS latest_run_requested_at,
           json_extract(r.payload_json, '$.startedAt') AS latest_run_started_at,
           r.completed_at AS latest_run_completed_at,
           (
             SELECT message.updated_at
             FROM orchestration_v2_projection_messages message
             WHERE message.thread_id = t.thread_id AND message.role = 'user'
             ORDER BY message.updated_at DESC, message.message_id DESC
             LIMIT 1
           ) AS latest_user_message_at,
           (
             SELECT message.updated_at
             FROM orchestration_v2_projection_messages message
             WHERE message.thread_id = t.thread_id AND message.role = 'user'
               AND json_extract(message.payload_json, '$.createdBy') = 'user'
             ORDER BY message.updated_at DESC, message.message_id DESC
             LIMIT 1
           ) AS latest_user_authored_message_at
         FROM orchestration_v2_projection_threads t
         LEFT JOIN orchestration_v2_projection_runs r ON r.run_id = (
           SELECT latest.run_id FROM orchestration_v2_projection_runs latest
           WHERE latest.thread_id = t.thread_id
           ORDER BY latest.ordinal DESC, latest.run_id DESC
           LIMIT 1
         )
         WHERE t.deleted_at IS NULL",
    );
    if let Some(thread_id) = thread_id {
        sql.push(" AND t.thread_id = ").bind(thread_id.to_string());
    }
    sql.push(
        "
           AND json_extract(t.payload_json, '$.archivedAt') IS NULL
           AND json_extract(t.payload_json, '$.settledOverride') IS NULL
           AND json_extract(t.payload_json, '$.pinnedAt') IS NULL
           AND json_extract(t.payload_json, '$.autoSettleDisabledAt') IS NULL
           AND NOT EXISTS (
             SELECT 1 FROM orchestration_v2_projection_runs active
             WHERE active.thread_id = t.thread_id
               AND active.status IN ('preparing', 'starting', 'running', 'waiting')
           )
           AND NOT EXISTS (
             SELECT 1 FROM orchestration_v2_projection_runtime_requests request
             WHERE request.thread_id = t.thread_id AND request.status = 'pending'
           )
         ORDER BY t.updated_at ASC, t.thread_id ASC",
    );
    struct SettlementRow {
        payload_json: String,
        latest_run_id: Option<String>,
        latest_run_status: Option<String>,
        latest_run_requested_at: Option<String>,
        latest_run_started_at: Option<String>,
        latest_run_completed_at: Option<String>,
        latest_user_message_at: Option<String>,
        latest_user_authored_message_at: Option<String>,
        thread_id: String,
    }
    let rows = {
        let mut statement = conn.prepare_cached(&sql.sql)?;
        let mapped = statement.query_map(rusqlite::params_from_iter(sql.params.iter()), |row| {
            Ok(SettlementRow {
                thread_id: row.get("thread_id")?,
                payload_json: row.get("payload_json")?,
                latest_run_id: row.get("latest_run_id")?,
                latest_run_status: row.get("latest_run_status")?,
                latest_run_requested_at: row.get("latest_run_requested_at")?,
                latest_run_started_at: row.get("latest_run_started_at")?,
                latest_run_completed_at: row.get("latest_run_completed_at")?,
                latest_user_message_at: row.get("latest_user_message_at")?,
                latest_user_authored_message_at: row.get("latest_user_authored_message_at")?,
            })
        })?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row?);
        }
        rows
    };
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let thread_ids = rows
        .iter()
        .filter_map(|row| ThreadId::new(&row.thread_id).ok())
        .collect::<Vec<_>>();
    let background = pending_background_data_by_thread_id(
        &select_shell_provider_thread_rows(conn, &thread_ids)?,
        &select_shell_pending_turn_item_rows(conn, &thread_ids)?,
    )?;
    let mut candidates = Vec::new();
    for row in rows {
        let thread: OrchestrationV2AppThread = decode(&row.payload_json)?;
        let status = shell_status_from_stored(row.latest_run_status.as_deref());
        let latest_run_id = row.latest_run_id.as_deref().map(RunId::new).transpose()?;
        let empty_threads = Vec::new();
        let empty_items = Vec::new();
        let pending_background_tasks = pending_background_tasks_for(
            latest_run_id.as_ref().map(|id| (id, status)),
            background
                .provider_threads_by_thread_id
                .get(&thread.id)
                .unwrap_or(&empty_threads),
            background
                .pending_turn_items_by_thread_id
                .get(&thread.id)
                .unwrap_or(&empty_items),
            &thread,
            false,
        );
        candidates.push(ProjectionSettlementCandidate {
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            pull_requests: thread.pull_requests.clone(),
            linked_pull_request: thread.linked_pull_request.clone(),
            branch_pull_request: thread.branch_pull_request.clone(),
            created_at: thread.created_at,
            updated_at: thread.updated_at,
            archived_at: thread.archived_at,
            settled_override: thread.settled_override,
            pinned_at: thread.pinned_at.flatten(),
            auto_settle_disabled_at: thread.auto_settle_disabled_at.flatten(),
            snoozed_until: thread.snoozed_until.flatten(),
            snoozed_at: thread.snoozed_at.flatten(),
            last_snooze_wake_at: thread.last_snooze_wake_at,
            latest_run_id,
            latest_run_requested_at: optional_time(row.latest_run_requested_at.as_deref())?,
            latest_run_started_at: optional_time(row.latest_run_started_at.as_deref())?,
            latest_run_completed_at: optional_time(row.latest_run_completed_at.as_deref())?,
            latest_user_message_at: optional_time(row.latest_user_message_at.as_deref())?,
            latest_user_authored_message_at: optional_time(
                row.latest_user_authored_message_at.as_deref(),
            )?,
            status,
            activity_run_started_at: None,
            activity_run_status: None,
            pending_runtime_request: None,
            pending_background_tasks,
        });
    }
    Ok(candidates)
}

/// `getThreadsWithPullRequests` body: active threads with at least one link, in
/// shell snapshot order. Skips run, message and item reads.
pub(crate) fn read_threads_with_pull_requests(
    conn: &Connection,
    thread_id: Option<&ThreadId>,
) -> CauseResult<Vec<ProjectionThreadPullRequests>> {
    let mut sql = crate::projection::sql::SqlBuilder::new(
        "SELECT payload_json
         FROM orchestration_v2_projection_threads
         WHERE deleted_at IS NULL",
    );
    if let Some(thread_id) = thread_id {
        sql.push(" AND thread_id = ").bind(thread_id.to_string());
    }
    sql.push(
        "
           AND json_extract(payload_json, '$.archivedAt') IS NULL
           AND json_array_length(payload_json, '$.pullRequests') > 0
         ORDER BY updated_at ASC, thread_id ASC",
    );
    let threads = sql.query_payloads::<OrchestrationV2AppThread>(conn)?;
    Ok(threads
        .into_iter()
        .map(|thread| ProjectionThreadPullRequests {
            id: thread.id,
            project_id: thread.project_id,
            lineage: thread.lineage,
            settled_override: thread.settled_override,
            settled_at: thread.settled_at,
            pull_requests: thread.pull_requests.unwrap_or_default(),
        })
        .collect())
}

/// An active thread's worktree path (`getThreadWorktreePaths`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadWorktreePath {
    pub thread_id: ThreadId,
    pub worktree_path: String,
}

/// Active (not deleted, not archived) threads that have a worktree path, the
/// same threads as the shell snapshot's `threads`. Reads only the thread rows.
pub(crate) fn read_thread_worktree_paths(
    conn: &Connection,
) -> CauseResult<Vec<ThreadWorktreePath>> {
    let rows = query_map(
        conn,
        "SELECT thread_id, json_extract(payload_json, '$.worktreePath') AS worktree_path
         FROM orchestration_v2_projection_threads
         WHERE deleted_at IS NULL
           AND json_extract(payload_json, '$.archivedAt') IS NULL
           AND json_extract(payload_json, '$.worktreePath') IS NOT NULL",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|(thread_id, worktree_path)| {
            Some(ThreadWorktreePath {
                thread_id: ThreadId::new(thread_id).ok()?,
                worktree_path,
            })
        })
        .collect())
}

/// `getLimitRecoveryCandidates` body. Indexed latest-run and root-error lookups
/// avoid reading run histories, counting transcript items, or walking fork
/// ancestors on scheduler ticks.
pub(crate) fn read_limit_recovery_candidates(
    conn: &Connection,
    now: &UtcDateTime,
    auto_resume: bool,
    snooze: bool,
) -> CauseResult<Vec<ProjectionLimitRecoveryCandidate>> {
    let now = now.to_string();
    let rows = query_map(
        conn,
        "SELECT t.payload_json, r.run_id, r.completed_at,
           item.payload_json AS failure_payload_json,
           (
             SELECT json_extract(session.payload_json, '$.lastError')
             -- Keep bindings outermost: one provider instance can hold every session.
             FROM orchestration_v2_projection_provider_session_bindings binding
             CROSS JOIN orchestration_v2_projection_provider_sessions session
               ON session.provider_session_id = binding.provider_session_id
             WHERE binding.thread_id = t.thread_id
               AND session.provider_instance_id = t.provider_instance_id
             ORDER BY session.updated_at DESC, session.provider_session_id DESC
             LIMIT 1
           ) AS last_error
         FROM orchestration_v2_projection_threads t
         INNER JOIN orchestration_v2_projection_runs r ON r.run_id = (
           SELECT latest.run_id FROM orchestration_v2_projection_runs latest
           WHERE latest.thread_id = t.thread_id
             AND latest.status <> 'queued'
             AND NOT (
               latest.status = 'cancelled'
               AND json_extract(latest.payload_json, '$.startedAt') IS NULL
             )
           -- latestExecutedRun: the run that ended last (runRanAfter).
           ORDER BY latest.completed_at IS NULL DESC, latest.completed_at DESC,
             latest.ordinal DESC, latest.run_id DESC
           LIMIT 1
         ) AND r.status = 'failed'
         INNER JOIN orchestration_v2_projection_turn_items item ON item.turn_item_id = (
           SELECT error.turn_item_id FROM orchestration_v2_projection_turn_items error
           WHERE error.thread_id = t.thread_id AND error.run_id = r.run_id
             AND error.type = 'error' AND error.status = 'failed'
             AND error.node_id IS json_extract(r.payload_json, '$.rootNodeId')
           ORDER BY error.updated_at DESC, error.ordinal DESC, error.turn_item_id DESC
           LIMIT 1
         )
         WHERE t.deleted_at IS NULL
           AND json_extract(t.payload_json, '$.archivedAt') IS NULL
           AND json_extract(t.payload_json, '$.settledOverride') IS NOT 'settled'
           AND json_extract(item.payload_json, '$.failure.class') = 'usage_limit'
           AND json_extract(item.payload_json, '$.failure.resetAt') IS NOT NULL
           AND julianday(json_extract(item.payload_json, '$.failure.resetAt')) > julianday(COALESCE(r.completed_at, json_extract(t.payload_json, '$.updatedAt')))
           AND (
             (
               json_extract(t.payload_json, '$.limitRecovery.runId') IS r.run_id
               AND json_extract(t.payload_json, '$.limitRecovery.resetAt') IS json_extract(item.payload_json, '$.failure.resetAt')
               AND json_extract(t.payload_json, '$.limitRecovery.autoResume') = 1
               AND julianday(json_extract(item.payload_json, '$.failure.resetAt')) <= julianday(?1)
               AND (
                 json_extract(t.payload_json, '$.snoozedUntil') IS NULL
                 OR julianday(json_extract(t.payload_json, '$.snoozedUntil')) <= julianday(?1)
               )
             )
             OR (
               (
                 json_extract(t.payload_json, '$.limitRecovery.runId') IS NOT r.run_id
                 OR json_extract(t.payload_json, '$.limitRecovery.resetAt') IS NOT json_extract(item.payload_json, '$.failure.resetAt')
               )
               AND (
                 ?2
                 OR (?3 AND julianday(json_extract(item.payload_json, '$.failure.resetAt')) > julianday(?1))
               )
             )
           )
           AND NOT EXISTS (
             SELECT 1 FROM orchestration_v2_projection_runtime_requests request
             WHERE request.thread_id = t.thread_id AND request.status = 'pending'
           )
         ORDER BY t.thread_id",
        params![now, i64::from(auto_resume), i64::from(snooze)],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        },
    )?;
    let mut candidates = Vec::new();
    for (thread_payload, run_id, completed_at, failure_payload, last_error) in rows {
        let thread: OrchestrationV2AppThread = decode(&thread_payload)?;
        let item: OrchestrationV2TurnItem = decode(&failure_payload)?;
        let failure = match &item {
            OrchestrationV2TurnItem::Error { failure, .. } => Some(failure),
            _ => None,
        };
        let summary = thread_error_summary(failure, last_error.as_deref());
        if summary.last_error_class != Some(OrchestrationV2ProviderFailureClass::UsageLimit) {
            continue;
        }
        candidates.push(ProjectionLimitRecoveryCandidate {
            id: thread.id.clone(),
            status: OrchestrationV2ShellThreadStatus::Failed,
            last_error_class: summary.last_error_class,
            usage_limit_reset_at: summary.usage_limit_reset_at,
            latest_run_id: Some(RunId::new(run_id)?),
            latest_run_completed_at: optional_time(completed_at.as_deref())?,
            updated_at: thread.updated_at,
            archived_at: thread.archived_at,
            settled_override: thread.settled_override,
            pending_runtime_request: None,
            limit_recovery: thread.limit_recovery.clone().flatten(),
            snoozed_until: thread.snoozed_until.flatten(),
        });
    }
    Ok(candidates)
}
