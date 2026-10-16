//! Port of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! (`codexRuntimeModeTurnDefaults`, `buildCodexTurnStartParams`, `codexThreadRuntimeParams`).
use serde_json::{Map, Value, json};
use turin_codex_app_server::extensions::thread_runtime_config;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

use crate::codex_developer_instructions::{
    CodexRuntimeInfo, T3CodeToolAvailability, build_codex_additional_context,
    build_codex_developer_instructions,
};

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimePolicy {
    pub cwd: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    /// Codex wire spelling: `untrusted`, `on-request`, `never`, ...
    pub approval_policy: Option<Value>,
    /// Codex wire object, e.g. `{ "type": "workspaceWrite" }`.
    pub sandbox_policy: Option<Value>,
    pub turn_context: TurnContext,
}

/// Per-turn context that shapes `turn/start`: the mode prompt, T3 Code tool
/// context, and app context. Mirrors the inputs of upstream's
/// `buildCodexTurnStartParams`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnContext {
    /// The `t3-code` MCP server is attached to this turn.
    pub has_t3_mcp: bool,
    /// `browserToolsAvailable`; absent means `true`.
    pub browser_tools_available: Option<bool>,
    /// `deviceToolsAvailable`; absent means `false`.
    pub device_tools_available: Option<bool>,
    /// App context entries as `(key, text)`. Sent as untrusted context, never
    /// as developer instructions.
    pub app_context: Vec<(String, String)>,
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
    let context = &policy.turn_context;
    let effort = reasoning_effort.unwrap_or("medium");
    // Upstream attaches the mode prompt and T3 context only when the t3-code
    // MCP server is on the turn, so the prompt never names missing tools.
    let developer_instructions = context
        .has_t3_mcp
        .then(|| build_codex_developer_instructions(policy.interaction_mode));
    let t3_context = context.has_t3_mcp.then(|| {
        build_codex_additional_context(
            &CodexRuntimeInfo {
                model: model.to_owned(),
                reasoning_effort: effort.to_owned(),
            },
            T3CodeToolAvailability {
                browser: context.browser_tools_available.unwrap_or(true),
                device: context.device_tools_available.unwrap_or(false),
            },
        )
    });
    // App context follows the T3 keys, so an app entry wins a key clash.
    let app_context = context
        .app_context
        .iter()
        .map(|(key, text)| (key.clone(), json!({"kind":"untrusted","value":text})));
    let mut additional_context = t3_context.unwrap_or_default();
    additional_context.extend(app_context);
    if !additional_context.is_empty() {
        params.insert(
            "additionalContext".into(),
            Value::Object(additional_context),
        );
    }
    if policy.interaction_mode == ProviderInteractionMode::Plan || developer_instructions.is_some()
    {
        let mut settings = Map::new();
        settings.insert("model".into(), json!(model));
        settings.insert("reasoning_effort".into(), json!(effort));
        if let Some(instructions) = developer_instructions {
            settings.insert("developer_instructions".into(), json!(instructions));
        }
        params.insert(
            "collaborationMode".into(),
            json!({
                "mode": if policy.interaction_mode == ProviderInteractionMode::Plan {
                    "plan"
                } else {
                    "default"
                },
                "settings": Value::Object(settings),
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
                    turn_context: TurnContext::default(),
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
                turn_context: TurnContext::default(),
            },
        );
        assert_eq!(
            p["collaborationMode"],
            json!({"mode":"plan","settings":{"model":"gpt","reasoning_effort":"high"}})
        );
    }

    #[test]
    fn t3_mcp_turns_carry_mode_prompt_and_context() {
        let policy = |interaction_mode| RuntimePolicy {
            cwd: "/tmp".into(),
            runtime_mode: RuntimeMode::Auto,
            interaction_mode,
            approval_policy: None,
            sandbox_policy: None,
            turn_context: TurnContext {
                has_t3_mcp: true,
                browser_tools_available: None,
                device_tools_available: Some(true),
                app_context: vec![("app".into(), "from mcp".into())],
            },
        };
        let p = turn_start_params(
            "t",
            vec![],
            "gpt",
            None,
            None,
            &policy(ProviderInteractionMode::Default),
        );
        assert_eq!(p["collaborationMode"]["mode"], "default");
        assert_eq!(
            p["collaborationMode"]["settings"]["developer_instructions"],
            build_codex_developer_instructions(ProviderInteractionMode::Default)
        );
        assert_eq!(
            p["collaborationMode"]["settings"]["reasoning_effort"],
            "medium"
        );
        let context = &p["additionalContext"];
        assert_eq!(context["t3_code_orchestration"]["kind"], "application");
        assert!(
            context["t3_code_tools"]["value"]
                .as_str()
                .unwrap()
                .contains("T3 Code devices")
        );
        assert_eq!(
            context["app"],
            json!({"kind":"untrusted","value":"from mcp"})
        );

        let plan = turn_start_params(
            "t",
            vec![],
            "gpt",
            None,
            None,
            &policy(ProviderInteractionMode::Plan),
        );
        assert_eq!(plan["collaborationMode"]["mode"], "plan");
    }

    #[test]
    fn no_t3_mcp_and_no_app_context_sends_neither_key() {
        let p = turn_start_params(
            "t",
            vec![],
            "gpt",
            None,
            None,
            &RuntimePolicy {
                cwd: "/tmp".into(),
                runtime_mode: RuntimeMode::Auto,
                interaction_mode: ProviderInteractionMode::Default,
                approval_policy: None,
                sandbox_policy: None,
                turn_context: TurnContext::default(),
            },
        );
        assert!(p.get("additionalContext").is_none());
        assert!(p.get("collaborationMode").is_none());
    }
}
