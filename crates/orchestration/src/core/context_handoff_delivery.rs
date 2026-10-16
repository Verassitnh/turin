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

#[cfg(test)]
mod tests {
    //! Port of the "handoff delivery" describe in
    //! upstream/apps/server/src/orchestration-v2/ContextHandoffBudget.test.ts.
    //!
    //! The budget argument is already an `i64`. Upstream's lazy budget Effect
    //! is not a seam here, so the "loads the history budget only when a
    //! handoff needs delivery" case asserts that the store and injector stay
    //! idle on the three early returns, then that a real delivery includes
    //! the first message.

    use std::collections::HashSet;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use turin_contracts::orchestration_v2::{
        OrchestrationV2ContextHandoff, OrchestrationV2ContextHandoffDelivery,
        OrchestrationV2ContextHandoffDeliveryStatus, OrchestrationV2ContextHandoffHistory,
        OrchestrationV2ContextHandoffStatus, OrchestrationV2ContextHandoffStrategy,
        OrchestrationV2CoveredRunOrdinals, OrchestrationV2HistoricalMessage,
        OrchestrationV2HistoricalMessageRole, OrchestrationV2NativeRefStrength,
        OrchestrationV2ProviderRef, OrchestrationV2ProviderThread,
        OrchestrationV2ProviderThreadStatus,
    };
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
    use turin_contracts::{
        ContextHandoffId, PositiveInt, ProviderThreadId, RunId, ThreadId, TrimmedNonEmptyString,
        TurnItemId, UtcDateTime,
    };
    use turin_provider_core::handoff_budget::{history_cost, history_response_items};
    use turin_provider_core::provider_adapter::ProviderAdapterV2HistoricalContext;

    use super::{
        ContextHandoffDeliveryError, ContextHandoffStore, DeliverContextHandoffsInput,
        HistoricalContextInjector, deliver_context_handoffs, persist_handoffs,
    };
    use crate::pipeline::orchestrator_port::BoxError;

    struct MemoryStore {
        latest: Mutex<Option<OrchestrationV2ContextHandoff>>,
        statuses: Mutex<Vec<OrchestrationV2ContextHandoffDeliveryStatus>>,
        writes: Mutex<usize>,
    }

    impl MemoryStore {
        fn new() -> Self {
            Self {
                latest: Mutex::new(None),
                statuses: Mutex::new(Vec::new()),
                writes: Mutex::new(0),
            }
        }

        fn latest(&self) -> Option<OrchestrationV2ContextHandoff> {
            self.latest.lock().unwrap().clone()
        }

        fn writes(&self) -> usize {
            *self.writes.lock().unwrap()
        }
    }

    #[async_trait]
    impl ContextHandoffStore for MemoryStore {
        async fn persist(&self, handoff: &OrchestrationV2ContextHandoff) -> Result<(), BoxError> {
            if let Some(delivery) = &handoff.delivery {
                self.statuses.lock().unwrap().push(delivery.status);
            }
            *self.latest.lock().unwrap() = Some(handoff.clone());
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }
    }

    enum InjectMode {
        Accept,
        Refuse,
        Fail,
    }

    struct FakeInjector {
        mode: InjectMode,
        calls: Mutex<Vec<ProviderAdapterV2HistoricalContext>>,
    }

    impl FakeInjector {
        fn new(mode: InjectMode) -> Self {
            Self {
                mode,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<ProviderAdapterV2HistoricalContext> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl HistoricalContextInjector for FakeInjector {
        async fn inject(
            &self,
            history: &ProviderAdapterV2HistoricalContext,
        ) -> Result<bool, BoxError> {
            self.calls.lock().unwrap().push(history.clone());
            match self.mode {
                InjectMode::Accept => Ok(true),
                InjectMode::Refuse => Ok(false),
                InjectMode::Fail => Err(anyhow::anyhow!("connection lost").into()),
            }
        }
    }

    fn now() -> UtcDateTime {
        "2026-09-17T00:00:00.000Z".parse().unwrap()
    }

    fn message(
        id: &str,
        role: OrchestrationV2HistoricalMessageRole,
        text: &str,
    ) -> OrchestrationV2HistoricalMessage {
        OrchestrationV2HistoricalMessage {
            role,
            text: text.to_owned(),
            run_status: None,
            thread_id: ThreadId::new("thread:handoff").unwrap(),
            run_id: Some(RunId::new("run:source").unwrap()),
            item_id: TurnItemId::new(id).unwrap(),
            provider_thread_id: Some(ProviderThreadId::new("provider-thread:source").unwrap()),
            status: "interrupted".to_owned(),
            kind: match role {
                OrchestrationV2HistoricalMessageRole::User => "user_message",
                OrchestrationV2HistoricalMessageRole::Assistant => "assistant_message",
            }
            .to_owned(),
        }
    }

    fn sample_messages() -> Vec<OrchestrationV2HistoricalMessage> {
        vec![
            message(
                "item:one",
                OrchestrationV2HistoricalMessageRole::User,
                "Preserve every line.\n\n  And this indentation.\n",
            ),
            message(
                "item:two",
                OrchestrationV2HistoricalMessageRole::Assistant,
                &format!("Partial work: 日本語 🧪 مرحبا\n{}", "x".repeat(600)),
            ),
        ]
    }

    fn handoff_with(
        id: &str,
        messages: Vec<OrchestrationV2HistoricalMessage>,
        omitted_items: u64,
        omitted_item_ids: Option<Vec<TurnItemId>>,
    ) -> OrchestrationV2ContextHandoff {
        let now = now();
        OrchestrationV2ContextHandoff {
            id: ContextHandoffId::new(id).unwrap(),
            transfer_id: None,
            thread_id: ThreadId::new("thread:handoff").unwrap(),
            target_run_id: RunId::new("run:target").unwrap(),
            from_provider_thread_ids: Vec::new(),
            to_provider_thread_id: ProviderThreadId::new("provider-thread:target").unwrap(),
            covered_run_ordinals: OrchestrationV2CoveredRunOrdinals {
                from: PositiveInt::new(1).unwrap(),
                to: PositiveInt::new(2).unwrap(),
            },
            strategy: OrchestrationV2ContextHandoffStrategy::FullThreadSummary,
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: String::new(),
            history: Some(OrchestrationV2ContextHandoffHistory {
                messages,
                coverage: "Historical context; retrieve thread:handoff with t3_thread_read."
                    .to_owned(),
                omitted_items,
                omitted_item_ids,
            }),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn handoff() -> OrchestrationV2ContextHandoff {
        handoff_with("handoff:one", sample_messages(), 0, None)
    }

    fn provider_thread() -> OrchestrationV2ProviderThread {
        let now = now();
        let driver = ProviderDriverKind::new("codex").unwrap();
        OrchestrationV2ProviderThread {
            id: ProviderThreadId::new("provider-thread:target").unwrap(),
            driver: driver.clone(),
            provider_instance_id: ProviderInstanceId::new("codex").unwrap(),
            provider_session_id: None,
            app_thread_id: Some(ThreadId::new("thread:handoff").unwrap()),
            owner_node_id: None,
            native_thread_ref: Some(OrchestrationV2ProviderRef {
                driver,
                native_id: Some(TrimmedNonEmptyString::new("native:target").unwrap()),
                strength: OrchestrationV2NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_conversation_head_ref: None,
            status: OrchestrationV2ProviderThreadStatus::Idle,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: now,
            updated_at: now,
        }
    }

    async fn deliver(
        handoffs: &[OrchestrationV2ContextHandoff],
        budget: i64,
        defer_inline: bool,
        already: &HashSet<TurnItemId>,
        store: &MemoryStore,
        inject: Option<&FakeInjector>,
    ) -> Result<super::ContextHandoffDelivery, ContextHandoffDeliveryError> {
        let thread = provider_thread();
        deliver_context_handoffs(
            DeliverContextHandoffsInput {
                handoffs,
                provider_thread: &thread,
                budget,
                defer_inline,
                already_delivered_item_ids: already,
                inject: inject.map(|inject| inject as &dyn HistoricalContextInjector),
            },
            store,
        )
        .await
    }

    #[tokio::test]
    async fn records_omitted_recovery_coverage_separately_from_injected_and_inline_text() {
        let messages = sample_messages();
        let omitted_before = TurnItemId::new("item:omitted-during-preparation").unwrap();
        let oversized = message(
            "item:oversized",
            OrchestrationV2HistoricalMessageRole::User,
            &"x".repeat(20_000),
        );
        for native in [true, false] {
            let source = handoff_with(
                "handoff:one",
                {
                    let mut messages = messages.clone();
                    messages.push(oversized.clone());
                    messages
                },
                1,
                Some(vec![omitted_before.clone()]),
            );
            let store = MemoryStore::new();
            let injector = FakeInjector::new(if native {
                InjectMode::Accept
            } else {
                InjectMode::Refuse
            });
            let result = deliver(
                std::slice::from_ref(&source),
                16_000,
                false,
                &HashSet::new(),
                &store,
                Some(&injector),
            )
            .await
            .unwrap();
            let captured = injector.calls();
            assert_eq!(captured.len(), 1);
            assert!(captured[0].context.contains("omitted 2 items"));
            assert!(
                !captured[0]
                    .messages
                    .iter()
                    .any(|item| item.item_id == oversized.item_id)
            );
            let durable = store.latest().unwrap();
            assert_eq!(
                durable.delivery.as_ref().unwrap().status,
                if native {
                    OrchestrationV2ContextHandoffDeliveryStatus::Injected
                } else {
                    OrchestrationV2ContextHandoffDeliveryStatus::Pending
                }
            );
            persist_handoffs(&store, &result.delivered).await.unwrap();
            let durable = store.latest().unwrap();
            assert_eq!(
                durable.delivery.as_ref().unwrap().status,
                if native {
                    OrchestrationV2ContextHandoffDeliveryStatus::Injected
                } else {
                    OrchestrationV2ContextHandoffDeliveryStatus::Inline
                }
            );
            assert_eq!(
                durable.delivery.as_ref().unwrap().item_ids,
                messages
                    .iter()
                    .map(|item| item.item_id.clone())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                durable.delivery.as_ref().unwrap().omitted_item_ids,
                Some(vec![omitted_before.clone(), oversized.item_id.clone()])
            );
            let decoded: OrchestrationV2ContextHandoff =
                serde_json::from_value(serde_json::to_value(&durable).unwrap()).unwrap();
            assert_eq!(decoded.delivery, durable.delivery);
        }
    }

    #[tokio::test]
    async fn skips_store_and_injector_until_a_handoff_needs_delivery() {
        let store = MemoryStore::new();
        let empty = deliver(&[], 16_000, false, &HashSet::new(), &store, None)
            .await
            .unwrap();
        assert_eq!(empty.context, "");
        let deferred = deliver(&[handoff()], 16_000, true, &HashSet::new(), &store, None)
            .await
            .unwrap();
        assert_eq!(deferred.context, "");
        let mut already = handoff();
        already.delivery = Some(OrchestrationV2ContextHandoffDelivery {
            native_thread_id: "native:target".to_owned(),
            status: OrchestrationV2ContextHandoffDeliveryStatus::Injected,
            item_ids: Vec::new(),
            omitted_item_ids: None,
        });
        let settled = deliver(&[already], 16_000, false, &HashSet::new(), &store, None)
            .await
            .unwrap();
        assert_eq!(settled.context, "");
        assert_eq!(store.writes(), 0);
        let result = deliver(&[handoff()], 16_000, false, &HashSet::new(), &store, None)
            .await
            .unwrap();
        assert!(result.context.contains(&sample_messages()[0].text));
    }

    #[tokio::test]
    async fn persists_successful_injection_before_turn_start_and_skips_it_on_retry() {
        let store = MemoryStore::new();
        let injector = FakeInjector::new(InjectMode::Accept);
        let source = handoff();
        let first = deliver(
            &[source],
            16_000,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await
        .unwrap();
        assert_eq!(first.context, "");
        assert_eq!(
            *store.statuses.lock().unwrap(),
            vec![
                OrchestrationV2ContextHandoffDeliveryStatus::Pending,
                OrchestrationV2ContextHandoffDeliveryStatus::Injected,
            ]
        );
        let captured = injector.calls();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            history_response_items(&captured[0].messages, &captured[0].context).len(),
            3
        );
        let durable = store.latest().unwrap();
        let second = deliver(
            std::slice::from_ref(&durable),
            16_000,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await
        .unwrap();
        assert_eq!(second.context, "");
        assert_eq!(injector.calls().len(), 1);
    }

    #[tokio::test]
    async fn uses_the_same_selection_on_explicit_native_fallback() {
        let messages = sample_messages();
        let store = MemoryStore::new();
        let injector = FakeInjector::new(InjectMode::Refuse);
        let result = deliver(
            &[handoff()],
            16_000,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await
        .unwrap();
        assert!(result.context.contains(&messages[0].text));
        assert!(result.context.contains(&messages[1].text));
        assert_eq!(
            store.latest().unwrap().delivery.unwrap().status,
            OrchestrationV2ContextHandoffDeliveryStatus::Pending
        );
        persist_handoffs(&store, &result.delivered).await.unwrap();
        assert_eq!(
            store.latest().unwrap().delivery.unwrap().status,
            OrchestrationV2ContextHandoffDeliveryStatus::Inline
        );
    }

    #[tokio::test]
    async fn defers_unsupported_compaction_history_until_a_normal_turn() {
        let store = MemoryStore::new();
        let injector = FakeInjector::new(InjectMode::Refuse);
        let compact = deliver(
            &[handoff()],
            16_000,
            true,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await
        .unwrap();
        persist_handoffs(&store, &compact.delivered).await.unwrap();
        assert_eq!(compact.context, "");
        assert!(store.latest().unwrap().delivery.is_none());
        let next = deliver(
            &[store.latest().unwrap()],
            16_000,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await
        .unwrap();
        assert!(next.context.contains(&sample_messages()[0].text));
        persist_handoffs(&store, &next.delivered).await.unwrap();
        assert_eq!(
            store.latest().unwrap().delivery.unwrap().status,
            OrchestrationV2ContextHandoffDeliveryStatus::Inline
        );
    }

    #[tokio::test]
    async fn bounds_accumulated_recovery_markers_while_keeping_omitted_history_discoverable() {
        let many: Vec<_> = (0..100)
            .map(|index| {
                handoff_with(
                    &format!("handoff:retry:{index}"),
                    sample_messages(),
                    0,
                    None,
                )
            })
            .collect();
        let store = MemoryStore::new();
        let injector = FakeInjector::new(InjectMode::Accept);
        let result = deliver(
            &many,
            2_500,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await
        .unwrap();
        assert_eq!(result.context, "");
        let captured = injector.calls();
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0]
                .context
                .contains("detailed coverage references omitted")
        );
        assert!(captured[0].context.contains("t3_thread_read"));
        assert!(captured[0].context.contains("thread:handoff"));
        assert!(history_cost(&captured[0].messages, &captured[0].context) <= 2_500);
        assert!(!captured[0].messages.is_empty());
    }

    #[tokio::test]
    async fn does_not_redeliver_after_ambiguous_injection_failure() {
        let store = MemoryStore::new();
        let injector = FakeInjector::new(InjectMode::Fail);
        let first = deliver(
            &[handoff()],
            16_000,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await;
        assert!(first.is_err());
        assert_eq!(
            store.latest().unwrap().delivery.unwrap().status,
            OrchestrationV2ContextHandoffDeliveryStatus::Pending
        );
        let retry = deliver(
            &[store.latest().unwrap()],
            16_000,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await;
        assert!(matches!(
            retry,
            Err(ContextHandoffDeliveryError::Uncertain(_))
        ));
        assert_eq!(injector.calls().len(), 1);
    }

    #[tokio::test]
    async fn budgets_multiple_handoffs_together_and_skips_previously_delivered_items() {
        let messages = sample_messages();
        let mut extra_messages = messages.clone();
        extra_messages.push(message(
            "item:three",
            OrchestrationV2HistoricalMessageRole::Assistant,
            "Latest result",
        ));
        let extra = handoff_with("handoff:two", extra_messages, 0, None);
        let already = HashSet::from([TurnItemId::new("item:one").unwrap()]);
        let store = MemoryStore::new();
        let injector = FakeInjector::new(InjectMode::Accept);
        deliver(
            &[handoff(), extra],
            2_500,
            false,
            &already,
            &store,
            Some(&injector),
        )
        .await
        .unwrap();
        let captured = injector.calls();
        let serialized = serde_json::to_string(&history_response_items(
            &captured[0].messages,
            &captured[0].context,
        ))
        .unwrap();
        assert!(serialized.len() <= 2_500);
        assert!(!serialized.contains("Preserve every line"));
        assert_eq!(serialized.matches("Partial work").count(), 1);
        assert!(serialized.contains("Latest result"));
    }

    #[tokio::test]
    async fn fails_before_delivery_when_even_the_coverage_marker_cannot_fit() {
        let store = MemoryStore::new();
        let injector = FakeInjector::new(InjectMode::Accept);
        let result = deliver(
            &[handoff()],
            0,
            false,
            &HashSet::new(),
            &store,
            Some(&injector),
        )
        .await;
        assert!(matches!(
            result,
            Err(ContextHandoffDeliveryError::Budget(_))
        ));
        assert!(injector.calls().is_empty());
        assert_eq!(store.writes(), 0);
    }
}
