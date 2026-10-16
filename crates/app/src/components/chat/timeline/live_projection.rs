//! The live half of the seam `source.rs` describes: builds a
//! [`ThreadTimelineSnapshot`] from the client store's [`ThreadProjection`].
//!
//! Turn items are read through their JSON form (the wire shape the contracts
//! already guarantee), so a variant this build has not mapped yet drops out of
//! the timeline instead of failing the thread. Mapped: user and assistant
//! messages, reasoning, command execution, file changes, errors, system
//! notices, the interrupt rows, and approval and user-input requests. Not
//! mapped yet (see docs/handoff/T22.md): plans, todo lists, search, subagents,
//! checkpoints, compaction, handoff, fork, secret requests, dynamic tools and
//! notifications.

use std::sync::Arc;

use serde_json::Value;

use super::session_logic::{TimelineEntriesInput, derive_timeline_entries_from_visible_turn_items};
use super::turn_item::{
    ProjectedTurnItem, ProjectedVisibility, ProviderApprovalDecision, ProviderApprovalOption,
    ProviderFailure, ProviderFailureClass, ProviderRequestKind, ProviderRetry, TurnItem,
    TurnItemPayload, TurnItemStatus, UserInputQuestion, UserInputQuestionOption,
};
use super::types::{
    Actor, CreationSource, InputIntent, PendingApproval, PendingUserInput, ResponseCapability,
    RunStatus, ThreadTimelineSnapshot, TimelineLatestRun,
};
use crate::state::thread_projection::ThreadProjection;
use crate::state::thread_requests::{
    ApprovalResponseCapability, PendingThreadRequests, UserInputResponseCapability,
};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProjectedTurnItemVisibility, OrchestrationV2Run, OrchestrationV2RunStatus,
};
use turin_contracts::provider_policy as policy;

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

fn status(value: &str) -> TurnItemStatus {
    match value {
        "idle" => TurnItemStatus::Idle,
        "pending" => TurnItemStatus::Pending,
        "running" => TurnItemStatus::Running,
        "waiting" => TurnItemStatus::Waiting,
        "failed" => TurnItemStatus::Failed,
        "cancelled" => TurnItemStatus::Cancelled,
        "interrupted" => TurnItemStatus::Interrupted,
        _ => TurnItemStatus::Completed,
    }
}

fn actor(value: &str) -> Actor {
    match value {
        "agent" => Actor::Agent,
        "system" => Actor::System,
        _ => Actor::User,
    }
}

fn creation_source(value: &str) -> CreationSource {
    match value {
        "mobile" => CreationSource::Mobile,
        "mcp" => CreationSource::Mcp,
        "provider" => CreationSource::Provider,
        "server" => CreationSource::Server,
        _ => CreationSource::Web,
    }
}

fn input_intent(value: &str) -> InputIntent {
    match value {
        "queued_turn" => InputIntent::QueuedTurn,
        "steer" => InputIntent::Steer,
        "promoted_queued_to_steer" => InputIntent::PromotedQueuedToSteer,
        _ => InputIntent::TurnStart,
    }
}

fn failure_class(value: &str) -> ProviderFailureClass {
    match value {
        "usage_limit" => ProviderFailureClass::UsageLimit,
        "provider_error" => ProviderFailureClass::ProviderError,
        "transport_error" => ProviderFailureClass::TransportError,
        "permission_error" => ProviderFailureClass::PermissionError,
        "validation_error" => ProviderFailureClass::ValidationError,
        _ => ProviderFailureClass::Unknown,
    }
}

fn approval_option(value: &Value) -> Option<ProviderApprovalOption> {
    let decision = match value.get("decision")?.as_str()? {
        "accept" => ProviderApprovalDecision::Accept,
        "acceptForSession" => ProviderApprovalDecision::AcceptForSession,
        "acceptAlways" => ProviderApprovalDecision::AcceptAlways,
        "decline" => ProviderApprovalDecision::Decline,
        "cancel" => ProviderApprovalDecision::Cancel,
        _ => return None,
    };
    Some(ProviderApprovalOption {
        decision,
        label: text(value, "label")?,
        warning: text(value, "warning"),
    })
}

fn question(value: &Value) -> Option<UserInputQuestion> {
    Some(UserInputQuestion {
        id: text(value, "id")?,
        header: text(value, "header")?,
        question: text(value, "question")?,
        options: value
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        Some(UserInputQuestionOption {
                            label: text(option, "label")?,
                            description: text(option, "description").unwrap_or_default(),
                            value: text(option, "value"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        multi_select: flag(value, "multiSelect"),
        allow_custom_answer: value.get("allowCustomAnswer").and_then(Value::as_bool),
        initial_answer: text(value, "initialAnswer"),
        required: value.get("required").and_then(Value::as_bool),
    })
}

fn payload(value: &Value) -> Option<TurnItemPayload> {
    let kind = value.get("type")?.as_str()?;
    Some(match kind {
        "user_message" => TurnItemPayload::UserMessage {
            message_id: text(value, "messageId")?,
            text: text(value, "text")?,
            created_by: actor(&text(value, "createdBy").unwrap_or_default()),
            creation_source: creation_source(&text(value, "creationSource").unwrap_or_default()),
            scheduled_task_id: text(value, "scheduledTaskId"),
            sender_thread_id: text(value, "senderThreadId"),
            input_intent: input_intent(&text(value, "inputIntent").unwrap_or_default()),
            attachments: Vec::new(),
        },
        "assistant_message" => TurnItemPayload::AssistantMessage {
            message_id: text(value, "messageId")?,
            text: text(value, "text")?,
            streaming: flag(value, "streaming"),
            attachments: Vec::new(),
        },
        "reasoning" => TurnItemPayload::Reasoning {
            text: text(value, "text")?,
            streaming: flag(value, "streaming"),
        },
        "command_execution" => TurnItemPayload::CommandExecution {
            input: text(value, "input")?,
            output: text(value, "output"),
            output_omitted: flag(value, "outputOmitted"),
            output_indicates_failure: value.get("outputIndicatesFailure").and_then(Value::as_bool),
            exit_code: value.get("exitCode").and_then(Value::as_i64),
        },
        "file_change" => TurnItemPayload::FileChange {
            file_name: text(value, "fileName")?,
            additions: number(value, "additions"),
            deletions: number(value, "deletions"),
            diff: text(value, "diffStr"),
            changes: None,
        },
        "error" => {
            let failure = value.get("failure")?;
            TurnItemPayload::Error {
                failure: ProviderFailure {
                    class: failure_class(&text(failure, "class").unwrap_or_default()),
                    message: text(failure, "message").unwrap_or_default(),
                    code: text(failure, "code"),
                    retryable: failure.get("retryable").and_then(Value::as_bool),
                    reset_at: text(failure, "resetAt"),
                },
                retry: value
                    .get("retry")
                    .filter(|retry| retry.is_object())
                    .and_then(|retry| {
                        Some(ProviderRetry {
                            attempt: number(retry, "attempt")?,
                            max_attempts: number(retry, "maxAttempts"),
                            retry_delay_ms: number(retry, "retryDelayMs"),
                        })
                    }),
            }
        }
        "approval_request" => TurnItemPayload::ApprovalRequest {
            request_id: text(value, "requestId")?,
            request_kind: match text(value, "requestKind")?.as_str() {
                "command" => ProviderRequestKind::Command,
                "file_read" => ProviderRequestKind::FileRead,
                "file_change" => ProviderRequestKind::FileChange,
                "mcp_elicitation" => ProviderRequestKind::McpElicitation,
                _ => ProviderRequestKind::Permission,
            },
            prompt: text(value, "prompt"),
            app_name: text(value, "appName"),
            options: value
                .get("options")
                .and_then(Value::as_array)
                .map(|options| options.iter().filter_map(approval_option).collect())
                .unwrap_or_default(),
        },
        "user_input_request" => TurnItemPayload::UserInputRequest {
            request_id: text(value, "requestId")?,
            questions: value
                .get("questions")
                .and_then(Value::as_array)
                .map(|questions| questions.iter().filter_map(question).collect())
                .unwrap_or_default(),
            question_answer: None,
        },
        "system_notice" => TurnItemPayload::SystemNotice {
            message: text(value, "message")?,
        },
        "run_interrupt_request" => TurnItemPayload::RunInterruptRequest {
            message: text(value, "message")?,
        },
        "run_interrupt_result" => TurnItemPayload::RunInterruptResult {
            message: text(value, "message")?,
        },
        _ => return None,
    })
}

/// One contract turn item as the timeline reads it; `None` for a variant not
/// mapped yet.
fn turn_item(
    item: &turin_contracts::orchestration_v2::OrchestrationV2TurnItem,
) -> Option<TurnItem> {
    let value = serde_json::to_value(item).ok()?;
    let mut converted = TurnItem::new(
        text(&value, "id")?,
        text(&value, "threadId")?,
        status(&text(&value, "status")?),
        text(&value, "updatedAt")?,
        payload(&value)?,
    );
    converted.run_id = text(&value, "runId");
    converted.node_id = text(&value, "nodeId");
    converted.provider_turn_id = text(&value, "providerTurnId");
    converted.parent_item_id = text(&value, "parentItemId");
    converted.ordinal = number(&value, "ordinal").unwrap_or(0);
    converted.title = text(&value, "title");
    converted.started_at = text(&value, "startedAt");
    converted.completed_at = text(&value, "completedAt");
    Some(converted)
}

fn visibility(value: OrchestrationV2ProjectedTurnItemVisibility) -> ProjectedVisibility {
    match value {
        OrchestrationV2ProjectedTurnItemVisibility::Local => ProjectedVisibility::Local,
        OrchestrationV2ProjectedTurnItemVisibility::Inherited => ProjectedVisibility::Inherited,
        OrchestrationV2ProjectedTurnItemVisibility::Synthetic => ProjectedVisibility::Synthetic,
    }
}

fn run_status(value: OrchestrationV2RunStatus) -> RunStatus {
    match value {
        OrchestrationV2RunStatus::Preparing => RunStatus::Preparing,
        OrchestrationV2RunStatus::Queued => RunStatus::Queued,
        OrchestrationV2RunStatus::Starting => RunStatus::Starting,
        OrchestrationV2RunStatus::Running => RunStatus::Running,
        OrchestrationV2RunStatus::Waiting => RunStatus::Waiting,
        OrchestrationV2RunStatus::Completed => RunStatus::Completed,
        OrchestrationV2RunStatus::Interrupted => RunStatus::Interrupted,
        OrchestrationV2RunStatus::Failed => RunStatus::Failed,
        OrchestrationV2RunStatus::Cancelled => RunStatus::Cancelled,
        OrchestrationV2RunStatus::RolledBack => RunStatus::RolledBack,
    }
}

fn latest_run(runs: &[Arc<OrchestrationV2Run>]) -> Option<TimelineLatestRun> {
    let run = runs.iter().max_by_key(|run| run.ordinal)?;
    Some(TimelineLatestRun {
        run_id: run.id.to_string(),
        status: run_status(run.status),
        started_at: run.started_at.map(|at| at.to_string()),
        completed_at: run.completed_at.map(|at| at.to_string()),
    })
}

/// The projected rows the timeline derives its entries from.
pub fn projected_items(projection: &ThreadProjection) -> Vec<ProjectedTurnItem> {
    projection
        .visible_turn_items
        .iter()
        .filter_map(|row| {
            Some(ProjectedTurnItem {
                position: row.position,
                visibility: visibility(row.visibility),
                source_thread_id: row.source_thread_id.to_string(),
                source_item_id: row.source_item_id.to_string(),
                item: turn_item(&row.item)?,
            })
        })
        .collect()
}

fn request_kind(kind: policy::ProviderRequestKind) -> ProviderRequestKind {
    match kind {
        policy::ProviderRequestKind::Command => ProviderRequestKind::Command,
        policy::ProviderRequestKind::FileRead => ProviderRequestKind::FileRead,
        policy::ProviderRequestKind::FileChange => ProviderRequestKind::FileChange,
        policy::ProviderRequestKind::McpElicitation => ProviderRequestKind::McpElicitation,
        policy::ProviderRequestKind::Permission => ProviderRequestKind::Permission,
    }
}

fn decision(value: policy::ProviderApprovalDecision) -> ProviderApprovalDecision {
    match value {
        policy::ProviderApprovalDecision::Accept => ProviderApprovalDecision::Accept,
        policy::ProviderApprovalDecision::AcceptForSession => {
            ProviderApprovalDecision::AcceptForSession
        }
        policy::ProviderApprovalDecision::AcceptAlways => ProviderApprovalDecision::AcceptAlways,
        policy::ProviderApprovalDecision::Decline => ProviderApprovalDecision::Decline,
        policy::ProviderApprovalDecision::Cancel => ProviderApprovalDecision::Cancel,
    }
}

/// The wire name of a decision (`runtime-request.respond`'s `decision`).
pub fn decision_wire(value: ProviderApprovalDecision) -> &'static str {
    match value {
        ProviderApprovalDecision::Accept => "accept",
        ProviderApprovalDecision::AcceptForSession => "acceptForSession",
        ProviderApprovalDecision::AcceptAlways => "acceptAlways",
        ProviderApprovalDecision::Decline => "decline",
        ProviderApprovalDecision::Cancel => "cancel",
    }
}

pub fn pending_approvals(requests: &PendingThreadRequests) -> Vec<PendingApproval> {
    requests
        .approvals
        .iter()
        .map(|approval| PendingApproval {
            request_id: approval.request_id.to_string(),
            request_kind: request_kind(approval.request_kind),
            created_at: approval.created_at.clone(),
            detail: approval.detail.clone(),
            app_name: approval.app_name.clone(),
            options: approval.options.as_ref().map(|options| {
                options
                    .iter()
                    .map(|option| ProviderApprovalOption {
                        decision: decision(option.decision),
                        label: option.label.to_string(),
                        warning: option.warning.as_ref().map(ToString::to_string),
                    })
                    .collect()
            }),
            response_capability: match approval.response_capability {
                ApprovalResponseCapability::Live => ResponseCapability::Live,
                ApprovalResponseCapability::NotResumable => ResponseCapability::NotResumable,
            },
        })
        .collect()
}

pub fn pending_user_inputs(requests: &PendingThreadRequests) -> Vec<PendingUserInput> {
    requests
        .user_inputs
        .iter()
        .map(|input| PendingUserInput {
            request_id: input.request_id.to_string(),
            created_at: input.created_at.clone(),
            questions: input
                .questions
                .iter()
                .map(|question| UserInputQuestion {
                    id: question.id.to_string(),
                    header: question.header.to_string(),
                    question: question.question.to_string(),
                    options: question
                        .options
                        .iter()
                        .map(|option| UserInputQuestionOption {
                            label: option.label.to_string(),
                            description: option.description.to_string(),
                            value: option.value.clone(),
                        })
                        .collect(),
                    multi_select: question.multi_select,
                    allow_custom_answer: question.allow_custom_answer,
                    initial_answer: question.initial_answer.clone(),
                    required: question.required,
                })
                .collect(),
            // A "message" capability answers through the normal reply path.
            response_capability: match input.response_capability {
                UserInputResponseCapability::NotResumable => ResponseCapability::NotResumable,
                UserInputResponseCapability::Live | UserInputResponseCapability::Message => {
                    ResponseCapability::Live
                }
            },
            dismissible: input.dismissible,
        })
        .collect()
}

pub fn snapshot_from_projection(
    projection: &ThreadProjection,
    pending: Option<&PendingThreadRequests>,
    workspace_root: Option<String>,
) -> ThreadTimelineSnapshot {
    let timeline_entries = derive_timeline_entries_from_visible_turn_items(&TimelineEntriesInput {
        visible_turn_items: projected_items(projection),
        ..Default::default()
    });
    let latest_run = latest_run(&projection.runs);
    let running = latest_run.as_ref().filter(|run| {
        matches!(
            run.status,
            RunStatus::Running | RunStatus::Starting | RunStatus::Waiting
        )
    });
    ThreadTimelineSnapshot {
        thread_id: projection.thread.id.to_string(),
        timeline_entries: Arc::new(timeline_entries),
        running_run_id: running.map(|run| run.run_id.clone()),
        is_working: running.is_some(),
        active_turn_started_at: running.and_then(|run| run.started_at.clone()),
        latest_run,
        pending_approvals: pending.map(pending_approvals).unwrap_or_default(),
        pending_user_inputs: pending.map(pending_user_inputs).unwrap_or_default(),
        workspace_root,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_messages_and_failures() {
        let user = payload(&json!({
            "type": "user_message", "messageId": "m1", "text": "hi",
            "createdBy": "user", "creationSource": "web", "inputIntent": "turn_start",
        }))
        .expect("user message");
        assert!(matches!(
            user,
            TurnItemPayload::UserMessage { ref text, created_by: Actor::User, .. } if text == "hi"
        ));
        let assistant = payload(&json!({
            "type": "assistant_message", "messageId": "m2", "text": "yo", "streaming": true,
        }))
        .expect("assistant message");
        assert!(matches!(
            assistant,
            TurnItemPayload::AssistantMessage {
                streaming: true,
                ..
            }
        ));
        let error = payload(&json!({
            "type": "error",
            "failure": {"class": "usage_limit", "message": "limit", "retryable": false},
            "retry": null,
        }))
        .expect("error");
        assert!(matches!(
            error,
            TurnItemPayload::Error { ref failure, retry: None }
                if failure.class == ProviderFailureClass::UsageLimit
        ));
    }

    #[test]
    fn unmapped_variants_drop_out() {
        assert!(payload(&json!({"type": "web_search", "patterns": []})).is_none());
    }
}
