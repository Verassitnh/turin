//! Port of upstream/apps/server/src/orchestration-v2/RuntimePolicy.ts

use turin_contracts::orchestration_v2::OrchestrationV2AppThread;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicy {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub cwd: String,
    pub approval_policy: Option<serde_json::Value>,
    pub sandbox_policy: Option<serde_json::Value>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimePolicyOverride {
    pub cwd: Option<String>,
    pub approval_policy: Option<serde_json::Value>,
    pub sandbox_policy: Option<serde_json::Value>,
    pub reasoning_effort: Option<String>,
}

pub fn provider_runtime_mode(mode: RuntimeMode, supported: Option<&[RuntimeMode]>) -> RuntimeMode {
    match supported {
        None | Some([]) => mode,
        Some(modes) if modes.contains(&mode) => mode,
        Some(_) => RuntimeMode::ApprovalRequired,
    }
}

/// The caller supplies the project's workspace root from its project store.
pub fn resolve(
    thread: &OrchestrationV2AppThread,
    project_root: &str,
    supported: Option<&[RuntimeMode]>,
    override_: &RuntimePolicyOverride,
) -> RuntimePolicy {
    RuntimePolicy {
        runtime_mode: provider_runtime_mode(thread.runtime_mode, supported),
        interaction_mode: thread.interaction_mode,
        cwd: override_
            .cwd
            .clone()
            .or_else(|| thread.worktree_path.as_ref().map(ToString::to_string))
            .unwrap_or_else(|| project_root.to_owned()),
        approval_policy: override_.approval_policy.clone(),
        sandbox_policy: override_.sandbox_policy.clone(),
        reasoning_effort: override_.reasoning_effort.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_mode_falls_back_to_supervised() {
        let supported = [RuntimeMode::ApprovalRequired, RuntimeMode::Auto];
        assert_eq!(
            provider_runtime_mode(RuntimeMode::AutoAcceptEdits, Some(&supported)),
            RuntimeMode::ApprovalRequired
        );
        assert_eq!(
            provider_runtime_mode(RuntimeMode::Auto, Some(&supported)),
            RuntimeMode::Auto
        );
        assert_eq!(
            provider_runtime_mode(RuntimeMode::FullAccess, None),
            RuntimeMode::FullAccess
        );
    }
}
