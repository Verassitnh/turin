//! Port of upstream/apps/server/src/orchestration-v2/UserFacingErrors.ts

use serde_json::Value;

const GENERIC: [&str; 3] = [
    "Failed to dispatch orchestration V2 command",
    "Failed to dispatch orchestration command ",
    "Provider adapter failed while dispatching orchestration command ",
];

fn provider_name(id: &str) -> String {
    match id {
        "codex" => "Codex".into(),
        "claudeAgent" => "Claude".into(),
        "opencode" => "OpenCode".into(),
        "cursor" => "Cursor".into(),
        _ => {
            let trimmed = id.strip_suffix("Agent").unwrap_or(id).trim();
            let mut chars = trimmed.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_else(|| id.into())
        }
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
            .or_else(|| {
                value
                    .get("detail")
                    .or_else(|| value.get("message"))
                    .and_then(|v| v.as_str())
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
    messages
        .into_iter()
        .rev()
        .find(|m| !GENERIC.iter().any(|prefix| m.starts_with(prefix)))
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
}
