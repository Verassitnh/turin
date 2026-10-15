//! ACP `session/update` projection.
//!
//! Unknown `sessionUpdate` variants are kept. Cursor's CLI still sends the
//! standard ACP plan update before `cursor/create_plan`.

use serde_json::Value;

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
