//! Port of upstream/apps/server/src/orchestration-v2/ProviderTurnStartService.ts
//! (lines 181-216 and 1092-1226): `makeDeliverySession` and `startWithHandoffs`.
//!
//! When a run has context to deliver (ready handoffs, missed history, a
//! restart note), the provider session handed to `RunExecutionService` is a
//! wrapper whose first `start_turn` (or `compact_thread`) delivers that
//! context before the turn. Only startup needs it: the wrapper drops its plan
//! after the call, so the event worker that keeps the session alive afterward
//! does not retain the thread's history.

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use turin_contracts::{
    ProviderSessionId, ProviderThreadId, RunId, ThreadId, TurnItemId, UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2ContextHandoff, OrchestrationV2DomainEvent, OrchestrationV2ProviderSession,
        OrchestrationV2ProviderThread, OrchestrationV2Run, OrchestrationV2TurnItem,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::{
    handoff_budget::{
        HandoffBudgetInput, attachment_token_allowance, handoff_budget, historical_message,
    },
    id_allocator::IdAllocator,
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Error,
        ProviderAdapterV2ForkThreadInput, ProviderAdapterV2HistoricalContext,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2McpApps,
        ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
        ProviderAdapterV2RollbackThreadInput, ProviderAdapterV2RuntimeRequestResponseInput,
        ProviderAdapterV2SteerInput, ProviderAdapterV2StopSubagentInput,
        ProviderAdapterV2StopTaskInput, ProviderAdapterV2SubagentOutcome,
        ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput,
        ProviderAdapterV2UploadFeedbackInput, ProviderEventStream, ProviderSessionRuntime,
    },
    rt::BoxFuture,
};

use super::TurnStartReads;
use crate::{
    core::{
        context_handoff_delivery::{
            DeliverContextHandoffsInput, HandoffBudgetSource, InjectHistory, PersistHandoff,
            deliver_context_handoffs,
        },
        context_handoff_service::{
            ContextHandoffService, PrepareProviderHandoffInput, ProviderHandoffStrategy,
        },
        run_execution_service::{AsyncBool, RunEventWriter, failure_cause},
    },
    pipeline::{event_sink::WriteInput, orchestrator_port::BoxError},
};

/// What the lazy history budget needs to estimate a native transcript.
pub(super) struct EstimateInput {
    pub same_native_thread: bool,
    pub run_id: RunId,
    pub provider_thread_id: ProviderThreadId,
    pub missed_run_ids: HashSet<RunId>,
    pub delivered_item_ids: HashSet<TurnItemId>,
    pub covered_item_ids: HashSet<TurnItemId>,
    pub native_input_run_ids: HashSet<RunId>,
    pub legacy_input_run_ids: HashSet<RunId>,
    pub legacy_recovered_run_ids: HashSet<RunId>,
}

/// `nativeContextEstimate`: saved text and actual native attachments, when
/// telemetry is absent. Legacy attempts lack native identity; their
/// explicitly recovered history is excluded, since its attachments were not
/// replayed into the replacement thread.
async fn native_context_estimate(
    reads: &dyn TurnStartReads,
    thread_id: &ThreadId,
    input: &EstimateInput,
) -> Result<i64, BoxError> {
    if !input.same_native_thread {
        return Ok(0);
    }
    let mut sum: i64 = 0;
    for item in reads.turn_start_history(thread_id, None).await? {
        let common = crate::projection::turn_item::common(&item);
        let run_id = common.run_id;
        if run_id == Some(&input.run_id)
            || (run_id.is_some_and(|run_id| input.missed_run_ids.contains(run_id))
                && !input.delivered_item_ids.contains(common.id))
            || (common.provider_thread_id != Some(&input.provider_thread_id)
                && !input.delivered_item_ids.contains(common.id))
        {
            continue;
        }
        let historical = historical_message(&item);
        let native_attachments = match (&item, run_id) {
            (OrchestrationV2TurnItem::UserMessage { attachments, .. }, Some(run_id))
                if common.provider_thread_id == Some(&input.provider_thread_id)
                    && (input.native_input_run_ids.contains(run_id)
                        || (input.legacy_input_run_ids.contains(run_id)
                            && !input.covered_item_ids.contains(common.id)
                            && !input.legacy_recovered_run_ids.contains(run_id))) =>
            {
                attachment_token_allowance(attachments)
            }
            _ => 0,
        };
        sum += historical.map_or(0, |message| message.text.len() as i64) + native_attachments;
    }
    Ok(sum)
}

/// Everything `startWithHandoffs` closes over.
pub(super) struct HandoffPlan {
    pub reads: Arc<dyn TurnStartReads>,
    pub events: Arc<dyn RunEventWriter>,
    pub handoff_service: ContextHandoffService,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub run_provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: ProviderThreadId,
    pub driver: ProviderDriverKind,
    pub runs: Vec<OrchestrationV2Run>,
    pub effective_handoffs: Vec<OrchestrationV2ContextHandoff>,
    pub missed_items: Vec<OrchestrationV2TurnItem>,
    /// Ordinals of the first and last missed run, when there are any.
    pub missed_run_ordinals: Option<(u64, u64)>,
    pub running_provider_thread: OrchestrationV2ProviderThread,
    pub restart_note: String,
    pub note_continuation: bool,
    pub user_text: String,
    pub attachments: Vec<turin_contracts::chat_attachment::ChatAttachment>,
    pub token_cap: i64,
    pub model_context_window: Option<i64>,
    pub delivered_item_ids: HashSet<TurnItemId>,
    pub estimate: EstimateInput,
    /// `isCurrentAttemptInStatus("running")`.
    pub is_running: AsyncBool,
}

fn covered(
    from: u64,
    to: u64,
) -> turin_contracts::orchestration_v2::OrchestrationV2CoveredRunOrdinals {
    turin_contracts::orchestration_v2::OrchestrationV2CoveredRunOrdinals {
        from: turin_contracts::PositiveInt::new(from.max(1)).expect("positive"),
        to: turin_contracts::PositiveInt::new(to.max(1)).expect("positive"),
    }
}

/// `refusedBeforePrompt`: Claude refuses to replace a process running
/// background work before it reads the prompt.
fn refused_before_prompt(error: &(dyn std::error::Error + 'static)) -> bool {
    use turin_provider_core::failure::ProviderFailureCause;
    fn tagged(cause: &ProviderFailureCause) -> bool {
        match cause {
            ProviderFailureCause::Tagged { tag, cause, .. } => {
                tag == "ClaudeBackgroundWorkBlocksQueryReplacementError"
                    || cause.as_deref().is_some_and(tagged)
            }
            ProviderFailureCause::Unreadable => false,
        }
    }
    match error.downcast_ref::<ProviderAdapterV2Error>() {
        Some(adapter) => adapter.cause.as_ref().is_some_and(tagged),
        None => error.source().is_some_and(refused_before_prompt),
    }
}

impl HandoffPlan {
    fn turn_start_error(&self, error: BoxError) -> ProviderAdapterV2Error {
        if let Some(adapter) = error.downcast_ref::<ProviderAdapterV2Error>()
            && matches!(adapter.kind, ProviderAdapterErrorKind::TurnStart { .. })
        {
            return adapter.clone();
        }
        ProviderAdapterV2Error::new(ProviderAdapterErrorKind::TurnStart {
            driver: self.driver.clone(),
            thread_id: self.thread_id.clone(),
            provider_thread_id: self.provider_thread_id.clone(),
            run_id: self.run_id.clone(),
        })
        .with_cause(failure_cause(&*error))
    }

    /// `startWithHandoffs`.
    async fn start(
        self,
        session: &Arc<dyn ProviderSessionRuntime>,
        mut input: ProviderAdapterV2TurnInput,
        compact: bool,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.deliver_and_start(session, &mut input, compact)
            .await
            .map_err(|error| self.turn_start_error(error))
    }

    async fn deliver_and_start(
        &self,
        session: &Arc<dyn ProviderSessionRuntime>,
        input: &mut ProviderAdapterV2TurnInput,
        compact: bool,
    ) -> Result<(), BoxError> {
        // A failed turn/start can leave the requested turn absent from native
        // history even when its preceding handoff was injected.
        let retry_handoff: Vec<OrchestrationV2ContextHandoff> = match self.missed_run_ordinals {
            Some((from, to)) if !self.missed_items.is_empty() => {
                vec![
                    self.handoff_service
                        .prepare_provider_handoff(&PrepareProviderHandoffInput {
                            thread_id: &self.thread_id,
                            target_run_id: &self.run_id,
                            transfer_id: None,
                            from_provider_thread_ids: std::slice::from_ref(
                                &self.provider_thread_id,
                            ),
                            to_provider_thread_id: &self.provider_thread_id,
                            from_provider_instance_id: &self.run_provider_instance_id,
                            to_provider_instance_id: &self.run_provider_instance_id,
                            covered_run_ordinals: covered(from, to),
                            runs: Some(&self.runs),
                            strategy: ProviderHandoffStrategy::DeltaSinceTargetLastSeen,
                            items: &self.missed_items,
                            created_at: UtcDateTime::now(),
                        }),
                ]
            }
            _ => Vec::new(),
        };
        let handoffs: Vec<_> = self
            .effective_handoffs
            .iter()
            .cloned()
            .chain(retry_handoff)
            .collect();

        // Hook presence is probed with an empty context; the probe's future is
        // dropped unawaited.
        let has_inject_hook = session
            .inject_history(
                &self.running_provider_thread,
                &ProviderAdapterV2HistoricalContext {
                    messages: Vec::new(),
                    context: String::new(),
                },
            )
            .is_some();
        let inject: Option<InjectHistory> = has_inject_hook.then(|| {
            let session = session.clone();
            let provider_thread = self.running_provider_thread.clone();
            Arc::new(move |history: ProviderAdapterV2HistoricalContext| {
                let session = session.clone();
                let provider_thread = provider_thread.clone();
                Box::pin(async move {
                    match session.inject_history(&provider_thread, &history) {
                        Some(injecting) => {
                            injecting.await.map_err(|error| Box::new(error) as BoxError)
                        }
                        None => Ok(false),
                    }
                }) as BoxFuture<'static, Result<bool, BoxError>>
            }) as InjectHistory
        });
        let persist: PersistHandoff = {
            let events = self.events.clone();
            let thread_id = self.thread_id.clone();
            let run_id = self.run_id.clone();
            let provider_instance_id = self.run_provider_instance_id.clone();
            Arc::new(move |mut handoff: OrchestrationV2ContextHandoff| {
                let events = events.clone();
                let thread_id = thread_id.clone();
                let run_id = run_id.clone();
                let provider_instance_id = provider_instance_id.clone();
                Box::pin(async move {
                    let updated_at = UtcDateTime::now();
                    handoff.updated_at = updated_at;
                    events
                        .write(WriteInput::events(vec![
                            OrchestrationV2DomainEvent::ContextHandoffUpdated {
                                id: IdAllocator.event(Some(&thread_id), None, None),
                                thread_id,
                                run_id: Some(run_id),
                                node_id: None,
                                driver: None,
                                provider_instance_id: Some(provider_instance_id),
                                raw_event_id: None,
                                occurred_at: updated_at,
                                payload: handoff,
                            },
                        ]))
                        .await
                }) as BoxFuture<'static, Result<(), BoxError>>
            })
        };
        let budget = {
            let reads = self.reads.clone();
            let thread_id = self.thread_id.clone();
            let user_text = if self.restart_note.is_empty() {
                self.user_text.clone()
            } else {
                // The note is sent with the user text, so it spends the same allowance.
                format!("{}\n\n{}", self.restart_note, self.user_text)
            };
            let attachments = self.attachments.clone();
            let token_cap = self.token_cap;
            let model_context_window = self.model_context_window;
            let context_usage = self.running_provider_thread.context_usage.clone();
            let estimate_input = EstimateInput {
                same_native_thread: self.estimate.same_native_thread,
                run_id: self.estimate.run_id.clone(),
                provider_thread_id: self.estimate.provider_thread_id.clone(),
                missed_run_ids: self.estimate.missed_run_ids.clone(),
                delivered_item_ids: self.estimate.delivered_item_ids.clone(),
                covered_item_ids: self.estimate.covered_item_ids.clone(),
                native_input_run_ids: self.estimate.native_input_run_ids.clone(),
                legacy_input_run_ids: self.estimate.legacy_input_run_ids.clone(),
                legacy_recovered_run_ids: self.estimate.legacy_recovered_run_ids.clone(),
            };
            HandoffBudgetSource::Load(Box::new(move || {
                Box::pin(async move {
                    let estimate = if context_usage.is_none() {
                        native_context_estimate(reads.as_ref(), &thread_id, &estimate_input).await?
                    } else {
                        0
                    };
                    Ok(handoff_budget(&HandoffBudgetInput {
                        token_cap,
                        user_text: &user_text,
                        attachments: &attachments,
                        context_usage: context_usage.as_ref(),
                        native_context_estimate: estimate,
                        model_context_window,
                    }))
                })
            }))
        };
        let delivery = deliver_context_handoffs(DeliverContextHandoffsInput {
            handoffs: &handoffs,
            provider_thread: &self.running_provider_thread,
            budget,
            defer_inline: compact,
            already_delivered_item_ids: &self.delivered_item_ids,
            inject,
            persist,
        })
        .await?;
        if !(self.is_running)().await? {
            return Ok(());
        }
        let context = [delivery.context.as_str(), self.restart_note.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        // A note continuation has no turn to resume; its text is the prompt.
        let mut prompted = input.clone();
        if self.note_continuation {
            prompted.restart_continuation_of_run_id = None;
        }
        prompted.message.text = if context.is_empty() {
            self.user_text.clone()
        } else {
            format!("{context}\n\nUser message:\n{}", self.user_text)
        };
        let started = if compact {
            match session.compact_thread(prompted) {
                Some(compacting) => compacting.await,
                None => Err(unsupported_compaction(
                    &self.driver,
                    &self.thread_id,
                    &self.provider_thread_id,
                    &self.run_id,
                )),
            }
        } else {
            session.start_turn(prompted).await
        };
        if let Err(error) = started {
            // A pending marker would make the next turn abandon this native
            // session, though the refused prompt never reached it.
            if refused_before_prompt(&error)
                && let Err(unsent) = delivery.unsent().await
            {
                tracing::warn!(
                    run_id = %self.run_id,
                    error = %unsent,
                    "Failed to restore unsent context handoffs"
                );
            }
            return Err(Box::new(error));
        }
        // The provider already accepted the turn. A stale pending marker can
        // force a fresh thread later, but must not stop live ingestion.
        if let Err(delivered) = delivery.delivered().await {
            tracing::warn!(
                run_id = %self.run_id,
                error = %delivered,
                delivery_status = "pending",
                "Failed to record accepted context handoff delivery"
            );
        }
        Ok(())
    }
}

fn unsupported_compaction(
    driver: &ProviderDriverKind,
    thread_id: &ThreadId,
    provider_thread_id: &ProviderThreadId,
    run_id: &RunId,
) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(ProviderAdapterErrorKind::TurnStart {
        driver: driver.clone(),
        thread_id: thread_id.clone(),
        provider_thread_id: provider_thread_id.clone(),
        run_id: run_id.clone(),
    })
    .with_cause(
        turin_provider_core::failure::ProviderFailureCause::tagged("Error")
            .with_message("This provider does not support context compaction."),
    )
}

/// The wrapper `RunExecutionService` drives instead of the raw session. Every
/// method forwards, except the first turn start, which delivers the plan.
pub(super) struct DeliverySession {
    inner: Arc<dyn ProviderSessionRuntime>,
    plan: Mutex<Option<HandoffPlan>>,
}

impl DeliverySession {
    pub fn new(inner: Arc<dyn ProviderSessionRuntime>, plan: HandoffPlan) -> Self {
        Self {
            inner,
            plan: Mutex::new(Some(plan)),
        }
    }

    async fn start(
        &self,
        input: ProviderAdapterV2TurnInput,
        compact: bool,
    ) -> Result<(), ProviderAdapterV2Error> {
        // Upstream's wrapper has no `compactThread` when the session has none, so
        // `RunExecutionService` fails the compaction before anything is delivered
        // and the plan stays. Creating the future runs nothing; it is dropped.
        if compact && self.inner.compact_thread(input.clone()).is_none() {
            return Err(unsupported_compaction(
                self.inner.driver(),
                &input.thread_id,
                &input.provider_thread.id,
                &input.run_id,
            ));
        }
        // Only startup needs the handoff history: taking the plan releases it
        // when the call ends, success or not.
        let plan = self.plan.lock().take();
        match plan {
            Some(plan) => plan.start(&self.inner, input, compact).await,
            None if compact => match self.inner.compact_thread(input.clone()) {
                Some(compacting) => compacting.await,
                None => Err(unsupported_compaction(
                    self.inner.driver(),
                    &input.thread_id,
                    &input.provider_thread.id,
                    &input.run_id,
                )),
            },
            None => self.inner.start_turn(input).await,
        }
    }
}

#[async_trait]
impl ProviderSessionRuntime for DeliverySession {
    fn instance_id(&self) -> &ProviderInstanceId {
        self.inner.instance_id()
    }
    fn driver(&self) -> &ProviderDriverKind {
        self.inner.driver()
    }
    fn provider_session_id(&self) -> &ProviderSessionId {
        self.inner.provider_session_id()
    }
    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        self.inner.provider_session()
    }
    fn events(&self) -> ProviderEventStream {
        self.inner.events()
    }
    fn subscribe_events(
        &self,
    ) -> Option<
        BoxFuture<
            '_,
            Result<
                turin_provider_core::provider_adapter::ProviderAdapterV2EventSubscription,
                ProviderAdapterV2Error,
            >,
        >,
    > {
        self.inner.subscribe_events()
    }
    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        self.inner.has_pending_background_work()
    }
    fn has_pending_background_work_for_thread(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        self.inner
            .has_pending_background_work_for_thread(provider_thread)
    }
    fn background_tasks_end_with_process(&self) -> bool {
        self.inner.background_tasks_end_with_process()
    }
    fn get_model_context_window(
        &self,
        model_selection: &ModelSelection,
        cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        self.inner.get_model_context_window(model_selection, cwd)
    }
    fn can_reuse_context_usage(
        &self,
        previous: &ModelSelection,
        next: &ModelSelection,
    ) -> Option<bool> {
        self.inner.can_reuse_context_usage(previous, next)
    }
    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        self.inner.ensure_thread(input).await
    }
    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        self.inner.resume_thread(input).await
    }
    fn inject_history(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
        context: &ProviderAdapterV2HistoricalContext,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        self.inner.inject_history(provider_thread, context)
    }
    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.start(input, false).await
    }
    /// Always offered: a provider that cannot compact fails the compaction
    /// with the same error `RunExecutionService` raises for a missing hook.
    fn compact_thread(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        Some(Box::pin(self.start(input, true)))
    }
    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.inner.steer_turn(input).await
    }
    fn stop_task(
        &self,
        input: ProviderAdapterV2StopTaskInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        self.inner.stop_task(input)
    }
    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.inner.interrupt_turn(input).await
    }
    fn stop_subagent(
        &self,
        input: ProviderAdapterV2StopSubagentInput,
    ) -> Option<
        BoxFuture<'_, Result<Option<ProviderAdapterV2SubagentOutcome>, ProviderAdapterV2Error>>,
    > {
        self.inner.stop_subagent(input)
    }
    fn close(&self) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        self.inner.close()
    }
    fn unload_thread(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        self.inner.unload_thread(provider_thread)
    }
    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.inner.respond_to_runtime_request(input).await
    }
    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        self.inner.read_thread_snapshot(input).await
    }
    fn upload_feedback(
        &self,
        input: ProviderAdapterV2UploadFeedbackInput,
    ) -> Option<BoxFuture<'_, Result<String, ProviderAdapterV2Error>>> {
        self.inner.upload_feedback(input)
    }
    fn mcp_apps(&self) -> Option<Arc<dyn ProviderAdapterV2McpApps>> {
        self.inner.mcp_apps()
    }
    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        self.inner.rollback_thread(input).await
    }
    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        self.inner.fork_thread(input).await
    }
}
