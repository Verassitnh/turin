//! ACP `session/update` projection.
//!
//! Unknown `sessionUpdate` variants are kept. Cursor's CLI still sends the
//! standard ACP plan update before `cursor/create_plan`.

use std::collections::HashMap;

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2ConversationMessage,
    OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
    OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
    OrchestrationV2ExecutionNodeStatus, OrchestrationV2FileSearchResult,
    OrchestrationV2NativeRefStrength, OrchestrationV2PlanArtifact,
    OrchestrationV2PlanArtifactStatus, OrchestrationV2PlanStep, OrchestrationV2PlanStepStatus,
    OrchestrationV2ProviderRef, OrchestrationV2ProviderTurnStatus, OrchestrationV2TurnItem,
    OrchestrationV2TurnItemStatus, OrchestrationV2UserInputQuestion,
};
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::provider_policy::{ProviderApprovalOption, ProviderRequestKind};
use turin_contracts::{
    MessageId, NodeId, PlanId, ProviderThreadId, ProviderTurnId, RunId, RuntimeRequestId, ThreadId,
    TrimmedNonEmptyString, TurnItemId, UtcDateTime,
};
use turin_provider_core::id_allocator::derive;

use crate::extensions;
use turin_provider_core::provider_adapter::ProviderAdapterV2Event;

#[derive(Debug, Clone, PartialEq)]
pub enum CursorUpdate {
    Text {
        text: String,
        message_id: Option<String>,
    },
    Thought {
        text: String,
    },
    Tool {
        tool_call_id: String,
        title: Option<String>,
        kind: Option<String>,
        status: String,
        raw: Value,
    },
    Plan {
        steps: Vec<PlanStep>,
        raw: Value,
    },
    Commands {
        raw: Value,
    },
    Config {
        raw: Value,
    },
    Mode {
        mode_id: String,
    },
    Usage {
        raw: Value,
    },
    Unknown {
        session_update: String,
        raw: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub text: String,
    pub status: String,
}

pub fn parse_session_update(params: &Value) -> Vec<CursorUpdate> {
    let update = params.get("update").unwrap_or(params);
    let kind = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("");
    match kind {
        "agent_message_chunk" => content_text(update.get("content"))
            .map(|text| CursorUpdate::Text {
                text,
                message_id: update
                    .get("messageId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
            .into_iter()
            .collect(),
        "agent_thought_chunk" => content_text(update.get("content"))
            .map(|text| CursorUpdate::Thought { text })
            .into_iter()
            .collect(),
        "tool_call" | "tool_call_update" => parse_tool(update).into_iter().collect(),
        "plan" => {
            let steps = plan_entries(update.get("entries"));
            if steps.is_empty() {
                Vec::new()
            } else {
                vec![CursorUpdate::Plan {
                    steps,
                    raw: params.clone(),
                }]
            }
        }
        "available_commands_update" => vec![CursorUpdate::Commands {
            raw: params.clone(),
        }],
        "config_option_update" => vec![CursorUpdate::Config {
            raw: params.clone(),
        }],
        "current_mode_update" => update
            .get("currentModeId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|mode| !mode.is_empty())
            .map(|mode| CursorUpdate::Mode {
                mode_id: mode.to_owned(),
            })
            .into_iter()
            .collect(),
        "usage_update" | "state_update" => vec![CursorUpdate::Usage {
            raw: params.clone(),
        }],
        other => vec![CursorUpdate::Unknown {
            session_update: other.to_owned(),
            raw: params.clone(),
        }],
    }
}

fn parse_tool(update: &Value) -> Option<CursorUpdate> {
    let tool_call_id = update.get("toolCallId").and_then(Value::as_str)?.to_owned();
    let status = match update.get("status").and_then(Value::as_str) {
        Some("completed") => "completed",
        Some("failed") => "failed",
        Some("pending") => "pending",
        Some("in_progress" | "inProgress") => "inProgress",
        _ => "inProgress",
    };
    Some(CursorUpdate::Tool {
        tool_call_id,
        title: update
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        kind: update
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        status: status.to_owned(),
        raw: update.clone(),
    })
}

fn plan_entries(entries: Option<&Value>) -> Vec<PlanStep> {
    let Some(Value::Array(entries)) = entries else {
        return Vec::new();
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let content = entry
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let text = if content.is_empty() {
                format!("Step {}", index + 1)
            } else {
                content.to_owned()
            };
            let status = match entry.get("status").and_then(Value::as_str) {
                Some("completed") => "completed",
                Some("in_progress" | "inProgress") => "inProgress",
                _ => "pending",
            };
            PlanStep {
                text,
                status: status.to_owned(),
            }
        })
        .collect()
}

fn content_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if content.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let text = content.get("text").and_then(Value::as_str)?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Turn item projection. Upstream `adapter.ts` builds these from SDK messages;
// Turin builds them from ACP `session/update` and `cursor/*` payloads.
// ---------------------------------------------------------------------------

/// Where a turn's items attach. Upstream `parentTarget` plus the provider ids.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub driver: ProviderDriverKind,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
}

/// Everything on a turn item except its variant body.
#[derive(Debug, Clone)]
pub(crate) struct ItemFrame {
    pub id: TurnItemId,
    pub node_id: NodeId,
    pub ordinal: u64,
    pub status: OrchestrationV2TurnItemStatus,
    pub title: Option<String>,
    pub started_at: UtcDateTime,
    pub completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
    pub native_ref: OrchestrationV2ProviderRef,
}

/// Variant-specific fields. Each body maps to one `OrchestrationV2TurnItem` variant.
#[derive(Debug, Clone)]
pub(crate) enum ItemBody {
    Assistant {
        message_id: MessageId,
        text: String,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
    },
    ProposedPlan {
        plan_id: PlanId,
        markdown: String,
        streaming: bool,
    },
    TodoList {
        plan_id: PlanId,
        steps: Vec<OrchestrationV2PlanStep>,
    },
    Command {
        input: String,
        output: Option<String>,
        exit_code: Option<i64>,
    },
    FileChange {
        file_name: TrimmedNonEmptyString,
        diff_str: Option<String>,
        old_str: Option<String>,
        new_str: Option<String>,
    },
    FileSearch {
        pattern: Option<String>,
        results: Option<Vec<OrchestrationV2FileSearchResult>>,
    },
    DynamicTool {
        tool_name: Option<TrimmedNonEmptyString>,
        input: Value,
        output: Option<Value>,
    },
    ApprovalRequest {
        request_id: RuntimeRequestId,
        request_kind: ProviderRequestKind,
        prompt: Option<String>,
        options: Vec<ProviderApprovalOption>,
    },
    UserInputRequest {
        request_id: RuntimeRequestId,
        questions: Vec<OrchestrationV2UserInputQuestion>,
    },
}

pub(crate) fn turn_item(
    scope: &Scope,
    frame: &ItemFrame,
    body: ItemBody,
) -> OrchestrationV2TurnItem {
    let id = frame.id.clone();
    let thread_id = scope.thread_id.clone();
    let run_id = Some(scope.run_id.clone());
    let node_id = Some(frame.node_id.clone());
    let provider_thread_id = Some(scope.provider_thread_id.clone());
    let provider_turn_id = Some(scope.provider_turn_id.clone());
    let native_item_ref = Some(frame.native_ref.clone());
    let ordinal = frame.ordinal;
    let status = frame.status;
    let title = frame.title.clone();
    let started_at = Some(frame.started_at);
    let completed_at = frame.completed_at;
    let updated_at = frame.updated_at;
    match body {
        ItemBody::Assistant {
            message_id,
            text,
            streaming,
        } => OrchestrationV2TurnItem::AssistantMessage {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            message_id,
            text,
            attachments: None,
            streaming,
        },
        ItemBody::Reasoning { text, streaming } => OrchestrationV2TurnItem::Reasoning {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            text,
            streaming,
        },
        ItemBody::ProposedPlan {
            plan_id,
            markdown,
            streaming,
        } => OrchestrationV2TurnItem::ProposedPlan {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            plan_id,
            markdown,
            streaming,
        },
        ItemBody::TodoList { plan_id, steps } => OrchestrationV2TurnItem::TodoList {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            plan_id,
            steps,
            explanation: None,
        },
        ItemBody::Command {
            input,
            output,
            exit_code,
        } => OrchestrationV2TurnItem::CommandExecution {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            input,
            output,
            output_omitted: None,
            output_indicates_failure: None,
            exit_code,
        },
        ItemBody::FileChange {
            file_name,
            diff_str,
            old_str,
            new_str,
        } => OrchestrationV2TurnItem::FileChange {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            file_name,
            additions: None,
            deletions: None,
            diff_str,
            old_str,
            new_str,
            changes: None,
        },
        ItemBody::FileSearch { pattern, results } => OrchestrationV2TurnItem::FileSearch {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            pattern,
            results,
        },
        ItemBody::DynamicTool {
            tool_name,
            input,
            output,
        } => OrchestrationV2TurnItem::DynamicTool {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            tool_name,
            viewed_image_path: None,
            input,
            output,
            output_omitted: None,
        },
        ItemBody::ApprovalRequest {
            request_id,
            request_kind,
            prompt,
            options,
        } => OrchestrationV2TurnItem::ApprovalRequest {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            request_id,
            request_kind,
            prompt,
            app_name: None,
            options: Some(options),
        },
        ItemBody::UserInputRequest {
            request_id,
            questions,
        } => OrchestrationV2TurnItem::UserInputRequest {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at,
            completed_at,
            updated_at,
            request_id,
            questions,
            question_answer: None,
            response_mode: None,
        },
    }
}

/// Execution-node fields that differ per item. Upstream sets `countsForRun: false`
/// on every item node and parents them on the turn root.
#[derive(Debug, Clone)]
pub(crate) struct NodeSpec {
    pub node_id: NodeId,
    pub kind: OrchestrationV2ExecutionNodeKind,
    pub status: OrchestrationV2ExecutionNodeStatus,
    pub started_at: UtcDateTime,
    pub completed_at: Option<UtcDateTime>,
    pub native_ref: Option<OrchestrationV2ProviderRef>,
    pub runtime_request_id: Option<RuntimeRequestId>,
}

pub(crate) fn execution_node(scope: &Scope, spec: NodeSpec) -> OrchestrationV2ExecutionNode {
    OrchestrationV2ExecutionNode {
        id: spec.node_id,
        thread_id: scope.thread_id.clone(),
        run_id: Some(scope.run_id.clone()),
        parent_node_id: Some(scope.root_node_id.clone()),
        root_node_id: scope.root_node_id.clone(),
        kind: spec.kind,
        status: spec.status,
        counts_for_run: false,
        provider_thread_id: Some(scope.provider_thread_id.clone()),
        provider_turn_id: Some(scope.provider_turn_id.clone()),
        native_item_ref: spec.native_ref,
        runtime_request_id: spec.runtime_request_id,
        checkpoint_scope_id: None,
        started_at: Some(spec.started_at),
        completed_at: spec.completed_at,
    }
}

/// Maps an item status onto its execution-node status. Upstream keeps the two in lockstep.
pub(crate) fn node_status(
    status: OrchestrationV2TurnItemStatus,
) -> OrchestrationV2ExecutionNodeStatus {
    use OrchestrationV2ExecutionNodeStatus as Node;
    use OrchestrationV2TurnItemStatus as Item;
    match status {
        Item::Idle => Node::Idle,
        Item::Pending => Node::Pending,
        Item::Running => Node::Running,
        Item::Waiting => Node::Waiting,
        Item::Completed => Node::Completed,
        Item::Failed => Node::Failed,
        Item::Cancelled => Node::Cancelled,
        Item::Interrupted => Node::Interrupted,
    }
}

pub(crate) fn native_ref(
    driver: &ProviderDriverKind,
    native: &str,
    strong: bool,
) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: driver.clone(),
        native_id: TrimmedNonEmptyString::new(native).ok(),
        strength: if strong {
            OrchestrationV2NativeRefStrength::Strong
        } else {
            OrchestrationV2NativeRefStrength::Weak
        },
        fingerprint: None,
        ordinal: None,
    }
}

/// Stable plan artifact id for a native plan or todo list.
pub(crate) fn plan_id(driver: &ProviderDriverKind, native: &str) -> PlanId {
    PlanId::new(format!("plan:{}:{native}", driver.as_str())).expect("plan id")
}

/// Steps from an ACP `plan` entry list. Entries are already mapped to status words.
pub(crate) fn plan_steps_from_acp(steps: &[PlanStep]) -> Vec<OrchestrationV2PlanStep> {
    steps
        .iter()
        .filter_map(|step| {
            let status = match step.status.as_str() {
                "completed" => OrchestrationV2PlanStepStatus::Completed,
                "inProgress" => OrchestrationV2PlanStepStatus::Running,
                _ => OrchestrationV2PlanStepStatus::Pending,
            };
            TrimmedNonEmptyString::new(step.text.trim())
                .ok()
                .map(|text| (text, status))
        })
        .enumerate()
        .map(|(index, (text, status))| extensions::plan_step(index, text, status))
        .collect()
}

/// Whether an ACP tool call is still running, or finished with a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    Running,
    Completed,
    Failed,
}

impl ToolStatus {
    pub(crate) fn from_acp(status: &str) -> Self {
        match status {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Running,
        }
    }

    pub(crate) fn item_status(self) -> OrchestrationV2TurnItemStatus {
        match self {
            Self::Running => OrchestrationV2TurnItemStatus::Running,
            Self::Completed => OrchestrationV2TurnItemStatus::Completed,
            Self::Failed => OrchestrationV2TurnItemStatus::Failed,
        }
    }
}

/// Merges a `tool_call_update` into the call's last payload. ACP sends only the
/// fields that changed, so a later field overwrites and absent fields stay.
pub(crate) fn merge_tool_update(previous: Option<&Value>, update: &Value) -> Value {
    let mut merged = previous.cloned().unwrap_or_else(|| json!({}));
    if let (Some(target), Some(fields)) = (merged.as_object_mut(), update.as_object()) {
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
    merged
}

/// Upstream `cursorToolOutputText`: text blocks of the tool's content, then the
/// raw output when it is a plain string or carries one.
pub(crate) fn tool_output_text(raw: &Value) -> String {
    let content: String = raw
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("content").and_then(|content| content.get("text")))
        .filter_map(Value::as_str)
        .collect();
    if !content.is_empty() {
        return content;
    }
    match raw.get("rawOutput") {
        Some(Value::String(text)) => text.clone(),
        Some(output) => ["output", "stdout", "text"]
            .iter()
            .find_map(|key| output.get(*key).and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned(),
        None => String::new(),
    }
}

/// Builds the variant body for a tool call from its merged payload. Upstream
/// `emitToolArtifacts` picks the item type by tool kind; ACP carries that kind
/// as `kind` (`execute`, `edit`, `delete`, `move`, `read`, `search`, ...).
pub(crate) fn tool_body(raw: &Value, status: ToolStatus) -> ItemBody {
    let kind = raw.get("kind").and_then(Value::as_str).unwrap_or("");
    let title = str_at(raw, "title");
    let input = raw.get("rawInput");
    let output_text = tool_output_text(raw);
    let failed = status == ToolStatus::Failed;
    let output_value = raw
        .get("rawOutput")
        .cloned()
        .or_else(|| (!output_text.is_empty()).then(|| Value::String(output_text.clone())));
    match kind {
        "execute" => ItemBody::Command {
            input: ["command", "cmd"]
                .iter()
                .find_map(|key| input.and_then(|input| str_at(input, key)))
                .or(title)
                .unwrap_or_default()
                .to_owned(),
            output: (!output_text.is_empty()).then(|| output_text.clone()),
            exit_code: raw
                .get("rawOutput")
                .and_then(|output| output.get("exitCode"))
                .and_then(Value::as_i64),
        },
        "edit" | "delete" | "move" => {
            let diff = raw
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|part| part.get("type").and_then(Value::as_str) == Some("diff"));
            let path = raw
                .get("locations")
                .and_then(|locations| locations.get(0))
                .and_then(|location| str_at(location, "path"))
                .or_else(|| diff.and_then(|diff| str_at(diff, "path")))
                .or_else(|| input.and_then(|input| str_at(input, "path")))
                .or(title)
                .unwrap_or("file");
            ItemBody::FileChange {
                file_name: trimmed(path),
                // A failed change keeps its error where the diff would be.
                diff_str: (failed && !output_text.is_empty()).then(|| output_text.clone()),
                old_str: diff
                    .and_then(|diff| str_at(diff, "oldText"))
                    .map(str::to_owned),
                new_str: diff
                    .and_then(|diff| str_at(diff, "newText"))
                    .map(str::to_owned),
            }
        }
        "read" => ItemBody::DynamicTool {
            tool_name: Some(trimmed("Read")),
            input: input.cloned().unwrap_or_else(|| json!({})),
            output: output_value,
        },
        "search" => {
            let pattern = input.and_then(|input| {
                ["pattern", "query", "glob", "globPattern", "path"]
                    .iter()
                    .find_map(|key| str_at(input, key))
            });
            // A failed search keeps its error as one row under the searched path.
            let search_path = input.and_then(|input| {
                ["path", "targetDirectory"]
                    .iter()
                    .find_map(|key| str_at(input, key))
            });
            let results = if failed && !output_text.is_empty() {
                Some(vec![file_search_result(
                    search_path.unwrap_or("."),
                    Some(output_text),
                )])
            } else {
                raw.get("rawOutput")
                    .and_then(|output| output.get("files"))
                    .and_then(Value::as_array)
                    .map(|files| {
                        files
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|file| file_search_result(file, None))
                            .collect()
                    })
            };
            ItemBody::FileSearch {
                pattern: pattern.map(str::to_owned),
                results,
            }
        }
        _ => ItemBody::DynamicTool {
            tool_name: title
                .or(Some(kind).filter(|kind| !kind.is_empty()))
                .map(trimmed),
            input: input.cloned().unwrap_or(Value::Null),
            output: output_value,
        },
    }
}

fn file_search_result(file: &str, preview: Option<String>) -> OrchestrationV2FileSearchResult {
    OrchestrationV2FileSearchResult {
        file_name: trimmed(file),
        line: None,
        column: None,
        preview,
    }
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// Callers pass non-blank literals or tool-supplied text; the fallback keeps the
/// row addressable when a tool sends none.
fn trimmed(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).unwrap_or_else(|_| trimmed_fallback())
}

fn trimmed_fallback() -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new("file").expect("fallback")
}

/// A request card's item and node, kept until the request resolves.
#[derive(Debug, Clone)]
pub(crate) struct CardItem {
    pub scope: Scope,
    pub node: NodeSpec,
    pub frame: ItemFrame,
    pub body: ItemBody,
}

/// Upstream `ActiveCursorTurn`: stream state for one prompt. Every method returns
/// the provider events it produced, so the caller decides when to send them.
#[derive(Debug)]
pub(crate) struct TurnStream {
    scope: Scope,
    provider_turn_ordinal: u64,
    started_at: UtcDateTime,
    item_ordinals: HashMap<String, u64>,
    segments: u64,
    assistant: Option<TextSegment>,
    reasoning: Option<TextSegment>,
    tools: HashMap<String, ToolState>,
    plans: HashMap<String, PlanState>,
    pub(crate) interrupted: bool,
}

#[derive(Debug, Clone)]
struct TextSegment {
    native_id: String,
    message_id: Option<String>,
    started_at: UtcDateTime,
    text: String,
}

#[derive(Debug, Clone)]
struct ToolState {
    raw: Value,
    started_at: UtcDateTime,
}

#[derive(Debug, Clone)]
struct PlanState {
    steps: Vec<OrchestrationV2PlanStep>,
    started_at: UtcDateTime,
}

#[derive(Clone, Copy)]
pub(crate) enum TextKind {
    Assistant,
    Reasoning,
}

pub(crate) type Events = Vec<ProviderAdapterV2Event>;

impl TurnStream {
    pub(crate) fn new(scope: Scope, provider_turn_ordinal: u64, started_at: UtcDateTime) -> Self {
        Self {
            scope,
            provider_turn_ordinal,
            started_at,
            item_ordinals: HashMap::new(),
            segments: 0,
            assistant: None,
            reasoning: None,
            tools: HashMap::new(),
            plans: HashMap::new(),
            interrupted: false,
        }
    }

    /// The turn's ordinal for a native item, allocated on first use.
    pub(crate) fn item_ordinal(&mut self, native: &str) -> u64 {
        self.ordinal(native)
    }

    pub(crate) fn scope(&self) -> &Scope {
        &self.scope
    }

    pub(crate) fn started_at(&self) -> UtcDateTime {
        self.started_at
    }

    /// Upstream `resolveItemOrdinal`: the turn's ordinal block, then position.
    fn ordinal(&mut self, native: &str) -> u64 {
        if let Some(existing) = self.item_ordinals.get(native) {
            return *existing;
        }
        let ordinal = self.provider_turn_ordinal * 100 + self.item_ordinals.len() as u64 + 1;
        self.item_ordinals.insert(native.to_owned(), ordinal);
        ordinal
    }

    fn frame(
        &mut self,
        native: &str,
        status: OrchestrationV2TurnItemStatus,
        started_at: UtcDateTime,
        completed_at: Option<UtcDateTime>,
        strong: bool,
        now: UtcDateTime,
    ) -> ItemFrame {
        let driver = self.scope.driver.clone();
        ItemFrame {
            id: derive::turn_item_from_provider_item(&driver, native),
            node_id: derive::node_from_provider_item(&driver, native),
            ordinal: self.ordinal(native),
            status,
            title: None,
            started_at,
            completed_at,
            updated_at: now,
            native_ref: native_ref(&driver, native, strong),
        }
    }

    fn node_event(&self, spec: NodeSpec) -> ProviderAdapterV2Event {
        ProviderAdapterV2Event::NodeUpdated {
            driver: self.scope.driver.clone(),
            node: execution_node(&self.scope, spec),
        }
    }

    fn item_event(&self, item: OrchestrationV2TurnItem) -> ProviderAdapterV2Event {
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.scope.driver.clone(),
            turn_item: item,
        }
    }

    /// Upstream `appendTextSegment` and `emitAssistant`/`emitReasoning`. Text
    /// streams into one segment until a boundary: a tool, a plan, a thought
    /// switching to text, a new ACP `messageId`, or the turn ending.
    pub(crate) fn push_text(
        &mut self,
        kind: TextKind,
        text: &str,
        message_id: Option<&str>,
        now: UtcDateTime,
    ) -> Events {
        let mut out = Vec::new();
        match kind {
            TextKind::Assistant => {
                self.complete_text(TextKind::Reasoning, now, &mut out);
                let changed_message = match (self.assistant.as_ref(), message_id) {
                    (Some(current), Some(incoming)) => current
                        .message_id
                        .as_deref()
                        .is_some_and(|id| id != incoming),
                    _ => false,
                };
                if changed_message {
                    self.complete_text(TextKind::Assistant, now, &mut out);
                }
            }
            TextKind::Reasoning => self.complete_text(TextKind::Assistant, now, &mut out),
        }
        let slot = match kind {
            TextKind::Assistant => &mut self.assistant,
            TextKind::Reasoning => &mut self.reasoning,
        };
        if slot.is_none() {
            self.segments += 1;
            let prefix = match kind {
                TextKind::Assistant => "assistant",
                TextKind::Reasoning => "reasoning",
            };
            let native_id = format!("{prefix}:{}:{}", self.scope.run_id.as_str(), self.segments);
            *slot = Some(TextSegment {
                native_id,
                message_id: message_id.map(str::to_owned),
                started_at: now,
                text: String::new(),
            });
        }
        if let Some(segment) = slot.as_mut() {
            segment.text.push_str(text);
        }
        self.flush_text(kind, false, now, &mut out);
        out
    }

    /// Upstream `completeAssistant` / `completeReasoning`.
    fn complete_text(&mut self, kind: TextKind, now: UtcDateTime, out: &mut Events) {
        let open = match kind {
            TextKind::Assistant => self.assistant.is_some(),
            TextKind::Reasoning => self.reasoning.is_some(),
        };
        if open {
            self.flush_text(kind, true, now, out);
        }
    }

    fn flush_text(&mut self, kind: TextKind, completed: bool, now: UtcDateTime, out: &mut Events) {
        let slot = match kind {
            TextKind::Assistant => self.assistant.clone(),
            TextKind::Reasoning => self.reasoning.clone(),
        };
        if let Some(segment) = slot.filter(|segment| !segment.text.is_empty()) {
            let frame = self.frame(
                &segment.native_id,
                if completed {
                    OrchestrationV2TurnItemStatus::Completed
                } else {
                    OrchestrationV2TurnItemStatus::Running
                },
                segment.started_at,
                completed.then_some(now),
                false,
                now,
            );
            let node_id = frame.node_id.clone();
            let kind_node = match kind {
                TextKind::Assistant => OrchestrationV2ExecutionNodeKind::AssistantMessage,
                TextKind::Reasoning => OrchestrationV2ExecutionNodeKind::Reasoning,
            };
            out.push(self.node_event(NodeSpec {
                node_id: node_id.clone(),
                kind: kind_node,
                status: node_status(frame.status),
                started_at: frame.started_at,
                completed_at: frame.completed_at,
                native_ref: Some(frame.native_ref.clone()),
                runtime_request_id: None,
            }));
            let body = match kind {
                TextKind::Assistant => {
                    let message_id =
                        derive::message_from_provider_item(&self.scope.driver, &segment.native_id);
                    out.push(ProviderAdapterV2Event::MessageUpdated {
                        driver: self.scope.driver.clone(),
                        message: OrchestrationV2ConversationMessage {
                            notification: None,
                            created_by: OrchestrationV2Actor::Agent,
                            creation_source: OrchestrationV2CreationSource::Provider,
                            scheduled_task_id: None,
                            sender_thread_id: None,
                            id: message_id.clone(),
                            thread_id: self.scope.thread_id.clone(),
                            run_id: Some(self.scope.run_id.clone()),
                            node_id: Some(node_id),
                            role: OrchestrationV2ConversationMessageRole::Assistant,
                            text: segment.text.clone(),
                            context: None,
                            attachments: Vec::new(),
                            streaming: !completed,
                            created_at: segment.started_at,
                            updated_at: frame.updated_at,
                            delegated_completion: None,
                        },
                    });
                    ItemBody::Assistant {
                        message_id,
                        text: segment.text,
                        streaming: !completed,
                    }
                }
                TextKind::Reasoning => ItemBody::Reasoning {
                    text: segment.text,
                    streaming: !completed,
                },
            };
            let item = turn_item(&self.scope, &frame, body);
            out.push(self.item_event(item));
        }
        if completed {
            match kind {
                TextKind::Assistant => self.assistant = None,
                TextKind::Reasoning => self.reasoning = None,
            }
        }
    }

    /// A `tool_call` or `tool_call_update`. Upstream `ensureToolStarted` plus
    /// `emitToolArtifacts`: the node and item carry the merged tool payload, so
    /// streamed output reaches the card as it arrives.
    pub(crate) fn tool_update(
        &mut self,
        call_id: &str,
        status: ToolStatus,
        update: &Value,
        now: UtcDateTime,
    ) -> Events {
        let mut out = Vec::new();
        self.complete_text(TextKind::Reasoning, now, &mut out);
        self.complete_text(TextKind::Assistant, now, &mut out);
        let previous = self.tools.get(call_id).cloned();
        let raw = merge_tool_update(previous.as_ref().map(|tool| &tool.raw), update);
        let started_at = previous.as_ref().map_or(now, |tool| tool.started_at);
        let state = ToolState { raw, started_at };
        let completed = status != ToolStatus::Running;
        let item_status = status.item_status();
        out.extend(self.tool_events(call_id, &state, item_status, completed.then_some(now), now));
        if completed {
            self.tools.remove(call_id);
        } else {
            self.tools.insert(call_id.to_owned(), state);
        }
        out
    }

    fn tool_events(
        &mut self,
        call_id: &str,
        state: &ToolState,
        status: OrchestrationV2TurnItemStatus,
        completed_at: Option<UtcDateTime>,
        now: UtcDateTime,
    ) -> Events {
        let mut frame = self.frame(call_id, status, state.started_at, completed_at, true, now);
        frame.title = str_at(&state.raw, "title").map(str::to_owned);
        let body_status = match status {
            OrchestrationV2TurnItemStatus::Running => ToolStatus::Running,
            OrchestrationV2TurnItemStatus::Completed => ToolStatus::Completed,
            _ => ToolStatus::Failed,
        };
        let body = tool_body(&state.raw, body_status);
        let node = self.node_event(NodeSpec {
            node_id: frame.node_id.clone(),
            kind: OrchestrationV2ExecutionNodeKind::ToolCall,
            status: node_status(status),
            started_at: frame.started_at,
            completed_at,
            native_ref: Some(frame.native_ref.clone()),
            runtime_request_id: None,
        });
        let item = self.item_event(turn_item(&self.scope, &frame, body));
        vec![node, item]
    }

    /// A todo list or ACP plan. Upstream `emitTodoArtifacts`: a `todo_list` node,
    /// the `plan.updated` artifact, and the item carry the same steps.
    pub(crate) fn plan_update(
        &mut self,
        native: &str,
        steps: Vec<OrchestrationV2PlanStep>,
        status: OrchestrationV2TurnItemStatus,
        now: UtcDateTime,
    ) -> Events {
        let mut out = Vec::new();
        self.complete_text(TextKind::Reasoning, now, &mut out);
        self.complete_text(TextKind::Assistant, now, &mut out);
        let started_at = self.plans.get(native).map_or(now, |plan| plan.started_at);
        let completed_at = (status == OrchestrationV2TurnItemStatus::Completed).then_some(now);
        let frame = self.frame(native, status, started_at, completed_at, false, now);
        let plan_id = plan_id(&self.scope.driver, native);
        out.push(self.node_event(NodeSpec {
            node_id: frame.node_id.clone(),
            kind: OrchestrationV2ExecutionNodeKind::TodoList,
            status: node_status(status),
            started_at,
            completed_at,
            native_ref: Some(frame.native_ref.clone()),
            runtime_request_id: None,
        }));
        out.push(ProviderAdapterV2Event::PlanUpdated {
            driver: self.scope.driver.clone(),
            plan: OrchestrationV2PlanArtifact::TodoList {
                id: plan_id.clone(),
                thread_id: self.scope.thread_id.clone(),
                run_id: Some(self.scope.run_id.clone()),
                node_id: frame.node_id.clone(),
                status: plan_artifact_status(status),
                detail_in_turn_item: None,
                steps: steps.clone(),
                explanation: None,
            },
        });
        out.push(self.item_event(turn_item(
            &self.scope,
            &frame,
            ItemBody::TodoList {
                plan_id,
                steps: steps.clone(),
            },
        )));
        if status == OrchestrationV2TurnItemStatus::Running {
            self.plans
                .insert(native.to_owned(), PlanState { steps, started_at });
        } else {
            self.plans.remove(native);
        }
        out
    }

    /// Builds the node and item for a request card. The card is emitted now and
    /// again when the request resolves.
    pub(crate) fn request_card(
        &mut self,
        request_id: &RuntimeRequestId,
        kind: OrchestrationV2ExecutionNodeKind,
        body: ItemBody,
        title: Option<String>,
        now: UtcDateTime,
    ) -> CardItem {
        let node_id = derive::approval_node(request_id);
        let frame = ItemFrame {
            id: derive::approval_turn_item(request_id),
            node_id: node_id.clone(),
            ordinal: self.ordinal(request_id.as_str()),
            status: OrchestrationV2TurnItemStatus::Waiting,
            title,
            started_at: now,
            completed_at: None,
            updated_at: now,
            native_ref: native_ref(&self.scope.driver, request_id.as_str(), false),
        };
        CardItem {
            scope: self.scope.clone(),
            node: NodeSpec {
                node_id,
                kind,
                status: OrchestrationV2ExecutionNodeStatus::Waiting,
                started_at: now,
                completed_at: None,
                native_ref: Some(frame.native_ref.clone()),
                runtime_request_id: Some(request_id.clone()),
            },
            frame,
            body,
        }
    }

    /// Closes everything the turn left open: tools and text first, as upstream
    /// `finalizeTurn` does. Unfinished work takes the turn's terminal status.
    pub(crate) fn finish(
        &mut self,
        status: OrchestrationV2ProviderTurnStatus,
        now: UtcDateTime,
    ) -> Events {
        let unfinished = unfinished_item_status(status);
        let mut out = Vec::new();
        let mut tools: Vec<(String, ToolState)> = self.tools.drain().collect();
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        for (call_id, state) in tools {
            let item_status = if status == OrchestrationV2ProviderTurnStatus::Completed {
                OrchestrationV2TurnItemStatus::Completed
            } else {
                unfinished
            };
            out.extend(self.tool_events(&call_id, &state, item_status, Some(now), now));
        }
        self.complete_text(TextKind::Reasoning, now, &mut out);
        self.complete_text(TextKind::Assistant, now, &mut out);
        let mut plans: Vec<(String, PlanState)> = self.plans.drain().collect();
        plans.sort_by(|a, b| a.0.cmp(&b.0));
        for (native, state) in plans {
            let item_status = if status == OrchestrationV2ProviderTurnStatus::Completed {
                OrchestrationV2TurnItemStatus::Completed
            } else {
                unfinished
            };
            out.extend(self.plan_update(&native, state.steps, item_status, now));
        }
        out
    }

    /// The `proposed_plan` artifact for a plan card, drafted until the user answers.
    pub(crate) fn proposed_plan(
        &self,
        plan_id: &PlanId,
        request_id: &RuntimeRequestId,
        markdown: &str,
        _now: UtcDateTime,
    ) -> Events {
        vec![ProviderAdapterV2Event::PlanUpdated {
            driver: self.scope.driver.clone(),
            plan: OrchestrationV2PlanArtifact::ProposedPlan {
                id: plan_id.clone(),
                thread_id: self.scope.thread_id.clone(),
                run_id: Some(self.scope.run_id.clone()),
                node_id: derive::approval_node(request_id),
                status: OrchestrationV2PlanArtifactStatus::Draft,
                detail_in_turn_item: None,
                markdown: markdown.to_owned(),
            },
        }]
    }

    /// Fills in the card's resolved status and emits its node and item.
    pub(crate) fn card_events(
        driver: &ProviderDriverKind,
        card: &CardItem,
        status: OrchestrationV2TurnItemStatus,
        now: UtcDateTime,
    ) -> Events {
        let terminal = status != OrchestrationV2TurnItemStatus::Waiting;
        let mut frame = card.frame.clone();
        frame.status = status;
        frame.updated_at = now;
        frame.completed_at = terminal.then_some(now);
        let mut node = card.node.clone();
        node.status = node_status(status);
        node.completed_at = frame.completed_at;
        vec![
            ProviderAdapterV2Event::NodeUpdated {
                driver: driver.clone(),
                node: execution_node(&card.scope, node),
            },
            ProviderAdapterV2Event::TurnItemUpdated {
                driver: driver.clone(),
                turn_item: turn_item(&card.scope, &frame, card.body.clone()),
            },
        ]
    }
}

fn unfinished_item_status(
    status: OrchestrationV2ProviderTurnStatus,
) -> OrchestrationV2TurnItemStatus {
    match status {
        OrchestrationV2ProviderTurnStatus::Completed => OrchestrationV2TurnItemStatus::Completed,
        OrchestrationV2ProviderTurnStatus::Interrupted => {
            OrchestrationV2TurnItemStatus::Interrupted
        }
        OrchestrationV2ProviderTurnStatus::Cancelled => OrchestrationV2TurnItemStatus::Cancelled,
        OrchestrationV2ProviderTurnStatus::Failed
        | OrchestrationV2ProviderTurnStatus::Pending
        | OrchestrationV2ProviderTurnStatus::Running => OrchestrationV2TurnItemStatus::Failed,
    }
}

fn plan_artifact_status(
    status: OrchestrationV2TurnItemStatus,
) -> OrchestrationV2PlanArtifactStatus {
    // Upstream: a finished list is `active`, an open one is `draft`.
    if status == OrchestrationV2TurnItemStatus::Completed {
        OrchestrationV2PlanArtifactStatus::Active
    } else {
        OrchestrationV2PlanArtifactStatus::Draft
    }
}

/// Projects one ACP update into the open turn. Mode, usage, command and config
/// updates have no turn item, so they produce nothing.
pub(crate) fn project(stream: &mut TurnStream, update: &CursorUpdate, now: UtcDateTime) -> Events {
    match update {
        CursorUpdate::Text { text, message_id } => {
            stream.push_text(TextKind::Assistant, text, message_id.as_deref(), now)
        }
        CursorUpdate::Thought { text } => stream.push_text(TextKind::Reasoning, text, None, now),
        CursorUpdate::Tool {
            tool_call_id,
            status,
            raw,
            ..
        } => stream.tool_update(tool_call_id, ToolStatus::from_acp(status), raw, now),
        CursorUpdate::Plan { steps, .. } => {
            let native = format!("plan:{}", stream.scope().run_id.as_str());
            stream.plan_update(
                &native,
                plan_steps_from_acp(steps),
                OrchestrationV2TurnItemStatus::Running,
                now,
            )
        }
        CursorUpdate::Commands { .. }
        | CursorUpdate::Config { .. }
        | CursorUpdate::Mode { .. }
        | CursorUpdate::Usage { .. }
        | CursorUpdate::Unknown { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_text_tools_plans_and_keeps_unknown_variants() {
        let text = parse_session_update(&json!({
            "sessionId": "s",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hi"}}
        }));
        assert!(matches!(text[0], CursorUpdate::Text { ref text, .. } if text == "Hi"));
        let tool = parse_session_update(&json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "t1",
                "title": "Read",
                "kind": "read",
                "status": "completed"
            }
        }));
        match &tool[0] {
            CursorUpdate::Tool {
                tool_call_id,
                status,
                ..
            } => {
                assert_eq!(tool_call_id, "t1");
                assert_eq!(status, "completed");
            }
            other => panic!("{other:?}"),
        }
        let plan = parse_session_update(&json!({
            "update": {
                "sessionUpdate": "plan",
                "entries": [{"content": "Inspect", "status": "in_progress", "priority": "high"}]
            }
        }));
        match &plan[0] {
            CursorUpdate::Plan { steps, raw } => {
                assert_eq!(steps[0].status, "inProgress");
                assert!(raw.get("update").unwrap().get("entries").is_some());
            }
            other => panic!("{other:?}"),
        }
        let unknown = parse_session_update(&json!({
            "update": {"sessionUpdate": "future_variant", "extra": true}
        }));
        assert!(matches!(
            &unknown[0],
            CursorUpdate::Unknown { session_update, .. } if session_update == "future_variant"
        ));
    }
}
