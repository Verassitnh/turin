use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tokio::time::sleep;

use super::content::{acp_content_block_display_text, content_has_text_blocks};
use super::initialize::synthetic_load_session_response_from_initialize;
use super::util::{SessionLoadGate, is_record};

const SESSION_LOAD_REPLAY_POLL: Duration = Duration::from_millis(25);

/// Port of upstream `waitForSessionLoadReplayIdle`.
pub async fn wait_for_session_load_replay_idle(
    gate: &Arc<Mutex<Option<SessionLoadGate>>>,
) -> Value {
    loop {
        sleep(SESSION_LOAD_REPLAY_POLL).await;
        let initialize_result = {
            let guard = gate.lock().await;
            let Some(gate) = guard.as_ref() else {
                continue;
            };
            if !gate.active {
                continue;
            }
            let Some(last) = gate.last_activity_at_millis else {
                continue;
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(last);
            if now.saturating_sub(last) < gate.idle_gap_ms {
                continue;
            }
            gate.initialize_result.clone()
        };
        let synthetic = synthetic_load_session_response_from_initialize(&initialize_result);
        let mut response = Map::new();
        if let Some(modes) = synthetic.modes {
            response.insert("modes".to_owned(), modes);
        }
        response.insert("_meta".to_owned(), Value::Object(synthetic.meta));
        return Value::Object(response);
    }
}

pub fn session_update_is_replay(params: &Value) -> bool {
    let Some(meta) = params.get("_meta").and_then(is_record) else {
        return false;
    };
    meta.get("isReplay").and_then(Value::as_bool) == Some(true)
}

/// Replay chunks and substantive updates during session/load; not Grok keepalives.
pub fn session_update_counts_as_load_replay_activity(
    params: &Value,
    gated_session_id: Option<&str>,
) -> bool {
    if let Some(gated) = gated_session_id
        && params.get("sessionId").and_then(Value::as_str) != Some(gated)
    {
        return false;
    }
    if session_update_is_replay(params) {
        return true;
    }
    let Some(update) = params.get("update") else {
        return false;
    };
    let session_update = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("");
    match session_update {
        "agent_message_chunk" | "agent_thought_chunk" => {
            acp_content_block_display_text(update.get("content").unwrap_or(&Value::Null))
                .map(|text| !text.is_empty())
                .unwrap_or(false)
        }
        "agent_message" | "agent_thought" => update
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| content_has_text_blocks(blocks))
            .unwrap_or(false),
        "tool_call"
        | "tool_call_update"
        | "plan"
        | "plan_update"
        | "plan_removed"
        | "available_commands_update"
        | "current_mode_update"
        | "config_option_update"
        | "session_info_update"
        | "usage_update"
        | "compaction_update"
        | "compaction_summary_chunk" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_grok_session_replay_updates_from_meta_is_replay() {
        assert!(session_update_is_replay(&json!({
            "_meta": { "isReplay": true },
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "replayed" },
            },
        })));
        assert!(!session_update_is_replay(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "live" },
            },
        })));
    }

    #[test]
    fn ignores_grok_keepalive_chunks_when_tracking_session_load_replay_activity() {
        assert!(!session_update_counts_as_load_replay_activity(
            &json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "" },
                },
            }),
            None,
        ));
        assert!(session_update_counts_as_load_replay_activity(
            &json!({
                "_meta": { "isReplay": true },
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "replay-tool",
                    "title": "Replay",
                    "kind": "search",
                    "status": "completed",
                },
            }),
            None,
        ));
    }

    #[test]
    fn ignores_load_replay_activity_from_other_sessions_while_a_load_gate_is_active() {
        assert!(!session_update_counts_as_load_replay_activity(
            &json!({
                "sessionId": "session-other",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "unrelated" },
                },
            }),
            Some("session-loading"),
        ));
        assert!(session_update_counts_as_load_replay_activity(
            &json!({
                "sessionId": "session-loading",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "replay" },
                },
            }),
            Some("session-loading"),
        ));
    }

    #[test]
    fn counts_mode_config_session_usage_updates_as_load_replay_activity() {
        for notification in [
            json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "current_mode_update",
                    "currentModeId": "code",
                },
            }),
            json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": [],
                },
            }),
            json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "session_info_update",
                    "title": "Restored",
                },
            }),
            json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 1,
                    "size": 10,
                },
            }),
        ] {
            assert!(session_update_counts_as_load_replay_activity(
                &notification,
                None
            ));
        }
    }
}
