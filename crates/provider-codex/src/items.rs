//! Port of the artifact builders in upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! (`buildAgentMessageArtifacts`, `buildCommandExecutionArtifacts`,
//! `buildWebSearchArtifacts`, `buildFileChangeArtifacts`, `buildProposedPlanArtifacts`,
//! `buildTodoListArtifacts`, and `resolveItemPosition`). Each builder returns the
//! execution node and turn item upstream emits for one Codex item.
use crate::{
    orchestration_adapter::driver,
    projection::TurnState,
    tool_presentation::{
        ToolPresentation, computer_use_tool_source, dynamic_tool_surface, dynamic_tool_title,
        mcp_tool_presentation,
    },
};
use serde_json::{Value, json};
use turin_contracts::{
    PlanId, ProviderSessionId, TrimmedNonEmptyString, TurnItemId, UtcDateTime,
    base_schemas::js_trim,
    orchestration_v2::{
        MessageResponseMode, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2PlanStep, OrchestrationV2ProviderRef,
        OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus, OrchestrationV2UserInputQuestion,
    },
    provider_runtime::{ToolActivitySourceKind, ToolActivitySurface},
};
use turin_provider_core::id_allocator::{IdAllocator, derive};

/// One item's node and turn item, plus the conversation message for a completed assistant text.
pub struct ItemArtifacts {
    pub node: OrchestrationV2ExecutionNode,
    pub turn_item: OrchestrationV2TurnItem,
    pub message: Option<OrchestrationV2ConversationMessage>,
}

/// Node and turn-item status for a Codex `ThreadItem` status.
#[derive(Clone, Copy)]
pub struct CodexItemStatus {
    pub node: OrchestrationV2ExecutionNodeStatus,
    pub turn_item: OrchestrationV2TurnItemStatus,
    pub completed: bool,
}

/// Upstream `codexItemStatus`: `declined` is a cancelled tool call.
pub fn codex_item_status(status: &str) -> CodexItemStatus {
    use OrchestrationV2ExecutionNodeStatus as N;
    use OrchestrationV2TurnItemStatus as T;
    match status {
        "completed" => CodexItemStatus {
            node: N::Completed,
            turn_item: T::Completed,
            completed: true,
        },
        "failed" => CodexItemStatus {
            node: N::Failed,
            turn_item: T::Failed,
            completed: true,
        },
        "declined" => CodexItemStatus {
            node: N::Cancelled,
            turn_item: T::Cancelled,
            completed: true,
        },
        _ => CodexItemStatus {
            node: N::Running,
            turn_item: T::Running,
            completed: false,
        },
    }
}

/// Upstream `codexNativeItemRef`.
pub fn native_item_ref(native_item: &str) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: driver(),
        native_id: Some(TrimmedNonEmptyString::new(native_item).expect("native item id")),
        strength: turin_contracts::orchestration_v2::OrchestrationV2NativeRefStrength::Strong,
        fingerprint: None,
        ordinal: None,
    }
}

/// Upstream `resolveItemPosition`: the first sight of an item fixes its ordinal
/// (provider turn ordinal times 100, plus position within the turn) and start time.
/// `started_at` seeds Codex's own start time; otherwise `now` is used.
pub fn resolve_position(
    turn: &mut TurnState,
    native_item: &str,
    started_at: Option<UtcDateTime>,
    now: UtcDateTime,
) -> (u64, UtcDateTime) {
    if let Some((_, started)) = turn.positions.iter().find(|(id, _)| id == native_item) {
        let index = turn
            .positions
            .iter()
            .position(|(id, _)| id == native_item)
            .expect("position present");
        return (position_ordinal(turn, index), *started);
    }
    turn.positions
        .push((native_item.to_owned(), started_at.unwrap_or(now)));
    let index = turn.positions.len() - 1;
    (position_ordinal(turn, index), turn.positions[index].1)
}

fn position_ordinal(turn: &TurnState, index: usize) -> u64 {
    turn.input.provider_turn_ordinal.max(0) as u64 * 100 + index as u64 + 1
}

/// The node fields every Codex item shares, as upstream builds them per item.
fn node(
    turn: &TurnState,
    native_item: &str,
    kind: OrchestrationV2ExecutionNodeKind,
    status: OrchestrationV2ExecutionNodeStatus,
    started_at: UtcDateTime,
    completed_at: Option<UtcDateTime>,
) -> OrchestrationV2ExecutionNode {
    let input = &turn.input;
    OrchestrationV2ExecutionNode {
        id: derive::node_from_provider_item(&driver(), native_item),
        thread_id: input.thread_id.clone(),
        run_id: turn.run_id(),
        parent_node_id: Some(input.root_node_id.clone()),
        root_node_id: input.root_node_id.clone(),
        kind,
        status,
        counts_for_run: false,
        provider_thread_id: Some(input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: Some(started_at),
        completed_at,
    }
}

fn turn_item_id(native_item: &str) -> TurnItemId {
    derive::turn_item_from_provider_item(&driver(), native_item)
}

/// Upstream `buildAgentMessageArtifacts`. Streaming text is `completed: false`;
/// a completed message also carries the conversation message.
pub fn agent_message(
    turn: &mut TurnState,
    native_item: &str,
    text: &str,
    completed: bool,
    now: UtcDateTime,
) -> ItemArtifacts {
    let (ordinal, started_at) = resolve_position(turn, native_item, None, now);
    let completed_at = completed.then_some(now);
    let status = if completed {
        OrchestrationV2ExecutionNodeStatus::Completed
    } else {
        OrchestrationV2ExecutionNodeStatus::Running
    };
    let item_status = if completed {
        OrchestrationV2TurnItemStatus::Completed
    } else {
        OrchestrationV2TurnItemStatus::Running
    };
    let node = node(
        turn,
        native_item,
        OrchestrationV2ExecutionNodeKind::AssistantMessage,
        status,
        started_at,
        completed_at,
    );
    let message_id = derive::message_from_provider_item(&driver(), native_item);
    let message = completed.then(|| OrchestrationV2ConversationMessage {
        notification: None,
        created_by: turin_contracts::orchestration_v2::OrchestrationV2Actor::Agent,
        creation_source: OrchestrationV2CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: None,
        id: message_id.clone(),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        role: OrchestrationV2ConversationMessageRole::Assistant,
        text: text.to_owned(),
        context: None,
        attachments: vec![],
        streaming: false,
        created_at: started_at,
        updated_at: now,
        delegated_completion: None,
    });
    let turn_item = OrchestrationV2TurnItem::AssistantMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: item_status,
        title: None,
        started_at: Some(started_at),
        completed_at,
        updated_at: now,
        message_id,
        text: text.to_owned(),
        attachments: None,
        streaming: !completed,
    };
    ItemArtifacts {
        node,
        turn_item,
        message,
    }
}

/// Reasoning parts stream under composite keys, but identify with the real item.
pub fn reasoning(
    turn: &mut TurnState,
    part_key: &str,
    native_item: &str,
    text: &str,
    completed: bool,
    interrupted: bool,
    now: UtcDateTime,
) -> ItemArtifacts {
    let (ordinal, started_at) = resolve_position(turn, part_key, None, now);
    let completed_at = completed.then_some(now);
    // A turn that did not complete leaves its reasoning interrupted, however far it got.
    let status = if interrupted {
        OrchestrationV2ExecutionNodeStatus::Interrupted
    } else if completed {
        OrchestrationV2ExecutionNodeStatus::Completed
    } else {
        OrchestrationV2ExecutionNodeStatus::Running
    };
    let item_status = if interrupted {
        OrchestrationV2TurnItemStatus::Interrupted
    } else if completed {
        OrchestrationV2TurnItemStatus::Completed
    } else {
        OrchestrationV2TurnItemStatus::Running
    };
    let mut node = node(
        turn,
        part_key,
        OrchestrationV2ExecutionNodeKind::Reasoning,
        status,
        started_at,
        completed_at,
    );
    // The node and the item both name the Codex item, not the composite part key.
    node.native_item_ref = Some(native_item_ref(native_item));
    let turn_item = OrchestrationV2TurnItem::Reasoning {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(part_key),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: item_status,
        title: None,
        started_at: Some(started_at),
        completed_at,
        updated_at: now,
        text: text.to_owned(),
        streaming: !completed,
    };
    ItemArtifacts {
        node,
        turn_item,
        message: None,
    }
}

fn str_field<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key).and_then(Value::as_str)
}

/// Upstream `buildCommandExecutionArtifacts`, from a `commandExecution` `ThreadItem`.
pub fn command_execution(
    turn: &mut TurnState,
    item: &Value,
    started_at_ms: Option<UtcDateTime>,
    now: UtcDateTime,
) -> ItemArtifacts {
    let native_item = str_field(item, "id").unwrap_or_default();
    let status = codex_item_status(str_field(item, "status").unwrap_or("inProgress"));
    let (ordinal, started_at) = resolve_position(turn, native_item, started_at_ms, now);
    let completed_at = status.completed.then_some(now);
    let node = node(
        turn,
        native_item,
        OrchestrationV2ExecutionNodeKind::ToolCall,
        status.node,
        started_at,
        completed_at,
    );
    let turn_item = OrchestrationV2TurnItem::CommandExecution {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: status.turn_item,
        title: None,
        started_at: Some(started_at),
        completed_at,
        updated_at: now,
        input: str_field(item, "command").unwrap_or_default().to_owned(),
        output: str_field(item, "aggregatedOutput").map(str::to_owned),
        output_omitted: None,
        output_indicates_failure: None,
        exit_code: item.get("exitCode").and_then(Value::as_i64),
    };
    ItemArtifacts {
        node,
        turn_item,
        message: None,
    }
}

/// Upstream `webSearchPatterns`: query and action strings, trimmed, in order. Repeats stay,
/// as `compactStrings` keeps them.
pub fn web_search_patterns(item: &Value) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    let mut push = |value: Option<&str>| {
        if let Some(text) = value.map(js_trim).filter(|text| !text.is_empty()) {
            patterns.push(text.to_owned());
        }
    };
    let query = str_field(item, "query");
    let action = item.get("action").filter(|a| !a.is_null());
    let Some(action) = action else {
        push(query);
        return patterns;
    };
    match str_field(action, "type") {
        Some("search") => {
            for q in action
                .get("queries")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                push(q.as_str());
            }
            push(str_field(action, "query"));
            push(query);
        }
        Some("openPage") => {
            push(str_field(action, "url"));
            push(query);
        }
        Some("findInPage") => {
            push(str_field(action, "pattern"));
            push(str_field(action, "url"));
            push(query);
        }
        _ => push(query),
    }
    patterns
}

/// Upstream `buildWebSearchArtifacts`.
pub fn web_search(
    turn: &mut TurnState,
    item: &Value,
    completed: bool,
    now: UtcDateTime,
) -> ItemArtifacts {
    let native_item = str_field(item, "id").unwrap_or_default();
    let (ordinal, started_at) = resolve_position(turn, native_item, None, now);
    let completed_at = completed.then_some(now);
    let status = if completed {
        OrchestrationV2TurnItemStatus::Completed
    } else {
        OrchestrationV2TurnItemStatus::Running
    };
    let node_status = if completed {
        OrchestrationV2ExecutionNodeStatus::Completed
    } else {
        OrchestrationV2ExecutionNodeStatus::Running
    };
    let node = node(
        turn,
        native_item,
        OrchestrationV2ExecutionNodeKind::ToolCall,
        node_status,
        started_at,
        completed_at,
    );
    let patterns = web_search_patterns(item);
    let turn_item = OrchestrationV2TurnItem::WebSearch {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status,
        title: None,
        started_at: Some(started_at),
        completed_at,
        updated_at: now,
        results: None,
        patterns: (!patterns.is_empty()).then_some(patterns),
    };
    ItemArtifacts {
        node,
        turn_item,
        message: None,
    }
}

/// Upstream `buildFileChangeArtifacts`: the turn item shows the first change.
/// Returns `None` when the item has no changes.
pub fn file_change(turn: &mut TurnState, item: &Value, now: UtcDateTime) -> Option<ItemArtifacts> {
    let first = item.get("changes")?.as_array()?.first()?;
    let native_item = str_field(item, "id").unwrap_or_default();
    let status = codex_item_status(str_field(item, "status").unwrap_or("inProgress"));
    let (ordinal, started_at) = resolve_position(turn, native_item, None, now);
    let completed_at = status.completed.then_some(now);
    let node = node(
        turn,
        native_item,
        OrchestrationV2ExecutionNodeKind::ToolCall,
        status.node,
        started_at,
        completed_at,
    );
    let turn_item = OrchestrationV2TurnItem::FileChange {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: status.turn_item,
        title: None,
        started_at: Some(started_at),
        completed_at,
        updated_at: now,
        file_name: TrimmedNonEmptyString::new(str_field(first, "path").unwrap_or_default()).ok()?,
        additions: None,
        deletions: None,
        diff_str: str_field(first, "diff").map(str::to_owned),
        old_str: None,
        new_str: None,
        changes: None,
    };
    Some(ItemArtifacts {
        node,
        turn_item,
        message: None,
    })
}

/// Upstream `codexMcpToolOutput`: the structured content (else the content blocks)
/// of an MCP result, with the error message when the call failed.
fn mcp_tool_output(item: &Value) -> Option<Value> {
    let result = item.get("result").filter(|result| !result.is_null());
    let result_output = result.and_then(|result| {
        match result
            .get("structuredContent")
            .filter(|value| !value.is_null())
        {
            Some(structured) => Some(structured.clone()),
            None => result.get("content").cloned(),
        }
    });
    match item.get("error").filter(|error| !error.is_null()) {
        None => result_output,
        Some(error) => {
            let message = error.get("message").cloned().unwrap_or(Value::Null);
            Some(match result_output {
                None => json!({ "error": message }),
                Some(result) => json!({ "error": message, "result": result }),
            })
        }
    }
}

/// Upstream `codexDynamicToolOutput`: the content items, or `{success: false}`.
fn dynamic_tool_output(item: &Value) -> Option<Value> {
    match item.get("contentItems").filter(|items| !items.is_null()) {
        Some(items) => Some(items.clone()),
        None => {
            (item.get("success") == Some(&Value::Bool(false))).then(|| json!({ "success": false }))
        }
    }
}

/// Upstream `projectCodexDynamicToolItem` with `buildDynamicToolArtifacts`: an
/// `mcpToolCall` or `dynamicToolCall`. The item shows the tool name, title,
/// input, and output.
pub fn dynamic_tool(turn: &mut TurnState, item: &Value, now: UtcDateTime) -> ItemArtifacts {
    let native_item = str_field(item, "id").unwrap_or_default();
    let is_mcp = str_field(item, "type") == Some("mcpToolCall");
    let status = codex_item_status(str_field(item, "status").unwrap_or("inProgress"));
    let (ordinal, started_at) = resolve_position(turn, native_item, None, now);
    let completed_at = status.completed.then_some(now);
    let node = node(
        turn,
        native_item,
        OrchestrationV2ExecutionNodeKind::ToolCall,
        status.node,
        started_at,
        completed_at,
    );
    let tool_name = if is_mcp {
        format!(
            "{}.{}",
            str_field(item, "server").unwrap_or_default(),
            str_field(item, "tool").unwrap_or_default()
        )
    } else {
        let namespace = str_field(item, "namespace")
            .map(js_trim)
            .filter(|ns| !ns.is_empty());
        [namespace, str_field(item, "tool")]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(".")
    };
    let input = item.get("arguments").cloned().unwrap_or(Value::Null);
    let presentation = if is_mcp {
        mcp_tool_presentation(item)
    } else {
        ToolPresentation::default()
    };
    let title = dynamic_tool_title(&tool_name, &input, presentation.title.as_deref())
        .or(presentation.title);
    let surface = presentation
        .tool_surface
        .or_else(|| dynamic_tool_surface(&tool_name, &input));
    // A computer-use call is attributed to the computer source unless its own source already is.
    let source_is_computer = presentation
        .tool_source
        .as_ref()
        .is_some_and(|source| source.kind == ToolActivitySourceKind::Computer);
    let tool_source = if surface == Some(ToolActivitySurface::Computer) && !source_is_computer {
        Some(computer_use_tool_source())
    } else {
        presentation.tool_source
    };
    let turn_item = OrchestrationV2TurnItem::DynamicTool {
        tool_non_execution_kind: None,
        tool_surface: surface,
        tool_icon: presentation.tool_icon,
        tool_source,
        id: turn_item_id(native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: status.turn_item,
        title,
        started_at: Some(started_at),
        completed_at,
        updated_at: now,
        tool_name: TrimmedNonEmptyString::new(tool_name).ok(),
        viewed_image_path: None,
        input,
        output: if is_mcp {
            mcp_tool_output(item)
        } else {
            dynamic_tool_output(item)
        },
        output_omitted: None,
    };
    ItemArtifacts {
        node,
        turn_item,
        message: None,
    }
}

/// Upstream `buildProposedPlanArtifacts`. `status` is the plan lifecycle; a finished
/// proposal stays `active` until the user implements it.
pub fn proposed_plan(
    turn: &mut TurnState,
    native_item: &str,
    plan_id: &PlanId,
    markdown: &str,
    completed: bool,
    now: UtcDateTime,
) -> (OrchestrationV2ExecutionNode, OrchestrationV2TurnItem) {
    let (ordinal, started_at) = resolve_position(turn, native_item, None, now);
    let completed_at = completed.then_some(now);
    let node = node(
        turn,
        native_item,
        OrchestrationV2ExecutionNodeKind::Plan,
        if completed {
            OrchestrationV2ExecutionNodeStatus::Completed
        } else {
            OrchestrationV2ExecutionNodeStatus::Running
        },
        started_at,
        completed_at,
    );
    let turn_item = OrchestrationV2TurnItem::ProposedPlan {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: if completed {
            OrchestrationV2TurnItemStatus::Completed
        } else {
            OrchestrationV2TurnItemStatus::Running
        },
        title: None,
        started_at: Some(started_at),
        completed_at,
        updated_at: now,
        plan_id: plan_id.clone(),
        markdown: markdown.to_owned(),
        streaming: !completed,
    };
    (node, turn_item)
}

/// Upstream `buildTodoListArtifacts`: the `turn/plan/updated` checklist.
pub fn todo_list(
    turn: &mut TurnState,
    native_item: &str,
    plan_id: &PlanId,
    steps: &[OrchestrationV2PlanStep],
    explanation: Option<&str>,
    now: UtcDateTime,
) -> (OrchestrationV2ExecutionNode, OrchestrationV2TurnItem) {
    let (ordinal, started_at) = resolve_position(turn, native_item, None, now);
    let node = node(
        turn,
        native_item,
        OrchestrationV2ExecutionNodeKind::TodoList,
        OrchestrationV2ExecutionNodeStatus::Running,
        started_at,
        None,
    );
    let turn_item = OrchestrationV2TurnItem::TodoList {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node.id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Running,
        title: None,
        started_at: Some(started_at),
        completed_at: None,
        updated_at: now,
        plan_id: plan_id.clone(),
        steps: steps.to_vec(),
        explanation: explanation.map(str::to_owned),
    };
    (node, turn_item)
}

/// What a user-input request projects to: its node, the runtime request, and its
/// turn item. Upstream's `buildUserInputRequestArtifacts`.
pub struct UserInputArtifacts {
    pub node: OrchestrationV2ExecutionNode,
    pub request: OrchestrationV2RuntimeRequest,
    pub turn_item: OrchestrationV2TurnItem,
}

/// Builds a user-input request under `turn`. A request with a live session is
/// answered on the session. Without one it is answered by dispatching a message.
pub fn user_input_request(
    turn: &mut TurnState,
    native_item: &str,
    native_request: &str,
    questions: Vec<OrchestrationV2UserInputQuestion>,
    session: Option<ProviderSessionId>,
    now: UtcDateTime,
) -> UserInputArtifacts {
    let provider_turn = derive::provider_turn(&driver(), &turn.native_turn);
    let request_id =
        IdAllocator.runtime_request(&driver(), Some(&provider_turn), Some(native_request));
    let (ordinal, _) = resolve_position(turn, native_item, None, now);
    let node_id = derive::node_from_provider_item(&driver(), native_item);
    let native_ref = native_item_ref(native_item);
    let node = OrchestrationV2ExecutionNode {
        id: node_id.clone(),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        parent_node_id: Some(turn.input.root_node_id.clone()),
        root_node_id: turn.input.root_node_id.clone(),
        kind: OrchestrationV2ExecutionNodeKind::UserInputRequest,
        status: OrchestrationV2ExecutionNodeStatus::Waiting,
        counts_for_run: false,
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(provider_turn.clone()),
        native_item_ref: Some(native_ref.clone()),
        runtime_request_id: Some(request_id.clone()),
        checkpoint_scope_id: None,
        started_at: Some(now),
        completed_at: None,
    };
    let response_mode = session.is_none().then_some(MessageResponseMode);
    let capability = match &session {
        Some(provider_session_id) => OrchestrationV2ResponseCapability::Live {
            provider_session_id: provider_session_id.clone(),
        },
        None => OrchestrationV2ResponseCapability::Message,
    };
    let turn_item = OrchestrationV2TurnItem::UserInputRequest {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: derive::turn_item_from_provider_item(&driver(), native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(node_id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(provider_turn.clone()),
        native_item_ref: Some(native_ref.clone()),
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Waiting,
        title: None,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
        request_id: request_id.clone(),
        questions,
        response_mode,
        question_answer: None,
    };
    let request = OrchestrationV2RuntimeRequest {
        id: request_id,
        node_id,
        provider_turn_id: Some(provider_turn),
        native_request_ref: Some(native_item_ref(native_request)),
        kind: OrchestrationV2RuntimeRequestKind::UserInput,
        status: OrchestrationV2RuntimeRequestStatus::Pending,
        response_capability: capability,
        created_at: now,
        resolved_at: None,
        decision: None,
        answers: None,
    };
    UserInputArtifacts {
        node,
        request,
        turn_item,
    }
}

/// Upstream's question mapping for `item/tool/requestUserInput`: each field falls
/// back to a default when blank, and an option's description falls back to its label.
pub fn codex_questions(questions: &Value) -> Vec<OrchestrationV2UserInputQuestion> {
    let text = |value: Option<&str>, fallback: &str| {
        value
            .map(js_trim)
            .filter(|text| !text.is_empty())
            .map_or_else(|| fallback.to_owned(), str::to_owned)
    };
    questions
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, question)| {
            let options: Vec<Value> = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(option_index, option)| {
                    let label = text(
                        option.get("label").and_then(Value::as_str),
                        &format!("Option {}", option_index + 1),
                    );
                    let description =
                        text(option.get("description").and_then(Value::as_str), &label);
                    json!({"label": label, "description": description})
                })
                .collect();
            user_input_question(
                &text(
                    question.get("id").and_then(Value::as_str),
                    &format!("question-{}", index + 1),
                ),
                &text(question.get("header").and_then(Value::as_str), "Question"),
                &text(
                    question.get("question").and_then(Value::as_str),
                    "Choose an answer.",
                ),
                options,
            )
        })
        .collect()
}

/// One user-input question in the contract's shape.
pub fn user_input_question(
    id: &str,
    header: &str,
    question: &str,
    options: Vec<Value>,
) -> OrchestrationV2UserInputQuestion {
    serde_json::from_value(json!({
        "id": id,
        "header": header,
        "question": question,
        "options": options,
    }))
    .expect("user input question")
}
