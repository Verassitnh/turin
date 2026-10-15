//! Port of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! (`codexRuntimeModeTurnDefaults`, `buildCodexTurnStartParams`, `codexThreadRuntimeParams`).
use serde_json::{Map, Value, json};
use turin_codex_app_server::extensions::thread_runtime_config;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimePolicy {
    pub cwd: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    /// Codex wire spelling: `untrusted`, `on-request`, `never`, ...
    pub approval_policy: Option<Value>,
    /// Codex wire object, e.g. `{ "type": "workspaceWrite" }`.
    pub sandbox_policy: Option<Value>,
}

pub fn thread_runtime_params(cwd: Option<&str>, model: Option<&str>) -> Value {
    let mut params = Map::new();
    params.insert("config".into(), thread_runtime_config());
    if let Some(cwd) = cwd {
        params.insert("cwd".into(), json!(cwd));
    }
    if let Some(model) = model {
        params.insert("model".into(), json!(model));
    }
    Value::Object(params)
}

pub fn turn_start_params(
    thread_id: &str,
    input: Vec<Value>,
    model: &str,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
    policy: &RuntimePolicy,
) -> Value {
    let (approval, reviewer, sandbox) = match policy.runtime_mode {
        RuntimeMode::ApprovalRequired => ("untrusted", "user", "readOnly"),
        RuntimeMode::AutoAcceptEdits => ("on-request", "user", "workspaceWrite"),
        RuntimeMode::Auto => ("on-request", "auto_review", "workspaceWrite"),
        RuntimeMode::FullAccess => ("never", "user", "dangerFullAccess"),
    };
    let mut params = Map::new();
    params.insert("threadId".into(), json!(thread_id));
    params.insert("input".into(), Value::Array(input));
    params.insert("cwd".into(), json!(policy.cwd));
    params.insert("model".into(), json!(model));
    params.insert("summary".into(), json!("detailed"));
    params.insert("approvalsReviewer".into(), json!(reviewer));
    params.insert(
        "approvalPolicy".into(),
        policy
            .approval_policy
            .clone()
            .unwrap_or_else(|| json!(approval)),
    );
    params.insert(
        "sandboxPolicy".into(),
        policy
            .sandbox_policy
            .clone()
            .unwrap_or_else(|| json!({"type":sandbox})),
    );
    if let Some(effort) = reasoning_effort {
        params.insert("effort".into(), json!(effort));
    }
    if let Some(tier) = service_tier {
        params.insert("serviceTier".into(), json!(tier));
    }
    if policy.interaction_mode == ProviderInteractionMode::Plan {
        params.insert(
            "collaborationMode".into(),
            json!({
                "mode":"plan",
                "settings":{"model":model,"reasoning_effort":reasoning_effort.unwrap_or("medium")}
            }),
        );
    }
    Value::Object(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_mode_table_and_plan_mode() {
        for (mode, approval, reviewer, sandbox) in [
            (
                RuntimeMode::ApprovalRequired,
                "untrusted",
                "user",
                "readOnly",
            ),
            (
                RuntimeMode::AutoAcceptEdits,
                "on-request",
                "user",
                "workspaceWrite",
            ),
            (
                RuntimeMode::Auto,
                "on-request",
                "auto_review",
                "workspaceWrite",
            ),
            (RuntimeMode::FullAccess, "never", "user", "dangerFullAccess"),
        ] {
            let p = turn_start_params(
                "t",
                vec![],
                "gpt",
                None,
                None,
                &RuntimePolicy {
                    cwd: "/tmp".into(),
                    runtime_mode: mode,
                    interaction_mode: ProviderInteractionMode::Default,
                    approval_policy: None,
                    sandbox_policy: None,
                },
            );
            assert_eq!(p["approvalPolicy"], approval);
            assert_eq!(p["approvalsReviewer"], reviewer);
            assert_eq!(p["sandboxPolicy"]["type"], sandbox);
            assert!(p.get("collaborationMode").is_none());
        }
        let p = turn_start_params(
            "t",
            vec![],
            "gpt",
            Some("high"),
            None,
            &RuntimePolicy {
                cwd: "/tmp".into(),
                runtime_mode: RuntimeMode::Auto,
                interaction_mode: ProviderInteractionMode::Plan,
                approval_policy: None,
                sandbox_policy: None,
            },
        );
        assert_eq!(
            p["collaborationMode"],
            json!({"mode":"plan","settings":{"model":"gpt","reasoning_effort":"high"}})
        );
    }
}
