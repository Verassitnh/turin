//! Port of upstream/packages/client-runtime/src/handoff.ts

use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Run, OrchestrationV2RunStatus, OrchestrationV2TurnItem,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::{PositiveInt, RunId};

use super::provider_types::ServerProvider;

/// The subset of a projection run that handoff rows read. Kept minimal so the
/// timeline can hold a content-stable snapshot: run status/timestamps churn on
/// every stream event, but these fields only change when a run is added.
#[derive(Debug, Clone, PartialEq)]
pub struct HandoffTimelineRun {
    pub id: RunId,
    pub ordinal: PositiveInt,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
}

impl From<&OrchestrationV2Run> for HandoffTimelineRun {
    fn from(run: &OrchestrationV2Run) -> Self {
        Self {
            id: run.id.clone(),
            ordinal: run.ordinal,
            provider_instance_id: run.provider_instance_id.clone(),
            model_selection: run.model_selection.clone(),
        }
    }
}

/// One side of a handoff: a provider instance and, when known, its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffEndpoint {
    pub instance_id: ProviderInstanceId,
    pub model: Option<String>,
}

impl From<&ModelSelection> for HandoffEndpoint {
    fn from(selection: &ModelSelection) -> Self {
        Self {
            instance_id: selection.instance_id.clone(),
            model: Some(selection.model.as_str().to_owned()),
        }
    }
}

/// `ReturnType<typeof resolveHandoffEndpoints>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffEndpoints {
    pub from: Vec<HandoffEndpoint>,
    pub to: HandoffEndpoint,
}

/// Preview the next send's handoff without adding anything to durable history.
///
/// Upstream takes the thread projection and reads only `projection.runs`; this takes those runs
/// (`None` stands for a missing projection).
pub fn resolve_pending_handoff(
    runs: Option<&[OrchestrationV2Run]>,
    selection: Option<&ModelSelection>,
    providers: &[ServerProvider],
) -> Option<HandoffEndpoints> {
    let from = runs?
        .iter()
        .rev()
        .find(|run| run.started_at.is_some() && run.status != OrchestrationV2RunStatus::RolledBack)
        .map(|run| &run.model_selection);
    let (from, selection) = (from?, selection?);
    let source = providers
        .iter()
        .find(|provider| provider.instance_id == from.instance_id);
    let target = providers
        .iter()
        .find(|provider| provider.instance_id == selection.instance_id);
    let needs_handoff = if from.instance_id == selection.instance_id {
        from.model != selection.model
            && target
                .is_some_and(|target| target.requires_new_thread_for_model_change == Some(true))
    } else {
        // Accounts sharing a continuation group resume the same native thread.
        match source.and_then(|source| source.continuation.as_ref().map(|c| (source, c))) {
            None => true,
            Some((source, continuation)) => match target {
                // `source.driver !== target?.driver` with no target
                None => true,
                Some(target) => {
                    source.driver != target.driver
                        || Some(&continuation.group_key)
                            != target.continuation.as_ref().map(|c| &c.group_key)
                }
            },
        }
    };
    needs_handoff.then(|| HandoffEndpoints {
        from: vec![HandoffEndpoint::from(from)],
        to: HandoffEndpoint::from(selection),
    })
}

/// `Pick<Extract<OrchestrationV2TurnItem, { type: "handoff" }>, "runId" | "fromModelSelections" |
/// "fromProviderInstanceIds" | "toModel" | "toProviderInstanceId">`
#[derive(Debug, Clone, Copy)]
pub struct HandoffItem<'a> {
    pub run_id: Option<&'a RunId>,
    pub from_model_selections: Option<&'a [ModelSelection]>,
    pub from_provider_instance_ids: &'a [ProviderInstanceId],
    pub to_model: Option<&'a str>,
    pub to_provider_instance_id: &'a ProviderInstanceId,
}

impl<'a> HandoffItem<'a> {
    /// The handoff fields of a turn item; `None` for any other kind of item.
    pub fn from_turn_item(item: &'a OrchestrationV2TurnItem) -> Option<Self> {
        match item {
            OrchestrationV2TurnItem::Handoff {
                run_id,
                from_model_selections,
                from_provider_instance_ids,
                to_model,
                to_provider_instance_id,
                ..
            } => Some(Self {
                run_id: run_id.as_ref(),
                from_model_selections: from_model_selections.as_deref(),
                from_provider_instance_ids,
                to_model: to_model.as_deref(),
                to_provider_instance_id,
            }),
            _ => None,
        }
    }
}

pub fn resolve_handoff_endpoints(
    item: &HandoffItem<'_>,
    runs: &[HandoffTimelineRun],
) -> HandoffEndpoints {
    // Items persisted before models were stamped only carry instance ids;
    // recover the models from the thread's runs (the handoff's own run is
    // the target, the newest earlier run per source instance is the origin).
    // Clients fall back to the provider display name when neither source has a model.
    let handoff_run = item
        .run_id
        .and_then(|run_id| runs.iter().find(|run| run.id == *run_id));
    let to_model = item.to_model.map(str::to_owned).or_else(|| {
        handoff_run
            .filter(|run| run.provider_instance_id == *item.to_provider_instance_id)
            .map(|run| run.model_selection.model.as_str().to_owned())
    });
    let from_endpoints: Vec<HandoffEndpoint> = match item.from_model_selections {
        Some(selections) if !selections.is_empty() => {
            selections.iter().map(HandoffEndpoint::from).collect()
        }
        _ => item
            .from_provider_instance_ids
            .iter()
            .map(|instance_id| HandoffEndpoint {
                instance_id: instance_id.clone(),
                model: latest_run_model_before(
                    runs,
                    instance_id,
                    handoff_run.map(|run| run.ordinal),
                ),
            })
            .collect(),
    };
    HandoffEndpoints {
        from: from_endpoints,
        to: HandoffEndpoint {
            instance_id: item.to_provider_instance_id.clone(),
            model: to_model,
        },
    }
}

/// Model of the newest run for `instance_id` that started before the handoff's
/// own run. Legacy handoff items don't record their source models, but the
/// covered runs are still in the projection.
fn latest_run_model_before(
    runs: &[HandoffTimelineRun],
    instance_id: &ProviderInstanceId,
    before_ordinal: Option<PositiveInt>,
) -> Option<String> {
    let mut latest: Option<&HandoffTimelineRun> = None;
    for run in runs {
        if run.provider_instance_id != *instance_id {
            continue;
        }
        if before_ordinal.is_some_and(|before| run.ordinal >= before) {
            continue;
        }
        if latest.is_none_or(|latest| run.ordinal > latest.ordinal) {
            latest = Some(run);
        }
    }
    latest.map(|run| run.model_selection.model.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::helpers::provider_types::ServerProviderContinuation;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::provider_instance::ProviderDriverKind;

    fn instance(id: &str) -> ProviderInstanceId {
        ProviderInstanceId::new(id).unwrap()
    }

    fn selection(instance_id: &ProviderInstanceId, model: &str) -> ModelSelection {
        ModelSelection {
            instance_id: instance_id.clone(),
            model: TrimmedNonEmptyString::new(model).unwrap(),
            options: None,
        }
    }

    fn run(
        id: &str,
        ordinal: u64,
        instance_id: &ProviderInstanceId,
        model: &str,
    ) -> HandoffTimelineRun {
        HandoffTimelineRun {
            id: RunId::new(id).unwrap(),
            ordinal: PositiveInt::new(ordinal).unwrap(),
            provider_instance_id: instance_id.clone(),
            model_selection: selection(instance_id, model),
        }
    }

    fn endpoint(instance_id: &ProviderInstanceId, model: Option<&str>) -> HandoffEndpoint {
        HandoffEndpoint {
            instance_id: instance_id.clone(),
            model: model.map(str::to_owned),
        }
    }

    fn item_parts() -> (RunId, Vec<ProviderInstanceId>, ProviderInstanceId) {
        (
            RunId::new("target").unwrap(),
            vec![instance("codex_personal")],
            instance("claudeAgent"),
        )
    }

    #[test]
    fn preserves_stamped_models_including_several_models_from_the_same_provider() {
        let (run_id, from_ids, to) = item_parts();
        let from = &from_ids[0];
        let from_model_selections = vec![selection(from, "source-a"), selection(from, "source-b")];
        let item = HandoffItem {
            run_id: Some(&run_id),
            from_model_selections: Some(&from_model_selections),
            from_provider_instance_ids: &from_ids,
            to_model: Some("destination"),
            to_provider_instance_id: &to,
        };
        assert_eq!(
            resolve_handoff_endpoints(&item, &[run("target", 2, &to, "later-model")]),
            HandoffEndpoints {
                from: vec![
                    endpoint(from, Some("source-a")),
                    endpoint(from, Some("source-b"))
                ],
                to: endpoint(&to, Some("destination")),
            }
        );
    }

    #[test]
    fn recovers_legacy_models_from_the_handoff_run_and_latest_earlier_source_run() {
        let (run_id, from_ids, to) = item_parts();
        let from = &from_ids[0];
        let item = HandoffItem {
            run_id: Some(&run_id),
            from_model_selections: None,
            from_provider_instance_ids: &from_ids,
            to_model: None,
            to_provider_instance_id: &to,
        };
        let runs = [
            run("later", 4, from, "wrong-later-model"),
            run("old", 1, from, "old-model"),
            run("target", 3, &to, "destination"),
            run("source", 2, from, "source-model"),
        ];
        assert_eq!(
            resolve_handoff_endpoints(&item, &runs),
            HandoffEndpoints {
                from: vec![endpoint(from, Some("source-model"))],
                to: endpoint(&to, Some("destination")),
            }
        );
    }

    #[test]
    fn retains_provider_identities_when_historical_runs_are_not_loaded() {
        let (run_id, from_ids, to) = item_parts();
        let item = HandoffItem {
            run_id: Some(&run_id),
            from_model_selections: None,
            from_provider_instance_ids: &from_ids,
            to_model: None,
            to_provider_instance_id: &to,
        };
        assert_eq!(
            resolve_handoff_endpoints(&item, &[]),
            HandoffEndpoints {
                from: vec![endpoint(&from_ids[0], None)],
                to: endpoint(&to, None),
            }
        );
    }

    #[test]
    fn does_not_borrow_a_target_model_from_another_provider() {
        let (run_id, from_ids, to) = item_parts();
        let item = HandoffItem {
            run_id: Some(&run_id),
            from_model_selections: None,
            from_provider_instance_ids: &from_ids,
            to_model: None,
            to_provider_instance_id: &to,
        };
        let endpoints =
            resolve_handoff_endpoints(&item, &[run("target", 2, &from_ids[0], "wrong-model")]);
        assert_eq!(endpoints.to.model, None);
    }

    /// One started run on `codex_personal` with model `source`, built from wire JSON.
    fn projection_runs(from: &ProviderInstanceId) -> Vec<OrchestrationV2Run> {
        serde_json::from_value(serde_json::json!([{
            "id": "source",
            "threadId": "thread",
            "ordinal": 1,
            "providerInstanceId": from.as_str(),
            "modelSelection": { "instanceId": from.as_str(), "model": "source" },
            "providerThreadId": null,
            "userMessageId": "message",
            "rootNodeId": null,
            "activeAttemptId": null,
            "status": "completed",
            "requestedAt": "2026-10-11T00:00:00Z",
            "startedAt": "2026-10-11T00:00:00Z",
            "completedAt": null,
            "checkpointId": null,
            "contextHandoffId": null,
        }]))
        .unwrap()
    }

    #[test]
    fn pending_handoff_appears_on_selection_updates_the_destination_and_disappears_on_returning() {
        let (_, from_ids, to) = item_parts();
        let from = &from_ids[0];
        let runs = projection_runs(from);

        assert_eq!(
            resolve_pending_handoff(Some(&runs), Some(&selection(&to, "first")), &[]),
            Some(HandoffEndpoints {
                from: vec![endpoint(from, Some("source"))],
                to: endpoint(&to, Some("first")),
            })
        );
        assert_eq!(
            resolve_pending_handoff(Some(&runs), Some(&selection(&to, "second")), &[])
                .map(|endpoints| endpoints.to.model),
            Some(Some("second".to_owned()))
        );
        assert_eq!(
            resolve_pending_handoff(Some(&runs), Some(&selection(from, "source")), &[]),
            None
        );
        assert_eq!(
            resolve_pending_handoff(Some(&runs), Some(&selection(from, "other")), &[]),
            None
        );
    }

    #[test]
    fn only_previews_a_same_provider_model_change_when_it_needs_a_fresh_native_thread() {
        let (_, from_ids, _) = item_parts();
        let from = &from_ids[0];
        let runs = projection_runs(from);
        let mut provider =
            ServerProvider::new(from.clone(), ProviderDriverKind::new("codex").unwrap());
        provider.requires_new_thread_for_model_change = Some(true);
        assert_eq!(
            resolve_pending_handoff(Some(&runs), Some(&selection(from, "other")), &[provider])
                .map(|endpoints| endpoints.to.model),
            Some(Some("other".to_owned()))
        );
    }

    #[test]
    fn resumes_compatible_account_overlays_without_showing_a_handoff() {
        let (_, from_ids, to) = item_parts();
        let runs = projection_runs(&from_ids[0]);
        let providers: Vec<ServerProvider> = [from_ids[0].clone(), to.clone()]
            .into_iter()
            .map(|instance_id| {
                let mut provider =
                    ServerProvider::new(instance_id, ProviderDriverKind::new("codex").unwrap());
                provider.continuation = Some(ServerProviderContinuation {
                    group_key: "shared".to_owned(),
                });
                provider
            })
            .collect();
        assert_eq!(
            resolve_pending_handoff(Some(&runs), Some(&selection(&to, "target")), &providers),
            None
        );
    }
}
