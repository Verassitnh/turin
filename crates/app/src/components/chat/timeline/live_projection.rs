//! The live half of the seam `source.rs` describes: builds a
//! [`ThreadTimelineSnapshot`] from the client store's [`ThreadProjection`].
//!
//! Turn items are read through their JSON form (the wire shape the contracts
//! already guarantee), so a variant this build has not mapped yet drops out of
//! the timeline instead of failing the thread. Every contract item type is
//! mapped; only a type newer than this build drops out.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::session_logic::{
    ExecutionNode, PlanArtifact, PlanArtifactKind, TimelineEntriesInput,
    derive_timeline_entries_from_visible_turn_items,
};
use super::turn_item::{
    CheckpointFileSummary, FileChangeDetail, NotificationOutcome, NotificationSource, PlanStep,
    PlanStepStatus, ProjectedTurnItem, ProjectedVisibility, ProviderApprovalDecision,
    ProviderApprovalOption, ProviderFailure, ProviderFailureClass, ProviderRequestKind,
    ProviderRetry, ToolActivityIcon, ToolActivitySource, ToolActivitySurface, TurnItem,
    TurnItemPayload, TurnItemStatus, UserInputQuestion, UserInputQuestionOption,
};
use super::types::{
    Actor, AttemptStatus, ChatAttachment, CreationSource, InputIntent, PendingApproval,
    PendingUserInput, PlanStatus, ResponseCapability, RunStatus, ThreadTimelineSnapshot,
    TimelineAttempt, TimelineLatestRun,
};
use crate::hooks::use_turn_diff_summaries::use_turn_diff_summaries;
use crate::state::helpers::handoff::{HandoffItem, HandoffTimelineRun, resolve_handoff_endpoints};
use crate::state::thread_projection::ThreadProjection;
use crate::state::thread_requests::{
    ApprovalResponseCapability, PendingThreadRequests, UserInputResponseCapability,
};
use turin_contracts::orchestration_v2::{
    OrchestrationV2NotificationSource, OrchestrationV2ProjectedTurnItemVisibility,
    OrchestrationV2Run, OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
    OrchestrationV2TurnItem,
};
use turin_contracts::provider_policy as policy;
use turin_orchestration::pipeline::attachment_store::resolve_attachment_path;

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
            attachments: attachments(value),
        },
        "assistant_message" => TurnItemPayload::AssistantMessage {
            message_id: text(value, "messageId")?,
            text: text(value, "text")?,
            streaming: flag(value, "streaming"),
            attachments: attachments(value),
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
            changes: value
                .get("changes")
                .and_then(Value::as_array)
                .map(|changes| changes.iter().filter_map(file_change_detail).collect()),
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
        "notification" => TurnItemPayload::Notification {
            source: notification_source(value.get("source")),
            outcome: match text(value, "outcome").as_deref() {
                Some("completed") => NotificationOutcome::Completed,
                Some("failed") => NotificationOutcome::Failed,
                Some("cancelled") => NotificationOutcome::Cancelled,
                Some("updated") => NotificationOutcome::Updated,
                _ => NotificationOutcome::Unknown,
            },
            summary: text(value, "summary")?,
            detail: text(value, "detail"),
        },
        "proposed_plan" => TurnItemPayload::ProposedPlan {
            plan_id: text(value, "planId")?,
            markdown: text(value, "markdown")?,
            streaming: flag(value, "streaming"),
        },
        "todo_list" => TurnItemPayload::TodoList {
            plan_id: text(value, "planId")?,
            steps: value
                .get("steps")
                .and_then(Value::as_array)
                .map(|steps| steps.iter().filter_map(plan_step).collect())
                .unwrap_or_default(),
            explanation: text(value, "explanation"),
        },
        "file_search" => TurnItemPayload::FileSearch {
            pattern: text(value, "pattern"),
            results: value
                .get("results")
                .and_then(Value::as_array)
                .map(|results| {
                    results
                        .iter()
                        .filter_map(|result| text(result, "fileName"))
                        .collect()
                })
                .unwrap_or_default(),
        },
        "web_search" => TurnItemPayload::WebSearch {
            patterns: value
                .get("patterns")
                .and_then(Value::as_array)
                .map(|patterns| {
                    patterns
                        .iter()
                        .filter_map(|pattern| pattern.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            results: value
                .get("results")
                .and_then(Value::as_array)
                .map(|results| {
                    results
                        .iter()
                        .map(|result| (text(result, "title"), text(result, "url")))
                        .collect()
                })
                .unwrap_or_default(),
        },
        "checkpoint" => TurnItemPayload::Checkpoint {
            checkpoint_id: text(value, "checkpointId")?,
            scope_id: text(value, "scopeId")?,
            files: value
                .get("files")
                .and_then(Value::as_array)
                .map(|files| files.iter().filter_map(checkpoint_file).collect())
                .unwrap_or_default(),
        },
        "compaction" => TurnItemPayload::Compaction {
            summary: text(value, "summary"),
            before_token_count: number(value, "beforeTokenCount"),
            after_token_count: number(value, "afterTokenCount"),
        },
        "handoff" => TurnItemPayload::Handoff {
            to_model: text(value, "toModel"),
            summary: text(value, "summary"),
            // Resolved against the thread's runs by `turn_item`.
            endpoints: None,
        },
        "fork" => TurnItemPayload::Fork {
            target_thread_id: text(value, "targetThreadId")?,
            source_thread_id: value
                .get("source")
                .filter(|source| text(source, "type").as_deref() == Some("run"))
                .and_then(|source| text(source, "threadId")),
        },
        "thread_created" => TurnItemPayload::ThreadCreated {
            target_thread_id: text(value, "targetThreadId")?,
            target_model: text(value, "targetModel")?,
        },
        "secret_request" => TurnItemPayload::SecretRequest {
            label: text(value, "label")?,
            reason: text(value, "reason").unwrap_or_default(),
        },
        "subagent" => TurnItemPayload::Subagent {
            subagent_id: text(value, "subagentId")?,
            origin: text(value, "origin").unwrap_or_else(|| "provider_native".to_owned()),
            child_thread_id: text(value, "childThreadId"),
            prompt: text(value, "prompt").unwrap_or_default(),
            progress: text(value, "progress"),
            result: text(value, "result"),
        },
        "dynamic_tool" => TurnItemPayload::DynamicTool {
            tool_name: text(value, "toolName"),
            viewed_image_path: text(value, "viewedImagePath"),
            input: value.get("input").cloned().unwrap_or(Value::Null),
            output: value
                .get("output")
                .filter(|output| !output.is_null())
                .cloned(),
            output_omitted: flag(value, "outputOmitted"),
        },
        _ => return None,
    })
}

fn notification_source(value: Option<&Value>) -> NotificationSource {
    let source = value
        .cloned()
        .and_then(|value| serde_json::from_value::<OrchestrationV2NotificationSource>(value).ok());
    match source {
        Some(OrchestrationV2NotificationSource::DelegatedTask {
            task_ids,
            child_thread_id,
        }) => NotificationSource::DelegatedTask {
            task_ids: task_ids.iter().map(ToString::to_string).collect(),
            child_thread_id: child_thread_id.map(|id| id.to_string()),
        },
        Some(OrchestrationV2NotificationSource::Subagent { child_thread_id }) => {
            NotificationSource::Subagent {
                child_thread_id: child_thread_id.map(|id| id.to_string()),
            }
        }
        Some(OrchestrationV2NotificationSource::Command) => NotificationSource::Command,
        Some(OrchestrationV2NotificationSource::Monitor) => NotificationSource::Monitor,
        Some(OrchestrationV2NotificationSource::System) => NotificationSource::System,
        Some(OrchestrationV2NotificationSource::BackgroundTask) | None => {
            NotificationSource::BackgroundTask
        }
    }
}

fn plan_step(value: &Value) -> Option<PlanStep> {
    Some(PlanStep {
        id: text(value, "id")?,
        text: text(value, "text")?,
        status: match text(value, "status")?.as_str() {
            "running" => PlanStepStatus::Running,
            "completed" => PlanStepStatus::Completed,
            _ => PlanStepStatus::Pending,
        },
        duration_anchor_at: text(value, "durationAnchorAt"),
        duration_ms: number(value, "durationMs"),
    })
}

fn checkpoint_file(value: &Value) -> Option<CheckpointFileSummary> {
    Some(CheckpointFileSummary {
        path: text(value, "path")?,
        kind: text(value, "kind")?,
        additions: number(value, "additions").unwrap_or(0),
        deletions: number(value, "deletions").unwrap_or(0),
    })
}

fn file_change_detail(value: &Value) -> Option<FileChangeDetail> {
    Some(FileChangeDetail {
        operation: text(value, "operation")?,
        path: text(value, "path")?,
        old_path: text(value, "oldPath"),
        file_type: text(value, "fileType"),
        mime_type: text(value, "mimeType"),
    })
}

/// A message's `attachments` (upstream `message.attachments`). The preview
/// file is filled in later from the stored attachment (`attachment_urls`).
fn attachments(value: &Value) -> Vec<ChatAttachment> {
    value
        .get("attachments")
        .and_then(Value::as_array)
        .map(|attachments| {
            attachments
                .iter()
                .filter_map(|attachment| {
                    Some(ChatAttachment {
                        id: text(attachment, "id")?,
                        kind: text(attachment, "type")?,
                        name: text(attachment, "name").unwrap_or_default(),
                        mime_type: text(attachment, "mimeType").unwrap_or_default(),
                        size_bytes: number(attachment, "sizeBytes").unwrap_or(0),
                        preview_path: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_icon(value: &Value) -> Option<ToolActivityIcon> {
    Some(match text(value, "_tag")?.as_str() {
        "website" => ToolActivityIcon::Website {
            page_url: text(value, "pageUrl")?,
            favicon_url: text(value, "faviconUrl"),
        },
        "native-app" => {
            let app = value.get("app")?;
            ToolActivityIcon::NativeApp {
                app: text(app, "appId").or_else(|| text(app, "displayName"))?,
            }
        }
        "themed-logo" => ToolActivityIcon::ThemedLogo {
            logo_url: text(value, "logoUrl")?,
        },
        _ => return None,
    })
}

fn tool_source(value: &Value) -> Option<ToolActivitySource> {
    Some(ToolActivitySource {
        key: text(value, "key")?,
        name: text(value, "name")?,
        kind: text(value, "kind")?,
        icon: value.get("icon").and_then(tool_icon),
    })
}

/// One contract turn item as the timeline reads it; `None` for a variant not
/// mapped yet. `runs` resolves a handoff's endpoints.
fn turn_item(item: &OrchestrationV2TurnItem, runs: &[HandoffTimelineRun]) -> Option<TurnItem> {
    let value = serde_json::to_value(item).ok()?;
    let mut converted = TurnItem::new(
        text(&value, "id")?,
        text(&value, "threadId")?,
        status(&text(&value, "status")?),
        text(&value, "updatedAt")?,
        payload(&value)?,
    );
    if let TurnItemPayload::Handoff { endpoints, .. } = &mut converted.payload
        && let Some(handoff) = HandoffItem::from_turn_item(item)
    {
        *endpoints = Some(resolve_handoff_endpoints(&handoff, runs));
    }
    converted.tool_surface = match text(&value, "toolSurface").as_deref() {
        Some("browser") => Some(ToolActivitySurface::Browser),
        Some("computer") => Some(ToolActivitySurface::Computer),
        _ => None,
    };
    converted.tool_icon = value.get("toolIcon").and_then(tool_icon);
    converted.tool_source = value.get("toolSource").and_then(tool_source);
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

/// Upstream's `ACTIVITY_RUN_STATUSES` (threadExecution.ts): a queued run is
/// waiting its turn, not active.
pub fn run_is_active(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

/// The thread's active run: the latest by ordinal among the active ones,
/// whatever queued or finished runs follow it (`findLast` in
/// `dispatchThreadMessage`).
pub fn active_run(
    runs: &[Arc<turin_contracts::orchestration_v2::OrchestrationV2Run>],
) -> Option<&Arc<turin_contracts::orchestration_v2::OrchestrationV2Run>> {
    runs.iter()
        .filter(|run| run_is_active(run.status))
        .max_by_key(|run| run.ordinal)
}

/// `deriveThreadActivityRun`: the run that owns live provider work, else the
/// newest run once the thread is idle. A newer queued run must not make an
/// older executing run look settled.
fn latest_run(runs: &[Arc<OrchestrationV2Run>]) -> Option<TimelineLatestRun> {
    let run = active_run(runs).or_else(|| runs.iter().max_by_key(|run| run.ordinal))?;
    Some(TimelineLatestRun {
        run_id: run.id.to_string(),
        status: run_status(run.status),
        started_at: run.started_at.map(|at| at.to_string()),
        completed_at: run.completed_at.map(|at| at.to_string()),
    })
}

/// The projected rows the timeline derives its entries from.
pub fn projected_items(projection: &ThreadProjection) -> Vec<ProjectedTurnItem> {
    let runs: Vec<HandoffTimelineRun> = projection
        .runs
        .iter()
        .map(|run| HandoffTimelineRun::from(run.as_ref()))
        .collect();
    projection
        .visible_turn_items
        .iter()
        .filter_map(|row| {
            Some(ProjectedTurnItem {
                position: row.position,
                visibility: visibility(row.visibility),
                source_thread_id: row.source_thread_id.to_string(),
                source_item_id: row.source_item_id.to_string(),
                item: turn_item(&row.item, &runs)?,
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

/// Upstream's `attachmentUrlById`: the stored file of every attachment on the
/// thread's messages, keyed by attachment id. Attachments whose file is gone
/// (or that have no stored file) are left out and render as their name.
pub fn attachment_urls(
    projection: &ThreadProjection,
    attachments_dir: &Path,
) -> HashMap<String, String> {
    let mut urls = HashMap::new();
    for row in projection.visible_turn_items.iter() {
        let attachments: &[turin_contracts::chat_attachment::ChatAttachment] = match &*row.item {
            OrchestrationV2TurnItem::UserMessage { attachments, .. } => attachments,
            OrchestrationV2TurnItem::AssistantMessage {
                attachments: Some(attachments),
                ..
            } => attachments,
            _ => continue,
        };
        for attachment in attachments {
            let id = match attachment {
                turin_contracts::chat_attachment::ChatAttachment::Image(image) => {
                    image.id.to_string()
                }
                turin_contracts::chat_attachment::ChatAttachment::File(file) => file.id.to_string(),
                turin_contracts::chat_attachment::ChatAttachment::Unknown(_) => continue,
            };
            if urls.contains_key(&id) {
                continue;
            }
            if let Some(path) =
                resolve_attachment_path(attachments_dir, attachment).filter(|path| path.exists())
            {
                urls.insert(id, path.to_string_lossy().into_owned());
            }
        }
    }
    urls
}

fn timeline_attempts(projection: &ThreadProjection) -> Vec<TimelineAttempt> {
    projection
        .attempts
        .iter()
        .map(|attempt| TimelineAttempt {
            id: attempt.id.to_string(),
            run_id: attempt.run_id.to_string(),
            attempt_ordinal: attempt.attempt_ordinal.into(),
            root_node_id: attempt.root_node_id.to_string(),
            status: match attempt.status {
                OrchestrationV2RunAttemptStatus::Pending => AttemptStatus::Pending,
                OrchestrationV2RunAttemptStatus::Running => AttemptStatus::Running,
                OrchestrationV2RunAttemptStatus::Completed => AttemptStatus::Completed,
                OrchestrationV2RunAttemptStatus::Interrupted => AttemptStatus::Interrupted,
                OrchestrationV2RunAttemptStatus::Failed => AttemptStatus::Failed,
                OrchestrationV2RunAttemptStatus::Cancelled => AttemptStatus::Cancelled,
                OrchestrationV2RunAttemptStatus::Superseded => AttemptStatus::Superseded,
            },
            completed_at: attempt.completed_at.map(|at| at.to_string()),
        })
        .collect()
}

fn plan_artifacts(projection: &ThreadProjection) -> Vec<PlanArtifact> {
    projection
        .plans
        .iter()
        .filter_map(|plan| {
            let value = serde_json::to_value(&**plan).ok()?;
            Some(PlanArtifact {
                id: text(&value, "id")?,
                run_id: text(&value, "runId"),
                status: match text(&value, "status")?.as_str() {
                    "draft" => PlanStatus::Draft,
                    "completed" => PlanStatus::Completed,
                    "superseded" => PlanStatus::Superseded,
                    _ => PlanStatus::Active,
                },
                kind: match text(&value, "kind")?.as_str() {
                    "proposed_plan" => PlanArtifactKind::ProposedPlan {
                        markdown: text(&value, "markdown")?,
                    },
                    "todo_list" => PlanArtifactKind::TodoList {
                        steps: value
                            .get("steps")
                            .and_then(Value::as_array)
                            .map(|steps| steps.iter().filter_map(plan_step).collect())
                            .unwrap_or_default(),
                        explanation: text(&value, "explanation"),
                    },
                    _ => return None,
                },
            })
        })
        .collect()
}

pub fn snapshot_from_projection(
    projection: &ThreadProjection,
    pending: Option<&PendingThreadRequests>,
    workspace_root: Option<String>,
    attachments_dir: Option<&Path>,
) -> ThreadTimelineSnapshot {
    let timeline_entries = derive_timeline_entries_from_visible_turn_items(&TimelineEntriesInput {
        visible_turn_items: projected_items(projection),
        attachment_url_by_id: attachments_dir
            .map(|dir| attachment_urls(projection, dir))
            .unwrap_or_default(),
        attempts: timeline_attempts(projection),
        nodes: projection
            .nodes
            .iter()
            .map(|node| ExecutionNode {
                id: node.id.to_string(),
                parent_node_id: node.parent_node_id.as_ref().map(ToString::to_string),
                root_node_id: node.root_node_id.to_string(),
            })
            .collect(),
        plans: plan_artifacts(projection),
        ..Default::default()
    });
    let latest_run = latest_run(&projection.runs);
    let running = latest_run.as_ref().filter(|run| {
        matches!(
            run.status,
            RunStatus::Preparing | RunStatus::Running | RunStatus::Starting | RunStatus::Waiting
        )
    });
    ThreadTimelineSnapshot {
        thread_id: projection.thread.id.to_string(),
        timeline_entries: Arc::new(timeline_entries),
        turn_diff_summaries: use_turn_diff_summaries(Some(projection)).turn_diff_summaries,
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
    use turin_contracts::{ThreadId, TurnItemId};

    use super::*;
    use crate::components::chat::timeline::turn_item::TurnItemType;
    use crate::components::chat::timeline::types::TimelineEntryKind;
    use crate::state::test_fixtures::{from_json, v2_projection};
    use crate::state::thread_projection::VisibleTurnItem;

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

    fn run(id: &str, ordinal: u64, status: &str) -> Arc<OrchestrationV2Run> {
        Arc::new(
            serde_json::from_value(json!({
                "id": id,
                "threadId": "thread-v2",
                "ordinal": ordinal,
                "providerInstanceId": "codex",
                "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
                "providerThreadId": null,
                "userMessageId": "message-1",
                "rootNodeId": null,
                "activeAttemptId": null,
                "status": status,
                "requestedAt": "2026-06-20T00:00:00.000Z",
                "startedAt": null,
                "completedAt": null,
                "checkpointId": null,
                "contextHandoffId": null,
            }))
            .unwrap(),
        )
    }

    #[test]
    fn a_newer_queued_run_does_not_make_the_running_run_look_settled() {
        let runs = vec![run("run-1", 1, "running"), run("run-2", 2, "queued")];
        let latest = latest_run(&runs).unwrap();
        assert_eq!(latest.run_id, "run-1");
        assert_eq!(latest.status, RunStatus::Running);
    }

    #[test]
    fn an_idle_thread_reports_its_newest_run() {
        let runs = vec![run("run-1", 1, "completed"), run("run-2", 2, "cancelled")];
        assert_eq!(latest_run(&runs).unwrap().run_id, "run-2");
    }

    #[test]
    fn queued_runs_are_not_active() {
        assert!(active_run(&[run("run-1", 1, "queued")]).is_none());
        assert_eq!(
            active_run(&[run("run-1", 1, "running"), run("run-2", 2, "queued")])
                .unwrap()
                .id
                .as_str(),
            "run-1"
        );
    }

    #[test]
    fn a_type_newer_than_this_build_drops_out() {
        assert!(payload(&json!({"type": "from_the_future"})).is_none());
    }

    const NOW: &str = crate::state::test_fixtures::V2_NOW;

    fn item(id: &str, ordinal: u64, fields: Value) -> Arc<VisibleTurnItem> {
        let mut value = json!({
            "id": id, "threadId": "thread-v2", "runId": null, "nodeId": null,
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": null,
            "startedAt": NOW, "completedAt": NOW, "updatedAt": NOW,
        });
        for (key, field) in fields.as_object().unwrap() {
            value[key] = field.clone();
        }
        Arc::new(VisibleTurnItem {
            position: ordinal,
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
            source_thread_id: ThreadId::new("thread-v2").unwrap(),
            source_item_id: TurnItemId::new(id).unwrap(),
            item: Arc::new(from_json(value)),
        })
    }

    fn projection_of(rows: Vec<Arc<VisibleTurnItem>>) -> ThreadProjection {
        let mut projection = ThreadProjection::from(v2_projection());
        projection.visible_turn_items = Arc::new(rows);
        projection
    }

    /// One row of every turn item type the contracts define.
    fn every_type() -> Vec<(&'static str, Arc<VisibleTurnItem>)> {
        vec![
            (
                "notification",
                item(
                    "i-notification",
                    1,
                    json!({"type": "notification", "source": {"kind": "monitor"},
                        "outcome": "completed", "summary": "Build done"}),
                ),
            ),
            (
                "reasoning",
                item(
                    "i-reasoning",
                    2,
                    json!({"type": "reasoning", "text": "hmm", "streaming": false}),
                ),
            ),
            (
                "proposed_plan",
                item(
                    "i-plan",
                    3,
                    json!({"type": "proposed_plan", "planId": "plan-1", "markdown": "# Plan", "streaming": false}),
                ),
            ),
            (
                "todo_list",
                item(
                    "i-todo",
                    4,
                    json!({"type": "todo_list", "planId": "todo-1", "steps": [
                        {"id": "s1", "text": "Do it", "status": "running"}]}),
                ),
            ),
            (
                "file_search",
                item(
                    "i-grep",
                    5,
                    json!({"type": "file_search", "pattern": "needle",
                        "results": [{"fileName": "src/lib.rs"}]}),
                ),
            ),
            (
                "web_search",
                item(
                    "i-web",
                    6,
                    json!({"type": "web_search", "patterns": ["rust gpui"],
                        "results": [{"title": "GPUI", "url": "https://gpui.rs"}]}),
                ),
            ),
            (
                "checkpoint",
                item(
                    "i-checkpoint",
                    7,
                    json!({"type": "checkpoint", "checkpointId": "cp-1", "scopeId": "scope-1",
                        "files": [{"path": "a.txt", "kind": "modified", "additions": 2, "deletions": 1}]}),
                ),
            ),
            (
                "compaction",
                item(
                    "i-compaction",
                    8,
                    json!({"type": "compaction", "driver": "claudeAgent", "summary": "Shrunk",
                        "beforeTokenCount": 900, "afterTokenCount": 100}),
                ),
            ),
            (
                "fork",
                item(
                    "i-fork",
                    9,
                    json!({"type": "fork", "source": {"type": "run", "threadId": "thread-v2", "runId": "run-1"},
                        "targetThreadId": "thread-fork"}),
                ),
            ),
            (
                "thread_created",
                item(
                    "i-created",
                    10,
                    json!({"type": "thread_created", "targetThreadId": "thread-new",
                        "targetRunId": null, "targetProviderInstanceId": "codex", "targetModel": "gpt-5.4"}),
                ),
            ),
            (
                "secret_request",
                item(
                    "i-secret",
                    11,
                    json!({"type": "secret_request", "label": "API key", "reason": "Needed",
                        "secretStatus": "pending"}),
                ),
            ),
            (
                "subagent",
                item(
                    "i-subagent",
                    12,
                    json!({"type": "subagent", "subagentId": "node-sub", "origin": "provider_native",
                        "driver": "claudeAgent", "providerInstanceId": "claudeAgent",
                        "childThreadId": null, "prompt": "Look around", "result": null}),
                ),
            ),
            (
                "dynamic_tool",
                item(
                    "i-tool",
                    13,
                    json!({"type": "dynamic_tool", "toolName": "Read",
                        "input": {"file_path": "README.md"}, "output": "contents"}),
                ),
            ),
        ]
    }

    #[test]
    fn every_contract_turn_item_type_reaches_the_timeline() {
        let rows = every_type();
        let projection = projection_of(rows.iter().map(|(_, row)| row.clone()).collect());
        let mapped = projected_items(&projection);
        assert_eq!(mapped.len(), rows.len(), "a type was dropped");
        for ((kind, _), mapped) in rows.iter().zip(&mapped) {
            assert_eq!(mapped.item.item_type().as_str(), *kind);
        }
        // The derived entries carry them too (the live view used to lose all of these).
        let snapshot = snapshot_from_projection(&projection, None, None, None);
        let Some(first_entry) = snapshot.timeline_entries.first() else {
            panic!("no timeline entries");
        };
        let _ = first_entry;
        let kinds: Vec<_> = snapshot
            .timeline_entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                TimelineEntryKind::Work { entry } => entry.item_type,
                TimelineEntryKind::Event { projected_item } => {
                    Some(projected_item.item.item_type())
                }
                _ => None,
            })
            .collect();
        for expected in [
            TurnItemType::WebSearch,
            TurnItemType::FileSearch,
            TurnItemType::DynamicTool,
            TurnItemType::Subagent,
            TurnItemType::Compaction,
            TurnItemType::Notification,
        ] {
            assert!(
                kinds.contains(&expected),
                "{expected:?} missing from {kinds:?}"
            );
        }
        // The proposed plan is its own entry kind; task progress and checkpoints
        // are shown elsewhere (composer, changed-files card).
        assert!(
            snapshot
                .timeline_entries
                .iter()
                .any(|entry| matches!(entry.kind, TimelineEntryKind::ProposedPlan { .. }))
        );
    }

    #[test]
    fn checkpoints_feed_the_changed_files_card() {
        let mut contract = v2_projection();
        contract.checkpoints = vec![from_json(json!({
            "id": "cp-1", "threadId": "thread-v2", "scopeId": "scope-1", "runId": "run-1",
            "nodeId": "node-1", "parentCheckpointId": null, "ordinalWithinScope": 1,
            "appRunOrdinal": 1, "ref": "refs/t3/checkpoints/x", "status": "ready",
            "files": [{"path": "new.txt", "kind": "added", "additions": 3, "deletions": 0}],
            "capturedAt": NOW,
        }))];
        let snapshot = snapshot_from_projection(&contract.into(), None, None, None);
        assert_eq!(snapshot.turn_diff_summaries.len(), 1);
        assert_eq!(snapshot.turn_diff_summaries[0].run_id, "run-1");
        assert_eq!(snapshot.turn_diff_summaries[0].files[0].path, "new.txt");
    }

    #[test]
    fn variant_fields_are_carried_over() {
        let rows = every_type();
        let projection = projection_of(rows.iter().map(|(_, row)| row.clone()).collect());
        let mapped: std::collections::HashMap<_, _> = projected_items(&projection)
            .into_iter()
            .map(|row| (row.item.id.clone(), row.item))
            .collect();
        assert!(matches!(
            &mapped["i-notification"].payload,
            TurnItemPayload::Notification { source: NotificationSource::Monitor, outcome: NotificationOutcome::Completed, summary, .. }
                if summary == "Build done"
        ));
        assert!(matches!(
            &mapped["i-web"].payload,
            TurnItemPayload::WebSearch { patterns, results }
                if patterns == &["rust gpui"]
                    && results == &[(Some("GPUI".to_owned()), Some("https://gpui.rs".to_owned()))]
        ));
        assert!(matches!(
            &mapped["i-checkpoint"].payload,
            TurnItemPayload::Checkpoint { files, .. }
                if files.len() == 1 && files[0].additions == 2 && files[0].deletions == 1
        ));
        assert!(matches!(
            &mapped["i-todo"].payload,
            TurnItemPayload::TodoList { steps, .. }
                if steps.len() == 1 && steps[0].status == PlanStepStatus::Running
        ));
        assert!(matches!(
            &mapped["i-fork"].payload,
            TurnItemPayload::Fork { source_thread_id: Some(source), target_thread_id }
                if source == "thread-v2" && target_thread_id == "thread-fork"
        ));
        assert!(matches!(
            &mapped["i-tool"].payload,
            TurnItemPayload::DynamicTool { tool_name: Some(name), output: Some(_), .. }
                if name == "Read"
        ));
    }

    fn message_with_attachment() -> Arc<VisibleTurnItem> {
        item(
            "i-user",
            1,
            json!({"type": "user_message", "createdBy": "user", "creationSource": "web",
                "messageId": "msg-1", "inputIntent": "turn_start", "text": "look at this",
                "attachments": [{"type": "image", "id": "thread-v2-0123abcd", "name": "shot.png",
                    "mimeType": "image/png", "sizeBytes": 4}]}),
        )
    }

    #[test]
    fn sent_attachments_show_on_the_message_with_their_stored_file() {
        let projection = projection_of(vec![message_with_attachment()]);
        let dir = tempfile::tempdir().unwrap();
        let stored = dir.path().join("thread-v2-0123abcd.png");
        std::fs::write(&stored, b"png!").unwrap();
        let snapshot = snapshot_from_projection(&projection, None, None, Some(dir.path()));
        let [entry] = snapshot.timeline_entries.as_slice() else {
            panic!("expected one entry");
        };
        let TimelineEntryKind::Message { message, .. } = &entry.kind else {
            panic!("expected a message");
        };
        assert_eq!(message.attachments.len(), 1);
        let attachment = &message.attachments[0];
        assert_eq!(attachment.id, "thread-v2-0123abcd");
        assert_eq!(attachment.name, "shot.png");
        assert!(attachment.is_image());
        assert_eq!(
            attachment.preview_path.as_deref(),
            Some(stored.to_str().unwrap())
        );
    }

    #[test]
    fn an_attachment_whose_file_is_gone_still_shows_by_name() {
        let projection = projection_of(vec![message_with_attachment()]);
        let dir = tempfile::tempdir().unwrap();
        let snapshot = snapshot_from_projection(&projection, None, None, Some(dir.path()));
        let TimelineEntryKind::Message { message, .. } = &snapshot.timeline_entries[0].kind else {
            panic!("expected a message");
        };
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].preview_path, None);
    }
}
