//! Port of upstream/apps/server/src/orchestration-v2/ContextHandoffDelivery.ts.
//!
//! Persist before and after injection: an ambiguous pending delivery requires
//! a fresh native thread. `unsent` restores the handoffs when the provider
//! refused the turn before reading the prompt, so that refusal does not leave
//! an ambiguous marker behind.

use std::{collections::HashSet, sync::Arc};

use serde::Serialize;
use turin_contracts::{
    TurnItemId,
    orchestration_v2::{
        OrchestrationV2ContextHandoff, OrchestrationV2ContextHandoffDelivery,
        OrchestrationV2ContextHandoffDeliveryStatus, OrchestrationV2ContextHandoffStrategy,
        OrchestrationV2ProviderThread,
    },
};
use turin_provider_core::{
    failure::ContextHandoffBudgetError,
    handoff_budget::{SelectHistoryInput, history_cost, render_history, select_history},
    provider_adapter::ProviderAdapterV2HistoricalContext,
    rt::BoxFuture,
};

use crate::pipeline::orchestrator_port::BoxError;

/// `ContextHandoffDeliveryUncertainError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Historical context delivery is uncertain; replace the native thread before retrying.")]
pub struct ContextHandoffDeliveryUncertainError;

/// `input.persist`.
pub type PersistHandoff = Arc<
    dyn Fn(OrchestrationV2ContextHandoff) -> BoxFuture<'static, Result<(), BoxError>> + Send + Sync,
>;

/// `input.inject`: `Ok(false)` when the native protocol does not support it.
pub type InjectHistory = Arc<
    dyn Fn(ProviderAdapterV2HistoricalContext) -> BoxFuture<'static, Result<bool, BoxError>>
        + Send
        + Sync,
>;

/// `input.budget`: a number, or a read that runs only when a handoff needs delivery.
pub enum HandoffBudgetSource {
    Fixed(i64),
    Load(Box<dyn FnOnce() -> BoxFuture<'static, Result<i64, BoxError>> + Send>),
}

pub struct DeliverContextHandoffsInput<'a> {
    pub handoffs: &'a [OrchestrationV2ContextHandoff],
    pub provider_thread: &'a OrchestrationV2ProviderThread,
    pub budget: HandoffBudgetSource,
    pub defer_inline: bool,
    pub already_delivered_item_ids: &'a HashSet<TurnItemId>,
    pub inject: Option<InjectHistory>,
    pub persist: PersistHandoff,
}

/// What `deliverContextHandoffs` returns: the inline `context` to send with
/// the prompt, and the two follow-ups upstream returns as effects.
pub struct ContextHandoffDelivery {
    pub context: String,
    persist: PersistHandoff,
    /// The handoffs marked `inline`, persisted by [`Self::delivered`].
    inline: Vec<OrchestrationV2ContextHandoff>,
    /// The handoffs as they were, persisted by [`Self::unsent`].
    originals: Vec<OrchestrationV2ContextHandoff>,
}

impl ContextHandoffDelivery {
    fn nothing(persist: PersistHandoff) -> Self {
        Self {
            context: String::new(),
            persist,
            inline: Vec::new(),
            originals: Vec::new(),
        }
    }

    /// `delivered`: the provider accepted the turn.
    pub async fn delivered(&self) -> Result<(), BoxError> {
        for handoff in &self.inline {
            (self.persist)(handoff.clone()).await?;
        }
        Ok(())
    }

    /// `unsent`: the provider refused the turn before reading the prompt.
    pub async fn unsent(&self) -> Result<(), BoxError> {
        for handoff in &self.originals {
            (self.persist)(handoff.clone()).await?;
        }
        Ok(())
    }
}

fn wire_text<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `deliverContextHandoffs`.
pub async fn deliver_context_handoffs(
    input: DeliverContextHandoffsInput<'_>,
) -> Result<ContextHandoffDelivery, BoxError> {
    let native_thread_id: Option<&str> = input
        .provider_thread
        .native_thread_ref
        .as_ref()
        .and_then(|reference| reference.native_id.as_ref())
        .map(|id| id.as_str());
    let pending: Vec<&OrchestrationV2ContextHandoff> = input
        .handoffs
        .iter()
        .filter(|handoff| match (native_thread_id, &handoff.delivery) {
            (None, _) | (_, None) => true,
            (Some(native), Some(delivery)) => {
                delivery.native_thread_id != native
                    || delivery.status == OrchestrationV2ContextHandoffDeliveryStatus::Pending
            }
        })
        .collect();
    if pending.is_empty() || (input.defer_inline && input.inject.is_none()) {
        return Ok(ContextHandoffDelivery::nothing(input.persist));
    }
    let budget = match input.budget {
        HandoffBudgetSource::Fixed(budget) => budget,
        HandoffBudgetSource::Load(load) => load().await?,
    };
    let label = |handoff: &OrchestrationV2ContextHandoff| {
        if handoff.strategy == OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary {
            "merge_back / fork_delta_summary".to_owned()
        } else {
            wire_text(&handoff.strategy)
        }
    };
    let mut coverage = pending
        .iter()
        .map(|handoff| {
            let body = handoff
                .history
                .as_ref()
                .map(|history| history.coverage.clone())
                .unwrap_or_else(|| {
                    format!(
                        "From thread {}, runs {}-{}. Recover history with t3_thread_read, view=activity; paginate with afterPosition, and use itemId/textOffset for long items.",
                        handoff.thread_id,
                        handoff.covered_run_ordinals.from,
                        handoff.covered_run_ordinals.to
                    )
                });
            format!("Context handoff ({}):\n{body}", label(handoff))
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Repeated failures can accumulate many recovery markers. Keep a single
    // thread-level entry point when detailed coverage would crowd out history;
    // its activity includes the original handoff/fork source references.
    if history_cost(&[], &coverage) as f64 > 4_000f64.min(budget as f64 / 2.0) {
        let mut strategies: Vec<String> = Vec::new();
        for handoff in &pending {
            let strategy = wire_text(&handoff.strategy);
            if !strategies.contains(&strategy) {
                strategies.push(strategy);
            }
        }
        let thread_id = input
            .provider_thread
            .app_thread_id
            .as_ref()
            .unwrap_or(&pending[0].thread_id);
        coverage = format!(
            "Context handoff ({}). {} handoff records; detailed coverage references omitted. Recover history with t3_thread_read({{threadId:\"{}\",view:\"activity\",limit:20,maxCharsPerItem:4000}}); paginate with afterPosition=nextPosition. Follow fork/handoff source references in activity. For long items use itemId and textOffset=nextTextOffset until null.",
            strategies.join(", "),
            pending.len(),
            thread_id
        );
    }
    let mut seen: HashSet<TurnItemId> = input.already_delivered_item_ids.clone();
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
    // Old preview handoffs remain readable. Their preformatted context is
    // included as a whole when it fits, otherwise the coverage marker points
    // to retrieval.
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
        coverage
    };
    let selected = select_history(&SelectHistoryInput {
        messages: &messages,
        coverage: &full_coverage,
        omitted_items: Some(
            pending
                .iter()
                .map(|handoff| {
                    handoff
                        .history
                        .as_ref()
                        .map_or(0, |history| history.omitted_items as i64)
                })
                .sum(),
        ),
        budget,
    });
    if history_cost(&selected.messages, &selected.context) > budget {
        if input.defer_inline {
            return Ok(ContextHandoffDelivery::nothing(input.persist));
        }
        return Err(Box::new(ContextHandoffBudgetError));
    }
    let omitted_item_ids: HashSet<&TurnItemId> = selected.omitted_item_ids.iter().collect();
    // Every pending handoff marked with `status` for this native thread.
    let marked = |status: OrchestrationV2ContextHandoffDeliveryStatus| {
        pending
            .iter()
            .map(|handoff| {
                let mut updated = (*handoff).clone();
                if let Some(native) = native_thread_id {
                    let history_messages = handoff
                        .history
                        .as_ref()
                        .map(|history| history.messages.as_slice())
                        .unwrap_or_default();
                    let mut omitted: Vec<TurnItemId> = handoff
                        .history
                        .as_ref()
                        .and_then(|history| history.omitted_item_ids.clone())
                        .unwrap_or_default();
                    omitted.extend(
                        history_messages
                            .iter()
                            .filter(|message| omitted_item_ids.contains(&message.item_id))
                            .map(|message| message.item_id.clone()),
                    );
                    updated.delivery = Some(OrchestrationV2ContextHandoffDelivery {
                        native_thread_id: native.to_owned(),
                        status,
                        omitted_item_ids: Some(omitted),
                        item_ids: selected
                            .messages
                            .iter()
                            .filter(|message| {
                                history_messages
                                    .iter()
                                    .any(|candidate| candidate.item_id == message.item_id)
                            })
                            .map(|message| message.item_id.clone())
                            .collect(),
                    });
                }
                updated
            })
            .collect::<Vec<_>>()
    };
    let persist_all = |handoffs: Vec<OrchestrationV2ContextHandoff>| {
        let persist = input.persist.clone();
        async move {
            for handoff in handoffs {
                persist(handoff).await?;
            }
            Ok::<(), BoxError>(())
        }
    };
    let originals: Vec<OrchestrationV2ContextHandoff> =
        pending.iter().map(|handoff| (*handoff).clone()).collect();
    if let (Some(inject), Some(native)) = (&input.inject, native_thread_id) {
        if pending.iter().any(|handoff| {
            handoff.delivery.as_ref().is_some_and(|delivery| {
                delivery.native_thread_id == native
                    && delivery.status == OrchestrationV2ContextHandoffDeliveryStatus::Pending
            })
        }) {
            return Err(Box::new(ContextHandoffDeliveryUncertainError));
        }
        persist_all(marked(OrchestrationV2ContextHandoffDeliveryStatus::Pending)).await?;
        let injected = inject(ProviderAdapterV2HistoricalContext {
            messages: selected.messages.clone(),
            context: selected.context.clone(),
        })
        .await?;
        if injected {
            persist_all(marked(
                OrchestrationV2ContextHandoffDeliveryStatus::Injected,
            ))
            .await?;
            return Ok(ContextHandoffDelivery::nothing(input.persist));
        }
    } else {
        // Text-only delivery can also be accepted before a connection drops.
        persist_all(marked(OrchestrationV2ContextHandoffDeliveryStatus::Pending)).await?;
    }
    if input.defer_inline {
        // Compaction APIs cannot accept an inline transcript. Keep it available
        // for the next ordinary turn when native injection is unsupported.
        persist_all(originals).await?;
        return Ok(ContextHandoffDelivery::nothing(input.persist));
    }
    Ok(ContextHandoffDelivery {
        context: render_history(&selected.messages, &selected.context),
        inline: marked(OrchestrationV2ContextHandoffDeliveryStatus::Inline),
        originals,
        persist: input.persist,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use turin_contracts::orchestration_v2::OrchestrationV2HistoricalMessage;
    use turin_provider_core::handoff_budget::history_response_items;

    use super::*;

    const NOW: &str = "2026-09-17T00:00:00.000Z";
    const THREAD: &str = "thread:handoff";

    fn provider_thread() -> OrchestrationV2ProviderThread {
        serde_json::from_value(json!({
            "id": "provider-thread:target", "driver": "codex", "providerInstanceId": "codex",
            "providerSessionId": null, "appThreadId": THREAD, "ownerNodeId": null,
            "nativeThreadRef": {"driver": "codex", "strength": "strong", "nativeId": "native:target"},
            "nativeConversationHeadRef": null, "status": "idle", "firstRunOrdinal": null,
            "lastRunOrdinal": null, "handoffIds": [], "forkedFrom": null,
            "createdAt": NOW, "updatedAt": NOW,
        }))
        .expect("provider thread")
    }

    fn message(id: &str, role: &str, text: &str) -> OrchestrationV2HistoricalMessage {
        serde_json::from_value(json!({
            "itemId": id, "role": role, "text": text, "threadId": THREAD,
            "runId": "run:source", "providerThreadId": "provider-thread:source",
            "status": "interrupted",
            "kind": if role == "user" { "user_message" } else { "assistant_message" },
        }))
        .expect("message")
    }

    fn messages() -> Vec<OrchestrationV2HistoricalMessage> {
        vec![
            message(
                "item:one",
                "user",
                "Preserve every line.\n\n  And this indentation.\n",
            ),
            message(
                "item:two",
                "assistant",
                &format!("Partial work: 日本語 🧪 مرحبا\n{}", "x".repeat(600)),
            ),
        ]
    }

    fn handoff() -> OrchestrationV2ContextHandoff {
        serde_json::from_value(json!({
            "id": "handoff:one", "threadId": THREAD, "targetRunId": "run:target",
            "fromProviderThreadIds": [], "toProviderThreadId": "provider-thread:target",
            "coveredRunOrdinals": {"from": 1, "to": 2}, "strategy": "full_thread_summary",
            "status": "ready", "summaryMessageId": null, "summaryText": "",
            "history": {
                "messages": messages(),
                "coverage": "Historical context; retrieve thread:handoff with t3_thread_read.",
                "omittedItems": 0,
            },
            "createdByProviderInstanceId": null, "createdAt": NOW, "updatedAt": NOW,
        }))
        .expect("handoff")
    }

    /// The durable copy a test's `persist` writes into.
    #[derive(Clone, Default)]
    struct Durable(Arc<Mutex<Option<OrchestrationV2ContextHandoff>>>);

    impl Durable {
        fn holding(handoff: OrchestrationV2ContextHandoff) -> Self {
            Self(Arc::new(Mutex::new(Some(handoff))))
        }
        fn get(&self) -> OrchestrationV2ContextHandoff {
            self.0.lock().clone().expect("durable handoff")
        }
        fn persist(&self) -> PersistHandoff {
            let slot = self.0.clone();
            Arc::new(move |value| {
                *slot.lock() = Some(value);
                Box::pin(async { Ok(()) })
            })
        }
    }

    fn persist_nothing() -> PersistHandoff {
        Arc::new(|_| Box::pin(async { Ok(()) }))
    }

    fn inject_returning(accepted: bool) -> InjectHistory {
        Arc::new(move |_| Box::pin(async move { Ok(accepted) }))
    }

    fn status(handoff: &OrchestrationV2ContextHandoff) -> Option<String> {
        handoff
            .delivery
            .as_ref()
            .map(|delivery| wire_text(&delivery.status))
    }

    async fn deliver(
        handoffs: &[OrchestrationV2ContextHandoff],
        budget: HandoffBudgetSource,
        defer_inline: bool,
        delivered: &HashSet<TurnItemId>,
        inject: Option<InjectHistory>,
        persist: PersistHandoff,
    ) -> Result<ContextHandoffDelivery, BoxError> {
        deliver_context_handoffs(DeliverContextHandoffsInput {
            handoffs,
            provider_thread: &provider_thread(),
            budget,
            defer_inline,
            already_delivered_item_ids: delivered,
            inject,
            persist,
        })
        .await
    }

    fn ids(values: &[&str]) -> Vec<TurnItemId> {
        values
            .iter()
            .map(|value| TurnItemId::new(*value).expect("item id"))
            .collect()
    }

    async fn records_omitted_recovery_coverage_separately(native: bool) {
        let omitted_before_delivery = TurnItemId::new("item:omitted-during-preparation").unwrap();
        let oversized = message("item:oversized", "user", &"x".repeat(20_000));
        let mut source = handoff();
        {
            let history = source.history.as_mut().unwrap();
            history.messages.push(oversized.clone());
            history.omitted_items = 1;
            history.omitted_item_ids = Some(vec![omitted_before_delivery.clone()]);
        }
        let durable = Durable::holding(source.clone());
        let inject: InjectHistory = {
            let oversized_id = oversized.item_id.clone();
            Arc::new(move |value| {
                assert!(value.context.contains("omitted 2 items"));
                assert!(
                    !value
                        .messages
                        .iter()
                        .any(|item| item.item_id == oversized_id)
                );
                Box::pin(async move { Ok(native) })
            })
        };
        let result = deliver(
            std::slice::from_ref(&source),
            HandoffBudgetSource::Fixed(16_000),
            false,
            &HashSet::new(),
            Some(inject),
            durable.persist(),
        )
        .await
        .expect("deliver");
        assert_eq!(
            status(&durable.get()),
            Some(if native { "injected" } else { "pending" }.to_owned())
        );
        result.delivered().await.expect("delivered");
        assert_eq!(
            status(&durable.get()),
            Some(if native { "injected" } else { "inline" }.to_owned())
        );
        let delivery = durable.get().delivery.expect("delivery");
        assert_eq!(delivery.item_ids, ids(&["item:one", "item:two"]));
        assert_eq!(
            delivery.omitted_item_ids,
            Some(vec![omitted_before_delivery, oversized.item_id])
        );
        // The persisted record decodes back to itself.
        let round_trip: OrchestrationV2ContextHandoff =
            serde_json::from_value(serde_json::to_value(durable.get()).unwrap()).unwrap();
        assert_eq!(round_trip.delivery, durable.get().delivery);
    }

    #[tokio::test]
    async fn records_omitted_recovery_coverage_separately_from_injected_text() {
        records_omitted_recovery_coverage_separately(true).await;
    }

    #[tokio::test]
    async fn records_omitted_recovery_coverage_separately_from_inline_text() {
        records_omitted_recovery_coverage_separately(false).await;
    }

    #[tokio::test]
    async fn loads_the_history_budget_only_when_a_handoff_needs_delivery() {
        let reads = Arc::new(AtomicUsize::new(0));
        let budget = || {
            let reads = reads.clone();
            HandoffBudgetSource::Load(Box::new(move || {
                reads.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(16_000) })
            }))
        };
        let delivered = HashSet::new();
        deliver(&[], budget(), false, &delivered, None, persist_nothing())
            .await
            .unwrap();
        deliver(
            &[handoff()],
            budget(),
            true,
            &delivered,
            None,
            persist_nothing(),
        )
        .await
        .unwrap();
        let mut settled = handoff();
        settled.delivery = Some(OrchestrationV2ContextHandoffDelivery {
            native_thread_id: "native:target".to_owned(),
            status: OrchestrationV2ContextHandoffDeliveryStatus::Injected,
            item_ids: Vec::new(),
            omitted_item_ids: None,
        });
        deliver(
            &[settled],
            budget(),
            false,
            &delivered,
            None,
            persist_nothing(),
        )
        .await
        .unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        let result = deliver(
            &[handoff()],
            budget(),
            false,
            &delivered,
            None,
            persist_nothing(),
        )
        .await
        .unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(result.context.contains(&messages()[0].text));
    }

    #[tokio::test]
    async fn persists_successful_injection_before_turn_start_and_skips_it_on_retry() {
        let durable = Durable::holding(handoff());
        let statuses = Arc::new(Mutex::new(Vec::<String>::new()));
        let history = Arc::new(Mutex::new(Vec::<Value>::new()));
        let persist: PersistHandoff = {
            let durable = durable.clone();
            let statuses = statuses.clone();
            Arc::new(move |value| {
                statuses.lock().push(status(&value).unwrap());
                *durable.0.lock() = Some(value);
                Box::pin(async { Ok(()) })
            })
        };
        let inject: InjectHistory = {
            let history = history.clone();
            Arc::new(move |value| {
                history
                    .lock()
                    .extend(history_response_items(&value.messages, &value.context));
                Box::pin(async { Ok(true) })
            })
        };
        let delivered = HashSet::new();
        let first = deliver(
            &[durable.get()],
            HandoffBudgetSource::Fixed(16_000),
            false,
            &delivered,
            Some(inject.clone()),
            persist.clone(),
        )
        .await
        .unwrap();
        assert_eq!(first.context, "");
        assert_eq!(*statuses.lock(), vec!["pending", "injected"]);
        assert_eq!(history.lock().len(), 3);
        // No turn has started. Reconstruct from the durable projection as a retry would.
        let second = deliver(
            &[durable.get()],
            HandoffBudgetSource::Fixed(16_000),
            false,
            &delivered,
            Some(inject),
            persist,
        )
        .await
        .unwrap();
        assert_eq!(second.context, "");
        assert_eq!(history.lock().len(), 3);
    }

    #[tokio::test]
    async fn uses_the_same_selection_on_explicit_native_fallback_marking_only_accepted_input() {
        let durable = Durable::holding(handoff());
        let result = deliver(
            &[handoff()],
            HandoffBudgetSource::Fixed(16_000),
            false,
            &HashSet::new(),
            Some(inject_returning(false)),
            durable.persist(),
        )
        .await
        .unwrap();
        assert!(result.context.contains(&messages()[0].text));
        assert!(result.context.contains(&messages()[1].text));
        assert_eq!(status(&durable.get()), Some("pending".to_owned()));
        result.delivered().await.unwrap();
        assert_eq!(status(&durable.get()), Some("inline".to_owned()));
    }

    #[tokio::test]
    async fn defers_unsupported_compaction_history_until_a_normal_turn_without_marking_delivery_uncertain()
     {
        let durable = Durable::holding(handoff());
        let delivered = HashSet::new();
        let compact = deliver(
            &[durable.get()],
            HandoffBudgetSource::Fixed(16_000),
            true,
            &delivered,
            Some(inject_returning(false)),
            durable.persist(),
        )
        .await
        .unwrap();
        compact.delivered().await.unwrap();
        assert_eq!(compact.context, "");
        assert!(durable.get().delivery.is_none());
        let next = deliver(
            &[durable.get()],
            HandoffBudgetSource::Fixed(16_000),
            false,
            &delivered,
            Some(inject_returning(false)),
            durable.persist(),
        )
        .await
        .unwrap();
        assert!(next.context.contains(&messages()[0].text));
        next.delivered().await.unwrap();
        assert_eq!(status(&durable.get()), Some("inline".to_owned()));
    }

    #[tokio::test]
    async fn bounds_accumulated_recovery_markers_while_keeping_omitted_history_discoverable() {
        let many: Vec<_> = (0..100)
            .map(|index| {
                let mut copy = handoff();
                copy.id = turin_contracts::ContextHandoffId::new(format!("handoff:retry:{index}"))
                    .unwrap();
                copy
            })
            .collect();
        let captured = Arc::new(Mutex::new(None::<ProviderAdapterV2HistoricalContext>));
        let inject: InjectHistory = {
            let captured = captured.clone();
            Arc::new(move |history| {
                *captured.lock() = Some(history);
                Box::pin(async { Ok(true) })
            })
        };
        let result = deliver(
            &many,
            HandoffBudgetSource::Fixed(2_500),
            false,
            &HashSet::new(),
            Some(inject),
            persist_nothing(),
        )
        .await
        .unwrap();
        assert_eq!(result.context, "");
        let captured = captured.lock().clone().expect("injected history");
        assert!(
            captured
                .context
                .contains("detailed coverage references omitted")
        );
        assert!(captured.context.contains("t3_thread_read"));
        assert!(captured.context.contains(THREAD));
        assert!(history_cost(&captured.messages, &captured.context) <= 2_500);
        assert!(!captured.messages.is_empty());
    }

    #[tokio::test]
    async fn does_not_redeliver_after_ambiguous_injection_failure() {
        let durable = Durable::holding(handoff());
        let calls = Arc::new(AtomicUsize::new(0));
        let inject: InjectHistory = {
            let calls = calls.clone();
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err::<bool, BoxError>("connection lost".into()) })
            })
        };
        let delivered = HashSet::new();
        let first = deliver(
            &[durable.get()],
            HandoffBudgetSource::Fixed(16_000),
            false,
            &delivered,
            Some(inject.clone()),
            durable.persist(),
        )
        .await;
        assert!(first.is_err());
        assert_eq!(status(&durable.get()), Some("pending".to_owned()));
        let retry = deliver(
            &[durable.get()],
            HandoffBudgetSource::Fixed(16_000),
            false,
            &delivered,
            Some(inject),
            durable.persist(),
        )
        .await;
        let error = retry.err().expect("retry fails");
        assert!(error.is::<ContextHandoffDeliveryUncertainError>());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn budgets_multiple_handoffs_together_and_skips_previously_delivered_items() {
        let mut extra = handoff();
        extra.id = turin_contracts::ContextHandoffId::new("handoff:two").unwrap();
        extra.history.as_mut().unwrap().messages.push(message(
            "item:three",
            "assistant",
            "Latest result",
        ));
        let delivered_items = Arc::new(Mutex::new(Vec::<Value>::new()));
        let inject: InjectHistory = {
            let delivered_items = delivered_items.clone();
            Arc::new(move |value| {
                *delivered_items.lock() = history_response_items(&value.messages, &value.context);
                Box::pin(async { Ok(true) })
            })
        };
        let already: HashSet<TurnItemId> = ids(&["item:one"]).into_iter().collect();
        deliver(
            &[handoff(), extra],
            HandoffBudgetSource::Fixed(2_500),
            false,
            &already,
            Some(inject),
            persist_nothing(),
        )
        .await
        .unwrap();
        let serialized = serde_json::to_string(&*delivered_items.lock()).unwrap();
        assert!(serialized.len() <= 2_500);
        assert!(!serialized.contains("Preserve every line"));
        assert_eq!(serialized.matches("Partial work").count(), 1);
        assert!(serialized.contains("Latest result"));
    }

    #[tokio::test]
    async fn fails_before_delivery_when_even_the_coverage_marker_cannot_fit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inject: InjectHistory = {
            let calls = calls.clone();
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(true) })
            })
        };
        let result = deliver(
            &[handoff()],
            HandoffBudgetSource::Fixed(0),
            false,
            &HashSet::new(),
            Some(inject),
            persist_nothing(),
        )
        .await;
        let error = result.err().expect("over budget");
        assert!(error.is::<ContextHandoffBudgetError>());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
