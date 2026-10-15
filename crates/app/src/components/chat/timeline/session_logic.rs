//! Port of upstream/apps/web/src/session-logic.ts (the pure helpers the
//! thread timeline and its request panels use).
//!
//! Adapted to the turn-item mirror in [`super::turn_item`]; the `WorkLogEntry`,
//! `TimelineEntry` and pending-request shapes live in [`super::types`].
//! Not ported: html-render / mcp-app entries (cut), the attachment image
//! resource selectors and preview projector (asset loading lands with media),
//! and `derivePhase` / `deriveCanInterruptRunningThread` (thread execution
//! state belongs to the composer task).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::js_text::js_trim;
// Upstream re-exports these; kept for callers of this module.
#[allow(unused_imports)]
pub use super::orchestration_timing::format_duration;
use super::tool_activity::{
    ClassifyToolActivityInput, ToolActivityAction, classify_tool_activity, collect_tool_file_paths,
    format_read_tool_label, format_search_tool_label,
};
use super::tool_presentation::extract_tool_activity_presentation;
use super::turn_item::{PlanStep, PlanStepStatus, ProjectedTurnItem, TurnItem, TurnItemPayload};
use super::turn_item::{ProviderFailureClass, TurnItemStatus, TurnItemType};
use super::turn_item_presentation::turn_item_is_workspace_preparation;
use super::types::{
    ChatMessage, CheckpointStatus, InputIntent, MessageRole, PendingApproval, PendingUserInput,
    PlanStatus, ProposedPlan, RunStatus, TimelineAttempt, TimelineEntry, TimelineEntryKind,
    ToolLifecycleStatus, TurnDiffSummary, WorkLogEntry, WorkTone,
};
use super::work_log_presentation::context_compaction_label;
// Upstream re-exports these; kept for callers of this module.
#[allow(unused_imports)]
pub use super::work_log_presentation::{
    work_entry_display_indicates_tool_failure, work_entry_indicates_tool_failure,
};

pub fn work_log_entry_is_tool_like(entry: &WorkLogEntry) -> bool {
    matches!(
        entry.tone,
        WorkTone::Tool | WorkTone::Thinking | WorkTone::Error
    ) || entry.command.is_some()
        || entry.request_kind.is_some()
}

/// Severe failures keep the red treatment ordinary tool failures lost: provider
/// runtime errors mean the turn or a core side effect broke, not that a
/// command exited nonzero.
pub fn work_entry_signals_severe_failure(entry: &WorkLogEntry) -> bool {
    entry.item_type == Some(TurnItemType::Error)
}

pub fn work_entry_indicates_tool_success(entry: &WorkLogEntry) -> bool {
    if !work_log_entry_is_tool_like(entry)
        || work_entry_indicates_tool_failure(entry)
        || (entry.tone == WorkTone::Thinking && entry.item_type != Some(TurnItemType::Reasoning))
    {
        return false;
    }
    !matches!(
        entry.tool_lifecycle_status,
        Some(
            ToolLifecycleStatus::Failed
                | ToolLifecycleStatus::Declined
                | ToolLifecycleStatus::InProgress
                | ToolLifecycleStatus::Stopped
                | ToolLifecycleStatus::Idle
        )
    )
}

/// Tool-like row with neither clear success nor failure (empty, incomplete, in progress, etc.).
pub fn work_entry_indicates_tool_neutral_status(entry: &WorkLogEntry) -> bool {
    work_log_entry_is_tool_like(entry)
        && !work_entry_indicates_tool_failure(entry)
        && !work_entry_indicates_tool_success(entry)
}

/// `Pick<ThreadRunSummary, "runId" | "startedAt" | "requestedAt" | "completedAt" | "status">`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestRunSummary {
    pub run_id: String,
    pub status: RunStatus,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// `Pick<ThreadRuntimeSummary, "activeRunId" | "activityStartedAt">`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeActivitySummary {
    pub active_run_id: Option<String>,
    /// `None` is upstream's absent key (older servers), `Some(None)` is `null`.
    pub activity_started_at: Option<Option<String>>,
}

pub fn is_latest_run_settled(
    latest_run: Option<&LatestRunSummary>,
    runtime: Option<&RuntimeActivitySummary>,
) -> bool {
    let Some(latest_run) = latest_run else {
        return false;
    };
    if matches!(
        latest_run.status,
        RunStatus::Preparing
            | RunStatus::Queued
            | RunStatus::Starting
            | RunStatus::Running
            | RunStatus::Waiting
    ) {
        return false;
    }
    runtime.and_then(|runtime| runtime.active_run_id.as_deref()) != Some(latest_run.run_id.as_str())
}

/// Port of `resolveThreadWorkingStartedAt` (upstream/packages/client-runtime/src/state/models.ts).
///
/// Both shell and detail timers count from the activity-owning run's work
/// start, never last activity. A wake keeps the start of the work it continues.
pub fn resolve_thread_working_started_at(
    latest_run: Option<&LatestRunSummary>,
    runtime: Option<&RuntimeActivitySummary>,
) -> Option<String> {
    let valid = |value: Option<&str>| {
        value
            .filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
            .map(str::to_string)
    };
    if let Some(activity_started_at) =
        runtime.and_then(|runtime| runtime.activity_started_at.as_ref())
    {
        return valid(activity_started_at.as_deref());
    }
    // Older servers can supply a timestamp only if the newest run owns the work.
    let run = latest_run?;
    if run.completed_at.is_none()
        && Some(run.run_id.as_str()) == runtime.and_then(|runtime| runtime.active_run_id.as_deref())
    {
        return valid(run.started_at.as_deref()).or_else(|| valid(run.requested_at.as_deref()));
    }
    None
}

pub fn derive_active_work_started_at(
    latest_run: Option<&LatestRunSummary>,
    runtime: Option<&RuntimeActivitySummary>,
    send_started_at: Option<&str>,
) -> Option<String> {
    let started_at = resolve_thread_working_started_at(latest_run, runtime);
    // Local dispatch has a clock only until the server supplies the owning run.
    started_at.or_else(|| {
        if runtime
            .and_then(|runtime| runtime.active_run_id.as_ref())
            .is_none()
        {
            send_started_at.map(str::to_string)
        } else {
            None
        }
    })
}

/// Sorted by `createdAt` (upstream `localeCompare`; ISO timestamps sort the
/// same lexically). The sort is stable.
pub fn derive_pending_approvals(approvals: &[PendingApproval]) -> Vec<PendingApproval> {
    let mut sorted = approvals.to_vec();
    sorted.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    sorted
}

pub fn derive_pending_user_inputs(inputs: &[PendingUserInput]) -> Vec<PendingUserInput> {
    let mut sorted = inputs.to_vec();
    sorted.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    sorted
}

/// `OrchestrationV2PlanArtifact` (the fields the session logic reads).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanArtifact {
    pub id: String,
    pub run_id: Option<String>,
    pub status: PlanStatus,
    pub kind: PlanArtifactKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlanArtifactKind {
    ProposedPlan {
        markdown: String,
    },
    TodoList {
        steps: Vec<PlanStep>,
        explanation: Option<String>,
    },
}

/// `Pick<OrchestrationV2ThreadProjection, "plans" | "turnItems" | "updatedAt">`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadPlanProjection {
    pub plans: Vec<PlanArtifact>,
    pub turn_items: Vec<TurnItem>,
    pub updated_at: String,
}

/// `ActivePlanState["steps"][number]["status"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivePlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivePlanStep {
    pub step: String,
    pub status: ActivePlanStepStatus,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivePlanState {
    pub created_at: String,
    pub run_id: Option<String>,
    pub explanation: Option<String>,
    pub steps: Vec<ActivePlanStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestProposedPlanState {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub run_id: Option<String>,
    pub plan_markdown: String,
    pub status: PlanStatus,
}

pub fn derive_active_plan_state(
    projection: Option<&ThreadPlanProjection>,
    latest_run_id: Option<&str>,
) -> Option<ActivePlanState> {
    let projection = projection?;
    let plans: Vec<&PlanArtifact> = projection
        .plans
        .iter()
        .filter(|plan| matches!(plan.kind, PlanArtifactKind::TodoList { .. }))
        .collect();
    let plan = plans
        .iter()
        .rev()
        .find(|candidate| latest_run_id.is_some() && candidate.run_id.as_deref() == latest_run_id)
        .or(plans.last())?;
    let PlanArtifactKind::TodoList { steps, explanation } = &plan.kind else {
        return None;
    };
    if steps.is_empty() {
        return None;
    }
    Some(ActivePlanState {
        created_at: plan_item_time(projection, &plan.id),
        run_id: plan.run_id.clone(),
        explanation: explanation.clone(),
        steps: steps
            .iter()
            .map(|step| ActivePlanStep {
                step: step.text.clone(),
                status: match step.status {
                    PlanStepStatus::Running => ActivePlanStepStatus::InProgress,
                    PlanStepStatus::Pending => ActivePlanStepStatus::Pending,
                    PlanStepStatus::Completed => ActivePlanStepStatus::Completed,
                },
                duration_ms: step.duration_ms,
            })
            .collect(),
    })
}

fn plan_item_time(projection: &ThreadPlanProjection, plan_id: &str) -> String {
    projection
        .turn_items
        .iter()
        .rev()
        .find(|candidate| match &candidate.payload {
            TurnItemPayload::ProposedPlan { plan_id: id, .. }
            | TurnItemPayload::TodoList { plan_id: id, .. } => id == plan_id,
            _ => false,
        })
        .map_or_else(
            || projection.updated_at.clone(),
            |item| item.updated_at.clone(),
        )
}

fn to_latest_proposed_plan_state(
    projection: &ThreadPlanProjection,
    plan: &PlanArtifact,
    markdown: &str,
) -> LatestProposedPlanState {
    let updated_at = plan_item_time(projection, &plan.id);
    LatestProposedPlanState {
        id: plan.id.clone(),
        created_at: updated_at.clone(),
        updated_at,
        run_id: plan.run_id.clone(),
        plan_markdown: markdown.to_string(),
        status: plan.status,
    }
}

pub fn find_latest_proposed_plan(
    projection: Option<&ThreadPlanProjection>,
    latest_run_id: Option<&str>,
) -> Option<LatestProposedPlanState> {
    let projection = projection?;
    let plans: Vec<&PlanArtifact> = projection
        .plans
        .iter()
        .filter(|plan| matches!(plan.kind, PlanArtifactKind::ProposedPlan { .. }))
        .collect();
    let candidates: Vec<&PlanArtifact> = match latest_run_id.filter(|id| !id.is_empty()) {
        Some(latest_run_id) => plans
            .iter()
            .copied()
            .filter(|plan| plan.run_id.as_deref() == Some(latest_run_id))
            .collect(),
        None => plans.clone(),
    };
    let mut sorted = if candidates.is_empty() {
        plans
    } else {
        candidates
    };
    sorted.sort_by(|left, right| {
        plan_item_time(projection, &left.id)
            .cmp(&plan_item_time(projection, &right.id))
            .then_with(|| left.id.cmp(&right.id))
    });
    let plan = sorted.last()?;
    let PlanArtifactKind::ProposedPlan { markdown } = &plan.kind else {
        return None;
    };
    Some(to_latest_proposed_plan_state(projection, plan, markdown))
}

pub fn has_actionable_proposed_plan(plan: Option<&LatestProposedPlanState>) -> bool {
    plan.is_some_and(|plan| plan.status == PlanStatus::Active)
}

fn is_standalone_v2_item_type(item_type: TurnItemType) -> bool {
    matches!(
        item_type,
        TurnItemType::Fork
            | TurnItemType::Handoff
            | TurnItemType::RunInterruptRequest
            | TurnItemType::RunInterruptResult
            | TurnItemType::SecretRequest
            | TurnItemType::Subagent
    )
}

fn is_persistent_resource_v2_item_type(item_type: TurnItemType) -> bool {
    matches!(
        item_type,
        TurnItemType::Fork
            | TurnItemType::ThreadCreated
            // Still answerable after a steer supersedes the attempt that asked.
            | TurnItemType::SecretRequest
    )
}

pub fn timeline_entry_is_persistent_resource_card(entry: &TimelineEntry) -> bool {
    match &entry.kind {
        TimelineEntryKind::Event { projected_item } => {
            is_persistent_resource_v2_item_type(projected_item.item.item_type())
        }
        _ => false,
    }
}

fn projected_item_created_at(row: &ProjectedTurnItem) -> String {
    row.item
        .started_at
        .clone()
        .unwrap_or_else(|| row.item.updated_at.clone())
}

pub fn projected_work_entry_status(item: &TurnItem) -> ToolLifecycleStatus {
    match item.status {
        TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting => {
            ToolLifecycleStatus::InProgress
        }
        TurnItemStatus::Completed => ToolLifecycleStatus::Completed,
        TurnItemStatus::Idle => ToolLifecycleStatus::Idle,
        TurnItemStatus::Failed => ToolLifecycleStatus::Failed,
        TurnItemStatus::Cancelled | TurnItemStatus::Interrupted => ToolLifecycleStatus::Stopped,
    }
}

pub fn projected_work_entry_tone(item: &TurnItem) -> WorkTone {
    match item.item_type() {
        TurnItemType::Error => WorkTone::Info,
        TurnItemType::Reasoning => WorkTone::Thinking,
        TurnItemType::CommandExecution
        | TurnItemType::FileChange
        | TurnItemType::FileSearch
        | TurnItemType::WebSearch
        | TurnItemType::DynamicTool
        | TurnItemType::Subagent
        | TurnItemType::ThreadCreated
        | TurnItemType::UserInputRequest
        | TurnItemType::ApprovalRequest => WorkTone::Tool,
        _ => WorkTone::Info,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderErrorPresentation {
    pub label: String,
    pub detail: String,
}

/// For an `error` item; any other item yields `None`.
pub fn provider_error_presentation(item: &TurnItem) -> Option<ProviderErrorPresentation> {
    let TurnItemPayload::Error { failure, retry } = &item.payload else {
        return None;
    };
    let usage_limit = failure.class == ProviderFailureClass::UsageLimit;
    let Some(retry) = retry else {
        return Some(ProviderErrorPresentation {
            label: if usage_limit {
                "Usage limit reached".to_string()
            } else {
                item.title
                    .as_deref()
                    .map(js_trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or("Provider error")
                    .to_string()
            },
            detail: failure.message.clone(),
        });
    };
    let progress = match retry.max_attempts {
        None => retry.attempt.to_string(),
        Some(max_attempts) => format!("{}/{max_attempts}", retry.attempt),
    };
    let label = match item.status {
        TurnItemStatus::Running => format!("Retrying provider ({progress})"),
        TurnItemStatus::Completed => format!("Provider recovered ({progress} retries)"),
        TurnItemStatus::Failed => format!(
            "{} after {progress} retries",
            if usage_limit {
                "Usage limit reached"
            } else {
                "Provider error"
            }
        ),
        _ => format!("Provider retry stopped ({progress})"),
    };
    let retry_delay = match retry.retry_delay_ms {
        Some(delay) if item.status == TurnItemStatus::Running && delay > 0 => {
            if delay < 1_000 {
                format!(" Retrying in {delay}ms.")
            } else {
                let seconds = format!("{:.1}", delay as f64 / 1_000.0);
                let seconds = seconds.strip_suffix(".0").unwrap_or(&seconds);
                format!(" Retrying in {seconds}s.")
            }
        }
        _ => String::new(),
    };
    Some(ProviderErrorPresentation {
        label,
        detail: format!("{}{retry_delay}", failure.message),
    })
}

fn item_type_label(item_type: TurnItemType) -> String {
    item_type.as_str().replace('_', " ")
}

pub fn projected_work_entry(row: &ProjectedTurnItem) -> WorkLogEntry {
    let item = &row.item;
    let title = item
        .title
        .as_deref()
        .map(js_trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    let presentation = extract_tool_activity_presentation(item);
    let mut entry = WorkLogEntry {
        run_id: item.run_id.clone(),
        item_type: Some(item.item_type()),
        tool_lifecycle_status: Some(projected_work_entry_status(item)),
        structured_payload: Some(item.clone()),
        projected_item: Some(row.clone()),
        viewed_image_path: presentation.viewed_image_path,
        tool_surface: presentation.tool_surface,
        tool_icon: presentation.tool_icon,
        tool_source: presentation.tool_source,
        ..WorkLogEntry::new(
            item.id.clone(),
            projected_item_created_at(row),
            String::new(),
            projected_work_entry_tone(item),
        )
    };
    let item_json = || turn_item_tool_data(item);

    match &item.payload {
        TurnItemPayload::ThreadCreated { .. } => {
            entry.label = "Created thread".to_string();
        }
        TurnItemPayload::Compaction {
            summary,
            before_token_count,
            after_token_count,
        } => {
            entry.label =
                context_compaction_label(item.status, *before_token_count, *after_token_count);
            entry.source_activity_kind = Some("context-compaction".to_string());
            entry.detail = summary.clone().filter(|summary| !summary.is_empty());
        }
        TurnItemPayload::Reasoning { text, .. } => {
            entry.label = title.unwrap_or_else(|| "Thinking".to_string());
            entry.detail = Some(text.clone()).filter(|text| !text.is_empty());
        }
        TurnItemPayload::CommandExecution { input, .. } => {
            entry.label = title.clone().unwrap_or_else(|| "Ran command".to_string());
            entry.command = Some(input.clone());
            entry.raw_command = Some(input.clone());
            entry.tool_title = Some(title.unwrap_or_else(|| "Command".to_string()));
            entry.tool_data = Some(item_json());
        }
        TurnItemPayload::FileChange {
            file_name, changes, ..
        } => {
            entry.label = title.clone().unwrap_or_else(|| match changes {
                Some(changes) if changes.len() > 1 => format!("Changed {} files", changes.len()),
                _ => format!("Changed {file_name}"),
            });
            entry.changed_files = Some(match changes {
                Some(changes) => changes.iter().map(|change| change.path.clone()).collect(),
                None => vec![file_name.clone()],
            });
            entry.tool_title = Some(title.unwrap_or_else(|| "File change".to_string()));
            entry.tool_data = Some(item_json());
        }
        TurnItemPayload::FileSearch { pattern, .. } => {
            let data = item_json();
            entry.label = title
                .clone()
                .or_else(|| format_search_tool_label(Some(&data)))
                .unwrap_or_else(|| "Searched files".to_string());
            entry.detail = pattern.clone().filter(|pattern| !pattern.is_empty());
            entry.tool_title = Some(title.unwrap_or_else(|| "File search".to_string()));
            entry.tool_data = Some(data);
        }
        TurnItemPayload::WebSearch { patterns, .. } => {
            entry.label = title
                .clone()
                .unwrap_or_else(|| "Searched the web".to_string());
            entry.detail = (!patterns.is_empty()).then(|| patterns.join(", "));
            entry.tool_title = Some(title.unwrap_or_else(|| "Web search".to_string()));
            entry.tool_data = Some(item_json());
        }
        TurnItemPayload::Checkpoint { files, .. } => {
            entry.label = title.unwrap_or_else(|| "Checkpoint captured".to_string());
            entry.changed_files = Some(files.iter().map(|file| file.path.clone()).collect());
            entry.tool_data = Some(item_json());
        }
        TurnItemPayload::SystemNotice { message } => {
            entry.label = message.clone();
            entry.source_activity_kind = Some("runtime.warning".to_string());
        }
        TurnItemPayload::Error { failure, retry } => {
            if let Some(presentation) = provider_error_presentation(item) {
                entry.label = presentation.label;
                entry.detail = Some(presentation.detail);
            }
            if failure.class == ProviderFailureClass::UsageLimit
                && item.status != TurnItemStatus::Completed
            {
                entry.source_activity_kind = Some("runtime.warning".to_string());
            } else if retry.is_none() {
                entry.source_activity_kind = Some("runtime.error".to_string());
            }
            entry.tool_data = Some(item_json());
        }
        TurnItemPayload::DynamicTool {
            tool_name,
            input,
            output,
            ..
        } => {
            let mut data = serde_json::Map::new();
            if let Some(tool_name) = tool_name {
                data.insert("toolName".to_string(), Value::String(tool_name.clone()));
            }
            data.insert("input".to_string(), input.clone());
            let classified = classify_tool_activity(ClassifyToolActivityInput {
                item_type: Some("dynamic_tool_call"),
                request_kind: None,
                title: None,
                data: Some(&data),
            });
            let input_data = serde_json::json!({ "input": input });
            let read_path = collect_tool_file_paths(Some(&input_data))
                .into_iter()
                .next();
            entry.label = title.clone().unwrap_or_else(|| match classified {
                ToolActivityAction::Read => {
                    format_read_tool_label(read_path.as_deref().unwrap_or(""), 0)
                }
                ToolActivityAction::Search => format_search_tool_label(Some(&input_data))
                    .or_else(|| tool_name.clone())
                    .unwrap_or_else(|| "Tool call".to_string()),
                _ => tool_name.clone().unwrap_or_else(|| "Tool call".to_string()),
            });
            entry.tool_title = Some(
                title
                    .or_else(|| tool_name.clone())
                    .unwrap_or_else(|| "Tool".to_string()),
            );
            let mut tool_data = serde_json::Map::new();
            tool_data.insert("input".to_string(), input.clone());
            if let Some(output) = output {
                tool_data.insert("output".to_string(), output.clone());
            }
            entry.tool_data = Some(Value::Object(tool_data));
        }
        TurnItemPayload::ApprovalRequest {
            request_kind,
            prompt,
            ..
        } => {
            entry.label = title.unwrap_or_else(|| "Approval requested".to_string());
            entry.detail = Some(
                prompt
                    .clone()
                    .unwrap_or_else(|| provider_request_kind_str(*request_kind).to_string()),
            );
            entry.request_kind = None;
            entry.tool_data = Some(item_json());
        }
        TurnItemPayload::UserInputRequest {
            question_answer, ..
        } => {
            entry.label = title.unwrap_or_else(|| {
                if question_answer.is_some() {
                    "Answered questions".to_string()
                } else {
                    "Input requested".to_string()
                }
            });
            entry.question_answer = question_answer.clone();
            entry.tool_data = Some(item_json());
        }
        _ => {
            entry.label = title.unwrap_or_else(|| item_type_label(item.item_type()));
            entry.tool_data = Some(item_json());
        }
    }
    entry
}

fn provider_request_kind_str(kind: super::turn_item::ProviderRequestKind) -> &'static str {
    use super::turn_item::ProviderRequestKind as Kind;
    match kind {
        Kind::Command => "command",
        Kind::FileRead => "file-read",
        Kind::FileChange => "file-change",
        Kind::McpElicitation => "mcp-elicitation",
        Kind::Permission => "permission",
    }
}

/// `toolData: item` keeps the whole item upstream. The mirror is not a wire
/// type, so the fields the work-log helpers read back (`type`, the command,
/// file and search fields) are rebuilt as JSON with upstream's keys.
fn turn_item_tool_data(item: &TurnItem) -> Value {
    let mut data = serde_json::Map::new();
    data.insert(
        "type".to_string(),
        Value::String(item.item_type().as_str().to_string()),
    );
    data.insert("id".to_string(), Value::String(item.id.clone()));
    data.insert(
        "status".to_string(),
        Value::String(item.status.as_str().to_string()),
    );
    if let Some(title) = &item.title {
        data.insert("title".to_string(), Value::String(title.clone()));
    }
    match &item.payload {
        TurnItemPayload::CommandExecution {
            input,
            output,
            exit_code,
            ..
        } => {
            data.insert("input".to_string(), Value::String(input.clone()));
            if let Some(output) = output {
                data.insert("output".to_string(), Value::String(output.clone()));
            }
            if let Some(exit_code) = exit_code {
                data.insert("exitCode".to_string(), Value::from(*exit_code));
            }
        }
        TurnItemPayload::FileChange {
            file_name,
            additions,
            deletions,
            changes,
            ..
        } => {
            data.insert("fileName".to_string(), Value::String(file_name.clone()));
            if let Some(additions) = additions {
                data.insert("additions".to_string(), Value::from(*additions));
            }
            if let Some(deletions) = deletions {
                data.insert("deletions".to_string(), Value::from(*deletions));
            }
            if let Some(changes) = changes {
                data.insert(
                    "changes".to_string(),
                    Value::Array(
                        changes
                            .iter()
                            .map(|change| {
                                serde_json::json!({
                                    "operation": change.operation,
                                    "path": change.path,
                                })
                            })
                            .collect(),
                    ),
                );
            }
        }
        TurnItemPayload::FileSearch { pattern, results } => {
            if let Some(pattern) = pattern {
                data.insert("pattern".to_string(), Value::String(pattern.clone()));
            }
            data.insert(
                "results".to_string(),
                Value::Array(
                    results
                        .iter()
                        .map(|file_name| serde_json::json!({ "fileName": file_name }))
                        .collect(),
                ),
            );
        }
        TurnItemPayload::WebSearch { patterns, .. } => {
            data.insert(
                "patterns".to_string(),
                Value::Array(patterns.iter().cloned().map(Value::String).collect()),
            );
        }
        TurnItemPayload::ApprovalRequest {
            request_id,
            request_kind,
            prompt,
            ..
        } => {
            data.insert("requestId".to_string(), Value::String(request_id.clone()));
            data.insert(
                "requestKind".to_string(),
                Value::String(provider_request_kind_str(*request_kind).to_string()),
            );
            if let Some(prompt) = prompt {
                data.insert("prompt".to_string(), Value::String(prompt.clone()));
            }
        }
        TurnItemPayload::Error { failure, .. } => {
            data.insert(
                "failure".to_string(),
                serde_json::json!({ "message": failure.message, "code": failure.code }),
            );
        }
        _ => {}
    }
    Value::Object(data)
}

/// Builds the timeline in the exact order committed by `visibleTurnItems`.
/// Committed rows are presented directly from their projected item. Queued
/// input is absent by construction until dispatch creates its user turn item.
/// Persistent client-owned messages are inserted by timestamp without sorting
/// the canonical sequence. True optimistic sends remain appended afterward.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineEntriesInput {
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    pub optimistic_messages: Vec<ChatMessage>,
    pub anchored_messages: Vec<ChatMessage>,
    /// Local preview location per attachment id (upstream `attachmentUrlById`).
    pub attachment_url_by_id: HashMap<String, String>,
    pub attempts: Vec<TimelineAttempt>,
    pub nodes: Vec<ExecutionNode>,
    pub plans: Vec<PlanArtifact>,
}

/// `Pick<OrchestrationV2ExecutionNode, "id" | "parentNodeId" | "rootNodeId">`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionNode {
    pub id: String,
    pub parent_node_id: Option<String>,
    pub root_node_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineEntriesProjection {
    pub input: TimelineEntriesInput,
    pub entries: Vec<TimelineEntry>,
}

pub fn derive_timeline_entries_from_visible_turn_items(
    input: &TimelineEntriesInput,
) -> Vec<TimelineEntry> {
    derive_timeline_entries_from_items(input, &input.visible_turn_items)
}

fn derive_timeline_entries_from_items(
    input: &TimelineEntriesInput,
    visible_turn_items: &[ProjectedTurnItem],
) -> Vec<TimelineEntry> {
    let mut committed_message_ids: HashSet<String> = HashSet::new();
    let mut entries: Vec<TimelineEntry> = Vec::new();
    let attempt_by_root_node_id: HashMap<&str, &TimelineAttempt> = input
        .attempts
        .iter()
        .map(|attempt| (attempt.root_node_id.as_str(), attempt))
        .collect();
    let node_by_id: HashMap<&str, &ExecutionNode> = input
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let plan_by_id: HashMap<&str, &PlanArtifact> = input
        .plans
        .iter()
        .map(|plan| (plan.id.as_str(), plan))
        .collect();

    let resolve_attempt = |item: &TurnItem| -> Option<TimelineAttempt> {
        let item_run_id = item.run_id.as_deref()?;
        let mut node_id = item.node_id.as_deref();
        let mut visited: HashSet<&str> = HashSet::new();
        while let Some(current) = node_id {
            if !visited.insert(current) {
                break;
            }
            if let Some(direct_attempt) = attempt_by_root_node_id.get(current)
                && direct_attempt.run_id == item_run_id
            {
                return Some((*direct_attempt).clone());
            }
            let node = node_by_id.get(current)?;
            if let Some(root_attempt) = attempt_by_root_node_id.get(node.root_node_id.as_str())
                && root_attempt.run_id == item_run_id
            {
                return Some((*root_attempt).clone());
            }
            node_id = node.parent_node_id.as_deref();
        }
        None
    };

    let folded_answer_message_ids: HashSet<String> = visible_turn_items
        .iter()
        .filter_map(|row| match &row.item.payload {
            TurnItemPayload::UserInputRequest {
                question_answer: Some(answer),
                ..
            } => Some(format!("async-answer:{}", answer.request_id)),
            _ => None,
        })
        .collect();
    for row in visible_turn_items {
        let item = &row.item;
        if turn_item_is_workspace_preparation(item) {
            continue;
        }
        // Task progress belongs in the composer, not between conversation entries.
        if matches!(
            item.item_type(),
            TurnItemType::TodoList | TurnItemType::Checkpoint
        ) {
            continue;
        }
        if let TurnItemPayload::UserMessage { message_id, .. } = &item.payload
            && folded_answer_message_ids.contains(message_id)
        {
            continue;
        }
        let created_at = projected_item_created_at(row);
        let attempt = resolve_attempt(item);
        match &item.payload {
            TurnItemPayload::Notification { summary, .. } => {
                entries.push(TimelineEntry {
                    id: item.id.clone(),
                    created_at: created_at.clone(),
                    kind: TimelineEntryKind::Work {
                        entry: WorkLogEntry {
                            run_id: item.run_id.clone(),
                            item_type: Some(item.item_type()),
                            structured_payload: Some(item.clone()),
                            projected_item: Some(row.clone()),
                            ..WorkLogEntry::new(
                                item.id.clone(),
                                created_at,
                                summary.clone(),
                                WorkTone::Info,
                            )
                        },
                    },
                    attempt,
                });
            }
            TurnItemPayload::UserMessage {
                message_id,
                text,
                created_by,
                creation_source,
                scheduled_task_id,
                sender_thread_id,
                input_intent,
                attachments,
            } => {
                let message = ChatMessage {
                    id: message_id.clone(),
                    role: MessageRole::User,
                    text: text.clone(),
                    attachments: with_preview_urls(attachments, &input.attachment_url_by_id),
                    run_id: item.run_id.clone(),
                    streaming: false,
                    created_by: Some(*created_by),
                    creation_source: Some(*creation_source),
                    scheduled_task_id: scheduled_task_id.clone(),
                    sender_thread_id: sender_thread_id.clone(),
                    created_at: created_at.clone(),
                    updated_at: item.updated_at.clone(),
                    input_intent: Some(*input_intent),
                };
                committed_message_ids.insert(message.id.clone());
                entries.push(TimelineEntry {
                    id: message.id.clone(),
                    created_at,
                    kind: TimelineEntryKind::Message {
                        message,
                        projected_item: Some(row.clone()),
                    },
                    attempt,
                });
            }
            TurnItemPayload::AssistantMessage {
                message_id,
                text,
                streaming,
                attachments,
            } => {
                let message = ChatMessage {
                    id: message_id.clone(),
                    role: MessageRole::Assistant,
                    text: text.clone(),
                    attachments: with_preview_urls(attachments, &input.attachment_url_by_id),
                    run_id: item.run_id.clone(),
                    streaming: *streaming,
                    created_by: None,
                    creation_source: None,
                    scheduled_task_id: None,
                    sender_thread_id: None,
                    created_at: created_at.clone(),
                    updated_at: item.updated_at.clone(),
                    input_intent: None,
                };
                committed_message_ids.insert(message.id.clone());
                entries.push(TimelineEntry {
                    id: message.id.clone(),
                    created_at,
                    kind: TimelineEntryKind::Message {
                        message,
                        projected_item: Some(row.clone()),
                    },
                    attempt,
                });
            }
            TurnItemPayload::ProposedPlan {
                plan_id, markdown, ..
            } => {
                let plan = plan_by_id.get(plan_id.as_str());
                let proposed_plan = ProposedPlan {
                    id: plan_id.clone(),
                    run_id: item.run_id.clone(),
                    plan_markdown: markdown.clone(),
                    status: match plan {
                        Some(plan)
                            if matches!(plan.kind, PlanArtifactKind::ProposedPlan { .. }) =>
                        {
                            plan.status
                        }
                        _ => PlanStatus::Active,
                    },
                    created_at: created_at.clone(),
                    updated_at: item.updated_at.clone(),
                };
                entries.push(TimelineEntry {
                    id: item.id.clone(),
                    created_at,
                    kind: TimelineEntryKind::ProposedPlan { proposed_plan },
                    attempt,
                });
            }
            _ if is_standalone_v2_item_type(item.item_type()) => {
                entries.push(TimelineEntry {
                    id: item.id.clone(),
                    created_at,
                    kind: TimelineEntryKind::Event {
                        projected_item: row.clone(),
                    },
                    attempt,
                });
            }
            _ => {
                entries.push(TimelineEntry {
                    id: item.id.clone(),
                    created_at,
                    kind: TimelineEntryKind::Work {
                        entry: projected_work_entry(row),
                    },
                    attempt,
                });
            }
        }
    }

    let mut retained_message_ids: HashSet<String> = committed_message_ids;
    retained_message_ids.extend(folded_answer_message_ids);
    for message in &input.anchored_messages {
        if !retained_message_ids.insert(message.id.clone()) {
            continue;
        }
        let entry = TimelineEntry {
            id: message.id.clone(),
            created_at: message.created_at.clone(),
            kind: TimelineEntryKind::Message {
                message: message.clone(),
                projected_item: None,
            },
            attempt: None,
        };
        match entries
            .iter()
            .position(|candidate| candidate.created_at > message.created_at)
        {
            Some(insertion_index) => entries.insert(insertion_index, entry),
            None => entries.push(entry),
        }
    }

    for message in &input.optimistic_messages {
        if message.input_intent != Some(InputIntent::QueuedTurn)
            && retained_message_ids.insert(message.id.clone())
        {
            entries.push(TimelineEntry {
                id: message.id.clone(),
                created_at: message.created_at.clone(),
                kind: TimelineEntryKind::Message {
                    message: message.clone(),
                    projected_item: None,
                },
                attempt: None,
            });
        }
    }

    entries
}

fn with_preview_urls(
    attachments: &[super::types::ChatAttachment],
    attachment_url_by_id: &HashMap<String, String>,
) -> Vec<super::types::ChatAttachment> {
    attachments
        .iter()
        .map(
            |attachment| match attachment_url_by_id.get(&attachment.id) {
                Some(preview) if !preview.is_empty() => super::types::ChatAttachment {
                    preview_path: Some(preview.clone()),
                    ..attachment.clone()
                },
                _ => attachment.clone(),
            },
        )
        .collect()
}

/// Text and update time do not change a streaming assistant message's row structure.
pub fn is_streaming_message_text_update(previous: &ChatMessage, next: &ChatMessage) -> bool {
    if previous.role != MessageRole::Assistant
        || next.role != MessageRole::Assistant
        || !previous.streaming
        || !next.streaming
    {
        return false;
    }
    let ChatMessage {
        id,
        role,
        text: _,
        attachments,
        run_id,
        streaming,
        created_by,
        creation_source,
        scheduled_task_id,
        sender_thread_id,
        created_at,
        updated_at: _,
        input_intent,
    } = previous;
    *id == next.id
        && *role == next.role
        && *attachments == next.attachments
        && *run_id == next.run_id
        && *streaming == next.streaming
        && *created_by == next.created_by
        && *creation_source == next.creation_source
        && *scheduled_task_id == next.scheduled_task_id
        && *sender_thread_id == next.sender_thread_id
        && *created_at == next.created_at
        && *input_intent == next.input_intent
}

/// Keep provenance and execution metadata in the rebuild boundary, including inspector data.
pub fn is_streaming_turn_item_text_update(
    previous: &ProjectedTurnItem,
    next: &ProjectedTurnItem,
) -> bool {
    let (
        TurnItemPayload::AssistantMessage {
            text: previous_text,
            streaming: true,
            ..
        },
        TurnItemPayload::AssistantMessage {
            streaming: true, ..
        },
    ) = (&previous.item.payload, &next.item.payload)
    else {
        return false;
    };
    if previous.position != next.position
        || previous.visibility != next.visibility
        || previous.source_thread_id != next.source_thread_id
        || previous.source_item_id != next.source_item_id
        || projected_item_created_at(previous) != projected_item_created_at(next)
    {
        return false;
    }
    // Wire decoding can recreate timestamps and attachments on each update.
    // Compare only the changed item's metadata, never the entire transcript.
    let mut next_metadata = next.item.clone();
    next_metadata.updated_at = previous.item.updated_at.clone();
    if let TurnItemPayload::AssistantMessage { text, .. } = &mut next_metadata.payload {
        *text = previous_text.clone();
    }
    next_metadata == previous.item
}

fn reuse_timeline_entries(
    input: &TimelineEntriesInput,
    previous: &TimelineEntriesProjection,
) -> Option<Vec<TimelineEntry>> {
    let before = &previous.input;
    if input.visible_turn_items.len() < before.visible_turn_items.len()
        || input.optimistic_messages != before.optimistic_messages
        || input.anchored_messages != before.anchored_messages
        || input.attachment_url_by_id != before.attachment_url_by_id
        || input.attempts != before.attempts
        || input.nodes != before.nodes
        || input.plans != before.plans
    {
        return None;
    }
    let appended = input.visible_turn_items.len() > before.visible_turn_items.len();
    // Anchored and optimistic messages need to be interleaved/deduplicated when
    // committed items arrive. Keep the full projection for that transition.
    if appended && (!input.optimistic_messages.is_empty() || !input.anchored_messages.is_empty()) {
        return None;
    }
    let appended_items = &input.visible_turn_items[before.visible_turn_items.len()..];
    // Answer rows can replace a message already present in the retained prefix.
    if appended
        && appended_items.iter().any(|row| match &row.item.payload {
            TurnItemPayload::UserInputRequest {
                question_answer, ..
            } => question_answer.is_some(),
            TurnItemPayload::UserMessage { message_id, .. } => {
                message_id.starts_with("async-answer:")
            }
            _ => false,
        })
    {
        return None;
    }
    let mut replacements: HashMap<&str, (&ProjectedTurnItem, &ProjectedTurnItem)> = HashMap::new();
    for (previous_item, item) in before
        .visible_turn_items
        .iter()
        .zip(&input.visible_turn_items)
    {
        if item == previous_item {
            continue;
        }
        if !is_streaming_turn_item_text_update(previous_item, item) {
            return None;
        }
        replacements.insert(&previous_item.item.id, (previous_item, item));
    }
    if replacements.is_empty() && !appended {
        return Some(previous.entries.clone());
    }
    let mut entries: Vec<TimelineEntry> = previous
        .entries
        .iter()
        .map(|entry| {
            let TimelineEntryKind::Message {
                message,
                projected_item: Some(projected_item),
            } = &entry.kind
            else {
                return entry.clone();
            };
            let Some((_, row)) = replacements
                .get(projected_item.item.id.as_str())
                .filter(|(previous_item, _)| *previous_item == projected_item)
            else {
                return entry.clone();
            };
            let TurnItemPayload::AssistantMessage { text, .. } = &row.item.payload else {
                return entry.clone();
            };
            TimelineEntry {
                kind: TimelineEntryKind::Message {
                    message: ChatMessage {
                        text: text.clone(),
                        updated_at: row.item.updated_at.clone(),
                        ..message.clone()
                    },
                    projected_item: Some((*row).clone()),
                },
                ..entry.clone()
            }
        })
        .collect();
    if appended {
        entries.extend(derive_timeline_entries_from_items(input, appended_items));
    }
    Some(entries)
}

/// Reuse immutable entries during streaming without reordering the canonical v2 sequence.
pub fn derive_timeline_entries_from_visible_turn_items_with_state(
    input: TimelineEntriesInput,
    previous: Option<&TimelineEntriesProjection>,
) -> TimelineEntriesProjection {
    if let Some(reused) = previous.and_then(|previous| reuse_timeline_entries(&input, previous)) {
        return TimelineEntriesProjection {
            input,
            entries: reused,
        };
    }
    let entries = derive_timeline_entries_from_visible_turn_items(&input);
    let Some(previous) = previous
        .filter(|previous| input.attachment_url_by_id == previous.input.attachment_url_by_id)
    else {
        return TimelineEntriesProjection { input, entries };
    };
    // Tool output and lifecycle changes rebuild grouping, but unchanged message
    // objects and previews still let memoized history rows stay mounted.
    let previous_messages: HashMap<&str, &TimelineEntry> = previous
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, TimelineEntryKind::Message { .. }))
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let entries = entries
        .into_iter()
        .map(|entry| {
            let TimelineEntryKind::Message {
                projected_item: Some(projected_item),
                ..
            } = &entry.kind
            else {
                return entry;
            };
            let Some(before) = previous_messages.get(entry.id.as_str()) else {
                return entry;
            };
            let TimelineEntryKind::Message {
                message: before_message,
                projected_item: before_projected,
            } = &before.kind
            else {
                return entry;
            };
            if before_projected.as_ref() != Some(projected_item) {
                return entry;
            }
            if before.attempt == entry.attempt {
                return (*before).clone();
            }
            let projected_item = projected_item.clone();
            TimelineEntry {
                kind: TimelineEntryKind::Message {
                    message: before_message.clone(),
                    projected_item: Some(projected_item),
                },
                ..entry
            }
        })
        .collect();
    TimelineEntriesProjection { input, entries }
}

/// `ThreadCheckpointSummary.runId` is always set in Turin's summaries, so
/// every summary contributes; later summaries win, like `Object.fromEntries`.
pub fn infer_checkpoint_turn_count_by_run_id(
    summaries: &[TurnDiffSummary],
) -> HashMap<String, u64> {
    summaries
        .iter()
        .map(|summary| (summary.run_id.clone(), summary.checkpoint_turn_count))
        .collect()
}

pub fn derive_revert_turn_count_by_user_message_id(
    timeline_entries: &[TimelineEntry],
    checkpoints: &[TurnDiffSummary],
) -> HashMap<String, u64> {
    let mut ready_checkpoint_by_run_id: HashMap<&str, &TurnDiffSummary> = HashMap::new();
    for checkpoint in checkpoints {
        if checkpoint.status == CheckpointStatus::Ready {
            ready_checkpoint_by_run_id.insert(&checkpoint.run_id, checkpoint);
        }
    }
    let mut by_user_message_id = HashMap::new();
    for entry in timeline_entries {
        let TimelineEntryKind::Message { message, .. } = &entry.kind else {
            continue;
        };
        if message.role != MessageRole::User {
            continue;
        }
        if !matches!(
            message.input_intent,
            Some(InputIntent::TurnStart | InputIntent::QueuedTurn)
        ) {
            continue;
        }
        let Some(run_id) = message.run_id.as_deref() else {
            continue;
        };
        let Some(checkpoint) = ready_checkpoint_by_run_id.get(run_id) else {
            continue;
        };
        by_user_message_id.insert(
            message.id.clone(),
            checkpoint.checkpoint_turn_count.saturating_sub(1),
        );
    }
    by_user_message_id
}
