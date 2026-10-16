//! Port of upstream/apps/server/src/orchestration-v2/ProviderEventIngestor.ts.
//!
//! Turns one provider adapter event into orchestration domain events, then
//! writes them through `EventSink` under the run or provider-thread guard the
//! caller asks for, so a stale attempt cannot clobber newer state.
//!
//! Also here: `rollBackRewoundRuns` for `provider_thread.updated` with retained
//! native turn ids, native-subagent thread model sync, provider turn analytics
//! (a [`ProviderTurnAnalytics`] seam, a no-op by default), the OOM-kill failure
//! rewrite (an [`OomProbe`] seam), plan step durations and file-change storage
//! bounding. The pure helpers are in [`extras`].

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use turin_contracts::{
    CommandId, EventId, NodeId, ProviderSessionId, ProviderThreadId, ProviderTurnId, RawEventId,
    RunAttemptId, RunId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2PlanArtifact, OrchestrationV2RunStatus, OrchestrationV2RuntimeRequest,
        OrchestrationV2TurnItem,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    failure::{ProviderFailureTurnItemInput, make_provider_failure_turn_item},
    id_allocator::IdAllocator,
    provider_adapter::ProviderAdapterV2Event,
};

use super::session_sink::SessionEventIngest;
use crate::pipeline::{
    event_sink::{EventSink, ProviderThreadOwnerWrite, RunCurrentWrite, SinkError, WriteInput},
    orchestrator_port::BoxError,
    thread_command_executor::ThreadCommandExecutor,
};
use crate::projection::tool_output_image_bytes::strip_unserved_tool_output_image_bytes;
pub use extras::{ProviderTurnAnalyticsContext, bound_file_change_for_storage};
use extras::{
    provider_turn_analytics_properties, runs_off_native_branch, with_plan_step_durations,
};

mod extras;

/// The rows `rollBackRewoundRuns` reads (`getThreadRecords` with runs,
/// attempts, provider turns, nodes and checkpoints).
#[derive(Debug, Clone, Default)]
pub struct RewindRecords {
    pub runs: Vec<turin_contracts::orchestration_v2::OrchestrationV2Run>,
    pub attempts: Vec<turin_contracts::orchestration_v2::OrchestrationV2RunAttempt>,
    pub provider_turns: Vec<turin_contracts::orchestration_v2::OrchestrationV2ProviderTurn>,
    pub nodes: Vec<OrchestrationV2ExecutionNode>,
    pub checkpoints: Vec<turin_contracts::orchestration_v2::OrchestrationV2Checkpoint>,
}

/// `ProviderTurnAnalytics`: where a finished provider turn is reported.
/// The default drops it.
pub trait ProviderTurnAnalytics: Send + Sync {
    fn record(&self, properties: serde_json::Map<String, Value>);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoProviderTurnAnalytics;

impl ProviderTurnAnalytics for NoProviderTurnAnalytics {
    fn record(&self, _properties: serde_json::Map<String, Value>) {}
}

/// `AgentScope.oomKilled`: whether the kernel killed this thread's agent scope
/// for memory. Only Linux's systemd scope can tell; elsewhere it is `false`.
pub type OomProbe = Arc<dyn Fn(&ThreadId) -> bool + Send + Sync>;

/// The pending native user inputs of one provider turn, which the turn's
/// terminal event cancels (`ProjectionPendingUserInputs`).
#[derive(Debug, Clone, Default)]
pub struct PendingNativeUserInputs {
    pub runtime_requests: Vec<OrchestrationV2RuntimeRequest>,
    pub nodes: Vec<OrchestrationV2ExecutionNode>,
    pub turn_items: Vec<OrchestrationV2TurnItem>,
}

/// The projection reads normalization needs. `ProjectionStore` implements it
/// in `core::projection_reads`; tests use an in-memory fake.
#[async_trait]
pub trait IngestReads: Send + Sync {
    /// `None` when the thread does not exist yet.
    async fn app_thread(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2AppThread>, BoxError>;

    /// True when the projection already stores this exact node.
    async fn node_stored(&self, node: &OrchestrationV2ExecutionNode) -> Result<bool, BoxError>;

    async fn pending_native_user_inputs(
        &self,
        thread_id: &ThreadId,
        provider_turn_id: &ProviderTurnId,
    ) -> Result<PendingNativeUserInputs, BoxError>;

    async fn plan(
        &self,
        thread_id: &ThreadId,
        plan_id: &turin_contracts::PlanId,
    ) -> Result<Option<OrchestrationV2PlanArtifact>, BoxError>;

    /// The rows a native rewind is judged against.
    async fn rewind_records(&self, thread_id: &ThreadId) -> Result<RewindRecords, BoxError>;
}

/// Who the event belongs to (`ProviderEventIngestInput`).
#[derive(Debug, Clone)]
pub struct ProviderEventInput {
    pub provider_session_id: ProviderSessionId,
    pub provider_instance_id: ProviderInstanceId,
    pub command_id: Option<CommandId>,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub raw_event_id: Option<RawEventId>,
    pub event: ProviderAdapterV2Event,
    /// What the run knew when the event arrived, for turn analytics.
    pub analytics_context: Option<ProviderTurnAnalyticsContext>,
}

/// Guards that make a write fail if the attempt lost ownership in flight.
#[derive(Debug, Clone, Default)]
pub struct IngestGuard {
    /// `writeIfRunCurrent`.
    pub run_current: Option<RunCurrentGuard>,
    /// `writeIfProviderThreadOwner`. Takes precedence over `run_current`.
    pub provider_thread_owner: Option<ProviderThreadOwnerGuard>,
}

#[derive(Debug, Clone)]
pub struct RunCurrentGuard {
    pub run_id: RunId,
    pub active_attempt_id: RunAttemptId,
    pub expected_status: OrchestrationV2RunStatus,
}

#[derive(Debug, Clone)]
pub struct ProviderThreadOwnerGuard {
    pub provider_thread_id: ProviderThreadId,
    pub run_id: RunId,
    pub active_attempt_id: RunAttemptId,
    pub expected_last_run_ordinal: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderEventIngestError {
    #[error("Failed to normalize provider event for thread {thread_id}.")]
    Normalize {
        thread_id: ThreadId,
        #[source]
        cause: BoxError,
    },
    #[error(
        "Failed to move a native subagent's thread to its reported model (provider session {provider_session_id})."
    )]
    SyncSubagentModel {
        provider_session_id: ProviderSessionId,
        #[source]
        cause: BoxError,
    },
    #[error("Failed to publish provider event for provider session {provider_session_id}.")]
    Publish {
        provider_session_id: ProviderSessionId,
        #[source]
        cause: SinkError,
    },
}

/// `ProviderEventIngestorV2`.
pub struct ProviderEventIngestor {
    events: Arc<EventSink>,
    reads: Arc<dyn IngestReads>,
    threads: Arc<ThreadCommandExecutor>,
    analytics: Arc<dyn ProviderTurnAnalytics>,
    oom_killed: OomProbe,
    /// Turns already reported, oldest first (`completedTurnAnalytics`, 4096).
    reported_turns: Mutex<(HashSet<String>, VecDeque<String>)>,
}

impl ProviderEventIngestor {
    pub fn new(
        events: Arc<EventSink>,
        reads: Arc<dyn IngestReads>,
        threads: Arc<ThreadCommandExecutor>,
    ) -> Self {
        Self {
            events,
            reads,
            threads,
            analytics: Arc::new(NoProviderTurnAnalytics),
            oom_killed: Arc::new(|thread_id| {
                turin_shared::agent_scope::default_oom_killed(thread_id.as_str())
            }),
            reported_turns: Mutex::new((HashSet::new(), VecDeque::new())),
        }
    }

    /// Reports finished provider turns to `analytics`.
    pub fn with_analytics(mut self, analytics: Arc<dyn ProviderTurnAnalytics>) -> Self {
        self.analytics = analytics;
        self
    }

    /// Replaces the OOM-kill check (`AgentScope.oomKilled`).
    pub fn with_oom_probe(mut self, probe: OomProbe) -> Self {
        self.oom_killed = probe;
        self
    }

    /// `normalize`: the domain events one provider event produces, without writing.
    pub async fn normalize(
        &self,
        input: &ProviderEventInput,
    ) -> Result<Vec<OrchestrationV2DomainEvent>, ProviderEventIngestError> {
        normalize(self.reads.as_ref(), self.oom_killed.as_ref(), input)
            .await
            .map_err(|cause| ProviderEventIngestError::Normalize {
                thread_id: input.thread_id.clone(),
                cause,
            })
    }

    /// `ingestNormalized`: normalizes and writes under `guard`. Returns the
    /// stored events, which is empty when nothing changed. A written native
    /// subagent update also moves the subagent's thread to its reported model,
    /// and a finished provider turn is reported to analytics once.
    pub async fn ingest(
        &self,
        input: &ProviderEventInput,
        guard: &IngestGuard,
    ) -> Result<
        Vec<turin_contracts::orchestration_v2::OrchestrationV2StoredEvent>,
        ProviderEventIngestError,
    > {
        let mut stored = self.write_normalized(input, guard).await?;
        if stored.is_empty() {
            return Ok(stored);
        }
        match &input.event {
            ProviderAdapterV2Event::SubagentUpdated { subagent, .. } => {
                let synced = self
                    .sync_subagent_thread_model(input, subagent)
                    .await
                    .map_err(|cause| ProviderEventIngestError::SyncSubagentModel {
                        provider_session_id: input.provider_session_id.clone(),
                        cause,
                    })?;
                stored.extend(synced);
            }
            ProviderAdapterV2Event::ProviderTurnUpdated {
                driver,
                provider_turn,
                ..
            } if is_terminal_turn_status(provider_turn.status) => {
                self.report_turn(input, driver, provider_turn);
            }
            _ => {}
        }
        Ok(stored)
    }

    /// Reports a finished turn once per `instance:turn`; the set is bounded.
    fn report_turn(
        &self,
        input: &ProviderEventInput,
        driver: &turin_contracts::provider_instance::ProviderDriverKind,
        provider_turn: &turin_contracts::orchestration_v2::OrchestrationV2ProviderTurn,
    ) {
        let key = format!("{}:{}", input.provider_instance_id, provider_turn.id);
        {
            let mut reported = self
                .reported_turns
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !reported.0.insert(key.clone()) {
                return;
            }
            reported.1.push_back(key);
            if reported.1.len() > 4096
                && let Some(oldest) = reported.1.pop_front()
            {
                reported.0.remove(&oldest);
            }
        }
        self.analytics.record(provider_turn_analytics_properties(
            driver,
            provider_turn,
            input.analytics_context.as_ref(),
        ));
    }

    /// `syncSubagentThreadModel`: a native subagent's thread starts on the
    /// parent's model when the provider names the real one later (a Claude
    /// agent file's model arrives with the subagent's first reply). Moves the
    /// thread to the reported selection, or to the reported model alone when
    /// the provider reports no selection. Thread commands rewrite the whole row
    /// under the thread's lock, so this read and write take it too.
    async fn sync_subagent_thread_model(
        &self,
        input: &ProviderEventInput,
        subagent: &turin_contracts::orchestration_v2::OrchestrationV2Subagent,
    ) -> Result<Vec<turin_contracts::orchestration_v2::OrchestrationV2StoredEvent>, BoxError> {
        use turin_contracts::orchestration_v2::OrchestrationV2SubagentOrigin;
        let Some(child_thread_id) = subagent.child_thread_id.clone() else {
            return Ok(Vec::new());
        };
        if subagent.origin != OrchestrationV2SubagentOrigin::ProviderNative
            || (subagent.model.is_none() && subagent.model_selection.is_none())
        {
            return Ok(Vec::new());
        }
        let reported = |thread: &OrchestrationV2AppThread| -> Result<
            turin_contracts::model_selection::ModelSelection,
            BoxError,
        > {
            let current = &thread.model_selection;
            Ok(match (&subagent.model_selection, &subagent.model) {
                (Some(selection), _) => selection.clone(),
                (None, Some(model)) if model.as_str() != current.model.as_str() => {
                    // The parent's options belong to the parent's model.
                    turin_contracts::model_selection::ModelSelection {
                        instance_id: current.instance_id.clone(),
                        model: turin_contracts::TrimmedNonEmptyString::new(model)?,
                        options: None,
                    }
                }
                _ => current.clone(),
            })
        };
        let stale = || async {
            let Some(thread) = self.reads.app_thread(&child_thread_id).await? else {
                return Ok::<_, BoxError>(None);
            };
            let wanted = reported(&thread)?;
            Ok(
                (!turin_shared::model::model_selections_equal(&thread.model_selection, &wanted))
                    .then_some((thread, wanted)),
            )
        };
        // Nearly every update already matches; only a mismatch takes the lock.
        if stale().await?.is_none() {
            return Ok(Vec::new());
        }
        self.threads
            .with_lock(&child_thread_id, || async {
                let Some((thread, wanted)) = stale().await? else {
                    return Ok(Vec::new());
                };
                let now = UtcDateTime::now();
                let mut updated = thread.clone();
                updated.model_selection = wanted;
                updated.updated_at = now;
                let event = domain_event(
                    input,
                    "thread.model-selection-updated",
                    &thread.id,
                    None,
                    None,
                    now,
                    &updated,
                )?;
                Ok(self.events.write(WriteInput::events(vec![event])).await?)
            })
            .await
    }

    async fn write_normalized(
        &self,
        input: &ProviderEventInput,
        guard: &IngestGuard,
    ) -> Result<
        Vec<turin_contracts::orchestration_v2::OrchestrationV2StoredEvent>,
        ProviderEventIngestError,
    > {
        let events = self.normalize(input).await?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let publish_error = |cause: SinkError| ProviderEventIngestError::Publish {
            provider_session_id: input.provider_session_id.clone(),
            cause,
        };
        let write = WriteInput {
            command_id: input.command_id.clone(),
            events: events.clone(),
            effects: Vec::new(),
            guard_pending_user_input_cancellations: true,
        };
        if let Some(owner) = &guard.provider_thread_owner {
            let result = self
                .events
                .write_if_provider_thread_owner(ProviderThreadOwnerWrite {
                    write,
                    provider_thread_id: owner.provider_thread_id.clone(),
                    run_id: owner.run_id.clone(),
                    active_attempt_id: owner.active_attempt_id.clone(),
                    expected_last_run_ordinal: owner.expected_last_run_ordinal,
                })
                .await
                .map_err(publish_error)?;
            return Ok(result.stored_events);
        }
        if let Some(run) = &guard.run_current {
            let result = self
                .events
                .write_if_run_current(RunCurrentWrite {
                    write,
                    thread_id: input.thread_id.clone(),
                    run_id: run.run_id.clone(),
                    active_attempt_id: run.active_attempt_id.clone(),
                    expected_status: run.expected_status,
                })
                .await
                .map_err(publish_error)?;
            return Ok(result.stored_events);
        }
        self.events.write(write).await.map_err(publish_error)
    }

    /// `withLock` around `app_thread.created`, so the thread row is read and
    /// written under the thread's command lock.
    pub async fn ingest_under_thread_lock(
        &self,
        input: &ProviderEventInput,
        guard: &IngestGuard,
    ) -> Result<
        Vec<turin_contracts::orchestration_v2::OrchestrationV2StoredEvent>,
        ProviderEventIngestError,
    > {
        if let ProviderAdapterV2Event::AppThreadCreated { app_thread, .. } = &input.event {
            let thread_id = app_thread.id.clone();
            return self
                .threads
                .with_lock(&thread_id, || self.ingest(input, guard))
                .await;
        }
        self.ingest(input, guard).await
    }
}

#[async_trait]
impl SessionEventIngest for ProviderEventIngestor {
    /// Runless requests and their node and turn item, persisted without a run.
    async fn ingest_normalized(
        &self,
        identity: &crate::core::provider_session_manager::ProviderSessionIdentity,
        thread_id: ThreadId,
        event: ProviderAdapterV2Event,
    ) -> Result<(), BoxError> {
        let input = ProviderEventInput {
            provider_session_id: identity.provider_session_id.clone(),
            provider_instance_id: identity.instance_id.clone(),
            command_id: None,
            thread_id,
            run_id: None,
            node_id: None,
            raw_event_id: None,
            event,
            analytics_context: None,
        };
        ProviderEventIngestor::ingest(self, &input, &IngestGuard::default())
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// Builds one domain event through the same JSON decode upstream uses
/// (`decodeDomainEvent` over `compactUndefined`).
fn domain_event<P: Serialize>(
    input: &ProviderEventInput,
    event_type: &str,
    thread_id: &ThreadId,
    run_id: Option<&RunId>,
    node_id: Option<&NodeId>,
    occurred_at: UtcDateTime,
    payload: &P,
) -> Result<OrchestrationV2DomainEvent, BoxError> {
    let id: EventId = IdAllocator.event(Some(thread_id), None, Some(&input.provider_session_id));
    let mut value = json!({
        "id": id,
        "type": event_type,
        "threadId": thread_id,
        "driver": event_driver(&input.event),
        "providerInstanceId": input.provider_instance_id,
        "occurredAt": occurred_at,
        "payload": serde_json::to_value(payload)?,
    });
    if let Some(run_id) = run_id.or(input.run_id.as_ref()) {
        value["runId"] = serde_json::to_value(run_id)?;
    }
    if let Some(node_id) = node_id.or(input.node_id.as_ref()) {
        value["nodeId"] = serde_json::to_value(node_id)?;
    }
    if let Some(raw_event_id) = &input.raw_event_id {
        value["rawEventId"] = serde_json::to_value(raw_event_id)?;
    }
    Ok(serde_json::from_value(value)?)
}

fn event_driver(
    event: &ProviderAdapterV2Event,
) -> &turin_contracts::provider_instance::ProviderDriverKind {
    match event {
        ProviderAdapterV2Event::AppThreadCreated { driver, .. }
        | ProviderAdapterV2Event::ProviderSessionUpdated { driver, .. }
        | ProviderAdapterV2Event::ProviderThreadUpdated { driver, .. }
        | ProviderAdapterV2Event::ProviderTurnUpdated { driver, .. }
        | ProviderAdapterV2Event::NodeUpdated { driver, .. }
        | ProviderAdapterV2Event::SubagentUpdated { driver, .. }
        | ProviderAdapterV2Event::MessageUpdated { driver, .. }
        | ProviderAdapterV2Event::TurnItemUpdated { driver, .. }
        | ProviderAdapterV2Event::RuntimeRequestUpdated { driver, .. }
        | ProviderAdapterV2Event::PlanUpdated { driver, .. }
        | ProviderAdapterV2Event::TurnTerminal { driver, .. } => driver,
    }
}

/// Thread, run and node of a plan artifact, read through its wire shape.
fn plan_scope(plan: &OrchestrationV2PlanArtifact) -> (ThreadId, Option<RunId>, Option<NodeId>) {
    let value = serde_json::to_value(plan).unwrap_or(Value::Null);
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let thread_id = text("threadId")
        .and_then(|id| ThreadId::new(id).ok())
        .unwrap_or_else(|| ThreadId::new("thread:unknown").expect("static id"));
    let run_id = text("runId").and_then(|id| RunId::new(id).ok());
    let node_id = text("nodeId").and_then(|id| NodeId::new(id).ok());
    (thread_id, run_id, node_id)
}

/// `normalize`: the mapping from provider events to domain events. Every
/// branch matches upstream's switch on `input.event.type`.
async fn normalize(
    reads: &dyn IngestReads,
    oom_killed: &(dyn Fn(&ThreadId) -> bool + Send + Sync),
    input: &ProviderEventInput,
) -> Result<Vec<OrchestrationV2DomainEvent>, BoxError> {
    let now = UtcDateTime::now();
    match &input.event {
        ProviderAdapterV2Event::AppThreadCreated { app_thread, .. } => {
            let existing = reads.app_thread(&app_thread.id).await?;
            if let Some(existing) = &existing
                && existing.title == app_thread.title
                && existing.model_selection == app_thread.model_selection
            {
                return Ok(Vec::new());
            }
            let (event_type, payload) = match &existing {
                None => ("thread.created", serde_json::to_value(app_thread)?),
                Some(existing) => {
                    let mut updated = existing.clone();
                    updated.title = app_thread.title.clone();
                    updated.model_selection = app_thread.model_selection.clone();
                    updated.updated_at = now;
                    ("thread.metadata-updated", serde_json::to_value(updated)?)
                }
            };
            Ok(vec![domain_event(
                input,
                event_type,
                &app_thread.id,
                None,
                None,
                now,
                &payload,
            )?])
        }
        ProviderAdapterV2Event::ProviderSessionUpdated {
            provider_session, ..
        } => Ok(vec![domain_event(
            input,
            "provider-session.updated",
            &input.thread_id,
            None,
            None,
            now,
            provider_session,
        )?]),
        ProviderAdapterV2Event::ProviderThreadUpdated {
            provider_thread,
            retained_native_turn_ids,
            ..
        } => {
            let thread_id = provider_thread
                .app_thread_id
                .clone()
                .unwrap_or_else(|| input.thread_id.clone());
            let mut events = vec![domain_event(
                input,
                "provider-thread.updated",
                &thread_id,
                None,
                None,
                now,
                provider_thread,
            )?];
            if let Some(retained) = retained_native_turn_ids {
                events.extend(
                    roll_back_rewound_runs(
                        reads,
                        input,
                        &thread_id,
                        &provider_thread.id,
                        &retained.iter().cloned().collect(),
                        now,
                    )
                    .await?,
                );
            }
            Ok(events)
        }
        ProviderAdapterV2Event::ProviderTurnUpdated {
            thread_id,
            provider_turn,
            ..
        } => {
            let mut events = Vec::new();
            if is_terminal_turn_status(provider_turn.status) {
                events.extend(
                    dismiss_native_user_inputs(
                        reads,
                        input,
                        &provider_turn.id,
                        thread_id.as_ref().unwrap_or(&input.thread_id),
                        now,
                    )
                    .await?,
                );
            }
            let target = thread_id.clone().unwrap_or_else(|| input.thread_id.clone());
            events.push(domain_event(
                input,
                "provider-turn.updated",
                &target,
                None,
                Some(&provider_turn.node_id),
                now,
                provider_turn,
            )?);
            Ok(events)
        }
        ProviderAdapterV2Event::NodeUpdated { node, .. } => {
            // Adapters resend an unchanged node with every text delta. Compare
            // with the stored node: the server also writes nodes.
            if reads.node_stored(node).await? {
                return Ok(Vec::new());
            }
            Ok(vec![domain_event(
                input,
                "node.updated",
                &node.thread_id,
                node.run_id.as_ref(),
                Some(&node.id),
                now,
                node,
            )?])
        }
        ProviderAdapterV2Event::SubagentUpdated { subagent, .. } => Ok(vec![domain_event(
            input,
            "subagent.updated",
            &subagent.thread_id,
            subagent.run_id.as_ref(),
            Some(&subagent.id),
            now,
            subagent,
        )?]),
        ProviderAdapterV2Event::MessageUpdated { message, .. } => Ok(vec![domain_event(
            input,
            "message.updated",
            &message.thread_id,
            message.run_id.as_ref(),
            message.node_id.as_ref(),
            now,
            message,
        )?]),
        ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => {
            let bounded = bound_file_change_for_storage(
                strip_unserved_tool_output_image_bytes(turn_item).into_owned(),
            );
            let (thread_id, run_id, node_id) = turn_item_scope(&bounded);
            Ok(vec![domain_event(
                input,
                "turn-item.updated",
                &thread_id,
                run_id.as_ref(),
                node_id.as_ref(),
                now,
                &bounded,
            )?])
        }
        ProviderAdapterV2Event::RuntimeRequestUpdated {
            thread_id,
            runtime_request,
            ..
        } => {
            let target = thread_id.clone().unwrap_or_else(|| input.thread_id.clone());
            Ok(vec![domain_event(
                input,
                "runtime-request.updated",
                &target,
                None,
                Some(&runtime_request.node_id),
                now,
                runtime_request,
            )?])
        }
        ProviderAdapterV2Event::PlanUpdated { plan, .. } => {
            let (plan_thread, run_id, node_id) = plan_scope(plan);
            // A todo list keeps the time each step ran, from the stored list.
            let payload = match plan {
                OrchestrationV2PlanArtifact::TodoList {
                    id,
                    thread_id,
                    steps,
                    ..
                } => {
                    let previous = reads.plan(thread_id, id).await?;
                    let previous_steps = match &previous {
                        Some(OrchestrationV2PlanArtifact::TodoList { steps, .. }) => {
                            Some(steps.as_slice())
                        }
                        _ => None,
                    };
                    let mut timed = plan.clone();
                    if let OrchestrationV2PlanArtifact::TodoList { steps: out, .. } = &mut timed {
                        *out = with_plan_step_durations(steps, previous_steps, now);
                    }
                    timed
                }
                other => other.clone(),
            };
            Ok(vec![domain_event(
                input,
                "plan.updated",
                &plan_thread,
                run_id.as_ref(),
                node_id.as_ref(),
                now,
                &payload,
            )?])
        }
        ProviderAdapterV2Event::TurnTerminal {
            driver,
            provider_thread_id,
            provider_turn_id,
            failure,
            failure_item_ordinal,
            retry,
            retry_started_at,
            status,
            ..
        } => {
            let dismissed =
                dismiss_native_user_inputs(reads, input, provider_turn_id, &input.thread_id, now)
                    .await?;
            if *status != turin_provider_core::provider_adapter::ProviderAdapterV2TurnTerminalStatus::Failed {
                return Ok(dismissed);
            }
            let mut events = dismissed;
            let Some(failure) = failure else {
                return Ok(events);
            };
            // A turn that failed because systemd killed the agent's scope for
            // memory would otherwise read as a plain provider crash.
            let rewritten;
            let failure = if oom_killed(&input.thread_id) {
                let mut killed = failure.clone();
                killed.message =
                    turin_contracts::orchestration_v2::OrchestrationV2ProviderFailureMessage::new(
                        "Killed: out of memory.",
                    )?;
                killed.code = Some(
                    turin_contracts::orchestration_v2::OrchestrationV2ProviderFailureCode::new(
                        "oom_kill",
                    )?,
                );
                rewritten = killed;
                &rewritten
            } else {
                failure
            };
            // Upstream always has an item ordinal on a failed terminal. When the
            // adapter omits it there is no safe ordinal, so no item is written.
            let Some(item_ordinal) = failure_item_ordinal else {
                tracing::warn!(
                    provider_turn_id = %provider_turn_id,
                    "orchestration-v2.provider-event.failure-item-ordinal-missing"
                );
                return Ok(events);
            };
            let item = make_provider_failure_turn_item(ProviderFailureTurnItemInput {
                driver,
                thread_id: input.thread_id.clone(),
                run_id: input.run_id.clone(),
                node_id: input.node_id.clone(),
                provider_thread_id: provider_thread_id.clone(),
                provider_turn_id: provider_turn_id.clone(),
                item_ordinal: item_ordinal.get(),
                failure: failure.clone(),
                retry: retry.clone(),
                retry_started_at: *retry_started_at,
                occurred_at: now,
            });
            let (thread_id, run_id, node_id) = turn_item_scope(&item);
            events.push(domain_event(
                input,
                "turn-item.updated",
                &thread_id,
                run_id.as_ref(),
                node_id.as_ref(),
                now,
                &item,
            )?);
            Ok(events)
        }
    }
}

fn is_terminal_turn_status(
    status: turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus,
) -> bool {
    use turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus as Status;
    matches!(
        status,
        Status::Completed | Status::Interrupted | Status::Failed | Status::Cancelled
    )
}

/// `dismissNativeUserInputs`: cancels the pending native user inputs of a
/// finished provider turn.
async fn dismiss_native_user_inputs(
    reads: &dyn IngestReads,
    input: &ProviderEventInput,
    provider_turn_id: &ProviderTurnId,
    thread_id: &ThreadId,
    now: UtcDateTime,
) -> Result<Vec<OrchestrationV2DomainEvent>, BoxError> {
    let pending = reads
        .pending_native_user_inputs(thread_id, provider_turn_id)
        .await?;
    let mut events = Vec::new();
    for request in pending.runtime_requests {
        let mut cancelled = request.clone();
        cancelled.status =
            turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequestStatus::Cancelled;
        cancelled.resolved_at = Some(now);
        events.push(domain_event(
            input,
            "runtime-request.updated",
            thread_id,
            None,
            Some(&request.node_id),
            now,
            &cancelled,
        )?);
    }
    for node in pending.nodes {
        let mut cancelled = node.clone();
        cancelled.status =
            turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus::Cancelled;
        cancelled.completed_at = Some(now);
        events.push(domain_event(
            input,
            "node.updated",
            thread_id,
            node.run_id.as_ref(),
            Some(&node.id),
            now,
            &cancelled,
        )?);
    }
    for item in pending.turn_items {
        let mut cancelled = item.clone();
        set_cancelled(&mut cancelled, now);
        let (_, run_id, node_id) = turn_item_scope(&item);
        events.push(domain_event(
            input,
            "turn-item.updated",
            thread_id,
            run_id.as_ref(),
            node_id.as_ref(),
            now,
            &cancelled,
        )?);
    }
    Ok(events)
}

/// Marks a pending user-input item cancelled and stamps its times. Other
/// variants have no pending state and are left alone.
fn set_cancelled(item: &mut OrchestrationV2TurnItem, now: UtcDateTime) {
    use turin_contracts::orchestration_v2::OrchestrationV2TurnItemStatus;
    if let OrchestrationV2TurnItem::UserInputRequest {
        status,
        completed_at,
        updated_at,
        ..
    }
    | OrchestrationV2TurnItem::ApprovalRequest {
        status,
        completed_at,
        updated_at,
        ..
    } = item
    {
        *status = OrchestrationV2TurnItemStatus::Cancelled;
        *completed_at = Some(now);
        *updated_at = now;
    }
}

fn turn_item_scope(item: &OrchestrationV2TurnItem) -> (ThreadId, Option<RunId>, Option<NodeId>) {
    let common = turin_provider_core::turn_item::turn_item_common(item);
    (
        common.thread_id.clone(),
        common.run_id.cloned(),
        common.node_id.cloned(),
    )
}

/// `rollBackRewoundRuns`: the provider rewound its conversation outside Turin.
/// Retires the runs that fell off its active branch with the same run,
/// root-node and checkpoint updates a Turin revert writes. The run reporting
/// the rewind performed it and stays, so it keeps `lastRunOrdinal`.
async fn roll_back_rewound_runs(
    reads: &dyn IngestReads,
    input: &ProviderEventInput,
    thread_id: &ThreadId,
    provider_thread_id: &ProviderThreadId,
    retained_native_turn_ids: &HashSet<String>,
    now: UtcDateTime,
) -> Result<Vec<OrchestrationV2DomainEvent>, BoxError> {
    use turin_contracts::orchestration_v2::{
        OrchestrationV2CheckpointStatus, OrchestrationV2ExecutionNodeStatus,
    };
    let records = reads.rewind_records(thread_id).await?;
    let runs = runs_off_native_branch(
        &records.runs,
        &records.attempts,
        &records.provider_turns,
        provider_thread_id,
        retained_native_turn_ids,
        input.run_id.as_ref(),
    );
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let event_id = || -> EventId {
        IdAllocator.event(Some(thread_id), None, Some(&input.provider_session_id))
    };
    let run_ids: HashSet<&RunId> = runs.iter().map(|run| &run.id).collect();
    let mut events = Vec::new();
    for checkpoint in &records.checkpoints {
        let Some(run_id) = &checkpoint.run_id else {
            continue;
        };
        if checkpoint.status != OrchestrationV2CheckpointStatus::Ready || !run_ids.contains(run_id)
        {
            continue;
        }
        events.push(OrchestrationV2DomainEvent::CheckpointCaptured {
            id: event_id(),
            thread_id: thread_id.clone(),
            run_id: Some(run_id.clone()),
            node_id: Some(checkpoint.node_id.clone()),
            driver: None,
            provider_instance_id: Some(input.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2Checkpoint {
                status: OrchestrationV2CheckpointStatus::Stale,
                ..checkpoint.clone()
            },
        });
    }
    for run in runs {
        let root_node = run
            .root_node_id
            .as_ref()
            .and_then(|id| records.nodes.iter().find(|node| &node.id == id));
        events.push(OrchestrationV2DomainEvent::RunUpdated {
            id: event_id(),
            thread_id: thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: root_node.map(|node| node.id.clone()),
            driver: None,
            provider_instance_id: Some(run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2Run {
                status: OrchestrationV2RunStatus::RolledBack,
                completed_at: Some(now),
                ..run.clone()
            },
        });
        if let Some(node) = root_node {
            events.push(OrchestrationV2DomainEvent::NodeUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(node.id.clone()),
                driver: None,
                provider_instance_id: Some(run.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: OrchestrationV2ExecutionNode {
                    status: OrchestrationV2ExecutionNodeStatus::RolledBack,
                    completed_at: Some(now),
                    ..node.clone()
                },
            });
        }
    }
    Ok(events)
}

#[cfg(test)]
mod sync_tests;
#[cfg(test)]
mod tests;
