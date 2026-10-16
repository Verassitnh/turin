//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts permissionResultFromDecision.
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

/// Produce the exact SDK `can_use_tool` response. A decline does not interrupt
/// the turn; a cancellation does.
pub fn permission_result_from_decision(
    tool_name: &str,
    decision: ApprovalDecision,
    tool_input: &Value,
    tool_use_id: &str,
    suggestions: Option<&[Value]>,
) -> Value {
    match decision {
        ApprovalDecision::Accept => json!({
            "behavior":"allow", "updatedInput":tool_input, "toolUseID":tool_use_id,
            "decisionClassification":"user_temporary"
        }),
        ApprovalDecision::AcceptForSession => {
            let updates: Vec<Value> = suggestions
                .unwrap_or_default()
                .iter()
                .filter_map(|value| value.as_object().cloned())
                .map(|mut value| {
                    value.insert("destination".into(), json!("session"));
                    Value::Object(value)
                })
                .collect();
            let updates = if updates.is_empty() {
                vec![
                    json!({"type":"addRules","rules":[{"toolName":tool_name}],"behavior":"allow","destination":"session"}),
                ]
            } else {
                updates
            };
            json!({
                "behavior":"allow", "updatedInput":tool_input, "toolUseID":tool_use_id,
                "decisionClassification":"user_permanent", "updatedPermissions":updates
            })
        }
        ApprovalDecision::Decline => json!({
            "behavior":"deny", "message":"User declined tool execution.",
            "toolUseID":tool_use_id, "decisionClassification":"user_reject"
        }),
        ApprovalDecision::Cancel => json!({
            "behavior":"deny", "message":"User cancelled tool execution.",
            "toolUseID":tool_use_id, "decisionClassification":"user_reject", "interrupt":true
        }),
    }
}

pub fn auto_approved(input: &Value, tool_use_id: &str) -> Value {
    json!({"behavior":"allow","updatedInput":input,"toolUseID":tool_use_id})
}

pub fn cancelled_question() -> Value {
    json!({"behavior":"deny","message":"User cancelled tool execution."})
}

pub fn captured_plan(tool_use_id: &str) -> Value {
    json!({"behavior":"deny","message":"The client captured your proposed plan. Stop here and wait for the user's feedback or implementation request in a later turn.","toolUseID":tool_use_id})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decision_shapes() {
        let input = json!({"command":"pwd"});
        let decline = permission_result_from_decision(
            "Bash",
            ApprovalDecision::Decline,
            &input,
            "toolu_1",
            None,
        );
        assert!(decline.get("interrupt").is_none());
        assert_eq!(decline["decisionClassification"], "user_reject");
        let cancel = permission_result_from_decision(
            "Bash",
            ApprovalDecision::Cancel,
            &input,
            "toolu_1",
            None,
        );
        assert_eq!(cancel["interrupt"], true);
        let permanent = permission_result_from_decision(
            "Bash",
            ApprovalDecision::AcceptForSession,
            &input,
            "toolu_1",
            None,
        );
        assert_eq!(permanent["updatedPermissions"][0]["destination"], "session");
        assert_eq!(
            permanent["updatedPermissions"][0]["rules"][0]["toolName"],
            "Bash"
        );
        assert!(cancelled_question().get("toolUseID").is_none());
        assert!(captured_plan("t").get("decisionClassification").is_none());
    }
}
