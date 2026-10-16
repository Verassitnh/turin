//! The pure helpers of `ProviderEventIngestor.ts`: plan step durations,
//! the native-branch rewind test, and provider-turn analytics properties;
//! plus `boundFileChangeForStorage` from `WireProjection.ts`.

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use turin_contracts::{
    ProviderThreadId, RunId, UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2NativeRefStrength, OrchestrationV2PlanStep, OrchestrationV2PlanStepStatus,
        OrchestrationV2ProviderTurn, OrchestrationV2Run, OrchestrationV2RunAttempt,
        OrchestrationV2RunStatus, OrchestrationV2TurnItem,
    },
    provider_instance::ProviderDriverKind,
    provider_policy::{ProviderInteractionMode, RuntimeMode},
};

const MAX_DETAIL_STRING_BYTES: usize = 32_768;
const TRUNCATION_NOTE: &str = "\n… output truncated for transport";

/// `truncateDetail`: at most `max_bytes` of UTF-8, cut on a character boundary.
fn truncate_detail(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let prefix = String::from_utf8_lossy(&value.as_bytes()[..max_bytes]).into_owned();
    let prefix = prefix.strip_suffix('\u{FFFD}').unwrap_or(&prefix);
    format!("{prefix}{TRUNCATION_NOTE}")
}

/// `boundFileChangeForStorage`: bounds the text a provider file change keeps in
/// the event log and projection. Clients never receive more than this.
pub fn bound_file_change_for_storage(mut item: OrchestrationV2TurnItem) -> OrchestrationV2TurnItem {
    if let OrchestrationV2TurnItem::FileChange {
        diff_str,
        old_str,
        new_str,
        ..
    } = &mut item
    {
        for text in [diff_str, old_str, new_str].into_iter().flatten() {
            if text.len() > MAX_DETAIL_STRING_BYTES {
                *text = truncate_detail(text, MAX_DETAIL_STRING_BYTES);
            }
        }
    }
    item
}

fn parse_millis(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

/// `withPlanStepDurations`: stamps the todo list's steps with the time they
/// ran. Provider step ids may be positional, so changed text inherits no timing.
pub fn with_plan_step_durations(
    steps: &[OrchestrationV2PlanStep],
    previous: Option<&[OrchestrationV2PlanStep]>,
    occurred_at: UtcDateTime,
) -> Vec<OrchestrationV2PlanStep> {
    use OrchestrationV2PlanStepStatus as Status;
    let occurred_iso = occurred_at.to_string();
    let occurred_ms = occurred_at.as_datetime().timestamp_millis();
    let previous_by_id: HashMap<_, _> = previous
        .unwrap_or_default()
        .iter()
        .map(|step| (step.id.as_str(), step))
        .collect();
    let previous_step = |step: &OrchestrationV2PlanStep| {
        previous_by_id
            .get(step.id.as_str())
            .copied()
            .filter(|prior| prior.text == step.text)
    };
    let has_new_completion = steps.iter().any(|step| {
        step.status == Status::Completed
            && previous_step(step).is_none_or(|prior| prior.status != Status::Completed)
    });
    let mut fallback_completion_consumed = false;
    steps
        .iter()
        .map(|step| {
            let prior = previous_step(step);
            let base = OrchestrationV2PlanStep {
                id: step.id.clone(),
                text: step.text.clone(),
                status: step.status,
                duration_anchor_at: None,
                duration_ms: None,
            };
            match step.status {
                Status::Completed => {
                    if let Some(prior) = prior.filter(|prior| prior.status == Status::Completed) {
                        return OrchestrationV2PlanStep {
                            duration_ms: prior.duration_ms,
                            ..base
                        };
                    }
                    let anchor = if prior.is_some_and(|prior| prior.status == Status::Running)
                        || !fallback_completion_consumed
                    {
                        prior.and_then(|prior| prior.duration_anchor_at.clone())
                    } else {
                        Some(occurred_iso.clone())
                    };
                    fallback_completion_consumed = true;
                    let duration = match anchor.as_deref() {
                        None => 0,
                        Some(anchor) => parse_millis(anchor)
                            .map_or(0, |anchor_ms| (occurred_ms - anchor_ms).max(0)),
                    };
                    OrchestrationV2PlanStep {
                        duration_ms: (duration > 0).then_some(duration as u64),
                        ..base
                    }
                }
                Status::Running => OrchestrationV2PlanStep {
                    duration_anchor_at: Some(match prior {
                        Some(prior) if prior.status == Status::Running => prior
                            .duration_anchor_at
                            .clone()
                            .unwrap_or_else(|| occurred_iso.clone()),
                        _ => occurred_iso.clone(),
                    }),
                    ..base
                },
                Status::Pending => OrchestrationV2PlanStep {
                    duration_anchor_at: Some(match prior {
                        Some(prior) if !has_new_completion && prior.status == Status::Pending => {
                            prior
                                .duration_anchor_at
                                .clone()
                                .unwrap_or_else(|| occurred_iso.clone())
                        }
                        _ => occurred_iso.clone(),
                    }),
                    ..base
                },
            }
        })
        .collect()
}

/// Settled runs that carried conversation to the provider.
fn is_conversation_run_status(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Interrupted
            | OrchestrationV2RunStatus::Failed
    )
}

/// `runsOffNativeBranch`: settled runs on a provider thread that a native
/// rewind left off the active branch. A run whose provider turn has a strong
/// native ref is judged by that ref. A run without one cannot be located, so it
/// falls with everything after the last run still on the branch.
pub fn runs_off_native_branch<'a>(
    runs: &'a [OrchestrationV2Run],
    attempts: &[OrchestrationV2RunAttempt],
    provider_turns: &[OrchestrationV2ProviderTurn],
    provider_thread_id: &ProviderThreadId,
    retained_native_turn_ids: &std::collections::HashSet<String>,
    reporting_run_id: Option<&RunId>,
) -> Vec<&'a OrchestrationV2Run> {
    let native_turn_id = |run: &OrchestrationV2Run| -> Option<String> {
        let attempt = attempts
            .iter()
            .find(|candidate| Some(&candidate.id) == run.active_attempt_id.as_ref())?;
        let turn = provider_turns.iter().find(|turn| {
            Some(&turn.id) == attempt.provider_turn_id.as_ref()
                || turn.run_attempt_id.as_ref() == Some(&attempt.id)
        })?;
        let reference = turn.native_turn_ref.as_ref()?;
        (reference.strength == OrchestrationV2NativeRefStrength::Strong)
            .then(|| {
                reference
                    .native_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned())
            })
            .flatten()
    };
    let settled: Vec<(&OrchestrationV2Run, Option<String>)> = runs
        .iter()
        .filter(|run| {
            run.provider_thread_id.as_ref() == Some(provider_thread_id)
                && Some(&run.id) != reporting_run_id
                && is_conversation_run_status(run.status)
        })
        .map(|run| (run, native_turn_id(run)))
        .collect();
    let last_retained_ordinal = settled
        .iter()
        .filter(|(_, native)| {
            native
                .as_ref()
                .is_some_and(|id| retained_native_turn_ids.contains(id))
        })
        .map(|(run, _)| run.ordinal.get())
        .max()
        .unwrap_or(0);
    settled
        .into_iter()
        .filter(|(run, native)| match native {
            None => run.ordinal.get() > last_retained_ordinal,
            Some(id) => !retained_native_turn_ids.contains(id),
        })
        .map(|(run, _)| run)
        .collect()
}

/// What a completed turn's analytics event knows about the run
/// (`ProviderTurnAnalyticsContext`).
#[derive(Debug, Clone)]
pub struct ProviderTurnAnalyticsContext {
    pub model_selection: ModelSelection,
    pub runtime_mode: Option<RuntimeMode>,
    pub interaction_mode: Option<ProviderInteractionMode>,
}

/// `providerTurnAnalyticsProperties`.
pub fn provider_turn_analytics_properties(
    driver: &ProviderDriverKind,
    provider_turn: &OrchestrationV2ProviderTurn,
    context: Option<&ProviderTurnAnalyticsContext>,
) -> Map<String, Value> {
    let usage = provider_turn
        .turn_token_usage
        .as_ref()
        .and_then(|usage| serde_json::to_value(usage).ok());
    let usage_field = |key: &str| usage.as_ref().and_then(|usage| usage.get(key)).cloned();
    let mut properties = Map::new();
    properties.insert("provider".into(), json!(driver));
    properties.insert("terminalStatus".into(), json!(provider_turn.status));
    properties.insert(
        "usageStatus".into(),
        usage_field("usageStatus").unwrap_or_else(|| json!("unavailable")),
    );
    properties.insert(
        "usageScope".into(),
        usage_field("usageScope").unwrap_or_else(|| json!("main_agent")),
    );
    for key in [
        "hasSubagents",
        "inputTokens",
        "cachedInputTokens",
        "cacheCreationTokens",
        "outputTokens",
        "reasoningTokens",
    ] {
        if let Some(value) = usage_field(key) {
            properties.insert(key.to_owned(), value);
        }
    }
    if let Some(context) = context {
        let selection = &context.model_selection;
        properties.insert("model".into(), json!(selection.model));
        properties.insert("mixedModels".into(), json!(false));
        let effort = turin_shared::model::get_model_selection_string_option_value(
            Some(selection),
            "reasoningEffort",
        )
        .or_else(|| {
            turin_shared::model::get_model_selection_string_option_value(Some(selection), "effort")
        });
        if let Some(effort) = effort.filter(|effort| !effort.is_empty()) {
            properties.insert("effort".into(), json!(effort));
        }
        if let Some(mode) = context.runtime_mode {
            properties.insert("runtimeMode".into(), json!(mode));
        }
        if let Some(mode) = context.interaction_mode {
            properties.insert("interactionMode".into(), json!(mode));
        }
    }
    if let (Some(started), Some(completed)) = (provider_turn.started_at, provider_turn.completed_at)
    {
        let duration =
            completed.as_datetime().timestamp_millis() - started.as_datetime().timestamp_millis();
        properties.insert("durationMs".into(), json!(duration.max(0)));
    }
    properties
}
