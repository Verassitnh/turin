//! Port of upstream/packages/provider-acp/src/server/coreRuntimeEvents.ts
//!
//! Builds canonical [`ProviderRuntimeEvent`] values from ACP session-update and
//! permission shapes produced by [`crate::runtime_model`].

use turin_contracts::base_schemas::{
    EventId, IsoDateTime, RuntimeItemId, RuntimeRequestId, ThreadId, TrimmedNonEmptyString, TurnId,
};
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::provider_policy::{ProviderApprovalDecision, ProviderApprovalOption};
use turin_contracts::provider_runtime::{
    CanonicalItemType, CanonicalRequestType, ContentDeltaPayload, ItemLifecyclePayload,
    ProviderRuntimeEvent, ProviderRuntimeEventBase, RequestOpenedPayload, RequestResolvedPayload,
    RuntimeContentStreamKind, RuntimeEventRaw, RuntimeEventRawSource, RuntimeItemStatus,
    RuntimePlanStep, RuntimePlanStepStatus, TurnPlanUpdatedPayload,
};

use crate::runtime_model::{
    AcpPermissionRequest, AcpPlanStepStatus, AcpPlanUpdate, AcpToolCallState, AcpToolCallStatus,
    canonical_item_type_from_acp_tool_kind,
};

/// Upstream `AcpEventStamp`.
#[derive(Debug, Clone)]
pub struct AcpEventStamp {
    pub event_id: EventId,
    pub created_at: IsoDateTime,
}

/// Raw-source slugs the ACP adapter attaches to runtime events.
pub type AcpAdapterRawSource = RuntimeEventRawSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpAssistantItemLifecycle {
    Started,
    Completed,
}

fn event_base(
    stamp: &AcpEventStamp,
    provider: &ProviderDriverKind,
    thread_id: &ThreadId,
    turn_id: Option<&TurnId>,
) -> ProviderRuntimeEventBase {
    ProviderRuntimeEventBase {
        event_id: stamp.event_id.clone(),
        provider: provider.clone(),
        provider_instance_id: None,
        thread_id: thread_id.clone(),
        created_at: stamp.created_at.clone(),
        turn_id: turn_id.cloned(),
        item_id: None,
        request_id: None,
        provider_refs: None,
        raw: None,
    }
}

fn canonical_request_type_from_acp_kind(kind: &str) -> CanonicalRequestType {
    match kind {
        "execute" => CanonicalRequestType::ExecCommandApproval,
        "read" => CanonicalRequestType::FileReadApproval,
        "edit" | "delete" | "move" => CanonicalRequestType::FileChangeApproval,
        _ => CanonicalRequestType::DynamicToolCall,
    }
}

fn runtime_item_status_from_acp_tool_status(
    status: Option<AcpToolCallStatus>,
) -> Option<RuntimeItemStatus> {
    match status {
        Some(AcpToolCallStatus::Pending) | Some(AcpToolCallStatus::InProgress) => {
            Some(RuntimeItemStatus::InProgress)
        }
        Some(AcpToolCallStatus::Completed) => Some(RuntimeItemStatus::Completed),
        Some(AcpToolCallStatus::Failed) => Some(RuntimeItemStatus::Failed),
        Some(AcpToolCallStatus::RequiresAction) | None => None,
    }
}

fn to_canonical_item_type(kind: Option<&str>) -> CanonicalItemType {
    use turin_contracts::provider_runtime::ToolLifecycleItemType;
    match canonical_item_type_from_acp_tool_kind(kind) {
        ToolLifecycleItemType::CommandExecution => CanonicalItemType::CommandExecution,
        ToolLifecycleItemType::FileChange => CanonicalItemType::FileChange,
        ToolLifecycleItemType::WebSearch => CanonicalItemType::WebSearch,
        ToolLifecycleItemType::DynamicToolCall => CanonicalItemType::DynamicToolCall,
        ToolLifecycleItemType::McpToolCall => CanonicalItemType::McpToolCall,
        ToolLifecycleItemType::CollabAgentToolCall => CanonicalItemType::CollabAgentToolCall,
        ToolLifecycleItemType::ImageView => CanonicalItemType::ImageView,
    }
}

fn plan_step_status(status: AcpPlanStepStatus) -> RuntimePlanStepStatus {
    match status {
        AcpPlanStepStatus::Pending => RuntimePlanStepStatus::Pending,
        AcpPlanStepStatus::InProgress => RuntimePlanStepStatus::InProgress,
        AcpPlanStepStatus::Completed => RuntimePlanStepStatus::Completed,
    }
}

fn approval_decision_wire(decision: ProviderApprovalDecision) -> TrimmedNonEmptyString {
    let wire = match decision {
        ProviderApprovalDecision::Accept => "accept",
        ProviderApprovalDecision::AcceptForSession => "acceptForSession",
        ProviderApprovalDecision::AcceptAlways => "acceptAlways",
        ProviderApprovalDecision::Decline => "decline",
        ProviderApprovalDecision::Cancel => "cancel",
    };
    TrimmedNonEmptyString::new(wire).expect("static approval decision wire values are valid")
}

fn runtime_raw(
    source: &AcpAdapterRawSource,
    method: &str,
    raw_payload: serde_json::Value,
) -> RuntimeEventRaw {
    RuntimeEventRaw {
        source: source.clone(),
        method: Some(
            TrimmedNonEmptyString::new(method).expect("ACP adapter methods are non-empty"),
        ),
        message_type: None,
        payload: raw_payload,
    }
}

pub struct MakeAcpRequestOpenedEventInput<'a> {
    pub stamp: &'a AcpEventStamp,
    pub provider: &'a ProviderDriverKind,
    pub thread_id: &'a ThreadId,
    pub turn_id: Option<&'a TurnId>,
    pub request_id: &'a RuntimeRequestId,
    pub permission_request: &'a AcpPermissionRequest,
    pub approval_options: Option<&'a [ProviderApprovalOption]>,
    pub detail: &'a str,
    pub args: serde_json::Value,
    pub source: &'a AcpAdapterRawSource,
    pub method: &'a str,
    pub raw_payload: serde_json::Value,
}

/// Upstream `makeAcpRequestOpenedEvent`.
pub fn make_acp_request_opened_event(
    input: MakeAcpRequestOpenedEventInput<'_>,
) -> ProviderRuntimeEvent {
    let detail = TrimmedNonEmptyString::new(input.detail).expect("detail is non-empty");
    let mut payload = RequestOpenedPayload {
        request_type: canonical_request_type_from_acp_kind(&input.permission_request.kind),
        detail: Some(detail),
        app_name: None,
        options: input.approval_options.map(|options| options.to_vec()),
        args: Some(input.args),
    };
    if input.approval_options.is_none() {
        payload.options = None;
    }
    let mut base = event_base(input.stamp, input.provider, input.thread_id, input.turn_id);
    base.request_id = Some(input.request_id.clone());
    base.raw = Some(runtime_raw(input.source, input.method, input.raw_payload));
    ProviderRuntimeEvent::RequestOpened { base, payload }
}

pub struct MakeAcpRequestResolvedEventInput<'a> {
    pub stamp: &'a AcpEventStamp,
    pub provider: &'a ProviderDriverKind,
    pub thread_id: &'a ThreadId,
    pub turn_id: Option<&'a TurnId>,
    pub request_id: &'a RuntimeRequestId,
    pub permission_request: &'a AcpPermissionRequest,
    pub decision: ProviderApprovalDecision,
}

/// Upstream `makeAcpRequestResolvedEvent`.
pub fn make_acp_request_resolved_event(
    input: MakeAcpRequestResolvedEventInput<'_>,
) -> ProviderRuntimeEvent {
    let mut base = event_base(input.stamp, input.provider, input.thread_id, input.turn_id);
    base.request_id = Some(input.request_id.clone());
    let payload = RequestResolvedPayload {
        request_type: canonical_request_type_from_acp_kind(&input.permission_request.kind),
        decision: Some(approval_decision_wire(input.decision)),
        resolution: None,
    };
    ProviderRuntimeEvent::RequestResolved { base, payload }
}

pub struct MakeAcpPlanUpdatedEventInput<'a> {
    pub stamp: &'a AcpEventStamp,
    pub provider: &'a ProviderDriverKind,
    pub thread_id: &'a ThreadId,
    pub turn_id: Option<&'a TurnId>,
    pub payload: &'a AcpPlanUpdate,
    pub source: &'a AcpAdapterRawSource,
    pub method: &'a str,
    pub raw_payload: serde_json::Value,
}

/// Upstream `makeAcpPlanUpdatedEvent`.
pub fn make_acp_plan_updated_event(
    input: MakeAcpPlanUpdatedEventInput<'_>,
) -> ProviderRuntimeEvent {
    let payload = match input.payload {
        AcpPlanUpdate::Items {
            plan, explanation, ..
        } => TurnPlanUpdatedPayload {
            explanation: explanation
                .as_ref()
                .map(|value| Some(TrimmedNonEmptyString::new(value).expect("plan explanation"))),
            plan: plan
                .iter()
                .map(|step| RuntimePlanStep {
                    step: TrimmedNonEmptyString::new(&step.step).expect("plan step"),
                    status: plan_step_status(step.status),
                })
                .collect(),
        },
        AcpPlanUpdate::Removed { .. } => TurnPlanUpdatedPayload {
            explanation: None,
            plan: Vec::new(),
        },
        AcpPlanUpdate::Markdown { markdown, .. } => TurnPlanUpdatedPayload {
            explanation: None,
            plan: vec![RuntimePlanStep {
                step: TrimmedNonEmptyString::new(markdown).expect("plan markdown"),
                status: RuntimePlanStepStatus::Pending,
            }],
        },
        AcpPlanUpdate::File { uri, .. } => TurnPlanUpdatedPayload {
            explanation: None,
            plan: vec![RuntimePlanStep {
                step: TrimmedNonEmptyString::new(format!("Plan file: {uri}"))
                    .expect("plan file step"),
                status: RuntimePlanStepStatus::Pending,
            }],
        },
        AcpPlanUpdate::Unknown { content_type, .. } => TurnPlanUpdatedPayload {
            explanation: None,
            plan: vec![RuntimePlanStep {
                step: TrimmedNonEmptyString::new(format!(
                    "[Unsupported ACP plan content: {content_type}]"
                ))
                .expect("unsupported plan step"),
                status: RuntimePlanStepStatus::Pending,
            }],
        },
    };
    let mut base = event_base(input.stamp, input.provider, input.thread_id, input.turn_id);
    base.raw = Some(runtime_raw(input.source, input.method, input.raw_payload));
    ProviderRuntimeEvent::TurnPlanUpdated { base, payload }
}

pub struct MakeAcpToolCallEventInput<'a> {
    pub stamp: &'a AcpEventStamp,
    pub provider: &'a ProviderDriverKind,
    pub thread_id: &'a ThreadId,
    pub turn_id: Option<&'a TurnId>,
    pub tool_call: &'a AcpToolCallState,
    pub raw_payload: serde_json::Value,
}

/// Upstream `makeAcpToolCallEvent`.
pub fn make_acp_tool_call_event(input: MakeAcpToolCallEventInput<'_>) -> ProviderRuntimeEvent {
    let runtime_status = runtime_item_status_from_acp_tool_status(input.tool_call.status);
    let item_type = to_canonical_item_type(input.tool_call.kind.as_deref());
    let mut payload = ItemLifecyclePayload {
        item_type,
        status: runtime_status,
        title: input
            .tool_call
            .title
            .as_ref()
            .map(|value| TrimmedNonEmptyString::new(value).expect("tool title")),
        detail: input
            .tool_call
            .detail
            .as_ref()
            .map(|value| TrimmedNonEmptyString::new(value).expect("tool detail")),
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        data: if input.tool_call.data.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(input.tool_call.data.clone()))
        },
        agent_id: None,
        parent_tool_use_id: None,
    };
    if runtime_status.is_none() {
        payload.status = None;
    }
    let mut base = event_base(input.stamp, input.provider, input.thread_id, input.turn_id);
    base.item_id = Some(RuntimeItemId::new(&input.tool_call.tool_call_id).expect("tool call id"));
    base.raw = Some(runtime_raw(
        &RuntimeEventRawSource::new("acp.jsonrpc").expect("known raw source"),
        "session/update",
        input.raw_payload,
    ));
    match input.tool_call.status {
        Some(AcpToolCallStatus::Completed) | Some(AcpToolCallStatus::Failed) => {
            ProviderRuntimeEvent::ItemCompleted { base, payload }
        }
        _ => ProviderRuntimeEvent::ItemUpdated { base, payload },
    }
}

pub struct MakeAcpAssistantItemEventInput<'a> {
    pub stamp: &'a AcpEventStamp,
    pub provider: &'a ProviderDriverKind,
    pub thread_id: &'a ThreadId,
    pub turn_id: Option<&'a TurnId>,
    pub item_id: &'a str,
    pub lifecycle: AcpAssistantItemLifecycle,
}

/// Upstream `makeAcpAssistantItemEvent`.
pub fn make_acp_assistant_item_event(
    input: MakeAcpAssistantItemEventInput<'_>,
) -> ProviderRuntimeEvent {
    let status = match input.lifecycle {
        AcpAssistantItemLifecycle::Completed => RuntimeItemStatus::Completed,
        AcpAssistantItemLifecycle::Started => RuntimeItemStatus::InProgress,
    };
    let payload = ItemLifecyclePayload {
        item_type: CanonicalItemType::AssistantMessage,
        status: Some(status),
        title: None,
        detail: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        data: None,
        agent_id: None,
        parent_tool_use_id: None,
    };
    let mut base = event_base(input.stamp, input.provider, input.thread_id, input.turn_id);
    base.item_id = Some(RuntimeItemId::new(input.item_id).expect("assistant item id"));
    match input.lifecycle {
        AcpAssistantItemLifecycle::Started => ProviderRuntimeEvent::ItemStarted { base, payload },
        AcpAssistantItemLifecycle::Completed => {
            ProviderRuntimeEvent::ItemCompleted { base, payload }
        }
    }
}

pub struct MakeAcpContentDeltaEventInput<'a> {
    pub stamp: &'a AcpEventStamp,
    pub provider: &'a ProviderDriverKind,
    pub thread_id: &'a ThreadId,
    pub turn_id: Option<&'a TurnId>,
    pub item_id: Option<&'a str>,
    pub stream_kind: Option<RuntimeContentStreamKind>,
    pub text: &'a str,
    pub raw_payload: serde_json::Value,
}

/// Upstream `makeAcpContentDeltaEvent`.
pub fn make_acp_content_delta_event(
    input: MakeAcpContentDeltaEventInput<'_>,
) -> ProviderRuntimeEvent {
    let mut base = event_base(input.stamp, input.provider, input.thread_id, input.turn_id);
    if let Some(item_id) = input.item_id {
        base.item_id = Some(RuntimeItemId::new(item_id).expect("content item id"));
    }
    base.raw = Some(runtime_raw(
        &RuntimeEventRawSource::new("acp.jsonrpc").expect("known raw source"),
        "session/update",
        input.raw_payload,
    ));
    let payload = ContentDeltaPayload {
        stream_kind: input
            .stream_kind
            .unwrap_or(RuntimeContentStreamKind::AssistantText),
        delta: input.text.to_owned(),
        content_index: None,
        summary_index: None,
    };
    ProviderRuntimeEvent::ContentDelta { base, payload }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::provider_policy::ProviderApprovalDecision;

    fn stamp() -> AcpEventStamp {
        AcpEventStamp {
            event_id: EventId::new("event-1").expect("event id"),
            created_at: "2026-03-27T00:00:00.000Z".into(),
        }
    }

    fn cursor() -> ProviderDriverKind {
        ProviderDriverKind::new("cursor").expect("cursor provider")
    }

    fn thread() -> ThreadId {
        ThreadId::new("thread-1").expect("thread id")
    }

    fn turn() -> TurnId {
        TurnId::new("turn-1").expect("turn id")
    }

    fn match_object(actual: &ProviderRuntimeEvent, expected: serde_json::Value) {
        fn partial_match(actual: &serde_json::Value, expected: &serde_json::Value) {
            match (actual, expected) {
                (
                    serde_json::Value::Object(actual_map),
                    serde_json::Value::Object(expected_map),
                ) => {
                    for (key, value) in expected_map {
                        partial_match(
                            actual_map
                                .get(key)
                                .unwrap_or_else(|| panic!("missing key {key} in {actual}")),
                            value,
                        );
                    }
                }
                _ => assert_eq!(actual, expected),
            }
        }
        partial_match(
            &serde_json::to_value(actual).expect("serialize event"),
            &expected,
        );
    }

    #[test]
    fn maps_acp_permission_requests_to_canonical_runtime_events() {
        let stamp = stamp();
        let permission_request = AcpPermissionRequest {
            kind: "execute".into(),
            detail: Some("cat package.json".into()),
            tool_call: Some(AcpToolCallState {
                tool_call_id: "tool-1".into(),
                kind: Some("execute".into()),
                title: None,
                status: Some(AcpToolCallStatus::Pending),
                command: Some("cat package.json".into()),
                detail: Some("cat package.json".into()),
                data: serde_json::Map::from_iter([
                    ("toolCallId".into(), json!("tool-1")),
                    ("kind".into(), json!("execute")),
                ]),
            }),
        };

        let opened = make_acp_request_opened_event(MakeAcpRequestOpenedEventInput {
            stamp: &stamp,
            provider: &cursor(),
            thread_id: &thread(),
            turn_id: Some(&turn()),
            request_id: &RuntimeRequestId::new("request-1").expect("request id"),
            permission_request: &permission_request,
            approval_options: None,
            detail: "cat package.json",
            args: json!({ "command": ["cat", "package.json"] }),
            source: &RuntimeEventRawSource::new("acp.jsonrpc").expect("raw source"),
            method: "session/request_permission",
            raw_payload: json!({ "sessionId": "session-1" }),
        });
        match_object(
            &opened,
            json!({
                "type": "request.opened",
                "payload": {
                    "requestType": "exec_command_approval",
                    "detail": "cat package.json",
                },
            }),
        );
        assert!(
            serde_json::to_value(&opened)
                .expect("serialize")
                .pointer("/payload/options")
                .is_none()
        );

        let resolved = make_acp_request_resolved_event(MakeAcpRequestResolvedEventInput {
            stamp: &stamp,
            provider: &cursor(),
            thread_id: &thread(),
            turn_id: Some(&turn()),
            request_id: &RuntimeRequestId::new("request-1").expect("request id"),
            permission_request: &permission_request,
            decision: ProviderApprovalDecision::Accept,
        });
        match_object(
            &resolved,
            json!({
                "type": "request.resolved",
                "payload": {
                    "requestType": "exec_command_approval",
                    "decision": "accept",
                },
            }),
        );
    }

    #[test]
    fn preserves_native_file_approval_without_remembered_allow_choice() {
        let event = make_acp_request_opened_event(MakeAcpRequestOpenedEventInput {
            stamp: &AcpEventStamp {
                event_id: EventId::new("approval-1").expect("event id"),
                created_at: "2026-09-02T00:00:00.000Z".into(),
            },
            provider: &ProviderDriverKind::new("antigravity").expect("provider"),
            thread_id: &thread(),
            turn_id: Some(&turn()),
            request_id: &RuntimeRequestId::new("request-1").expect("request id"),
            permission_request: &AcpPermissionRequest {
                kind: "edit".into(),
                detail: None,
                tool_call: None,
            },
            approval_options: Some(&[
                ProviderApprovalOption {
                    decision: ProviderApprovalDecision::Accept,
                    label: TrimmedNonEmptyString::new("Allow once").expect("label"),
                    warning: None,
                },
                ProviderApprovalOption {
                    decision: ProviderApprovalDecision::Decline,
                    label: TrimmedNonEmptyString::new("Reject").expect("label"),
                    warning: None,
                },
            ]),
            detail: "Edit package.json",
            args: json!({}),
            source: &RuntimeEventRawSource::new("acp.jsonrpc").expect("raw source"),
            method: "session/request_permission",
            raw_payload: json!({ "sessionId": "session-1" }),
        });

        assert_eq!(
            serde_json::to_value(event).expect("serialize")["payload"],
            json!({
                "requestType": "file_change_approval",
                "detail": "Edit package.json",
                "args": {},
                "options": [
                    { "decision": "accept", "label": "Allow once" },
                    { "decision": "decline", "label": "Reject" },
                ],
            })
        );
    }

    #[test]
    fn maps_generic_acp_permission_kinds_to_dynamic_tool_approvals() {
        let stamp = stamp();
        for kind in ["search", "fetch", "other", "unknown", "future-tool-kind"] {
            let permission_request = AcpPermissionRequest {
                kind: kind.into(),
                detail: None,
                tool_call: None,
            };
            let opened = make_acp_request_opened_event(MakeAcpRequestOpenedEventInput {
                stamp: &stamp,
                provider: &cursor(),
                thread_id: &thread(),
                turn_id: Some(&turn()),
                request_id: &RuntimeRequestId::new(format!("request-{kind}")).expect("request id"),
                permission_request: &permission_request,
                approval_options: None,
                detail: kind,
                args: json!({}),
                source: &RuntimeEventRawSource::new("acp.jsonrpc").expect("raw source"),
                method: "session/request_permission",
                raw_payload: json!({ "sessionId": "session-1" }),
            });
            match_object(
                &opened,
                json!({
                    "type": "request.opened",
                    "payload": { "requestType": "dynamic_tool_call" },
                }),
            );

            let resolved = make_acp_request_resolved_event(MakeAcpRequestResolvedEventInput {
                stamp: &stamp,
                provider: &cursor(),
                thread_id: &thread(),
                turn_id: Some(&turn()),
                request_id: &RuntimeRequestId::new(format!("request-{kind}")).expect("request id"),
                permission_request: &permission_request,
                decision: ProviderApprovalDecision::Accept,
            });
            match_object(
                &resolved,
                json!({
                    "type": "request.resolved",
                    "payload": { "requestType": "dynamic_tool_call" },
                }),
            );
        }
    }

    #[test]
    fn maps_acp_core_plan_tool_call_and_content_updates() {
        let stamp = stamp();
        let turn_id = turn();

        let plan = make_acp_plan_updated_event(MakeAcpPlanUpdatedEventInput {
            stamp: &stamp,
            provider: &cursor(),
            thread_id: &thread(),
            turn_id: Some(&turn_id),
            payload: &AcpPlanUpdate::Items {
                native_plan_id: "plan-1".into(),
                explanation: None,
                plan: vec![crate::runtime_model::AcpPlanStep {
                    step: "Inspect state".into(),
                    status: AcpPlanStepStatus::InProgress,
                }],
            },
            source: &RuntimeEventRawSource::new("acp.cursor.extension").expect("raw source"),
            method: "cursor/update_todos",
            raw_payload: json!({ "todos": [] }),
        });
        match_object(
            &plan,
            json!({
                "type": "turn.plan.updated",
                "raw": { "method": "cursor/update_todos" },
            }),
        );

        let tool = make_acp_tool_call_event(MakeAcpToolCallEventInput {
            stamp: &stamp,
            provider: &cursor(),
            thread_id: &thread(),
            turn_id: Some(&turn_id),
            tool_call: &AcpToolCallState {
                tool_call_id: "tool-1".into(),
                kind: Some("execute".into()),
                title: Some("Terminal".into()),
                status: Some(AcpToolCallStatus::Completed),
                command: None,
                detail: Some("bun run test".into()),
                data: serde_json::Map::from_iter([("command".into(), json!("bun run test"))]),
            },
            raw_payload: json!({ "sessionId": "session-1" }),
        });
        match_object(
            &tool,
            json!({
                "type": "item.completed",
                "payload": {
                    "itemType": "command_execution",
                    "status": "completed",
                },
            }),
        );

        let content = make_acp_content_delta_event(MakeAcpContentDeltaEventInput {
            stamp: &stamp,
            provider: &cursor(),
            thread_id: &thread(),
            turn_id: Some(&turn_id),
            item_id: Some("assistant:session-1:segment:0"),
            stream_kind: None,
            text: "hello",
            raw_payload: json!({ "sessionId": "session-1" }),
        });
        match_object(
            &content,
            json!({
                "type": "content.delta",
                "itemId": "assistant:session-1:segment:0",
                "payload": { "delta": "hello" },
            }),
        );

        let assistant = make_acp_assistant_item_event(MakeAcpAssistantItemEventInput {
            stamp: &stamp,
            provider: &cursor(),
            thread_id: &thread(),
            turn_id: Some(&turn_id),
            item_id: "assistant:session-1:segment:0",
            lifecycle: AcpAssistantItemLifecycle::Started,
        });
        match_object(
            &assistant,
            json!({
                "type": "item.started",
                "itemId": "assistant:session-1:segment:0",
                "payload": {
                    "itemType": "assistant_message",
                    "status": "inProgress",
                },
            }),
        );
    }

    #[test]
    fn maps_thoughts_to_the_reasoning_stream() {
        let event = make_acp_content_delta_event(MakeAcpContentDeltaEventInput {
            stamp: &AcpEventStamp {
                event_id: EventId::new("thought-1").expect("event id"),
                created_at: "2026-09-02T00:00:00.000Z".into(),
            },
            provider: &cursor(),
            thread_id: &thread(),
            turn_id: Some(&turn()),
            item_id: None,
            stream_kind: Some(RuntimeContentStreamKind::ReasoningText),
            text: "Inspect the current implementation first.",
            raw_payload: json!({ "sessionId": "session-1" }),
        });
        match_object(
            &event,
            json!({
                "type": "content.delta",
                "payload": {
                    "streamKind": "reasoning_text",
                    "delta": "Inspect the current implementation first.",
                },
            }),
        );
    }
}
