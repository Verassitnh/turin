//! Port of upstream/apps/server/src/orchestration-v2/ProviderEventIngestor.ts.
//!
//! Turns one provider adapter event into orchestration domain events, then
//! writes them through `EventSink` under the run or provider-thread guard the
//! caller asks for, so a stale attempt cannot clobber newer state.
//!
//! Not ported yet (see docs/handoff/T13c.md): `rollBackRewoundRuns` for
//! `provider_thread.updated` with retained native turn ids, subagent thread
//! model sync, provider turn analytics, the OOM-kill failure rewrite, plan step
//! durations, and file-change storage bounding.

use std::sync::Arc;

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
        }
    }

    /// `normalize`: the domain events one provider event produces, without writing.
    pub async fn normalize(
        &self,
        input: &ProviderEventInput,
    ) -> Result<Vec<OrchestrationV2DomainEvent>, ProviderEventIngestError> {
        normalize(self.reads.as_ref(), input)
            .await
            .map_err(|cause| ProviderEventIngestError::Normalize {
                thread_id: input.thread_id.clone(),
                cause,
            })
    }

    /// `ingestNormalized`: normalizes and writes under `guard`. Returns the
    /// stored events, which is empty when nothing changed.
    pub async fn ingest(
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
            // Rewound-run rollback (`retained_native_turn_ids`) is not ported yet.
            let _ = retained_native_turn_ids;
            Ok(vec![domain_event(
                input,
                "provider-thread.updated",
                &thread_id,
                None,
                None,
                now,
                provider_thread,
            )?])
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
            let bounded = strip_unserved_tool_output_image_bytes(turn_item).into_owned();
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
            Ok(vec![domain_event(
                input,
                "plan.updated",
                &plan_thread,
                run_id.as_ref(),
                node_id.as_ref(),
                now,
                plan,
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
    match item {
        OrchestrationV2TurnItem::Notification {
            thread_id,
            run_id,
            node_id,
            ..
        }
        | OrchestrationV2TurnItem::UserMessage {
            thread_id,
            run_id,
            node_id,
            ..
        }
        | OrchestrationV2TurnItem::AssistantMessage {
            thread_id,
            run_id,
            node_id,
            ..
        }
        | OrchestrationV2TurnItem::Reasoning {
            thread_id,
            run_id,
            node_id,
            ..
        }
        | OrchestrationV2TurnItem::ProposedPlan {
            thread_id,
            run_id,
            node_id,
            ..
        }
        | OrchestrationV2TurnItem::TodoList {
            thread_id,
            run_id,
            node_id,
            ..
        }
        | OrchestrationV2TurnItem::UserInputRequest {
            thread_id,
            run_id,
            node_id,
            ..
        }
        | OrchestrationV2TurnItem::ApprovalRequest {
            thread_id,
            run_id,
            node_id,
            ..
        } => (thread_id.clone(), run_id.clone(), node_id.clone()),
        other => {
            let value = serde_json::to_value(other).unwrap_or(Value::Null);
            let thread_id = value
                .get("threadId")
                .and_then(Value::as_str)
                .and_then(|id| ThreadId::new(id).ok())
                .unwrap_or_else(|| ThreadId::new("thread:unknown").expect("static id"));
            (thread_id, None, None)
        }
    }
}

#[cfg(test)]
mod tests;
