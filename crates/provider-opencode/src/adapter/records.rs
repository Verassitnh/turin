//! Node, message and plan records that upstream's OpenCode adapters emit beside each
//! turn item: `node.updated` (`emitTextPart`, `emitToolPart`, `emitTodo` in
//! `server/adapter.ts`), `message.updated` for assistant text, and `plan.updated` for a
//! todo list. Both generations build them here, so the field mapping lives in one place.

use turin_contracts::orchestration_v2::{
    OrchestrationV2Subagent, OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus,
};
use turin_contracts::{
    NodeId, PlanId, ProviderThreadId, ProviderTurnId, RuntimeRequestId, ThreadId,
    TrimmedNonEmptyString, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2PlanArtifact,
        OrchestrationV2PlanArtifactStatus, OrchestrationV2PlanStep, OrchestrationV2PlanStepStatus,
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus, OrchestrationV2UserInputQuestion,
        OrchestrationV2UserInputQuestionOption,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::ProviderRequestKind,
};
use turin_provider_core::id_allocator::derive;

use serde_json::Value;

use crate::adapter::logic::native_ref;

/// `dateTimeFromEpoch`: a provider timestamp in epoch milliseconds, or the fallback.
pub fn epoch_ms_or(value: Option<i64>, fallback: UtcDateTime) -> UtcDateTime {
    value
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(UtcDateTime::from_datetime)
        .unwrap_or(fallback)
}

/// The identity a node is built from: the item it stands for and the turn it belongs to.
pub struct NodeOf<'a> {
    pub thread_id: &'a ThreadId,
    pub root_node_id: &'a NodeId,
    pub provider_thread_id: &'a ProviderThreadId,
    pub provider_turn_id: &'a ProviderTurnId,
    /// The native item id the node id derives from.
    pub native_item_id: &'a str,
    /// A weak ref when the id is synthesised (a todo list has no native part id).
    pub weak_ref: bool,
}

pub struct NodeTiming {
    pub kind: OrchestrationV2ExecutionNodeKind,
    pub status: OrchestrationV2ExecutionNodeStatus,
    pub started_at: UtcDateTime,
    pub completed_at: Option<UtcDateTime>,
}

/// `node.updated` for one item. `countsForRun` is false for every OpenCode item.
pub fn execution_node(
    driver: &ProviderDriverKind,
    of: &NodeOf<'_>,
    timing: NodeTiming,
) -> OrchestrationV2ExecutionNode {
    OrchestrationV2ExecutionNode {
        id: derive::node_from_provider_item(driver, of.native_item_id),
        thread_id: of.thread_id.clone(),
        run_id: None,
        parent_node_id: Some(of.root_node_id.clone()),
        root_node_id: of.root_node_id.clone(),
        kind: timing.kind,
        status: timing.status,
        counts_for_run: false,
        provider_thread_id: Some(of.provider_thread_id.clone()),
        provider_turn_id: Some(of.provider_turn_id.clone()),
        native_item_ref: Some(native_ref(of.native_item_id, of.weak_ref)),
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: Some(timing.started_at),
        completed_at: timing.completed_at,
    }
}

/// The assistant message a text part projects as (`emitTextPart`, text parts only).
pub fn assistant_message(
    driver: &ProviderDriverKind,
    of: &NodeOf<'_>,
    node_id: &NodeId,
    text: &str,
    streaming: bool,
    created_at: UtcDateTime,
    updated_at: UtcDateTime,
) -> OrchestrationV2ConversationMessage {
    OrchestrationV2ConversationMessage {
        notification: None,
        created_by: OrchestrationV2Actor::Agent,
        creation_source: OrchestrationV2CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: None,
        id: derive::message_from_provider_item(driver, of.native_item_id),
        thread_id: of.thread_id.clone(),
        run_id: None,
        node_id: Some(node_id.clone()),
        role: OrchestrationV2ConversationMessageRole::Assistant,
        text: text.to_owned(),
        context: None,
        attachments: Vec::new(),
        streaming,
        created_at,
        updated_at,
        delegated_completion: None,
    }
}

/// `plan.updated` for a todo list, `kind: "todo_list"`. The status maps from the
/// `completed` flag the way `emitTodo` does.
pub fn todo_plan(
    plan_id: &PlanId,
    thread_id: &ThreadId,
    node_id: &NodeId,
    completed: bool,
    steps: Vec<OrchestrationV2PlanStep>,
) -> OrchestrationV2PlanArtifact {
    OrchestrationV2PlanArtifact::TodoList {
        id: plan_id.clone(),
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: node_id.clone(),
        status: if completed {
            OrchestrationV2PlanArtifactStatus::Completed
        } else {
            OrchestrationV2PlanArtifactStatus::Active
        },
        detail_in_turn_item: None,
        steps,
        explanation: None,
    }
}

/// One todo step, as `emitTodo` builds it from a `todo.updated` entry.
pub fn todo_steps(
    native_item_id: &str,
    todos: &[(String, String)],
) -> Vec<OrchestrationV2PlanStep> {
    todos
        .iter()
        .enumerate()
        .map(|(index, (content, status))| {
            let trimmed = content.trim();
            let text = if trimmed.is_empty() {
                format!("Todo {}", index + 1)
            } else {
                trimmed.to_owned()
            };
            OrchestrationV2PlanStep {
                id: TrimmedNonEmptyString::new(format!("{native_item_id}:{}", index + 1))
                    .expect("todo step id"),
                text: TrimmedNonEmptyString::new(text).expect("todo step text"),
                status: match status.as_str() {
                    "completed" => OrchestrationV2PlanStepStatus::Completed,
                    "in_progress" => OrchestrationV2PlanStepStatus::Running,
                    _ => OrchestrationV2PlanStepStatus::Pending,
                },
                duration_anchor_at: None,
                duration_ms: None,
            }
        })
        .collect()
}

/// The todo list's native id. Upstream is `${sessionId}:todo:${providerTurnId}`.
pub fn todo_native_id(session_id: &str, provider_turn_id: &str) -> String {
    format!("{session_id}:todo:{provider_turn_id}")
}

/// `completed` for a todo list: at least one step and all of them done.
pub fn todos_complete(steps: &[OrchestrationV2PlanStep]) -> bool {
    !steps.is_empty()
        && steps
            .iter()
            .all(|step| step.status == OrchestrationV2PlanStepStatus::Completed)
}

/// One text or reasoning block's turn item, as `emitText` and `emitTextPart` build it.
pub struct TextBlock<'a> {
    pub reasoning: bool,
    pub text: &'a str,
    pub streaming: bool,
    pub status: OrchestrationV2TurnItemStatus,
    pub ordinal: u64,
    pub started_at: UtcDateTime,
    pub completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
}

pub fn text_turn_item(
    driver: &ProviderDriverKind,
    of: &NodeOf<'_>,
    node_id: &NodeId,
    block: TextBlock<'_>,
) -> OrchestrationV2TurnItem {
    let id = derive::turn_item_from_provider_item(driver, of.native_item_id);
    let native_item_ref = Some(native_ref(of.native_item_id, of.weak_ref));
    let provider_thread_id = Some(of.provider_thread_id.clone());
    let provider_turn_id = Some(of.provider_turn_id.clone());
    if block.reasoning {
        return OrchestrationV2TurnItem::Reasoning {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id: of.thread_id.clone(),
            run_id: None,
            node_id: Some(node_id.clone()),
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal: block.ordinal,
            status: block.status,
            title: None,
            started_at: Some(block.started_at),
            completed_at: block.completed_at,
            updated_at: block.updated_at,
            text: block.text.to_owned(),
            streaming: block.streaming,
        };
    }
    OrchestrationV2TurnItem::AssistantMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id,
        thread_id: of.thread_id.clone(),
        run_id: None,
        node_id: Some(node_id.clone()),
        provider_thread_id,
        provider_turn_id,
        native_item_ref,
        parent_item_id: None,
        ordinal: block.ordinal,
        status: block.status,
        title: None,
        started_at: Some(block.started_at),
        completed_at: block.completed_at,
        updated_at: block.updated_at,
        message_id: derive::message_from_provider_item(driver, of.native_item_id),
        text: block.text.to_owned(),
        attachments: None,
        streaming: block.streaming,
    }
}

/// `ordinalOf` / `itemOrdinal`: a native item keeps the ordinal it got on first sight.
pub fn next_ordinal(
    ordinals: &mut std::collections::HashMap<String, u64>,
    next: &mut u64,
    native_item_id: &str,
) -> u64 {
    if let Some(ordinal) = ordinals.get(native_item_id) {
        return *ordinal;
    }
    let ordinal = *next;
    *next += 1;
    ordinals.insert(native_item_id.to_owned(), ordinal);
    ordinal
}

/// The node status an item status settles into (`toolStatus`, `emitTool`).
pub fn node_status_of(status: OrchestrationV2TurnItemStatus) -> OrchestrationV2ExecutionNodeStatus {
    match status {
        OrchestrationV2TurnItemStatus::Pending => OrchestrationV2ExecutionNodeStatus::Pending,
        OrchestrationV2TurnItemStatus::Completed => OrchestrationV2ExecutionNodeStatus::Completed,
        OrchestrationV2TurnItemStatus::Failed => OrchestrationV2ExecutionNodeStatus::Failed,
        _ => OrchestrationV2ExecutionNodeStatus::Running,
    }
}

/// What a waiting runtime request asks: a permission, or structured questions.
pub enum RequestBody {
    Approval {
        request_kind: ProviderRequestKind,
        title: Option<String>,
    },
    UserInput {
        questions: Vec<OrchestrationV2UserInputQuestion>,
    },
}

/// `showRequest`'s node: the request is waiting at `approvalNode(requestId)`.
pub fn request_node(
    driver: &ProviderDriverKind,
    of: &NodeOf<'_>,
    request_id: &RuntimeRequestId,
    user_input: bool,
    now: UtcDateTime,
) -> OrchestrationV2ExecutionNode {
    let node = execution_node(
        driver,
        of,
        NodeTiming {
            kind: if user_input {
                OrchestrationV2ExecutionNodeKind::UserInputRequest
            } else {
                OrchestrationV2ExecutionNodeKind::ApprovalRequest
            },
            status: OrchestrationV2ExecutionNodeStatus::Waiting,
            started_at: now,
            completed_at: None,
        },
    );
    OrchestrationV2ExecutionNode {
        id: derive::approval_node(request_id),
        runtime_request_id: Some(request_id.clone()),
        ..node
    }
}

/// `showRequest`'s turn item, at `approvalTurnItem(requestId)` and waiting on `node_id`.
pub fn request_turn_item(
    of: &NodeOf<'_>,
    request_id: &RuntimeRequestId,
    ordinal: u64,
    now: UtcDateTime,
    body: RequestBody,
) -> OrchestrationV2TurnItem {
    let id = derive::approval_turn_item(request_id);
    let node_id = Some(derive::approval_node(request_id));
    let thread_id = of.thread_id.clone();
    let provider_thread_id = of.provider_thread_id.clone();
    let provider_turn_id = of.provider_turn_id.clone();
    let native_item_ref = native_ref(of.native_item_id, false);
    match body {
        RequestBody::Approval {
            request_kind,
            title,
        } => OrchestrationV2TurnItem::ApprovalRequest {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id: None,
            node_id,
            provider_thread_id: Some(provider_thread_id),
            provider_turn_id: Some(provider_turn_id),
            native_item_ref: Some(native_item_ref),
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Waiting,
            title,
            started_at: Some(now),
            completed_at: None,
            updated_at: now,
            request_id: request_id.clone(),
            request_kind,
            prompt: None,
            app_name: None,
            options: None,
        },
        RequestBody::UserInput { questions } => OrchestrationV2TurnItem::UserInputRequest {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id,
            run_id: None,
            node_id,
            provider_thread_id: Some(provider_thread_id),
            provider_turn_id: Some(provider_turn_id),
            native_item_ref: Some(native_item_ref),
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Waiting,
            title: None,
            started_at: Some(now),
            completed_at: None,
            updated_at: now,
            request_id: request_id.clone(),
            questions,
            question_answer: None,
            response_mode: None,
        },
    }
}

/// `settleRequest`: the node a request stood for, once it resolves or is cancelled.
pub fn settle_request_node(
    node: &OrchestrationV2ExecutionNode,
    cancelled: bool,
    now: UtcDateTime,
) -> OrchestrationV2ExecutionNode {
    OrchestrationV2ExecutionNode {
        status: if cancelled {
            OrchestrationV2ExecutionNodeStatus::Cancelled
        } else {
            OrchestrationV2ExecutionNodeStatus::Completed
        },
        completed_at: Some(now),
        ..node.clone()
    }
}

/// `settleRequest`: the request's turn item, completed or cancelled with its request.
pub fn settle_request_item(
    item: &OrchestrationV2TurnItem,
    cancelled: bool,
    now: UtcDateTime,
) -> OrchestrationV2TurnItem {
    let status = if cancelled {
        OrchestrationV2TurnItemStatus::Cancelled
    } else {
        OrchestrationV2TurnItemStatus::Completed
    };
    let mut item = item.clone();
    match &mut item {
        OrchestrationV2TurnItem::ApprovalRequest {
            status: current,
            completed_at,
            updated_at,
            ..
        }
        | OrchestrationV2TurnItem::UserInputRequest {
            status: current,
            completed_at,
            updated_at,
            ..
        } => {
            *current = status;
            *completed_at = Some(now);
            *updated_at = now;
        }
        _ => {}
    }
    item
}

/// `questions` for a user-input request, in the order OpenCode asked them.
pub fn user_input_questions(questions: &[Value]) -> Vec<OrchestrationV2UserInputQuestion> {
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let header = question
                .get("header")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|header| !header.is_empty())
                .unwrap_or("Question")
                .to_owned();
            let text = question
                .get("question")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or(&header)
                .to_owned();
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    let label = option.get("label").and_then(Value::as_str)?.trim();
                    let label = TrimmedNonEmptyString::new(label).ok()?;
                    let description = option
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .unwrap_or(label.as_str())
                        .to_owned();
                    Some(OrchestrationV2UserInputQuestionOption {
                        description: TrimmedNonEmptyString::new(description).ok()?,
                        label,
                        value: None,
                    })
                })
                .collect();
            OrchestrationV2UserInputQuestion {
                id: TrimmedNonEmptyString::new(open_code_question_id(index, &header))
                    .expect("question id"),
                header: TrimmedNonEmptyString::new(header).expect("header"),
                question: TrimmedNonEmptyString::new(text).expect("question text"),
                options,
                multi_select: question.get("multiple").and_then(Value::as_bool),
                allow_custom_answer: question.get("custom").and_then(Value::as_bool),
                initial_answer: None,
                required: None,
            }
        })
        .collect()
}

/// The id of a question OpenCode asked: its index and header, as the answer map keys it.
pub fn open_code_question_id(index: usize, header: &str) -> String {
    let header = header
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    if header.is_empty() {
        format!("question-{index}")
    } else {
        format!("question-{index}-{header}")
    }
}

/// One compaction, as `emitCompaction` (v2) and `emitCompactionItem` (v1) build it. The
/// item sits on the turn's root node: upstream emits no node of its own for it.
pub struct Compaction<'a> {
    pub thread_id: &'a ThreadId,
    pub root_node_id: &'a NodeId,
    pub provider_thread_id: &'a ProviderThreadId,
    pub provider_turn_id: &'a ProviderTurnId,
    pub native_item_id: &'a str,
    pub ordinal: u64,
    pub title: &'a str,
    pub status: OrchestrationV2TurnItemStatus,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
    pub now: UtcDateTime,
    pub summary: Option<String>,
    pub weak_ref: bool,
}

pub fn compaction_turn_item(
    driver: &ProviderDriverKind,
    of: Compaction<'_>,
) -> OrchestrationV2TurnItem {
    OrchestrationV2TurnItem::Compaction {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: derive::turn_item_from_provider_item(driver, of.native_item_id),
        thread_id: of.thread_id.clone(),
        run_id: None,
        node_id: Some(of.root_node_id.clone()),
        provider_thread_id: Some(of.provider_thread_id.clone()),
        provider_turn_id: Some(of.provider_turn_id.clone()),
        native_item_ref: Some(native_ref(of.native_item_id, of.weak_ref)),
        parent_item_id: None,
        ordinal: of.ordinal,
        status: of.status,
        title: Some(of.title.to_owned()),
        started_at: of.started_at,
        completed_at: of.completed_at,
        updated_at: of.now,
        driver: Some(driver.clone()),
        summary: of.summary,
        before_token_count: None,
        after_token_count: None,
    }
}

/// A `task` tool call, which upstream projects as a subagent rather than a tool call
/// (`emitSubagent`). The node, the subagent record and the subagent item share one id:
/// the node derived from the tool part.
pub struct SubagentCall<'a> {
    pub instance_id: &'a ProviderInstanceId,
    pub of: NodeOf<'a>,
    pub ordinal: u64,
    pub prompt: &'a str,
    pub title: Option<&'a str>,
    pub status: OrchestrationV2TurnItemStatus,
    pub started_at: UtcDateTime,
    pub completed_at: Option<UtcDateTime>,
    pub result: Option<String>,
    /// The child session the call started, when OpenCode has named it.
    pub child_session: Option<&'a str>,
    pub now: UtcDateTime,
}

pub struct SubagentRecords {
    pub node: OrchestrationV2ExecutionNode,
    pub subagent: OrchestrationV2Subagent,
    pub item: OrchestrationV2TurnItem,
}

pub fn subagent_records(driver: &ProviderDriverKind, call: SubagentCall<'_>) -> SubagentRecords {
    let subagent_id = derive::node_from_provider_item(driver, call.of.native_item_id);
    let node = execution_node(
        driver,
        &call.of,
        NodeTiming {
            kind: OrchestrationV2ExecutionNodeKind::Subagent,
            status: node_status_of(call.status),
            started_at: call.started_at,
            completed_at: call.completed_at,
        },
    );
    let subagent_status = match call.status {
        OrchestrationV2TurnItemStatus::Pending => OrchestrationV2SubagentStatus::Pending,
        OrchestrationV2TurnItemStatus::Completed => OrchestrationV2SubagentStatus::Completed,
        OrchestrationV2TurnItemStatus::Failed => OrchestrationV2SubagentStatus::Failed,
        _ => OrchestrationV2SubagentStatus::Running,
    };
    let subagent = OrchestrationV2Subagent {
        id: subagent_id.clone(),
        thread_id: call.of.thread_id.clone(),
        run_id: None,
        parent_node_id: call.of.root_node_id.clone(),
        origin: OrchestrationV2SubagentOrigin::ProviderNative,
        created_by: OrchestrationV2Actor::Agent,
        driver: driver.clone(),
        provider_instance_id: call.instance_id.clone(),
        provider_thread_id: Some(call.of.provider_thread_id.clone()),
        child_thread_id: None,
        native_task_ref: call.child_session.map(|child| native_ref(child, false)),
        prompt: call.prompt.to_owned(),
        title: call.title.map(str::to_owned),
        model: None,
        model_selection: None,
        completion_wake: None,
        completion_delivery: None,
        status: subagent_status,
        progress: None,
        workflow: None,
        result: call.result.clone(),
        started_at: Some(call.started_at),
        completed_at: call.completed_at,
        updated_at: call.now,
    };
    let item = OrchestrationV2TurnItem::Subagent {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: derive::turn_item_from_provider_item(driver, call.of.native_item_id),
        thread_id: call.of.thread_id.clone(),
        run_id: None,
        node_id: Some(subagent_id.clone()),
        provider_thread_id: Some(call.of.provider_thread_id.clone()),
        provider_turn_id: Some(call.of.provider_turn_id.clone()),
        native_item_ref: Some(native_ref(call.of.native_item_id, false)),
        parent_item_id: None,
        ordinal: call.ordinal,
        status: call.status,
        title: call.title.map(str::to_owned),
        started_at: Some(call.started_at),
        completed_at: call.completed_at,
        updated_at: call.now,
        subagent_id,
        origin: OrchestrationV2SubagentOrigin::ProviderNative,
        driver: driver.clone(),
        provider_instance_id: call.instance_id.clone(),
        child_thread_id: None,
        prompt: call.prompt.to_owned(),
        progress: None,
        result: call.result,
    };
    SubagentRecords {
        node,
        subagent,
        item,
    }
}
