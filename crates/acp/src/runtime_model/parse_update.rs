use serde_json::Value;
use turin_contracts::base_schemas::NonNegativeInt;
use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString};
use turin_contracts::orchestration_v2::OrchestrationV2ProviderThreadNativeMetadata;
use turin_contracts::provider_runtime::{ThreadTokenUsageCost, ThreadTokenUsageSnapshot};

use super::content::acp_content_block_display_text;
use super::session_setup::{AcpSessionSetupResponse, parse_session_mode_state};
use super::tool_call::{
    AcpToolCallState, AcpToolCallStatus, MakeToolCallStateInput, bound_tool_call_raw_payload,
    make_tool_call_state, parse_typed_tool_call_state,
};
use super::util::{bounded_content_metadata, is_record};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpPlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AcpPlanUpdate {
    Items {
        native_plan_id: String,
        explanation: Option<String>,
        plan: Vec<AcpPlanStep>,
    },
    Markdown {
        native_plan_id: String,
        markdown: String,
    },
    File {
        native_plan_id: String,
        uri: String,
    },
    Unknown {
        native_plan_id: String,
        content_type: String,
    },
    Removed {
        native_plan_id: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpPlanStep {
    pub step: String,
    pub status: AcpPlanStepStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AcpParsedSessionEvent {
    ModeChanged {
        mode_id: String,
    },
    AvailableCommandsUpdated {
        available_commands: Value,
        raw_payload: Value,
    },
    ConfigOptionsUpdated {
        config_options: Value,
        raw_payload: Value,
    },
    AssistantItemStarted {
        item_id: String,
    },
    AssistantItemCompleted {
        item_id: String,
    },
    PlanUpdated {
        payload: AcpPlanUpdate,
        raw_payload: Value,
    },
    ToolCallUpdated {
        tool_call: AcpToolCallState,
        raw_payload: Value,
    },
    ContentDelta {
        item_id: Option<String>,
        message_id: Option<String>,
        text: String,
        raw_payload: Value,
    },
    ThoughtDelta {
        text: String,
        raw_payload: Value,
    },
    UsageUpdated {
        usage: ThreadTokenUsageSnapshot,
        raw_payload: Value,
    },
    SessionInfoUpdated {
        metadata: OrchestrationV2ProviderThreadNativeMetadata,
        raw_payload: Value,
    },
    UnknownUpdate {
        update_type: String,
        raw_payload: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseSessionUpdateResult {
    pub mode_id: Option<String>,
    pub events: Vec<AcpParsedSessionEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpPermissionRequest {
    pub kind: String,
    pub detail: Option<String>,
    pub tool_call: Option<AcpToolCallState>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedTerminalIds {
    pub tool_call_id: String,
    pub terminal_ids: Vec<String>,
}

fn usage_snapshot(
    used_tokens: NonNegativeInt,
    max_tokens: Option<PositiveInt>,
    cost: Option<ThreadTokenUsageCost>,
) -> ThreadTokenUsageSnapshot {
    ThreadTokenUsageSnapshot {
        used_tokens,
        total_processed_tokens: None,
        max_tokens,
        input_tokens: None,
        cached_input_tokens: None,
        output_tokens: None,
        reasoning_output_tokens: None,
        last_used_tokens: None,
        last_input_tokens: None,
        last_cached_input_tokens: None,
        last_output_tokens: None,
        last_reasoning_output_tokens: None,
        tool_uses: None,
        duration_ms: None,
        compacts_automatically: None,
        auto_compact_threshold: None,
        cost,
    }
}

fn normalize_plan_step_status(raw: &str) -> AcpPlanStepStatus {
    match raw {
        "completed" => AcpPlanStepStatus::Completed,
        "in_progress" | "inProgress" => AcpPlanStepStatus::InProgress,
        _ => AcpPlanStepStatus::Pending,
    }
}

pub fn embedded_terminal_ids_from_session_update(
    notification: &Value,
) -> Option<EmbeddedTerminalIds> {
    let update = notification.get("update")?;
    let session_update = update.get("sessionUpdate").and_then(Value::as_str)?;
    if session_update != "tool_call" && session_update != "tool_call_update" {
        return None;
    }
    let terminal_ids: Vec<String> = update
        .get("content")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("terminal"))
                .filter_map(|entry| {
                    entry
                        .get("terminalId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    if terminal_ids.is_empty() {
        return None;
    }
    Some(EmbeddedTerminalIds {
        tool_call_id: update
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        terminal_ids,
    })
}

pub fn parse_permission_request(params: &Value) -> AcpPermissionRequest {
    let tool_call_obj = params.get("toolCall").unwrap_or(&Value::Null);
    let tool_call = make_tool_call_state(
        MakeToolCallStateInput {
            tool_call_id: tool_call_obj
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or(""),
            title: tool_call_obj.get("title"),
            kind: tool_call_obj.get("kind"),
            status: tool_call_obj.get("status"),
            raw_input: tool_call_obj.get("rawInput"),
            raw_output: tool_call_obj.get("rawOutput"),
            content: tool_call_obj
                .get("content")
                .and_then(Value::as_array)
                .map(|a| a.as_slice()),
            locations: tool_call_obj
                .get("locations")
                .and_then(Value::as_array)
                .map(|a| a.as_slice()),
            meta: None,
        },
        Some(AcpToolCallStatus::Pending),
    );
    let kind =
        normalize_tool_kind_wire(tool_call_obj.get("kind")).unwrap_or_else(|| "unknown".to_owned());
    let detail = tool_call
        .as_ref()
        .and_then(|tc| tc.command.as_deref())
        .or(tool_call.as_ref().and_then(|tc| tc.title.as_deref()))
        .or(tool_call.as_ref().and_then(|tc| tc.detail.as_deref()))
        .map(str::to_owned)
        .or_else(|| {
            params
                .get("sessionId")
                .and_then(Value::as_str)
                .map(|id| format!("Session {id}"))
        });
    AcpPermissionRequest {
        kind,
        detail,
        tool_call,
    }
}

fn normalize_tool_kind_wire(kind: Option<&Value>) -> Option<String> {
    kind.and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
}

pub fn parse_session_update_event(params: &Value) -> ParseSessionUpdateResult {
    let upd = params.get("update").unwrap_or(&Value::Null);
    let mut events = Vec::new();
    let mut mode_id = None;
    let session_update = upd
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("");
    match session_update {
        "config_option_update" => {
            if let Ok(setup) =
                serde_json::from_value::<AcpSessionSetupResponse>(serde_json::json!({
                    "configOptions": upd.get("configOptions"),
                }))
                && let Some(modes) = parse_session_mode_state(&setup)
            {
                mode_id = Some(modes.current_mode_id.clone());
                events.push(AcpParsedSessionEvent::ModeChanged {
                    mode_id: modes.current_mode_id,
                });
            }
            events.push(AcpParsedSessionEvent::ConfigOptionsUpdated {
                config_options: upd
                    .get("configOptions")
                    .cloned()
                    .unwrap_or(Value::Array(vec![])),
                raw_payload: params.clone(),
            });
        }
        "available_commands_update" => {
            events.push(AcpParsedSessionEvent::AvailableCommandsUpdated {
                available_commands: upd
                    .get("availableCommands")
                    .cloned()
                    .unwrap_or(Value::Array(vec![])),
                raw_payload: params.clone(),
            });
        }
        "current_mode_update" => {
            let id = upd
                .get("currentModeId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
            if let Some(id) = id.clone() {
                mode_id = Some(id.clone());
                events.push(AcpParsedSessionEvent::ModeChanged { mode_id: id });
            }
        }
        "plan" => {
            let plan: Vec<AcpPlanStep> = upd
                .get("entries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .enumerate()
                        .filter_map(|(index, entry)| {
                            let content = entry.get("content").and_then(Value::as_str)?;
                            let status = entry
                                .get("status")
                                .and_then(Value::as_str)
                                .unwrap_or("pending");
                            Some(AcpPlanStep {
                                step: {
                                    let trimmed = content.trim();
                                    if trimmed.is_empty() {
                                        format!("Step {}", index + 1)
                                    } else {
                                        trimmed.to_owned()
                                    }
                                },
                                status: normalize_plan_step_status(status),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !plan.is_empty() {
                events.push(AcpParsedSessionEvent::PlanUpdated {
                    payload: AcpPlanUpdate::Items {
                        native_plan_id: "legacy".to_owned(),
                        explanation: None,
                        plan,
                    },
                    raw_payload: params.clone(),
                });
            }
        }
        "plan_update" => {
            let plan_obj = upd.get("plan").unwrap_or(&Value::Null);
            let native_plan_id = plan_obj
                .get("planId")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if !native_plan_id.is_empty() {
                let plan_type = plan_obj.get("type").and_then(Value::as_str).unwrap_or("");
                let payload = match plan_type {
                    "items" if plan_obj.get("entries").and_then(Value::as_array).is_some() => {
                        let plan: Vec<AcpPlanStep> = plan_obj
                            .get("entries")
                            .and_then(Value::as_array)
                            .map(|entries| {
                                entries
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(index, entry)| {
                                        let content =
                                            entry.get("content").and_then(Value::as_str)?;
                                        let status = entry
                                            .get("status")
                                            .and_then(Value::as_str)
                                            .unwrap_or("pending");
                                        Some(AcpPlanStep {
                                            step: {
                                                let trimmed = content.trim();
                                                if trimmed.is_empty() {
                                                    format!("Step {}", index + 1)
                                                } else {
                                                    trimmed.to_owned()
                                                }
                                            },
                                            status: normalize_plan_step_status(status),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        AcpPlanUpdate::Items {
                            native_plan_id: native_plan_id.to_owned(),
                            explanation: None,
                            plan,
                        }
                    }
                    "markdown" => AcpPlanUpdate::Markdown {
                        native_plan_id: native_plan_id.to_owned(),
                        markdown: plan_obj
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    },
                    "file" => AcpPlanUpdate::File {
                        native_plan_id: native_plan_id.to_owned(),
                        uri: plan_obj
                            .get("uri")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    },
                    other => AcpPlanUpdate::Unknown {
                        native_plan_id: native_plan_id.to_owned(),
                        content_type: other.to_owned(),
                    },
                };
                events.push(AcpParsedSessionEvent::PlanUpdated {
                    payload,
                    raw_payload: params.clone(),
                });
            }
        }
        "plan_removed" => {
            let native_plan_id = upd
                .get("planId")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if !native_plan_id.is_empty() {
                events.push(AcpParsedSessionEvent::PlanUpdated {
                    payload: AcpPlanUpdate::Removed {
                        native_plan_id: native_plan_id.to_owned(),
                    },
                    raw_payload: params.clone(),
                });
            }
        }
        "tool_call" => {
            if let Some(tool_call) =
                parse_typed_tool_call_state(upd, Some(AcpToolCallStatus::Pending))
            {
                events.push(AcpParsedSessionEvent::ToolCallUpdated {
                    raw_payload: bound_tool_call_raw_payload(params, upd, &tool_call),
                    tool_call,
                });
            }
        }
        "tool_call_update" => {
            if let Some(tool_call) = parse_typed_tool_call_state(upd, None) {
                events.push(AcpParsedSessionEvent::ToolCallUpdated {
                    raw_payload: bound_tool_call_raw_payload(params, upd, &tool_call),
                    tool_call,
                });
            }
        }
        "agent_message_chunk" => {
            if let Some(text) = upd
                .get("content")
                .and_then(acp_content_block_display_text)
                .filter(|text| !text.is_empty())
            {
                events.push(AcpParsedSessionEvent::ContentDelta {
                    item_id: None,
                    message_id: upd
                        .get("messageId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    text,
                    raw_payload: params.clone(),
                });
            }
        }
        "agent_thought_chunk" => {
            let content = upd.get("content").unwrap_or(&Value::Null);
            if content.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = content.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                events.push(AcpParsedSessionEvent::ThoughtDelta {
                    text: text.to_owned(),
                    raw_payload: params.clone(),
                });
            }
        }
        "compaction_update" => {
            let summary: String = upd
                .get("summary")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(acp_content_block_display_text)
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let status = match upd.get("status").and_then(Value::as_str) {
                Some("completed") => AcpToolCallStatus::Completed,
                Some("failed") | Some("cancelled") => AcpToolCallStatus::Failed,
                _ => AcpToolCallStatus::InProgress,
            };
            let compaction_id = upd
                .get("compactionId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let raw_output = upd
                .get("error")
                .cloned()
                .or_else(|| (!summary.is_empty()).then(|| Value::String(summary.clone())));
            if let Some(tool_call) = make_tool_call_state(
                MakeToolCallStateInput {
                    tool_call_id: &format!("acp-compaction:{compaction_id}"),
                    title: Some(&Value::String("Compact context".to_owned())),
                    kind: Some(&Value::String("think".to_owned())),
                    status: Some(&Value::String(
                        match status {
                            AcpToolCallStatus::Completed => "completed",
                            AcpToolCallStatus::Failed => "failed",
                            _ => "in_progress",
                        }
                        .to_owned(),
                    )),
                    raw_input: None,
                    raw_output: raw_output.as_ref(),
                    content: None,
                    locations: None,
                    meta: upd.get("_meta"),
                },
                None,
            ) {
                events.push(AcpParsedSessionEvent::ToolCallUpdated {
                    tool_call,
                    raw_payload: params.clone(),
                });
            }
        }
        "compaction_summary_chunk" => {
            let text = upd.get("content").and_then(acp_content_block_display_text);
            let compaction_id = upd
                .get("compactionId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let raw_output = text.map(Value::String);
            if let Some(tool_call) = make_tool_call_state(
                MakeToolCallStateInput {
                    tool_call_id: &format!("acp-compaction:{compaction_id}"),
                    title: Some(&Value::String("Compact context".to_owned())),
                    kind: Some(&Value::String("think".to_owned())),
                    status: Some(&Value::String("in_progress".to_owned())),
                    raw_input: None,
                    raw_output: raw_output.as_ref(),
                    content: None,
                    locations: None,
                    meta: upd.get("_meta"),
                },
                None,
            ) {
                events.push(AcpParsedSessionEvent::ToolCallUpdated {
                    tool_call,
                    raw_payload: params.clone(),
                });
            }
        }
        "state_update" => {
            if upd.get("state").and_then(Value::as_str) == Some("idle")
                && let Some(usage) = upd.get("usage").and_then(is_record)
                && let Some(total) = usage.get("totalTokens").and_then(Value::as_u64)
            {
                events.push(AcpParsedSessionEvent::UsageUpdated {
                    usage: usage_snapshot(total, None, None),
                    raw_payload: params.clone(),
                });
            }
        }
        "usage_update" => {
            let used = upd.get("used").and_then(Value::as_u64).unwrap_or(0);
            let size = upd.get("size").and_then(Value::as_u64).unwrap_or(0);
            let cost = upd.get("cost").and_then(is_record).and_then(|cost| {
                let currency = cost
                    .get("currency")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(|c| c.chars().take(32).collect::<String>())
                    .and_then(|c| TrimmedNonEmptyString::new(c).ok())?;
                let amount = cost.get("amount").and_then(Value::as_f64)?;
                Some(ThreadTokenUsageCost { amount, currency })
            });
            events.push(AcpParsedSessionEvent::UsageUpdated {
                usage: usage_snapshot(used, PositiveInt::try_from(size).ok(), cost),
                raw_payload: params.clone(),
            });
        }
        "session_info_update" => {
            let mut metadata = OrchestrationV2ProviderThreadNativeMetadata {
                model_selection: None,
                title: None,
                updated_at: None,
                item_identity_version: None,
            };
            match upd.get("title") {
                Some(v) if v.is_null() => metadata.title = Some(None),
                Some(v) => {
                    if let Some(text) = v
                        .as_str()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(|t| t.chars().take(1024).collect::<String>())
                        .and_then(|t| TrimmedNonEmptyString::new(t).ok())
                    {
                        metadata.title = Some(Some(text));
                    }
                }
                None => {}
            }
            match upd.get("updatedAt") {
                Some(v) if v.is_null() => metadata.updated_at = Some(None),
                Some(v) => {
                    if let Some(text) = v
                        .as_str()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(|t| t.chars().take(128).collect::<String>())
                        .and_then(|t| TrimmedNonEmptyString::new(t).ok())
                    {
                        metadata.updated_at = Some(Some(text));
                    }
                }
                None => {}
            }
            events.push(AcpParsedSessionEvent::SessionInfoUpdated {
                metadata,
                raw_payload: params.clone(),
            });
        }
        "_t3_unknown" => {
            let update_type = bounded_content_metadata(
                upd.get("originalSessionUpdate").and_then(Value::as_str),
                128,
            );
            events.push(AcpParsedSessionEvent::UnknownUpdate {
                update_type: if update_type.is_empty() {
                    "unknown".to_owned()
                } else {
                    update_type
                },
                raw_payload: params.clone(),
            });
        }
        _ => {}
    }
    ParseSessionUpdateResult { mode_id, events }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_model::{
        AcpToolCallEmitDecisionInput, decide_tool_call_update_emission, merge_tool_call_state,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn projects_typed_acp_tool_call_updates_into_runtime_events() {
        let created = parse_session_update_event(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tool-1",
                "title": "Terminal",
                "kind": "execute",
                "status": "pending",
                "rawInput": {
                    "executable": "bun",
                    "args": ["run", "typecheck"],
                },
                "content": [{
                    "type": "content",
                    "content": { "type": "text", "text": "Running checks" },
                }],
            },
        }));
        assert_eq!(created.events.len(), 1);
        let AcpParsedSessionEvent::ToolCallUpdated { tool_call, .. } = &created.events[0] else {
            panic!("expected tool call");
        };
        assert_eq!(tool_call.tool_call_id, "tool-1");
        assert_eq!(tool_call.command.as_deref(), Some("bun run typecheck"));
        assert_eq!(tool_call.title.as_deref(), Some("Ran command"));

        let updated = parse_session_update_event(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tool-1",
                "status": "completed",
                "rawOutput": { "exitCode": 0 },
            },
        }));
        let merged = merge_tool_call_state(
            match &created.events[0] {
                AcpParsedSessionEvent::ToolCallUpdated { tool_call, .. } => Some(tool_call),
                _ => None,
            },
            match &updated.events[0] {
                AcpParsedSessionEvent::ToolCallUpdated { tool_call, .. } => tool_call.clone(),
                _ => panic!("expected update"),
            },
        );
        assert_eq!(merged.tool_call_id, "tool-1");
        assert_eq!(merged.status, Some(AcpToolCallStatus::Completed));
        assert_eq!(merged.title.as_deref(), Some("Ran command"));
        assert_eq!(merged.detail.as_deref(), Some("bun run typecheck"));
    }

    #[test]
    fn clears_prior_acp_locations_when_an_update_explicitly_replaces_them() {
        fn tool_call(locations: Option<Value>) -> AcpToolCallState {
            let mut update = json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "read-1",
                "kind": "read",
            });
            if let Some(locations) = locations {
                update["locations"] = locations;
            }
            match &parse_session_update_event(
                &json!({ "sessionId": "session-1", "update": update }),
            )
            .events[0]
            {
                AcpParsedSessionEvent::ToolCallUpdated { tool_call, .. } => tool_call.clone(),
                _ => panic!("expected tool call"),
            }
        }
        let created = tool_call(Some(json!([{ "path": "README" }])));
        assert_eq!(
            merge_tool_call_state(Some(&created), tool_call(None))
                .data
                .get("locations"),
            created.data.get("locations")
        );
        let cleared = merge_tool_call_state(Some(&created), tool_call(Some(json!([]))));
        assert_eq!(cleared.data.get("locations"), Some(&Value::Array(vec![])));
        assert_eq!(
            merge_tool_call_state(Some(&created), tool_call(Some(Value::Null)))
                .data
                .get("locations"),
            Some(&Value::Array(vec![]))
        );
        assert!(
            decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                previous: Some(&created),
                next: &cleared,
                last_emitted_detail_length: Some(0),
                skipped_since_emit: 0,
            })
            .emit
        );
    }

    #[test]
    fn trims_padded_current_mode_updates_before_emitting_a_mode_change() {
        let result = parse_session_update_event(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "current_mode_update",
                "currentModeId": " code ",
            },
        }));
        assert_eq!(result.mode_id.as_deref(), Some("code"));
        assert_eq!(
            result.events,
            vec![AcpParsedSessionEvent::ModeChanged {
                mode_id: "code".to_owned(),
            }]
        );
    }

    #[test]
    fn keeps_permission_request_parsing_compatible_with_loose_extension_payloads() {
        let request = parse_permission_request(&json!({
            "sessionId": "session-1",
            "options": [{
                "optionId": "allow-once",
                "name": "Allow once",
                "kind": "allow_once",
            }],
            "toolCall": {
                "toolCallId": "tool-1",
                "title": "`cat package.json`",
                "kind": "execute",
                "status": "pending",
                "content": [{
                    "type": "content",
                    "content": { "type": "text", "text": "Not in allowlist" },
                }],
            },
        }));
        assert_eq!(request.kind, "execute");
        assert_eq!(request.detail.as_deref(), Some("cat package.json"));
        let tool_call = request.tool_call.expect("tool call");
        assert_eq!(tool_call.tool_call_id, "tool-1");
        assert_eq!(tool_call.kind.as_deref(), Some("execute"));
        assert_eq!(tool_call.status, Some(AcpToolCallStatus::Pending));
        assert_eq!(tool_call.command.as_deref(), Some("cat package.json"));
    }

    #[test]
    fn embedded_terminal_ids_from_session_update_collects_terminal_ids() {
        assert_eq!(
            embedded_terminal_ids_from_session_update(&json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call_1",
                    "status": "in_progress",
                    "content": [
                        { "type": "terminal", "terminalId": "t3-term-9" },
                        { "type": "content", "content": { "type": "text", "text": "noise" } },
                    ],
                },
            })),
            Some(EmbeddedTerminalIds {
                tool_call_id: "call_1".to_owned(),
                terminal_ids: vec!["t3-term-9".to_owned()],
            })
        );
        assert!(
            embedded_terminal_ids_from_session_update(&json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call_2",
                    "title": "Tool",
                    "status": "pending",
                },
            }))
            .is_none()
        );
    }
}
