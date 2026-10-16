use serde_json::Value;

use super::content::{acp_content_block_display_text, content_has_text_blocks};
use super::util::is_record;

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
