//! Port of upstream/apps/server/src/orchestration-v2/DispatchModeLimit.ts
//!
//! The broadest modes a command may touch, for commands a limited sender
//! (upstream: an MCP caller) issues. A caller checks its target's modes before
//! dispatching, but the target's user can raise them meanwhile; the
//! orchestrator re-checks this limit inside the thread's command lock, where
//! nothing else can change them. Unset for the user's own commands, which have
//! no limit.
//!
//! Upstream carries the limit as an ambient `Context.Reference`; Rust has no
//! ambient context, so callers pass `Option<&DispatchModeLimit>` explicitly.
//! The orchestrator's MCP surface is cut, so only `stop_workflow` (and the
//! orchestrator's own re-check) read it today.

use super::orchestrator_port::ModeKind;
use parking_lot::Mutex;
use std::sync::Arc;
use turin_contracts::ThreadId;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchModes {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
}

/// A thread the orchestrator refused to touch because it ran above the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchModeRefusal {
    pub thread_id: ThreadId,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub mode: ModeKind,
}

/// `DispatchModeLimitValue`. The orchestrator also records its refusal in
/// `refused`, so the sender can report it however the code in between wrapped
/// the dispatch error.
#[derive(Debug, Clone)]
pub struct DispatchModeLimit {
    pub modes: DispatchModes,
    pub refused: Option<Arc<Mutex<Option<DispatchModeRefusal>>>>,
}

impl DispatchModeLimit {
    pub fn new(modes: DispatchModes) -> Self {
        Self {
            modes,
            refused: None,
        }
    }

    pub fn with_refusal_slot(
        modes: DispatchModes,
    ) -> (Self, Arc<Mutex<Option<DispatchModeRefusal>>>) {
        let slot = Arc::new(Mutex::new(None));
        (
            Self {
                modes,
                refused: Some(Arc::clone(&slot)),
            },
            slot,
        )
    }

    pub fn record_refusal(&self, refusal: DispatchModeRefusal) {
        if let Some(slot) = &self.refused {
            *slot.lock() = Some(refusal);
        }
    }
}

const fn runtime_mode_rank(mode: RuntimeMode) -> u8 {
    match mode {
        RuntimeMode::ApprovalRequired => 0,
        RuntimeMode::AutoAcceptEdits => 1,
        RuntimeMode::Auto => 2,
        RuntimeMode::FullAccess => 3,
    }
}

const fn interaction_mode_rank(mode: ProviderInteractionMode) -> u8 {
    match mode {
        ProviderInteractionMode::Plan => 0,
        ProviderInteractionMode::Default => 1,
    }
}

/// Which of `modes` is broader than `limit`, if either.
pub fn exceeded_dispatch_mode_limit(
    limit: DispatchModes,
    modes: DispatchModes,
) -> Option<ModeKind> {
    if runtime_mode_rank(modes.runtime_mode) > runtime_mode_rank(limit.runtime_mode) {
        Some(ModeKind::Runtime)
    } else if interaction_mode_rank(modes.interaction_mode)
        > interaction_mode_rank(limit.interaction_mode)
    {
        Some(ModeKind::Interaction)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: DispatchModes = DispatchModes {
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: ProviderInteractionMode::Plan,
    };
    const BROAD: DispatchModes = DispatchModes {
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: ProviderInteractionMode::Default,
    };

    #[test]
    fn runtime_takes_priority_and_plan_is_narrower() {
        assert_eq!(
            exceeded_dispatch_mode_limit(PLAN, BROAD),
            Some(ModeKind::Runtime)
        );
        assert_eq!(
            exceeded_dispatch_mode_limit(
                DispatchModes {
                    runtime_mode: RuntimeMode::FullAccess,
                    ..PLAN
                },
                BROAD
            ),
            Some(ModeKind::Interaction)
        );
        assert_eq!(exceeded_dispatch_mode_limit(BROAD, PLAN), None);
        assert_eq!(exceeded_dispatch_mode_limit(PLAN, PLAN), None);
    }

    #[test]
    fn every_runtime_mode_is_ranked_in_order() {
        let order = [
            RuntimeMode::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits,
            RuntimeMode::Auto,
            RuntimeMode::FullAccess,
        ];
        for (index, limit) in order.iter().enumerate() {
            for (other, mode) in order.iter().enumerate() {
                let exceeded = exceeded_dispatch_mode_limit(
                    DispatchModes {
                        runtime_mode: *limit,
                        interaction_mode: ProviderInteractionMode::Default,
                    },
                    DispatchModes {
                        runtime_mode: *mode,
                        interaction_mode: ProviderInteractionMode::Default,
                    },
                );
                assert_eq!(exceeded.is_some(), other > index);
            }
        }
    }

    #[test]
    fn a_refusal_is_recorded_in_the_senders_slot() {
        let (limit, slot) = DispatchModeLimit::with_refusal_slot(PLAN);
        let refusal = DispatchModeRefusal {
            thread_id: ThreadId::new("thread:limit").unwrap(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Plan,
            mode: ModeKind::Runtime,
        };
        limit.record_refusal(refusal.clone());
        assert_eq!(*slot.lock(), Some(refusal));
        // No slot: recording is a no-op.
        DispatchModeLimit::new(PLAN).record_refusal(DispatchModeRefusal {
            thread_id: ThreadId::new("thread:none").unwrap(),
            runtime_mode: RuntimeMode::Auto,
            interaction_mode: ProviderInteractionMode::Plan,
            mode: ModeKind::Runtime,
        });
    }
}
