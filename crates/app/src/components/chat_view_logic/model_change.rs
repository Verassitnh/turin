//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`getStartedThreadModelChangeBlockReason`).

use turin_contracts::provider_instance::ProviderInstanceId;

use crate::state::models::ModelSelection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelChangeProviderInfo<'a> {
    pub instance_id: &'a ProviderInstanceId,
    pub requires_new_thread_for_model_change: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChangeBlockCopy {
    pub title: String,
    pub description: String,
}

pub fn get_started_thread_model_change_block_reason(
    providers: &[ModelChangeProviderInfo<'_>],
    has_started_session: bool,
    supports_provider_switching_via_handoff: bool,
    current_model_selection: &ModelSelection,
    current_provider_instance_id: Option<&ProviderInstanceId>,
    next_model_selection: &ModelSelection,
) -> Option<ModelChangeBlockCopy> {
    if !has_started_session {
        return None;
    }
    let current_instance = current_provider_instance_id
        .map(|id| id.as_str())
        .unwrap_or(current_model_selection.instance_id.as_str());
    let current = ModelSelection {
        instance_id: current_instance.to_owned(),
        model: current_model_selection.model.clone(),
    };
    if current.instance_id == next_model_selection.instance_id
        && current.model == next_model_selection.model
    {
        return None;
    }
    if current.instance_id != next_model_selection.instance_id {
        if supports_provider_switching_via_handoff {
            return None;
        }
        return Some(ModelChangeBlockCopy {
            title: "Start a new chat to switch providers".to_owned(),
            description: "This thread does not support switching providers after it has started."
                .to_owned(),
        });
    }
    let current_provider = providers
        .iter()
        .find(|provider| provider.instance_id.as_str() == current.instance_id);
    let next_provider = providers
        .iter()
        .find(|provider| provider.instance_id.as_str() == next_model_selection.instance_id);
    if !current_provider.is_some_and(|provider| provider.requires_new_thread_for_model_change)
        && !next_provider.is_some_and(|provider| provider.requires_new_thread_for_model_change)
    {
        return None;
    }
    Some(ModelChangeBlockCopy {
        title: "Start a new chat to change models".to_owned(),
        description:
            "This provider does not allow switching models after a conversation has started."
                .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn providers<'a>(
        codex: &'a ProviderInstanceId,
        grok: &'a ProviderInstanceId,
    ) -> [ModelChangeProviderInfo<'a>; 2] {
        [
            ModelChangeProviderInfo {
                instance_id: codex,
                requires_new_thread_for_model_change: false,
            },
            ModelChangeProviderInfo {
                instance_id: grok,
                requires_new_thread_for_model_change: true,
            },
        ]
    }

    #[test]
    fn allows_model_changes_before_a_provider_session_has_started() {
        let codex = ProviderInstanceId::new("codex").unwrap();
        let grok = ProviderInstanceId::new("grok").unwrap();
        assert_eq!(
            get_started_thread_model_change_block_reason(
                &providers(&codex, &grok),
                false,
                false,
                &ModelSelection {
                    instance_id: "grok".to_owned(),
                    model: "grok-build".to_owned(),
                },
                None,
                &ModelSelection {
                    instance_id: "grok".to_owned(),
                    model: "grok-other".to_owned(),
                },
            ),
            None
        );
    }

    #[test]
    fn allows_unchanged_model_selections_for_restricted_providers() {
        let codex = ProviderInstanceId::new("codex").unwrap();
        let grok = ProviderInstanceId::new("grok").unwrap();
        let selection = ModelSelection {
            instance_id: "grok".to_owned(),
            model: "grok-build".to_owned(),
        };
        assert_eq!(
            get_started_thread_model_change_block_reason(
                &providers(&codex, &grok),
                true,
                false,
                &selection,
                None,
                &selection,
            ),
            None
        );
    }

    #[test]
    fn blocks_started_session_model_changes_for_providers_that_require_a_new_thread() {
        let codex = ProviderInstanceId::new("codex").unwrap();
        let grok = ProviderInstanceId::new("grok").unwrap();
        assert_eq!(
            get_started_thread_model_change_block_reason(
                &providers(&codex, &grok),
                true,
                false,
                &ModelSelection {
                    instance_id: "grok".to_owned(),
                    model: "grok-build".to_owned(),
                },
                None,
                &ModelSelection {
                    instance_id: "grok".to_owned(),
                    model: "grok-other".to_owned(),
                },
            ),
            Some(ModelChangeBlockCopy {
                title: "Start a new chat to change models".to_owned(),
                description:
                    "This provider does not allow switching models after a conversation has started."
                        .to_owned(),
            })
        );
    }
}
