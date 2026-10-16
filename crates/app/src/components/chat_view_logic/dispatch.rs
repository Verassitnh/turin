//! Port of upstream/apps/web/src/components/ChatView.logic.ts (local dispatch acknowledgement).

use std::collections::HashSet;

use turin_contracts::{MessageId, RunId};

use crate::components::chat_view_logic::draft::ComposerSubmissionIntent;
use crate::state::contracts::{OrchestrationV2ProjectedTurnItem, OrchestrationV2TurnItem};
use crate::state::models::{RunStatus, ThreadRunSummary, ThreadRuntimeSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Disconnected,
    Connecting,
    Running,
    Ready,
}

pub fn derive_phase(runtime: Option<&ThreadRuntimeSummary>) -> SessionPhase {
    let Some(runtime) = runtime else {
        return SessionPhase::Disconnected;
    };
    match runtime.status {
        RunStatus::Preparing | RunStatus::Starting | RunStatus::Queued => SessionPhase::Connecting,
        RunStatus::Running | RunStatus::Waiting => SessionPhase::Running,
        _ => SessionPhase::Ready,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDispatchSnapshot {
    pub started_at: String,
    pub preparing_worktree: bool,
    pub submission_intent: ComposerSubmissionIntent,
    pub latest_user_message_id: Option<MessageId>,
    pub latest_run_id: Option<RunId>,
    pub latest_run_requested_at: Option<String>,
    pub latest_run_started_at: Option<String>,
    pub latest_run_completed_at: Option<String>,
    pub runtime_status: Option<RunStatus>,
    pub runtime_updated_at: Option<String>,
}

pub fn create_local_dispatch_snapshot(
    latest_run: Option<&ThreadRunSummary>,
    runtime: Option<&ThreadRuntimeSummary>,
    preparing_worktree: bool,
    latest_user_message_id: Option<MessageId>,
    submission_intent: ComposerSubmissionIntent,
) -> LocalDispatchSnapshot {
    LocalDispatchSnapshot {
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        preparing_worktree,
        submission_intent,
        latest_user_message_id,
        latest_run_id: latest_run.map(|run| run.run_id.clone()),
        latest_run_requested_at: latest_run.and_then(|run| run.requested_at.clone()),
        latest_run_started_at: latest_run.and_then(|run| run.started_at.clone()),
        latest_run_completed_at: latest_run.and_then(|run| run.completed_at.clone()),
        runtime_status: runtime.map(|runtime| runtime.status),
        runtime_updated_at: runtime.map(|runtime| runtime.updated_at.clone()),
    }
}

pub fn derive_committed_server_user_message_ids(
    visible_turn_items: &[OrchestrationV2ProjectedTurnItem],
) -> HashSet<MessageId> {
    visible_turn_items
        .iter()
        .filter_map(|row| match &row.item {
            OrchestrationV2TurnItem::UserMessage { message_id, .. } => Some(message_id.clone()),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HasServerAcknowledgedLocalDispatchInput<'a> {
    pub local_dispatch: Option<&'a LocalDispatchSnapshot>,
    pub phase: SessionPhase,
    pub latest_run: Option<&'a ThreadRunSummary>,
    pub latest_user_message_id: Option<&'a MessageId>,
    pub runtime: Option<&'a ThreadRuntimeSummary>,
    pub has_pending_approval: bool,
    pub has_pending_user_input: bool,
    pub thread_error: Option<&'a str>,
}

pub fn has_server_acknowledged_local_dispatch(
    input: HasServerAcknowledgedLocalDispatchInput<'_>,
) -> bool {
    let Some(local_dispatch) = input.local_dispatch else {
        return false;
    };
    if input.has_pending_approval || input.has_pending_user_input || input.thread_error.is_some() {
        return true;
    }
    if input.phase == SessionPhase::Connecting
        || derive_phase(input.runtime) == SessionPhase::Connecting
    {
        return false;
    }

    let latest_run = input.latest_run;
    let runtime = input.runtime;
    let latest_user_message_changed =
        local_dispatch.latest_user_message_id.as_ref() != input.latest_user_message_id;
    let latest_run_changed = local_dispatch.latest_run_id.as_ref()
        != latest_run.map(|run| &run.run_id)
        || local_dispatch.latest_run_requested_at.as_deref()
            != latest_run.and_then(|run| run.requested_at.as_deref())
        || local_dispatch.latest_run_started_at.as_deref()
            != latest_run.and_then(|run| run.started_at.as_deref())
        || local_dispatch.latest_run_completed_at.as_deref()
            != latest_run.and_then(|run| run.completed_at.as_deref());

    if input.phase == SessionPhase::Running {
        if latest_user_message_changed {
            return true;
        }
        if !latest_run_changed {
            return false;
        }
        if latest_run
            .and_then(|run| run.started_at.as_deref())
            .is_none()
        {
            return false;
        }
        if let (Some(runtime), Some(latest_run)) = (runtime, latest_run)
            && runtime.active_run_id.is_some()
            && runtime.active_run_id.as_ref() != Some(&latest_run.run_id)
        {
            return false;
        }
        return true;
    }

    latest_run_changed
        || local_dispatch.runtime_status != runtime.map(|runtime| runtime.status)
        || local_dispatch.runtime_updated_at.as_deref()
            != runtime.map(|runtime| runtime.updated_at.as_str())
}
