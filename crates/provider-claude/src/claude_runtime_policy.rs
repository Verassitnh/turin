//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts
//! `permissionModeForClaudeRuntimePolicy` / `claudeRuntimeQueryPolicyForRuntimePolicy`.
//!
//! The thread's runtime mode decides the CLI permission mode and whether the
//! host's `can_use_tool` callback asks the user or allows. Full access never
//! prompts: it launches with `bypassPermissions` and the callback auto-allows.

use serde_json::Value;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};
use turin_provider_core::provider_adapter::ProviderAdapterV2RuntimePolicy;

/// Upstream `CLAUDE_READ_ONLY_ALLOWED_TOOLS`.
pub const CLAUDE_READ_ONLY_ALLOWED_TOOLS: [&str; 3] = ["Read", "Glob", "Grep"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeRuntimeQueryPolicy {
    /// `--permission-mode` for the launch.
    pub permission_mode: &'static str,
    /// A restricted tool list (read-only sandbox), else the preset.
    pub tools: Option<Vec<String>>,
    /// Pre-approved tools (read-only sandbox with global reads).
    pub allowed_tools: Option<Vec<String>>,
    /// `--allow-dangerously-skip-permissions`, set for `bypassPermissions`.
    pub allow_dangerously_skip_permissions: bool,
    /// Whether the `can_use_tool` callback asks the user. `false` auto-allows
    /// (upstream leaves the callback uninstalled).
    pub install_permission_callback: bool,
}

impl ClaudeRuntimeQueryPolicy {
    /// Upstream `claudeRuntimeQueryPolicyKey`: a live query is reused only
    /// while its policy is unchanged.
    pub fn key(&self) -> String {
        serde_json::json!({
            "permissionMode": self.permission_mode,
            "tools": self.tools,
            "allowedTools": self.allowed_tools,
            "allowDangerouslySkipPermissions": self.allow_dangerously_skip_permissions,
            "installPermissionCallback": self.install_permission_callback,
        })
        .to_string()
    }
}

fn sandbox_kind(policy: &ProviderAdapterV2RuntimePolicy) -> Option<&str> {
    policy
        .sandbox_policy
        .as_ref()
        .and_then(|sandbox| sandbox.get("type"))
        .and_then(Value::as_str)
        .filter(|kind| {
            matches!(
                *kind,
                "readOnly" | "workspaceWrite" | "dangerFullAccess" | "externalSandbox"
            )
        })
}

fn read_only_allows_global_reads(policy: &ProviderAdapterV2RuntimePolicy) -> bool {
    policy.sandbox_policy.as_ref().is_some_and(|sandbox| {
        sandbox.get("type").and_then(Value::as_str) == Some("readOnly")
            && sandbox
                .get("access")
                .and_then(|access| access.get("type"))
                .and_then(Value::as_str)
                == Some("fullAccess")
    })
}

fn approval_policy(policy: &ProviderAdapterV2RuntimePolicy) -> Option<&str> {
    policy.approval_policy.as_ref().and_then(Value::as_str)
}

/// Upstream `permissionModeForClaudeRuntimePolicy`.
pub fn permission_mode_for_runtime_policy(policy: &ProviderAdapterV2RuntimePolicy) -> &'static str {
    if policy.interaction_mode == ProviderInteractionMode::Plan {
        return "plan";
    }
    let approval = approval_policy(policy);
    if approval == Some("never") {
        return match sandbox_kind(policy) {
            Some("readOnly") => "dontAsk",
            Some("dangerFullAccess" | "externalSandbox") => "bypassPermissions",
            _ => match policy.runtime_mode {
                RuntimeMode::ApprovalRequired => "dontAsk",
                RuntimeMode::AutoAcceptEdits => "acceptEdits",
                RuntimeMode::Auto | RuntimeMode::FullAccess => "bypassPermissions",
            },
        };
    }
    if approval.is_some() {
        return "default";
    }
    match sandbox_kind(policy) {
        Some("readOnly") => return "dontAsk",
        Some("dangerFullAccess") => {
            return if policy.runtime_mode == RuntimeMode::ApprovalRequired {
                "default"
            } else {
                "bypassPermissions"
            };
        }
        _ => {}
    }
    match policy.runtime_mode {
        RuntimeMode::ApprovalRequired => "default",
        RuntimeMode::AutoAcceptEdits => "acceptEdits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "bypassPermissions",
    }
}

/// Upstream `claudeRuntimeQueryPolicyForRuntimePolicy`.
pub fn claude_runtime_query_policy(
    policy: &ProviderAdapterV2RuntimePolicy,
) -> ClaudeRuntimeQueryPolicy {
    let permission_mode = permission_mode_for_runtime_policy(policy);
    let read_only_tools = (sandbox_kind(policy) == Some("readOnly")).then(|| {
        CLAUDE_READ_ONLY_ALLOWED_TOOLS
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect::<Vec<_>>()
    });
    let allowed_tools = read_only_tools
        .clone()
        .filter(|_| read_only_allows_global_reads(policy));
    // acceptEdits approves edits before the callback runs; everything else it
    // leaves to the callback, which must ask rather than allow.
    let install_permission_callback = match approval_policy(policy) {
        None => matches!(
            policy.runtime_mode,
            RuntimeMode::ApprovalRequired | RuntimeMode::AutoAcceptEdits
        ),
        Some(approval) => approval != "never",
    };
    ClaudeRuntimeQueryPolicy {
        permission_mode,
        tools: read_only_tools,
        allowed_tools,
        allow_dangerously_skip_permissions: permission_mode == "bypassPermissions",
        install_permission_callback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(runtime_mode: RuntimeMode) -> ProviderAdapterV2RuntimePolicy {
        ProviderAdapterV2RuntimePolicy {
            runtime_mode,
            interaction_mode: ProviderInteractionMode::Default,
            cwd: Some("/tmp/project".into()),
            approval_policy: None,
            sandbox_policy: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn full_access_bypasses_permissions_and_never_asks() {
        let query = claude_runtime_query_policy(&policy(RuntimeMode::FullAccess));
        assert_eq!(query.permission_mode, "bypassPermissions");
        assert!(query.allow_dangerously_skip_permissions);
        assert!(!query.install_permission_callback);
        assert_eq!(query.tools, None);
    }

    #[test]
    fn each_runtime_mode_maps_like_upstream() {
        let cases = [
            (RuntimeMode::ApprovalRequired, "default", false, true),
            (RuntimeMode::AutoAcceptEdits, "acceptEdits", false, true),
            (RuntimeMode::Auto, "auto", false, false),
            (RuntimeMode::FullAccess, "bypassPermissions", true, false),
        ];
        for (mode, permission_mode, skip, asks) in cases {
            let query = claude_runtime_query_policy(&policy(mode));
            assert_eq!(query.permission_mode, permission_mode, "{mode:?}");
            assert_eq!(query.allow_dangerously_skip_permissions, skip, "{mode:?}");
            assert_eq!(query.install_permission_callback, asks, "{mode:?}");
        }
    }

    #[test]
    fn plan_mode_wins_over_the_runtime_mode() {
        let mut full = policy(RuntimeMode::FullAccess);
        full.interaction_mode = ProviderInteractionMode::Plan;
        let query = claude_runtime_query_policy(&full);
        assert_eq!(query.permission_mode, "plan");
        assert!(!query.allow_dangerously_skip_permissions);
        assert!(!query.install_permission_callback);
    }

    #[test]
    fn an_explicit_approval_policy_overrides_the_mode() {
        let mut never = policy(RuntimeMode::ApprovalRequired);
        never.approval_policy = Some(json!("never"));
        let query = claude_runtime_query_policy(&never);
        assert_eq!(query.permission_mode, "dontAsk");
        assert!(!query.install_permission_callback);

        let mut on_request = policy(RuntimeMode::FullAccess);
        on_request.approval_policy = Some(json!("on-request"));
        let query = claude_runtime_query_policy(&on_request);
        assert_eq!(query.permission_mode, "default");
        assert!(query.install_permission_callback);
    }

    #[test]
    fn a_read_only_sandbox_restricts_tools() {
        let mut read_only = policy(RuntimeMode::FullAccess);
        read_only.sandbox_policy = Some(json!({"type": "readOnly"}));
        let query = claude_runtime_query_policy(&read_only);
        assert_eq!(query.permission_mode, "dontAsk");
        assert_eq!(
            query.tools.as_deref(),
            Some(&["Read".to_owned(), "Glob".to_owned(), "Grep".to_owned()][..])
        );
        assert_eq!(query.allowed_tools, None);

        read_only.sandbox_policy =
            Some(json!({"type": "readOnly", "access": {"type": "fullAccess"}}));
        let query = claude_runtime_query_policy(&read_only);
        assert_eq!(query.allowed_tools, query.tools);
    }

    #[test]
    fn danger_full_access_sandbox_keeps_approval_required_asking() {
        let mut danger = policy(RuntimeMode::ApprovalRequired);
        danger.sandbox_policy = Some(json!({"type": "dangerFullAccess"}));
        assert_eq!(permission_mode_for_runtime_policy(&danger), "default");
        danger.runtime_mode = RuntimeMode::Auto;
        assert_eq!(
            permission_mode_for_runtime_policy(&danger),
            "bypassPermissions"
        );
    }

    #[test]
    fn the_key_changes_with_the_mode() {
        let full = claude_runtime_query_policy(&policy(RuntimeMode::FullAccess));
        let ask = claude_runtime_query_policy(&policy(RuntimeMode::ApprovalRequired));
        assert_ne!(full.key(), ask.key());
        assert_eq!(full.key(), full.clone().key());
    }
}
