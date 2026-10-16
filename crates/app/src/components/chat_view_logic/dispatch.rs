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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::chat_view_logic::draft::ComposerSubmissionIntent;
    use crate::components::chat_view_logic::test_fixtures::dispatch::{
        ack_input, completed_turn, ready_session, snapshot_from_thread,
    };
    use crate::state::contracts::OrchestrationV2ProjectedTurnItem;

    #[test]
    fn does_not_acknowledge_unchanged_server_state() {
        let turn = completed_turn();
        let runtime = ready_session();
        let local_dispatch = snapshot_from_thread(
            Some(turn.clone()),
            Some(runtime.clone()),
            false,
            None,
            ComposerSubmissionIntent::Foreground,
        );
        assert!(!has_server_acknowledged_local_dispatch(ack_input(
            &local_dispatch,
            SessionPhase::Ready,
            Some(&turn),
            None,
            Some(&runtime),
            (false, false),
            None,
        )));
    }

    #[test]
    fn acknowledges_a_settled_newer_background_turn() {
        let turn = completed_turn();
        let runtime = ready_session();
        let local_dispatch = snapshot_from_thread(
            Some(turn.clone()),
            Some(runtime.clone()),
            false,
            None,
            ComposerSubmissionIntent::Background,
        );
        let newer_turn = ThreadRunSummary {
            run_id: RunId::new("turn-2").unwrap(),
            status: RunStatus::Completed,
            requested_at: Some("2026-03-29T00:01:00.000Z".to_owned()),
            started_at: Some("2026-03-29T00:01:01.000Z".to_owned()),
            completed_at: Some("2026-03-29T00:01:30.000Z".to_owned()),
        };
        let newer_runtime = ThreadRuntimeSummary {
            updated_at: newer_turn.completed_at.clone().unwrap(),
            ..runtime.clone()
        };
        assert!(has_server_acknowledged_local_dispatch(ack_input(
            &local_dispatch,
            SessionPhase::Ready,
            Some(&newer_turn),
            None,
            Some(&newer_runtime),
            (false, false),
            None,
        )));
    }

    #[test]
    fn holds_a_first_send_while_the_thread_shell_still_reports_a_preparing_run() {
        let local_dispatch = snapshot_from_thread(
            None,
            None,
            false,
            None,
            ComposerSubmissionIntent::Foreground,
        );
        let preparing_run = ThreadRunSummary {
            status: RunStatus::Preparing,
            started_at: None,
            completed_at: None,
            ..completed_turn()
        };
        let runtime = ThreadRuntimeSummary {
            status: RunStatus::Preparing,
            active_run_id: Some(preparing_run.run_id.clone()),
            ..ready_session()
        };
        assert!(!has_server_acknowledged_local_dispatch(ack_input(
            &local_dispatch,
            SessionPhase::Disconnected,
            Some(&preparing_run),
            None,
            Some(&runtime),
            (false, false),
            None,
        )));
    }

    #[test]
    fn waits_for_the_matching_running_turn_before_acknowledging() {
        let turn = completed_turn();
        let runtime = ready_session();
        let local_dispatch = snapshot_from_thread(
            Some(turn.clone()),
            Some(runtime.clone()),
            false,
            None,
            ComposerSubmissionIntent::Foreground,
        );
        let running_turn = ThreadRunSummary {
            run_id: RunId::new("turn-2").unwrap(),
            status: RunStatus::Running,
            requested_at: Some("2026-03-29T00:01:00.000Z".to_owned()),
            started_at: Some("2026-03-29T00:01:01.000Z".to_owned()),
            completed_at: None,
        };
        let mismatched_runtime = ThreadRuntimeSummary {
            status: RunStatus::Running,
            active_run_id: Some(RunId::new("turn-other").unwrap()),
            ..runtime.clone()
        };
        assert!(!has_server_acknowledged_local_dispatch(ack_input(
            &local_dispatch,
            SessionPhase::Running,
            Some(&running_turn),
            None,
            Some(&mismatched_runtime),
            (false, false),
            None,
        )));
        let matched_runtime = ThreadRuntimeSummary {
            status: RunStatus::Running,
            active_run_id: Some(running_turn.run_id.clone()),
            ..runtime
        };
        assert!(has_server_acknowledged_local_dispatch(ack_input(
            &local_dispatch,
            SessionPhase::Running,
            Some(&running_turn),
            None,
            Some(&matched_runtime),
            (false, false),
            None,
        )));
    }

    #[test]
    fn acknowledges_a_steering_message_projected_onto_the_current_running_run() {
        let running_run = ThreadRunSummary {
            status: RunStatus::Running,
            completed_at: None,
            ..completed_turn()
        };
        let running_runtime = ThreadRuntimeSummary {
            status: RunStatus::Running,
            active_run_id: Some(running_run.run_id.clone()),
            ..ready_session()
        };
        let before_steer = MessageId::new("message-before-steer").unwrap();
        let local_dispatch = snapshot_from_thread(
            Some(running_run.clone()),
            Some(running_runtime.clone()),
            false,
            Some(before_steer.clone()),
            ComposerSubmissionIntent::Foreground,
        );
        let steer = MessageId::new("message-steer").unwrap();
        assert!(has_server_acknowledged_local_dispatch(ack_input(
            &local_dispatch,
            SessionPhase::Running,
            Some(&running_run),
            Some(&steer),
            Some(&running_runtime),
            (false, false),
            None,
        )));
    }

    #[test]
    fn acknowledges_pending_user_interaction_and_errors_immediately() {
        let local_dispatch = snapshot_from_thread(
            None,
            None,
            false,
            None,
            ComposerSubmissionIntent::Foreground,
        );
        let base = ack_input(
            &local_dispatch,
            SessionPhase::Ready,
            None,
            None,
            None,
            (false, false),
            None,
        );
        assert!(has_server_acknowledged_local_dispatch(
            HasServerAcknowledgedLocalDispatchInput {
                has_pending_approval: true,
                ..base
            }
        ));
        assert!(has_server_acknowledged_local_dispatch(
            HasServerAcknowledgedLocalDispatchInput {
                has_pending_user_input: true,
                ..base
            }
        ));
        assert!(has_server_acknowledged_local_dispatch(
            HasServerAcknowledgedLocalDispatchInput {
                thread_error: Some("failed"),
                ..base
            }
        ));
    }

    #[test]
    fn derive_committed_tracks_only_committed_user_turn_items() {
        let rows: Vec<OrchestrationV2ProjectedTurnItem> =
            serde_json::from_value(serde_json::json!([
              {
                "position": 0,
                "visibility": "local",
                "sourceThreadId": "thread-1",
                "sourceItemId": "turn-item:message-turn-start",
                "item": {
                  "type": "user_message",
                  "id": "turn-item:message-turn-start",
                  "threadId": "thread-1",
                  "runId": "run:thread:thread-1:ordinal:1",
                  "ordinal": 1,
                  "status": "completed",
                  "updatedAt": "2026-06-26T17:50:15.180Z",
                  "createdBy": "user",
                  "creationSource": "web",
                  "messageId": "message-turn-start",
                  "inputIntent": "turn_start",
                  "text": "start",
                  "attachments": []
                }
              },
              {
                "position": 1,
                "visibility": "local",
                "sourceThreadId": "thread-1",
                "sourceItemId": "turn-item:message-assistant",
                "item": {
                  "type": "assistant_message",
                  "id": "turn-item:message-assistant",
                  "threadId": "thread-1",
                  "runId": "run:thread:thread-1:ordinal:1",
                  "ordinal": 2,
                  "status": "completed",
                  "updatedAt": "2026-06-26T17:50:15.180Z",
                  "messageId": "message-assistant",
                  "text": "working",
                  "streaming": false
                }
              },
              {
                "position": 2,
                "visibility": "local",
                "sourceThreadId": "thread-1",
                "sourceItemId": "turn-item:message-steer",
                "item": {
                  "type": "user_message",
                  "id": "turn-item:message-steer",
                  "threadId": "thread-1",
                  "runId": "run:thread:thread-1:ordinal:1",
                  "ordinal": 3,
                  "status": "completed",
                  "updatedAt": "2026-06-26T17:50:15.180Z",
                  "createdBy": "user",
                  "creationSource": "web",
                  "messageId": "message-steer",
                  "inputIntent": "steer",
                  "text": "continue",
                  "attachments": []
                }
              }
            ]))
            .expect("fixture rows");
        let ids = derive_committed_server_user_message_ids(&rows);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&MessageId::new("message-turn-start").unwrap()));
        assert!(ids.contains(&MessageId::new("message-steer").unwrap()));
        assert!(!ids.contains(&MessageId::new("message-assistant").unwrap()));
    }
}
