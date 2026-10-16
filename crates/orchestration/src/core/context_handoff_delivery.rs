//! Port of upstream/apps/server/src/orchestration-v2/ContextHandoffDelivery.ts
//!
//! Decides how pending context handoffs reach a provider thread for one turn:
//! injected natively when the adapter supports it, otherwise rendered inline
//! into the prompt. Every decision is persisted before the provider is touched,
//! so a crash mid-delivery leaves a marker instead of silently losing history.
//!
//! Deviations from upstream:
//! - The budget is passed as a resolved `i64`. Upstream accepted an Effect that
//!   yielded it; the only caller-visible difference is that the caller reports
//!   the budget lookup's own failure before calling in.
//! - Upstream's lazy `delivered` and `unsent` Effects are returned as data. The
//!   caller persists `delivered` once the provider accepts the turn and
//!   `unsent` if the provider refused before reading the prompt
//!   (`persist_handoffs`).

use std::collections::HashSet;

use async_trait::async_trait;
use turin_contracts::TurnItemId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ContextHandoff, OrchestrationV2ContextHandoffDelivery,
    OrchestrationV2ContextHandoffDeliveryStatus, OrchestrationV2ContextHandoffStrategy,
    OrchestrationV2ProviderThread,
};
use turin_provider_core::failure::ContextHandoffBudgetError;
use turin_provider_core::handoff_budget::{
    SelectHistoryInput, history_cost, render_history, select_history,
};
use turin_provider_core::provider_adapter::ProviderAdapterV2HistoricalContext;

use crate::pipeline::orchestrator_port::BoxError;

/// Persists one handoff record (upstream `input.persist`).
#[async_trait]
pub trait ContextHandoffStore: Send + Sync {
    async fn persist(&self, handoff: &OrchestrationV2ContextHandoff) -> Result<(), BoxError>;
}

/// Native history injection (upstream `input.inject`). Narrow seam: the provider
/// session owner implements it. Returns `false` when the provider refused the
/// turn before reading the history.
#[async_trait]
pub trait HistoricalContextInjector: Send + Sync {
    async fn inject(&self, history: &ProviderAdapterV2HistoricalContext) -> Result<bool, BoxError>;
}

/// `ContextHandoffDeliveryUncertainError`: a previous injection may or may not
/// have reached the provider, so the native thread must be replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Historical context delivery is uncertain; replace the native thread before retrying.")]
pub struct ContextHandoffDeliveryUncertainError;

#[derive(Debug, thiserror::Error)]
pub enum ContextHandoffDeliveryError {
    #[error(transparent)]
    Budget(#[from] ContextHandoffBudgetError),
    #[error(transparent)]
    Uncertain(#[from] ContextHandoffDeliveryUncertainError),
    #[error("context handoff persistence failed: {0}")]
    Persist(BoxError),
    #[error("context handoff injection failed: {0}")]
    Inject(BoxError),
}

pub struct DeliverContextHandoffsInput<'a> {
    pub handoffs: &'a [OrchestrationV2ContextHandoff],
    pub provider_thread: &'a OrchestrationV2ProviderThread,
    /// Resolved token budget for the handoff history.
    pub budget: i64,
    /// When true, the caller cannot put a transcript inline (compaction APIs);
    /// nothing is rendered and pending handoffs stay for the next ordinary turn.
    pub defer_inline: bool,
    pub already_delivered_item_ids: &'a HashSet<TurnItemId>,
    /// `None` means text-only delivery.
    pub inject: Option<&'a dyn HistoricalContextInjector>,
}

/// What the caller does with a delivery.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextHandoffDelivery {
    /// Text to prepend to the prompt. Empty when nothing was rendered inline.
    pub context: String,
    /// Records to persist with `inline` status once the turn is accepted.
    pub delivered: Vec<OrchestrationV2ContextHandoff>,
    /// Records to persist unchanged if the provider refused the turn.
    pub unsent: Vec<OrchestrationV2ContextHandoff>,
}

/// `Effect.forEach(handoffs, persist, { discard: true })`.
pub async fn persist_handoffs(
    store: &dyn ContextHandoffStore,
    handoffs: &[OrchestrationV2ContextHandoff],
) -> Result<(), BoxError> {
    for handoff in handoffs {
        store.persist(handoff).await?;
    }
    Ok(())
}

/// `deliverContextHandoffs`. Persist before and after injection: an ambiguous
/// pending delivery requires a fresh native thread.
pub async fn deliver_context_handoffs(
    input: DeliverContextHandoffsInput<'_>,
    store: &dyn ContextHandoffStore,
) -> Result<ContextHandoffDelivery, ContextHandoffDeliveryError> {
    let native_thread_id = input
        .provider_thread
        .native_thread_ref
        .as_ref()
        .and_then(|reference| reference.native_id.as_ref())
        .map(|id| id.as_str());
    let pending: Vec<&OrchestrationV2ContextHandoff> = input
        .handoffs
        .iter()
        .filter(|handoff| match (native_thread_id, &handoff.delivery) {
            (None, _) => true,
            (Some(native), Some(delivery)) => {
                delivery.native_thread_id != native
                    || delivery.status == OrchestrationV2ContextHandoffDeliveryStatus::Pending
            }
            (Some(_), None) => true,
        })
        .collect();
    if pending.is_empty() || (input.defer_inline && input.inject.is_none()) {
        return Ok(ContextHandoffDelivery::default());
    }

    let budget = input.budget;
    let mut coverage = pending
        .iter()
        .map(|handoff| {
            let coverage = handoff
                .history
                .as_ref()
                .map(|history| history.coverage.clone())
                .unwrap_or_else(|| {
                    format!(
                        "From thread {}, runs {}-{}. Recover history with t3_thread_read, view=activity; paginate with afterPosition, and use itemId/textOffset for long items.",
                        handoff.thread_id.as_str(),
                        handoff.covered_run_ordinals.from.0,
                        handoff.covered_run_ordinals.to.0,
                    )
                });
            format!(
                "Context handoff ({}):\n{coverage}",
                strategy_label(handoff.strategy)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Repeated failures can accumulate many recovery markers. Keep a single
    // thread-level entry point when detailed coverage would crowd out history;
    // its activity includes the original handoff/fork source references.
    if history_cost(&[], &coverage) as f64 > f64::min(4_000.0, budget as f64 / 2.0) {
        let mut strategies: Vec<&'static str> = Vec::new();
        for handoff in &pending {
            let label = strategy_wire(handoff.strategy);
            if !strategies.contains(&label) {
                strategies.push(label);
            }
        }
        let thread_id = input
            .provider_thread
            .app_thread_id
            .as_ref()
            .map(|id| id.as_str())
            .unwrap_or_else(|| pending[0].thread_id.as_str());
        coverage = format!(
            "Context handoff ({}). {} handoff records; detailed coverage references omitted. Recover history with t3_thread_read({{threadId:\"{thread_id}\",view:\"activity\",limit:20,maxCharsPerItem:4000}}); paginate with afterPosition=nextPosition. Follow fork/handoff source references in activity. For long items use itemId and textOffset=nextTextOffset until null.",
            strategies.join(", "),
            pending.len(),
        );
    }

    let mut seen = input.already_delivered_item_ids.clone();
    let messages: Vec<_> = pending
        .iter()
        .flat_map(|handoff| {
            handoff
                .history
                .iter()
                .flat_map(|history| history.messages.iter())
        })
        .filter(|message| seen.insert(message.item_id.clone()))
        .cloned()
        .collect();
    // Old preview handoffs remain readable. Their preformatted context is included
    // as a whole when it fits, otherwise the coverage marker points to retrieval.
    let old_context = pending
        .iter()
        .filter(|handoff| handoff.history.is_none())
        .map(|handoff| handoff.summary_text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let full_coverage = if !old_context.is_empty()
        && history_cost(&[], &format!("{coverage}\n{old_context}")) + 512 <= budget
    {
        format!("{coverage}\n{old_context}")
    } else {
        coverage.clone()
    };
    let omitted_items: u64 = pending
        .iter()
        .map(|handoff| {
            handoff
                .history
                .as_ref()
                .map_or(0, |history| history.omitted_items)
        })
        .sum();
    let selected = select_history(&SelectHistoryInput {
        messages: &messages,
        coverage: &full_coverage,
        omitted_items: Some(omitted_items as i64),
        budget,
    });
    if history_cost(&selected.messages, &selected.context) > budget {
        if input.defer_inline {
            return Ok(ContextHandoffDelivery::default());
        }
        return Err(ContextHandoffBudgetError.into());
    }

    let persist = |status: OrchestrationV2ContextHandoffDeliveryStatus| -> Vec<OrchestrationV2ContextHandoff> {
        pending
            .iter()
            .map(|handoff| {
                let mut record = (*handoff).clone();
                if let Some(native) = native_thread_id {
                    record.delivery = Some(OrchestrationV2ContextHandoffDelivery {
                        native_thread_id: native.to_owned(),
                        status,
                        omitted_item_ids: Some(omitted_item_ids(handoff, &selected.omitted_item_ids)),
                        item_ids: selected_item_ids(handoff, &selected.messages),
                    });
                }
                record
            })
            .collect()
    };

    if let (Some(inject), Some(native)) = (input.inject, native_thread_id) {
        if pending.iter().any(|handoff| {
            handoff.delivery.as_ref().is_some_and(|delivery| {
                delivery.native_thread_id == native
                    && delivery.status == OrchestrationV2ContextHandoffDeliveryStatus::Pending
            })
        }) {
            return Err(ContextHandoffDeliveryUncertainError.into());
        }
        persist_handoffs(
            store,
            &persist(OrchestrationV2ContextHandoffDeliveryStatus::Pending),
        )
        .await
        .map_err(ContextHandoffDeliveryError::Persist)?;
        let injected = inject
            .inject(&ProviderAdapterV2HistoricalContext {
                messages: selected.messages.clone(),
                context: selected.context.clone(),
            })
            .await
            .map_err(ContextHandoffDeliveryError::Inject)?;
        if injected {
            persist_handoffs(
                store,
                &persist(OrchestrationV2ContextHandoffDeliveryStatus::Injected),
            )
            .await
            .map_err(ContextHandoffDeliveryError::Persist)?;
            return Ok(ContextHandoffDelivery::default());
        }
    } else {
        // Text-only delivery can also be accepted before a connection drops.
        persist_handoffs(
            store,
            &persist(OrchestrationV2ContextHandoffDeliveryStatus::Pending),
        )
        .await
        .map_err(ContextHandoffDeliveryError::Persist)?;
    }

    let unsent: Vec<OrchestrationV2ContextHandoff> =
        pending.iter().map(|handoff| (*handoff).clone()).collect();
    if input.defer_inline {
        // Compaction APIs cannot accept an inline transcript. Keep it available
        // for the next ordinary turn when native injection is unsupported.
        persist_handoffs(store, &unsent)
            .await
            .map_err(ContextHandoffDeliveryError::Persist)?;
        return Ok(ContextHandoffDelivery::default());
    }
    Ok(ContextHandoffDelivery {
        context: render_history(&selected.messages, &selected.context),
        delivered: persist(OrchestrationV2ContextHandoffDeliveryStatus::Inline),
        unsent,
    })
}

/// `handoff.strategy === "fork_delta_summary" ? "merge_back / fork_delta_summary" : handoff.strategy`.
fn strategy_label(strategy: OrchestrationV2ContextHandoffStrategy) -> &'static str {
    match strategy {
        OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary => {
            "merge_back / fork_delta_summary"
        }
        other => strategy_wire(other),
    }
}

/// The wire spelling of a strategy (`"manual_context"`, …).
fn strategy_wire(strategy: OrchestrationV2ContextHandoffStrategy) -> &'static str {
    match strategy {
        OrchestrationV2ContextHandoffStrategy::DeltaSinceTargetLastSeen => {
            "delta_since_target_last_seen"
        }
        OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary => "fork_delta_summary",
        OrchestrationV2ContextHandoffStrategy::FullThreadSummary => "full_thread_summary",
        OrchestrationV2ContextHandoffStrategy::CheckpointSummary => "checkpoint_summary",
        OrchestrationV2ContextHandoffStrategy::ManualContext => "manual_context",
    }
}

/// Items the handoff already had omitted, plus its own items the budget dropped.
fn omitted_item_ids(
    handoff: &OrchestrationV2ContextHandoff,
    selected_omitted: &[TurnItemId],
) -> Vec<TurnItemId> {
    let Some(history) = &handoff.history else {
        return Vec::new();
    };
    let mut ids = history.omitted_item_ids.clone().unwrap_or_default();
    ids.extend(
        history
            .messages
            .iter()
            .filter(|message| selected_omitted.contains(&message.item_id))
            .map(|message| message.item_id.clone()),
    );
    ids
}

/// The handoff's own items that made it into the selected history.
fn selected_item_ids(
    handoff: &OrchestrationV2ContextHandoff,
    selected: &[turin_contracts::orchestration_v2::OrchestrationV2HistoricalMessage],
) -> Vec<TurnItemId> {
    let Some(history) = &handoff.history else {
        return Vec::new();
    };
    selected
        .iter()
        .filter(|message| {
            history
                .messages
                .iter()
                .any(|candidate| candidate.item_id == message.item_id)
        })
        .map(|message| message.item_id.clone())
        .collect()
}
