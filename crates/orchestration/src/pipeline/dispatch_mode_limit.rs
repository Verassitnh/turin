//! Port of upstream/apps/server/src/orchestration-v2/DispatchModeLimit.ts

use turin_contracts::ThreadId;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchModes {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceededMode {
    Runtime,
    Interaction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchModeRefusal {
    pub thread_id: ThreadId,
    pub modes: DispatchModes,
    pub mode: ExceededMode,
}

pub fn exceeded_dispatch_mode_limit(
    limit: DispatchModes,
    modes: DispatchModes,
) -> Option<ExceededMode> {
    fn rank(mode: RuntimeMode) -> u8 {
        match mode {
            RuntimeMode::ApprovalRequired => 0,
            RuntimeMode::AutoAcceptEdits => 1,
            RuntimeMode::Auto => 2,
            RuntimeMode::FullAccess => 3,
        }
    }
    if rank(modes.runtime_mode) > rank(limit.runtime_mode) {
        Some(ExceededMode::Runtime)
    } else if modes.interaction_mode == ProviderInteractionMode::Default
        && limit.interaction_mode == ProviderInteractionMode::Plan
    {
        Some(ExceededMode::Interaction)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_takes_priority_and_plan_is_narrower() {
        let plan = DispatchModes {
            runtime_mode: RuntimeMode::ApprovalRequired,
            interaction_mode: ProviderInteractionMode::Plan,
        };
        let broad = DispatchModes {
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Default,
        };
        assert_eq!(
            exceeded_dispatch_mode_limit(plan, broad),
            Some(ExceededMode::Runtime)
        );
        assert_eq!(
            exceeded_dispatch_mode_limit(
                DispatchModes {
                    runtime_mode: RuntimeMode::FullAccess,
                    ..plan
                },
                broad
            ),
            Some(ExceededMode::Interaction)
        );
        assert_eq!(exceeded_dispatch_mode_limit(broad, plan), None);
    }
}
