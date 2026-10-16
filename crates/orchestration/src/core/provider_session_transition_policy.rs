//! Port of upstream/apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.ts.
//!
//! Decides what a thread's provider session must do when its next run asks
//! for a different selection, instance, runtime mode, or workspace. Pure: the
//! caller reads the current session and the target, asks the adapter for a
//! [`ProviderSelectionTransitionPlan`] when only the selection changed, and
//! carries out the returned transition.

use turin_contracts::{
    model::ProviderOptionSelection,
    model_selection::ModelSelection,
    orchestration_v2::OrchestrationV2ProviderCapabilities,
    provider_instance::ProviderDriverKind,
    provider_policy::{ProviderInteractionMode, RuntimeMode},
};
use turin_provider_core::{
    driver::ProviderContinuationIdentity, selection_transition::ProviderSelectionTransitionPlan,
};

/// `ProviderSessionTransition`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSessionTransition {
    Reuse,
    SwitchModelInSession,
    RestartAndResume,
    CreateWithHandoff,
    Reject { reason: String },
}

/// `ProviderSessionTransitionState`.
#[derive(Debug, Clone)]
pub struct ProviderSessionTransitionState {
    pub driver: ProviderDriverKind,
    pub continuation_identity: ProviderContinuationIdentity,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub workspace: String,
    pub capabilities: OrchestrationV2ProviderCapabilities,
}

/// `ProviderSessionTransitionTarget`.
#[derive(Debug, Clone)]
pub struct ProviderSessionTransitionTarget {
    pub state: ProviderSessionTransitionState,
    pub available: bool,
}

/// `decideProviderSessionTransition`. `selection_transition` is the adapter's
/// classification; the caller asks for it only when the selection changed.
pub fn decide_provider_session_transition(
    current: Option<&ProviderSessionTransitionState>,
    target: &ProviderSessionTransitionTarget,
    selection_transition: Option<&ProviderSelectionTransitionPlan>,
) -> ProviderSessionTransition {
    if !target.available {
        return ProviderSessionTransition::Reject {
            reason: "The target provider instance is unavailable.".to_owned(),
        };
    }
    let Some(current) = current else {
        return ProviderSessionTransition::CreateWithHandoff;
    };

    let target = &target.state;
    let continuation_compatible = current.continuation_identity.driver_kind
        == target.continuation_identity.driver_kind
        && current.continuation_identity.continuation_key
            == target.continuation_identity.continuation_key;
    if !continuation_compatible || current.driver != target.driver {
        return ProviderSessionTransition::CreateWithHandoff;
    }

    let instance_changed =
        current.model_selection.instance_id != target.model_selection.instance_id;
    let runtime_changed = current.runtime_mode != target.runtime_mode;
    let workspace_changed = current.workspace != target.workspace;
    let selection_changed =
        !model_selections_equal(&current.model_selection, &target.model_selection);
    if selection_changed && !instance_changed {
        match selection_transition {
            Some(ProviderSelectionTransitionPlan::CreateWithHandoff) => {
                return ProviderSessionTransition::CreateWithHandoff;
            }
            Some(ProviderSelectionTransitionPlan::Reject { reason }) => {
                return ProviderSessionTransition::Reject {
                    reason: reason.clone(),
                };
            }
            None => {
                return ProviderSessionTransition::Reject {
                    reason: "The provider adapter did not classify the selection change."
                        .to_owned(),
                };
            }
            Some(
                ProviderSelectionTransitionPlan::ApplyOnNextTurn
                | ProviderSelectionTransitionPlan::RestartSession,
            ) => {}
        }
    }
    if instance_changed || runtime_changed || workspace_changed {
        return ProviderSessionTransition::RestartAndResume;
    }

    if selection_changed {
        return match selection_transition {
            Some(ProviderSelectionTransitionPlan::ApplyOnNextTurn) => {
                ProviderSessionTransition::SwitchModelInSession
            }
            Some(ProviderSelectionTransitionPlan::RestartSession) => {
                ProviderSessionTransition::RestartAndResume
            }
            Some(ProviderSelectionTransitionPlan::CreateWithHandoff) => {
                ProviderSessionTransition::CreateWithHandoff
            }
            Some(ProviderSelectionTransitionPlan::Reject { reason }) => {
                ProviderSessionTransition::Reject {
                    reason: reason.clone(),
                }
            }
            None => ProviderSessionTransition::RestartAndResume,
        };
    }

    // Interaction mode is turn-scoped and is applied when the next turn starts.
    ProviderSessionTransition::Reuse
}

/// `modelSelectionsEqual` (`@t3tools/shared/model`): same instance and model,
/// and the same option set once option order and an empty list are ignored.
pub fn model_selections_equal(left: &ModelSelection, right: &ModelSelection) -> bool {
    if left.instance_id != right.instance_id || left.model != right.model {
        return false;
    }
    let left_options = canonical_options(left);
    let right_options = canonical_options(right);
    left_options.len() == right_options.len()
        && left_options
            .iter()
            .zip(&right_options)
            .all(|(left, right)| left == right)
}

/// `canonicalModelSelectionOptions`: sorted by id, then by value. Upstream
/// sorts with `localeCompare`; byte order gives the same equality result.
fn canonical_options(selection: &ModelSelection) -> Vec<(&str, OptionValue<'_>)> {
    let mut options: Vec<(&str, OptionValue<'_>)> = selection
        .options
        .iter()
        .flatten()
        .map(option_entry)
        .collect();
    options.sort_by(|left, right| left.0.cmp(right.0).then_with(|| left.1.cmp(&right.1)));
    options
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum OptionValue<'a> {
    Bool(bool),
    Str(&'a str),
}

fn option_entry(option: &ProviderOptionSelection) -> (&str, OptionValue<'_>) {
    use turin_contracts::model::ProviderOptionSelectionValue;
    let value = match &option.value {
        ProviderOptionSelectionValue::Bool(value) => OptionValue::Bool(*value),
        ProviderOptionSelectionValue::Str(value) => OptionValue::Str(value.as_str()),
    };
    (option.id.as_str(), value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::default_capabilities;

    fn driver(value: &str) -> ProviderDriverKind {
        ProviderDriverKind::new(value).expect("driver kind")
    }

    fn selection(instance_id: &str, model: &str) -> ModelSelection {
        serde_json::from_value(serde_json::json!({
            "instanceId": instance_id,
            "model": model,
        }))
        .expect("model selection")
    }

    fn with_options(mut selection: ModelSelection, options: serde_json::Value) -> ModelSelection {
        selection.options = Some(serde_json::from_value(options).expect("options"));
        selection
    }

    fn base_state() -> ProviderSessionTransitionState {
        let codex = driver("codex");
        ProviderSessionTransitionState {
            driver: codex.clone(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: codex,
                continuation_key: "codex:account:one".to_owned(),
            },
            model_selection: selection("codex", "gpt-5.1-codex"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Default,
            workspace: "/repo".to_owned(),
            capabilities: default_capabilities(),
        }
    }

    fn target(state: ProviderSessionTransitionState) -> ProviderSessionTransitionTarget {
        ProviderSessionTransitionTarget {
            state,
            available: true,
        }
    }

    fn with_model(
        mut state: ProviderSessionTransitionState,
        model: &str,
    ) -> ProviderSessionTransitionState {
        state.model_selection = selection("codex", model);
        state
    }

    #[test]
    fn reuses_compatible_sessions_and_treats_interaction_mode_as_turn_scoped() {
        let base = base_state();
        let mut next = base.clone();
        next.interaction_mode = ProviderInteractionMode::Plan;
        assert_eq!(
            decide_provider_session_transition(Some(&base), &target(next), None),
            ProviderSessionTransition::Reuse
        );
    }

    #[test]
    fn uses_the_adapters_selection_transition_classification() {
        let base = base_state();
        let next = with_model(base.clone(), "gpt-5.2-codex");
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &target(next.clone()),
                Some(&ProviderSelectionTransitionPlan::ApplyOnNextTurn),
            ),
            ProviderSessionTransition::SwitchModelInSession
        );

        let mut unsupported = base.clone();
        unsupported
            .capabilities
            .sessions
            .supports_model_switch_in_session = false;
        let mut unsupported_target = next;
        unsupported_target
            .capabilities
            .sessions
            .supports_model_switch_in_session = false;
        assert_eq!(
            decide_provider_session_transition(
                Some(&unsupported),
                &target(unsupported_target),
                Some(&ProviderSelectionTransitionPlan::RestartSession),
            ),
            ProviderSessionTransition::RestartAndResume
        );
    }

    #[test]
    fn treats_provider_option_changes_as_selection_changes() {
        let mut base = base_state();
        base.model_selection = with_options(
            base.model_selection.clone(),
            serde_json::json!([{"id": "reasoningEffort", "value": "medium"}]),
        );
        let mut next = base.clone();
        next.model_selection = with_options(
            selection("codex", "gpt-5.1-codex"),
            serde_json::json!([{"id": "reasoningEffort", "value": "high"}]),
        );
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &target(next),
                Some(&ProviderSelectionTransitionPlan::ApplyOnNextTurn),
            ),
            ProviderSessionTransition::SwitchModelInSession
        );
    }

    #[test]
    fn option_order_and_empty_option_lists_are_not_selection_changes() {
        let mut left = base_state();
        left.model_selection = with_options(
            selection("codex", "gpt-5.1-codex"),
            serde_json::json!([
                {"id": "b", "value": true},
                {"id": "a", "value": "x"},
            ]),
        );
        let mut right = left.clone();
        right.model_selection = with_options(
            selection("codex", "gpt-5.1-codex"),
            serde_json::json!([
                {"id": "a", "value": "x"},
                {"id": "b", "value": true},
            ]),
        );
        assert!(model_selections_equal(
            &left.model_selection,
            &right.model_selection
        ));
        assert_eq!(
            decide_provider_session_transition(Some(&left), &target(right), None),
            ProviderSessionTransition::Reuse
        );
        // A string "true" is not the boolean true.
        let mut string_true = left.clone();
        string_true.model_selection = with_options(
            selection("codex", "gpt-5.1-codex"),
            serde_json::json!([
                {"id": "b", "value": "true"},
                {"id": "a", "value": "x"},
            ]),
        );
        assert!(!model_selections_equal(
            &left.model_selection,
            &string_true.model_selection
        ));
    }

    #[test]
    fn restarts_compatible_instances_for_workspace_or_runtime_changes() {
        let base = base_state();
        let mut workspace = base.clone();
        workspace.workspace = "/other".to_owned();
        assert_eq!(
            decide_provider_session_transition(Some(&base), &target(workspace), None),
            ProviderSessionTransition::RestartAndResume
        );
    }

    #[test]
    fn preserves_a_rejected_selection_when_the_workspace_also_changes() {
        let base = base_state();
        let mut next = with_model(base.clone(), "gpt-5.2-codex");
        next.workspace = "/other".to_owned();
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &target(next),
                Some(&ProviderSelectionTransitionPlan::Reject {
                    reason: "The active session cannot apply that model.".to_owned(),
                }),
            ),
            ProviderSessionTransition::Reject {
                reason: "The active session cannot apply that model.".to_owned(),
            }
        );
    }

    #[test]
    fn preserves_handoff_and_missing_classification_outcomes_across_workspace_changes() {
        let base = base_state();
        let mut next = with_model(base.clone(), "gpt-5.2-codex");
        next.workspace = "/other".to_owned();
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &target(next.clone()),
                Some(&ProviderSelectionTransitionPlan::CreateWithHandoff),
            ),
            ProviderSessionTransition::CreateWithHandoff
        );
        assert_eq!(
            decide_provider_session_transition(Some(&base), &target(next), None),
            ProviderSessionTransition::Reject {
                reason: "The provider adapter did not classify the selection change.".to_owned(),
            }
        );
    }

    #[test]
    fn uses_portable_handoff_for_incompatible_continuation_identities() {
        let base = base_state();
        let mut next = base.clone();
        next.model_selection = selection("codex_other", "gpt-5.1-codex");
        next.continuation_identity.continuation_key = "codex:account:other".to_owned();
        assert_eq!(
            decide_provider_session_transition(Some(&base), &target(next), None),
            ProviderSessionTransition::CreateWithHandoff
        );
    }

    #[test]
    fn uses_portable_handoff_for_cross_driver_transitions() {
        let base = base_state();
        let claude = driver("claude");
        let next = ProviderSessionTransitionState {
            driver: claude.clone(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: claude,
                continuation_key: "claude:account:one".to_owned(),
            },
            model_selection: selection("claude", "claude-opus-4-1"),
            ..base.clone()
        };
        assert_eq!(
            decide_provider_session_transition(Some(&base), &target(next), None),
            ProviderSessionTransition::CreateWithHandoff
        );
    }

    #[test]
    fn creates_with_handoff_when_there_is_no_current_session_and_rejects_unavailable_targets() {
        let base = base_state();
        assert_eq!(
            decide_provider_session_transition(None, &target(base.clone()), None),
            ProviderSessionTransition::CreateWithHandoff
        );
        let unavailable = ProviderSessionTransitionTarget {
            state: base.clone(),
            available: false,
        };
        assert_eq!(
            decide_provider_session_transition(Some(&base), &unavailable, None),
            ProviderSessionTransition::Reject {
                reason: "The target provider instance is unavailable.".to_owned(),
            }
        );
    }
}
