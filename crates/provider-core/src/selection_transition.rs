//! Port of upstream/packages/provider-core/src/server/selectionTransition.ts.

use turin_contracts::{
    model_selection::ModelSelection,
    orchestration_v2::OrchestrationV2ProviderCapabilities,
};

pub struct ProviderSelectionTransitionInput {
    pub current: ModelSelection,
    pub target: ModelSelection,
    pub session_capabilities: OrchestrationV2ProviderCapabilities,
}

/// Provider-owned classification of how a complete selection can be applied.
/// The orchestrator remains responsible for attempts and resource lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSelectionTransitionPlan {
    ApplyOnNextTurn,
    RestartSession,
    CreateWithHandoff,
    Reject { reason: String },
}

pub fn turn_scoped_selection_transition() -> ProviderSelectionTransitionPlan {
    ProviderSelectionTransitionPlan::ApplyOnNextTurn
}

/// ACP models require a negotiated model/config mutation capability.
///
/// Kept on the shared transition helper even though the ACP registry itself
/// is cut: Cursor's ACP adapter classifies model switches the same way.
pub fn acp_selection_transition(
    input: &ProviderSelectionTransitionInput,
) -> ProviderSelectionTransitionPlan {
    if input.current.model != input.target.model
        && !input
            .session_capabilities
            .sessions
            .supports_model_switch_in_session
    {
        return ProviderSelectionTransitionPlan::Reject {
            reason: "The active ACP session does not expose a model-switch capability.".into(),
        };
    }
    ProviderSelectionTransitionPlan::ApplyOnNextTurn
}
