//! Port of upstream/apps/server/src/orchestration-v2/UserFacingErrors.ts

use serde_json::Value;
use std::collections::HashSet;

const GENERIC: [&str; 3] = [
    "Failed to dispatch orchestration V2 command",
    "Failed to dispatch orchestration command ",
    "Provider adapter failed while dispatching orchestration command ",
];

/// Upstream `PROVIDER_DISPLAY_NAMES` (packages/contracts/src/model.ts).
fn provider_name(id: &str) -> String {
    match id {
        "antigravity" => "Antigravity".into(),
        "codex" => "Codex".into(),
        "claudeAgent" => "Claude".into(),
        "cursor" => "Cursor".into(),
        "grok" => "Grok".into(),
        "muse" => "Muse Code".into(),
        "acpRegistry" => "ACP Registry".into(),
        "pi" => "Pi".into(),
        "opencode" => "OpenCode".into(),
        _ => {
            // Upstream `instanceId.replace(/Agent$/i, "")`: trailing only, any case.
            let trimmed = strip_agent_suffix(id).trim();
            let mut chars = trimmed.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_else(|| id.into())
        }
    }
}

fn strip_agent_suffix(id: &str) -> &str {
    const AGENT: &str = "agent";
    let Some(cut) = id.len().checked_sub(AGENT.len()) else {
        return id;
    };
    match (id.get(..cut), id.get(cut..)) {
        (Some(head), Some(tail)) if tail.eq_ignore_ascii_case(AGENT) => head,
        _ => id,
    }
}

fn friendly(value: &Value) -> Option<String> {
    let id = value.get("providerInstanceId")?.as_str()?;
    let provider = provider_name(id);
    match value.get("_tag")?.as_str()? {
        "CommandPolicyUnsupportedError" => Some(format!(
            "{provider} cannot deliver a message that way right now."
        )),
        "CommandPolicyCapabilityUnsupportedError" => {
            Some(match value.get("capability")?.as_str()? {
                "queued_messages" => {
                    format!("{provider} cannot queue messages behind an active run.")
                }
                "active_steering" | "interrupt_restart_steering" => format!(
                    "{provider} cannot redirect an active run. Stop it first, then send the message."
                ),
                "interrupt" => format!("{provider} cannot stop a run once it has started."),
                "native_fork" => format!("{provider} cannot fork this thread natively."),
                "fork_from_turn" => {
                    format!("{provider} cannot fork from an earlier point in this thread.")
                }
                "rollback" => format!(
                    "{provider} cannot rewind its conversation, so this checkpoint cannot be restored on this thread."
                ),
                "rollback_snapshot" => format!(
                    "{provider} did not report its rewound conversation state, so the checkpoint was not restored."
                ),
                "context_handoff" => {
                    format!("{provider} cannot receive the context handoff needed for this switch.")
                }
                "strong_terminal_status" => format!(
                    "{provider} cannot confirm when its runs finish reliably enough for this."
                ),
                _ => return None,
            })
        }
        _ => None,
    }
}

pub fn user_facing_dispatch_error_message(cause: &Value) -> Option<String> {
    fn collect(value: &Value, out: &mut Vec<String>, depth: usize) {
        if depth > 64 {
            return;
        }
        if let Some(array) = value.as_array() {
            for item in array {
                collect(item, out, depth + 1);
            }
            return;
        }
        let message = value
            .as_str()
            .map(str::to_owned)
            .or_else(|| friendly(value))
            // Upstream `textValue(detail) ?? textValue(message)`: a missing,
            // non-string, or blank detail falls through to message.
            .or_else(|| {
                ["detail", "message"]
                    .iter()
                    .find_map(|key| {
                        value
                            .get(*key)?
                            .as_str()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                    })
                    .map(str::to_owned)
            });
        if let Some(message) = message
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty())
        {
            out.push(message);
        }
        for key in ["cause", "error", "errors"] {
            if let Some(nested) = value.get(key) {
                collect(nested, out, depth + 1);
            }
        }
    }
    let mut messages = Vec::new();
    collect(cause, &mut messages, 0);
    // Upstream keeps each message's first occurrence, then takes `.at(-1)`, so
    // a repeat of an earlier message does not move it to the end.
    let mut seen = HashSet::new();
    messages
        .retain(|m| !GENERIC.iter().any(|prefix| m.starts_with(prefix)) && seen.insert(m.clone()));
    messages.pop()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unwraps_generic_error_and_translates_capability() {
        let value = serde_json::json!({"message":"Failed to dispatch orchestration V2 command X", "cause":{"_tag":"CommandPolicyCapabilityUnsupportedError","providerInstanceId":"codex","capability":"interrupt"}});
        assert_eq!(
            user_facing_dispatch_error_message(&value).as_deref(),
            Some("Codex cannot stop a run once it has started.")
        );
    }

    #[test]
    fn returns_the_deepest_actionable_domain_error() {
        let value = serde_json::json!({
            "message": "Failed to dispatch orchestration command checkpoint.rollback (command-1).",
            "cause": {
                "message": "Provider adapter failed while dispatching orchestration command command-1.",
                "cause": {
                    "message": "claudeAgent cannot satisfy rollback for command command-1: provider conversation rollback is unavailable"
                }
            }
        });
        assert_eq!(
            user_facing_dispatch_error_message(&value).as_deref(),
            Some(
                "claudeAgent cannot satisfy rollback for command command-1: provider conversation rollback is unavailable"
            )
        );
    }

    #[test]
    fn translates_policy_capability_rejections_into_provider_named_prose() {
        let rollback_snapshot = serde_json::json!({
            "message": "Failed to dispatch orchestration command checkpoint.rollback (command-1).",
            "cause": {
                "_tag": "CommandPolicyCapabilityUnsupportedError",
                "commandId": "command-1",
                "threadId": "thread-1",
                "providerInstanceId": "pi",
                "capability": "rollback_snapshot",
                "detail": "rollback must return a provider thread snapshot",
                "message": "pi cannot satisfy rollback_snapshot for command command-1: rollback must return a provider thread snapshot"
            }
        });
        assert_eq!(
            user_facing_dispatch_error_message(&rollback_snapshot).as_deref(),
            Some(
                "Pi did not report its rewound conversation state, so the checkpoint was not restored."
            )
        );
        let rollback = serde_json::json!({
            "message": "Failed to dispatch orchestration command checkpoint.rollback (command-2).",
            "cause": {
                "_tag": "CommandPolicyCapabilityUnsupportedError",
                "commandId": "command-2",
                "threadId": "thread-1",
                "providerInstanceId": "grok",
                "capability": "rollback",
                "detail": "provider conversation rollback is unavailable"
            }
        });
        assert_eq!(
            user_facing_dispatch_error_message(&rollback).as_deref(),
            Some(
                "Grok cannot rewind its conversation, so this checkpoint cannot be restored on this thread."
            )
        );
    }

    #[test]
    fn repeated_message_keeps_its_first_position_before_taking_the_last() {
        // Upstream probe: [X, Y, X] dedupes to [X, Y] and returns Y.
        let value = serde_json::json!({
            "message": "Claude provider thread provider-thread-1 has no live query.",
            "cause": {
                "message": "Codex cannot stop a run once it has started.",
                "cause": {
                    "message": "Claude provider thread provider-thread-1 has no live query."
                }
            }
        });
        assert_eq!(
            user_facing_dispatch_error_message(&value).as_deref(),
            Some("Codex cannot stop a run once it has started.")
        );
    }

    #[test]
    fn names_providers_from_upstream_display_names_and_any_case_agent_suffix() {
        let name_for = |id: &str| {
            let value = serde_json::json!({
                "message": "Failed to dispatch orchestration command message.dispatch (command-1).",
                "cause": {
                    "_tag": "CommandPolicyUnsupportedError",
                    "providerInstanceId": id,
                }
            });
            user_facing_dispatch_error_message(&value).unwrap()
        };
        // Upstream probe: `/Agent$/i` strips "agent" from "myagent" -> "My".
        assert_eq!(
            name_for("myagent"),
            "My cannot deliver a message that way right now."
        );
        assert_eq!(
            name_for("acpRegistry"),
            "ACP Registry cannot deliver a message that way right now."
        );
        assert_eq!(
            name_for("muse"),
            "Muse Code cannot deliver a message that way right now."
        );
    }

    #[test]
    fn blank_or_non_string_detail_falls_back_to_message() {
        // Upstream probes: {detail:"", message:"Real"} and {detail:5, message:"Real"}.
        for detail in [serde_json::json!(""), serde_json::json!(5)] {
            let value = serde_json::json!({
                "message": "Failed to dispatch orchestration command message.dispatch (command-1).",
                "cause": { "detail": detail, "message": "Real" }
            });
            assert_eq!(
                user_facing_dispatch_error_message(&value).as_deref(),
                Some("Real")
            );
        }
    }

    #[test]
    fn uses_explicit_detail_fields_as_user_facing_messages() {
        let value = serde_json::json!({
            "message": "Failed to dispatch orchestration command message.dispatch (command-1).",
            "cause": {
                "detail": "Claude provider thread provider-thread-1 has no live query."
            }
        });
        assert_eq!(
            user_facing_dispatch_error_message(&value).as_deref(),
            Some("Claude provider thread provider-thread-1 has no live query.")
        );
    }
}
