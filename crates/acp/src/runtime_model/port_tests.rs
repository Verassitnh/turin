//! Remaining ports of upstream runtimeModel.test.ts (AcpRuntimeModel + MCP + embedded terminal).

use super::*;
use crate::runtime_model::parse_update::{AcpParsedSessionEvent, AcpPlanStepStatus, AcpPlanUpdate};
use crate::runtime_model::tool_call::{AcpToolCallEmitDecisionInput, AcpToolCallStatus};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

fn tool_call_json(state: &AcpToolCallState) -> Value {
    json!({
        "toolCallId": state.tool_call_id,
        "kind": state.kind,
        "title": state.title,
        "status": state.status.map(|s| match s {
            AcpToolCallStatus::Pending => "pending",
            AcpToolCallStatus::InProgress => "inProgress",
            AcpToolCallStatus::RequiresAction => "requiresAction",
            AcpToolCallStatus::Completed => "completed",
            AcpToolCallStatus::Failed => "failed",
        }),
        "command": state.command,
        "detail": state.detail,
        "data": state.data,
    })
}

fn tool_call_from_update(update: Value) -> AcpToolCallState {
    let parsed = parse_session_update_event(&json!({ "sessionId": "session-1", "update": update }));
    let event = parsed
        .events
        .into_iter()
        .find(|e| matches!(e, AcpParsedSessionEvent::ToolCallUpdated { .. }))
        .expect("expected a tool call event");
    match event {
        AcpParsedSessionEvent::ToolCallUpdated { tool_call, .. } => tool_call,
        _ => unreachable!(),
    }
}

#[test]
fn projects_typed_acp_plan_and_content_updates() {
    let plan_result = parse_session_update_event(&json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "plan",
            "entries": [
                { "content": " Inspect state ", "priority": "high", "status": "completed" },
                { "content": "", "priority": "medium", "status": "in_progress" },
            ],
        },
    }));
    assert_eq!(plan_result.events.len(), 1);
    match &plan_result.events[0] {
        AcpParsedSessionEvent::PlanUpdated { payload, .. } => match payload {
            AcpPlanUpdate::Items {
                native_plan_id,
                plan,
                ..
            } => {
                assert_eq!(native_plan_id, "legacy");
                assert_eq!(plan.len(), 2);
                assert_eq!(plan[0].step, "Inspect state");
                assert_eq!(plan[0].status, AcpPlanStepStatus::Completed);
                assert_eq!(plan[1].step, "Step 2");
                assert_eq!(plan[1].status, AcpPlanStepStatus::InProgress);
            }
            _ => panic!("expected items plan"),
        },
        _ => panic!("expected plan event"),
    }

    let content_result = parse_session_update_event(&json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hello from acp" },
        },
    }));
    match &content_result.events[0] {
        AcpParsedSessionEvent::ContentDelta { text, .. } => {
            assert_eq!(text, "hello from acp");
        }
        _ => panic!("expected content delta"),
    }
}

#[test]
fn keeps_thought_chunks_separate_from_assistant_text() {
    let notification = json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": "Inspect the current implementation first." },
        },
    });
    match &parse_session_update_event(&notification).events[0] {
        AcpParsedSessionEvent::ThoughtDelta { text, raw_payload } => {
            assert_eq!(text, "Inspect the current implementation first.");
            assert_eq!(raw_payload, &notification);
        }
        _ => panic!("expected thought delta"),
    }
}

#[test]
fn preserves_native_command_inputs_and_empty_command_lists() {
    let available_commands = json!([
        { "name": "plan", "description": "Plan a task", "input": { "type": "text", "hint": "task" } },
        { "name": "logout", "description": "Sign out" },
    ]);
    for commands in [available_commands.clone(), json!([])] {
        let notification = json!({
            "sessionId": "session-1",
            "update": { "sessionUpdate": "available_commands_update", "availableCommands": commands },
        });
        match &parse_session_update_event(&notification).events[0] {
            AcpParsedSessionEvent::AvailableCommandsUpdated {
                available_commands,
                raw_payload,
            } => {
                assert_eq!(available_commands, &commands);
                assert_eq!(raw_payload, &notification);
            }
            _ => panic!("expected commands update"),
        }
    }
}

#[test]
fn bounds_oversized_cumulative_tool_call_update_content_buffer_to_a_tail_window() {
    let huge_text: String = (0..2_000)
        .map(|i| format!("line {i}: {}", "x".repeat(50)))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(huge_text.len() > 60_000);

    let result = parse_session_update_event(&json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "tool_call_update",
            "toolCallId": "tool-1",
            "kind": "other",
            "status": "in_progress",
            "content": [{ "type": "content", "content": { "type": "text", "text": huge_text } }],
        },
    }));
    let AcpParsedSessionEvent::ToolCallUpdated {
        tool_call,
        raw_payload,
    } = &result.events[0]
    else {
        panic!("expected tool call");
    };
    let detail = tool_call.detail.as_ref().expect("detail");
    assert_eq!(detail.len(), 8_028);
    assert!(detail.starts_with("[Earlier output truncated]"));
    assert!(detail.ends_with(&huge_text[huge_text.len() - 100..]));
    let raw_update = raw_payload.get("update").expect("update");
    let text_len = raw_update["content"][0]["content"]["text"]
        .as_str()
        .expect("text")
        .len();
    assert!(text_len < 8_100);
    assert!(serde_json::to_string(raw_payload).unwrap().len() < huge_text.len());
}

#[test]
fn coalesces_rapid_cumulative_tool_call_updates_for_redrawing_progress_bar() {
    let mut previous: Option<AcpToolCallState> = None;
    let mut last_emitted_detail_length = None;
    let mut skipped_since_emit = 0;
    let mut emitted_count = 0;
    let mut emitted_bytes = 0;
    let mut largest_emitted_event_bytes = 0;
    let mut final_detail = None;
    let mut cumulative_buffer = String::new();
    let mut notification_bytes = 0usize;

    for i in 0..1_000 {
        cumulative_buffer.push_str(&format!("frame {i}: {}\n", "#".repeat(50)));
        let is_last = i == 999;
        let notification = json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tool-1",
                "kind": "other",
                "status": if is_last { "completed" } else { "in_progress" },
                "content": [{ "type": "content", "content": { "type": "text", "text": cumulative_buffer } }],
            },
        });
        notification_bytes += serde_json::to_string(&notification).unwrap().len();
        let events = parse_session_update_event(&notification).events;
        let AcpParsedSessionEvent::ToolCallUpdated {
            tool_call,
            raw_payload,
        } = &events[0]
        else {
            continue;
        };
        let merged = merge_tool_call_state(previous.as_ref(), tool_call.clone());
        let decision = decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
            previous: previous.as_ref(),
            next: &merged,
            last_emitted_detail_length,
            skipped_since_emit,
        });
        previous = Some(merged);
        skipped_since_emit = decision.skipped_since_emit;
        if decision.emit {
            emitted_count += 1;
            let event_bytes = serde_json::to_string(&json!({
                "toolCall": tool_call_json(previous.as_ref().expect("previous")),
                "rawPayload": raw_payload,
            }))
            .unwrap()
            .len();
            emitted_bytes += event_bytes;
            largest_emitted_event_bytes = largest_emitted_event_bytes.max(event_bytes);
            last_emitted_detail_length = previous
                .as_ref()
                .and_then(|s| s.detail.as_ref().map(String::len));
            final_detail = previous.as_ref().and_then(|s| s.detail.clone());
        }
    }
    assert!(notification_bytes > 31_000_000);
    assert_eq!(emitted_count, 114);
    assert!(largest_emitted_event_bytes < 25_000);
    assert!(emitted_bytes < 2_600_000);
    let final_detail = final_detail.expect("final detail");
    assert!(final_detail.ends_with(&format!("frame 999: {}", "#".repeat(50))));
}

#[test]
fn keeps_non_text_tool_call_content_entries_in_order_when_bounding_oversized_text() {
    let huge_prefix = "x".repeat(25_000);
    let huge_tail = "y".repeat(25_000);
    let events = parse_session_update_event(&json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "tool_call_update",
            "toolCallId": "tool-1",
            "kind": "edit",
            "status": "in_progress",
            "content": [
                { "type": "content", "content": { "type": "text", "text": huge_prefix } },
                { "type": "diff", "path": "/repo/file.ts", "oldText": "before", "newText": "after" },
                { "type": "content", "content": { "type": "text", "text": huge_tail } },
                { "type": "diff", "path": "/repo/other.ts", "oldText": "old", "newText": "new" },
                { "type": "content", "content": { "type": "text", "text": "   " } },
            ],
        },
    }))
    .events;
    let AcpParsedSessionEvent::ToolCallUpdated { tool_call, .. } = &events[0] else {
        panic!("expected tool call");
    };
    let content = tool_call
        .data
        .get("content")
        .and_then(Value::as_array)
        .expect("content");
    assert_eq!(content.len(), 3);
    assert_eq!(
        content[0],
        json!({ "type": "diff", "path": "/repo/file.ts", "oldText": "before", "newText": "after" })
    );
    let text = content[1]["content"]["text"].as_str().expect("text");
    assert!(text.len() < 8_100);
    assert!(text.ends_with(&huge_tail[huge_tail.len() - 100..]));
    assert_eq!(
        content[2],
        json!({ "type": "diff", "path": "/repo/other.ts", "oldText": "old", "newText": "new" })
    );
}

#[test]
fn preserves_message_identity_usage_metadata_and_non_text_output() {
    let message = parse_session_update_event(&json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "messageId": "message-1",
            "content": {
                "type": "resource_link",
                "name": "report.txt",
                "title": "Report",
                "description": "Generated artifact",
                "uri": "file:///workspace/report.txt",
            },
        },
    }));
    match &message.events[0] {
        AcpParsedSessionEvent::ContentDelta {
            message_id, text, ..
        } => {
            assert_eq!(message_id.as_deref(), Some("message-1"));
            assert_eq!(
                text,
                "Report: Generated artifact\nfile:///workspace/report.txt"
            );
        }
        _ => panic!("expected content"),
    }
    let usage = parse_session_update_event(&json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "usage_update",
            "used": 2500,
            "size": 10000,
            "cost": { "amount": 0.42, "currency": "USD" },
        },
    }));
    match &usage.events[0] {
        AcpParsedSessionEvent::UsageUpdated { usage, .. } => {
            assert_eq!(usage.used_tokens, 2500);
            assert_eq!(usage.max_tokens.map(|v| v.get()), Some(10000));
            let cost = usage.cost.as_ref().expect("cost");
            assert_eq!(cost.amount, 0.42);
            assert_eq!(cost.currency.as_str(), "USD");
        }
        _ => panic!("expected usage"),
    }
}

#[test]
fn projects_binary_acp_content_without_retaining_encoded_payloads() {
    let encoded = "sensitive-base64-payload";
    let binary = acp_content_block_display_text(&json!({
        "type": "resource",
        "resource": {
            "blob": encoded,
            "mimeType": "application/octet-stream",
            "uri": "file:///workspace/archive.bin",
        },
    }));
    let image = acp_content_block_display_text(&json!({
        "type": "image",
        "data": encoded,
        "mimeType": "image/png",
        "uri": "file:///workspace/image.png",
    }));
    assert!(!format!("{binary:?}{image:?}").contains(encoded));
    let tool_update = parse_session_update_event(&json!({
        "sessionId": "session-1",
        "update": {
            "sessionUpdate": "tool_call",
            "toolCallId": "tool-with-image",
            "title": "Generated image",
            "content": [{
                "type": "content",
                "content": {
                    "type": "image",
                    "data": encoded,
                    "mimeType": "image/png",
                    "uri": format!("data:image/png;base64,{encoded}"),
                },
            }],
        },
    }));
    let event = tool_update.events.first().expect("event");
    let AcpParsedSessionEvent::ToolCallUpdated { tool_call, .. } = event else {
        panic!("expected tool call");
    };
    assert!(
        !serde_json::to_string(&tool_call_json(tool_call))
            .unwrap()
            .contains(encoded)
    );
}

#[test]
fn recovers_server_and_tool_from_codex_acp_tagged_execute_calls() {
    let tool_call = tool_call_from_update(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "exec-f4591587-0754-4bb4-990b-f2767894ba93",
        "kind": "execute",
        "title": "mcp.t3-code.orchestrator_capabilities",
        "status": "in_progress",
        "rawInput": { "server": "t3-code", "tool": "orchestrator_capabilities", "arguments": {} },
        "_meta": { "is_mcp_tool_call": true },
    }));
    assert_eq!(
        extract_mcp_tool_call_identity(&tool_call, None),
        Some(AcpMcpToolCallIdentity {
            server: "t3-code".to_owned(),
            tool: "orchestrator_capabilities".to_owned(),
            input: None,
        })
    );
}

#[test]
fn recovers_t3_identity_from_acp_mcp_call_fallback_commands() {
    let tool_call = tool_call_from_update(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "exec-1",
        "kind": "execute",
        "title": "Ran command",
        "status": "in_progress",
    }));
    assert_eq!(
        extract_mcp_tool_call_identity(
            &tool_call,
            Some(&[
                "/usr/bin/node /srv/t3/bin.ts acp-mcp-call delegate_task {\"task\":\"x\"}"
                    .to_owned()
            ]),
        ),
        Some(AcpMcpToolCallIdentity {
            server: "t3-code".to_owned(),
            tool: "delegate_task".to_owned(),
            input: Some(serde_json::from_value(json!({ "task": "x" })).unwrap()),
        })
    );
}

#[test]
fn leaves_ordinary_execute_calls_unidentified() {
    let tool_call = tool_call_from_update(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "exec-2",
        "kind": "execute",
        "title": "cat package.json",
        "status": "in_progress",
        "rawInput": { "command": "cat package.json" },
    }));
    assert!(extract_mcp_tool_call_identity(&tool_call, None).is_none());
}

#[test]
fn applies_acp_v2_agent_owned_terminal_snapshots_and_output_chunks_public_api() {
    let snapshot = apply_acp_agent_terminal_update(
        None,
        &json!({
            "sessionUpdate": "terminal_update",
            "terminalId": "terminal-1",
            "command": "pnpm test",
            "cwd": "/workspace",
            "output": { "data": STANDARD.encode("first line\n") },
        }),
    );
    let chunked = apply_acp_agent_terminal_update(
        Some(&snapshot),
        &json!({
            "sessionUpdate": "terminal_output_chunk",
            "terminalId": "terminal-1",
            "data": STANDARD.encode("second line\n"),
        }),
    );
    let completed = apply_acp_agent_terminal_update(
        Some(&chunked),
        &json!({
            "sessionUpdate": "terminal_update",
            "terminalId": "terminal-1",
            "exitStatus": { "exitCode": 0 },
        }),
    );
    assert_eq!(completed.output, "first line\nsecond line\n");
    assert_eq!(completed.command.as_deref(), Some("pnpm test"));
}
