//! Port of upstream/apps/server/src/orchestration-v2/EventSink.ts
//!
//! # Transaction seam with the projection side (T13a)
//!
//! A write appends events, folds them into the projections the UI reads,
//! records the command receipt and enqueues outbox effects in **one**
//! `Db::write` transaction (upstream `sql.withTransaction`). The projection
//! store plugs in through [`ProjectionApplier`]: the sink calls it
//! synchronously inside that transaction, after the event append, so a failed
//! application rolls back the events, the receipt and the effects together.
//! The trait must not do async work or publish anything.
//!
//! Publishing (the persistence event hub, this sink's live streams, the outbox
//! wakeup) happens only after the commit, in sequence order: a writer takes
//! the publish lane before it starts its transaction and holds it until it has
//! published. Upstream takes the lane as the last step of its transaction;
//! with one SQLite writer the order of commits is the order of lane
//! acquisition either way, and publishing never waits, so the lane is never
//! held for long. No bounded `send().await` ever runs while a transaction is
//! open: live fan-out uses unbounded per-subscriber channels.

use super::command_receipt_store::{
    AnyReceipt, CommandReceipt, ProjectCommandReceipt, get_project_in, get_thread_in,
    insert_if_absent_in, upsert_in,
};
use super::domain_event::{
    agent_event_from_domain, event_thread_id, event_type, stored_event_from_agent,
};
use super::effect_outbox::{EffectOutbox, EffectType, PendingEffect};
use futures::stream::{self, BoxStream, StreamExt};
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use turin_contracts::orchestration_v2::{
    OrchestrationV2DomainEvent, OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderThread,
    OrchestrationV2ResponseCapability,
    OrchestrationV2RunStatus, OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
    OrchestrationV2StoredEvent, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};
use turin_contracts::{
    CommandId, EventId, ProjectId, ProviderThreadId, RunAttemptId, RunId, RuntimeRequestId,
    ThreadId, UtcDateTime,
};
use turin_persistence::{
    AggregateKind, AppendAgentEvents, ApplicationStoredEvent, Db, OrchestrationEventStore, PersistenceDecodeError, PersistenceError, ProjectEvent,
    ProjectEventInput, ReadAgentEvents, ReceiptStatus, StoredAgentEvent, utc_millis_now,
};

/// Replay page size when a stream catches up (upstream `catchUp`).
const CATCH_UP_PAGE_SIZE: u64 = 256;

/// Implemented by the projection store (T13a). Every method runs inside the
/// sink's write transaction on the writer connection.
pub trait ProjectionApplier: Send + Sync {
    /// Assigns a stable, collision-free position to a `turn-item.updated`
    /// event's item (upstream `TurnItemPositionStore.normalize`). Other event
    /// kinds are never passed in. `run_ordinal` is the ordinal of the item's
    /// run when the same write creates or updates that run.
    fn normalize_turn_item(
        &self,
        tx: &Transaction<'_>,
        event: OrchestrationV2DomainEvent,
        run_ordinal: Option<u64>,
    ) -> Result<OrchestrationV2DomainEvent, PersistenceError>;

    /// Folds one appended thread event into the projection rows.
    fn apply_event(
        &self,
        tx: &Transaction<'_>,
        event: &OrchestrationV2DomainEvent,
    ) -> Result<(), PersistenceError>;

    /// Folds one appended project event into the project row.
    fn apply_project_event(
        &self,
        tx: &Transaction<'_>,
        event: &ProjectEvent,
    ) -> Result<(), PersistenceError>;

    /// `ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION`, recorded with the
    /// last-applied sequence.
    fn schema_version(&self) -> i64;
}

/// `EventSinkWriteError` / `EventSinkStreamError`.
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("Failed to write {event_count} orchestration V2 event(s).")]
    Write {
        event_count: usize,
        command_id: Option<CommandId>,
        #[source]
        cause: PersistenceError,
    },
    #[error("{}", stream_message(.thread_id))]
    Stream {
        thread_id: Option<ThreadId>,
        after_sequence: Option<i64>,
        #[source]
        cause: PersistenceError,
    },
}

fn stream_message(thread_id: &Option<ThreadId>) -> String {
    match thread_id {
        None => "Failed to stream orchestration V2 events.".to_owned(),
        Some(id) => format!("Failed to stream orchestration V2 events for thread {id}."),
    }
}

fn decode_err(operation: &str, issue: impl Into<String>) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(operation, issue))
}

/// Input of [`EventSink::write`] (upstream `write` / `writeWithEffects`).
#[derive(Debug, Clone, Default)]
pub struct WriteInput {
    pub command_id: Option<CommandId>,
    pub events: Vec<OrchestrationV2DomainEvent>,
    pub effects: Vec<PendingEffect>,
    /// Drops user-input cancellations a user answered after terminal
    /// normalization read the pending request.
    pub guard_pending_user_input_cancellations: bool,
}

impl WriteInput {
    pub fn events(events: Vec<OrchestrationV2DomainEvent>) -> Self {
        Self {
            events,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConditionalWriteResult {
    pub committed: bool,
    pub stored_events: Vec<OrchestrationV2StoredEvent>,
}

/// Cancels the thread's unsettled effects of these types in the same
/// transaction as the command.
#[derive(Debug, Clone)]
pub struct CancelUnsettled {
    pub effect_types: Vec<EffectType>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct CommandCommit {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub command_type: String,
    pub accepted_at: UtcDateTime,
    pub events: Vec<OrchestrationV2DomainEvent>,
    pub effects: Vec<PendingEffect>,
    pub cancel_unsettled_effects: Option<CancelUnsettled>,
}

#[derive(Debug, Clone)]
pub struct CommitResult {
    pub receipt: CommandReceipt,
    pub stored_events: Vec<OrchestrationV2StoredEvent>,
    /// False when the command id already had a receipt: nothing was written.
    pub committed: bool,
    pub cancelled_effect_count: usize,
}

#[derive(Debug, Clone)]
pub struct ProjectCommit {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub command_type: String,
    pub accepted_at: UtcDateTime,
    pub event: ProjectEventInput,
}

#[derive(Debug, Clone)]
pub struct ProjectCommitResult {
    pub receipt: ProjectCommandReceipt,
    pub committed: bool,
}

/// `writeIfRunCurrent`: commits only while the run is still `expected_status`
/// and still owned by `active_attempt_id`.
#[derive(Debug, Clone)]
pub struct RunCurrentWrite {
    pub write: WriteInput,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub active_attempt_id: RunAttemptId,
    pub expected_status: OrchestrationV2RunStatus,
}

/// `writeIfProviderThreadOwner`: commits only when the provider thread is
/// still owned by the expected run attempt and ordinal. Used for late
/// post-terminal provider_thread updates so a completed or superseded attempt
/// cannot clobber a newer attempt that already claimed the thread.
#[derive(Debug, Clone)]
pub struct ProviderThreadOwnerWrite {
    pub write: WriteInput,
    pub provider_thread_id: ProviderThreadId,
    pub run_id: RunId,
    pub active_attempt_id: RunAttemptId,
    pub expected_last_run_ordinal: u64,
}

/// What a stream yields (upstream stream options).
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    pub thread_id: Option<ThreadId>,
    pub after_sequence: Option<i64>,
    /// Filter before queueing live events so a busy worker retains only the
    /// events it handles.
    pub event_type: Option<String>,
}

pub type SinkStream = BoxStream<'static, Result<OrchestrationV2StoredEvent, SinkError>>;

struct LiveEvent {
    stored: OrchestrationV2StoredEvent,
    event_type: String,
    thread_id: Option<ThreadId>,
}

struct LiveSubscriber {
    event_type: Option<String>,
    tx: mpsc::UnboundedSender<Arc<LiveEvent>>,
}

/// Unbounded fan-out (upstream `PubSub.unbounded`): publishing never waits,
/// and an internal worker's subscription never drops events under load.
#[derive(Default)]
struct LiveHub {
    next_id: AtomicU64,
    subscribers: Mutex<HashMap<u64, LiveSubscriber>>,
}

impl LiveHub {
    fn subscribe(
        self: &Arc<Self>,
        event_type: Option<String>,
    ) -> (LiveSubscription, mpsc::UnboundedReceiver<Arc<LiveEvent>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers
            .lock()
            .insert(id, LiveSubscriber { event_type, tx });
        (
            LiveSubscription {
                id,
                hub: Arc::clone(self),
            },
            rx,
        )
    }

    fn publish(&self, events: &[Arc<LiveEvent>]) {
        let mut subscribers = self.subscribers.lock();
        subscribers.retain(|_, subscriber| {
            for event in events {
                if subscriber
                    .event_type
                    .as_ref()
                    .is_some_and(|wanted| wanted != &event.event_type)
                {
                    continue;
                }
                if subscriber.tx.send(Arc::clone(event)).is_err() {
                    return false;
                }
            }
            true
        });
    }
}

struct LiveSubscription {
    id: u64,
    hub: Arc<LiveHub>,
}

impl Drop for LiveSubscription {
    fn drop(&mut self) {
        self.hub.subscribers.lock().remove(&self.id);
    }
}

struct Appended {
    rows: Vec<StoredAgentEvent>,
    typed: Vec<OrchestrationV2StoredEvent>,
}

#[derive(Clone)]
pub struct EventSink {
    db: Db,
    events: OrchestrationEventStore,
    outbox: EffectOutbox,
    projections: Arc<dyn ProjectionApplier>,
    /// Held from before a writer's transaction until it has published.
    publish_lane: Arc<AsyncMutex<()>>,
    live: Arc<LiveHub>,
}

impl EventSink {
    pub fn new(
        events: OrchestrationEventStore,
        outbox: EffectOutbox,
        projections: Arc<dyn ProjectionApplier>,
    ) -> Self {
        Self {
            db: events.db().clone(),
            events,
            outbox,
            projections,
            publish_lane: Arc::new(AsyncMutex::new(())),
            live: Arc::new(LiveHub::default()),
        }
    }

    pub fn outbox(&self) -> &EffectOutbox {
        &self.outbox
    }

    pub fn events(&self) -> &OrchestrationEventStore {
        &self.events
    }

    fn write_error(
        event_count: usize,
        command_id: Option<&CommandId>,
        cause: PersistenceError,
    ) -> SinkError {
        SinkError::Write {
            event_count,
            command_id: command_id.cloned(),
            cause,
        }
    }

    // A user can answer after terminal normalization reads the pending
    // request. Recheck inside the write transaction so stale cleanup cannot
    // erase answers.
    fn guard_user_input_cancellations(
        tx: &Transaction<'_>,
        events: Vec<OrchestrationV2DomainEvent>,
    ) -> Result<Vec<OrchestrationV2DomainEvent>, PersistenceError> {
        let mut stale_requests: HashSet<RuntimeRequestId> = HashSet::new();
        let mut stale_nodes = HashSet::new();
        for event in &events {
            let OrchestrationV2DomainEvent::RuntimeRequestUpdated { thread_id, payload, .. } = event
            else {
                continue;
            };
            if payload.kind != OrchestrationV2RuntimeRequestKind::UserInput
                || payload.status != OrchestrationV2RuntimeRequestStatus::Cancelled
            {
                continue;
            }
            let current: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM orchestration_v2_projection_runtime_requests WHERE runtime_request_id = ?1 AND thread_id = ?2",
                    params![payload.id.as_str(), thread_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| PersistenceError::sql("EventSink.guard-cancellation", &err))?;
            let still_pending = current
                .and_then(|json| {
                    serde_json::from_str::<turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequest>(&json).ok()
                })
                .is_some_and(|request| {
                    request.status == OrchestrationV2RuntimeRequestStatus::Pending
                        && request.kind == OrchestrationV2RuntimeRequestKind::UserInput
                        && request.provider_turn_id == payload.provider_turn_id
                        && !matches!(
                            request.response_capability,
                            OrchestrationV2ResponseCapability::Message
                        )
                });
            if !still_pending {
                stale_requests.insert(payload.id.clone());
                stale_nodes.insert(payload.node_id.clone());
            }
        }
        Ok(events
            .into_iter()
            .filter(|event| match event {
                OrchestrationV2DomainEvent::RuntimeRequestUpdated { payload, .. } => {
                    payload.status != OrchestrationV2RuntimeRequestStatus::Cancelled
                        || !stale_requests.contains(&payload.id)
                }
                OrchestrationV2DomainEvent::NodeUpdated { payload, .. } => {
                    payload.status != OrchestrationV2ExecutionNodeStatus::Cancelled
                        || !stale_nodes.contains(&payload.id)
                }
                OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } => !matches!(
                    payload,
                    OrchestrationV2TurnItem::UserInputRequest { status, request_id, .. }
                        if *status == OrchestrationV2TurnItemStatus::Cancelled
                            && stale_requests.contains(request_id)
                ),
                _ => true,
            })
            .collect())
    }

    fn normalize_events(
        &self,
        tx: &Transaction<'_>,
        events: Vec<OrchestrationV2DomainEvent>,
    ) -> Result<Vec<OrchestrationV2DomainEvent>, PersistenceError> {
        let mut run_ordinals: HashMap<RunId, u64> = HashMap::new();
        for event in &events {
            match event {
                OrchestrationV2DomainEvent::RunCreated { payload, .. }
                | OrchestrationV2DomainEvent::RunUpdated { payload, .. } => {
                    run_ordinals.insert(payload.id.clone(), payload.ordinal.get());
                }
                _ => {}
            }
        }
        let mut normalized = Vec::with_capacity(events.len());
        for event in events {
            if let OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } = &event {
                let run_ordinal = turn_item_run_id(payload)
                    .and_then(|run_id| run_ordinals.get(run_id))
                    .copied();
                normalized.push(self.projections.normalize_turn_item(tx, event, run_ordinal)?);
            } else {
                normalized.push(event);
            }
        }
        Ok(normalized)
    }

    /// Appends, then folds the stored events into the projections and records
    /// the last applied sequence (upstream `applyStoredEvents`).
    fn append_and_apply(
        &self,
        tx: &Transaction<'_>,
        command_id: Option<&CommandId>,
        events: Vec<OrchestrationV2DomainEvent>,
    ) -> Result<Appended, PersistenceError> {
        let rows = events
            .iter()
            .map(agent_event_from_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let stored_rows = self.events.append_agent_events_in(
            tx,
            &AppendAgentEvents {
                command_id: command_id.cloned(),
                events: rows,
            },
        )?;
        if stored_rows.len() != events.len() {
            return Err(decode_err(
                "EventSink.append",
                format!(
                    "appended {} event(s) for {} requested",
                    stored_rows.len(),
                    events.len()
                ),
            ));
        }
        let typed = stored_rows
            .iter()
            .zip(events)
            .map(|(stored, event)| {
                Ok(OrchestrationV2StoredEvent {
                    sequence: u64::try_from(stored.sequence)
                        .map_err(|_| decode_err("EventSink.append", "negative sequence"))?,
                    command_id: stored.command_id.clone(),
                    event,
                })
            })
            .collect::<Result<Vec<_>, PersistenceError>>()?;
        for stored in &typed {
            self.projections.apply_event(tx, &stored.event)?;
        }
        if let Some(last) = stored_rows.last() {
            tx.execute(
                "INSERT INTO orchestration_v2_projection_metadata (projection_name, schema_version, last_sequence, updated_at) VALUES ('thread-projections', ?1, ?2, ?3) ON CONFLICT(projection_name) DO UPDATE SET schema_version = excluded.schema_version, last_sequence = excluded.last_sequence, updated_at = excluded.updated_at",
                params![self.projections.schema_version(), last.sequence, utc_millis_now()],
            )
            .map_err(|err| PersistenceError::sql("EventSink.projection-metadata", &err))?;
        }
        Ok(Appended {
            rows: stored_rows,
            typed,
        })
    }

    fn write_in(
        &self,
        tx: &Transaction<'_>,
        input: WriteInput,
    ) -> Result<Appended, PersistenceError> {
        let events = if input.guard_pending_user_input_cancellations {
            Self::guard_user_input_cancellations(tx, input.events)?
        } else {
            input.events
        };
        let events = self.normalize_events(tx, events)?;
        let appended = self.append_and_apply(tx, input.command_id.as_ref(), events)?;
        EffectOutbox::enqueue_in(tx, &input.effects)?;
        Ok(appended)
    }

    /// After the commit: wake claimers, then publish events to the persistence
    /// hub and this sink's live streams.
    fn publish(&self, appended: &Appended, effect_count: usize) {
        if effect_count > 0 {
            self.outbox.notify_available(effect_count);
        }
        self.publish_stored(appended);
    }

    fn publish_stored(&self, appended: &Appended) {
        if appended.rows.is_empty() {
            return;
        }
        self.events.publish_committed(
            appended
                .rows
                .iter()
                .cloned()
                .map(ApplicationStoredEvent::Agent)
                .collect(),
        );
        let live: Vec<Arc<LiveEvent>> = appended
            .typed
            .iter()
            .map(|stored| {
                Arc::new(LiveEvent {
                    event_type: event_type(&stored.event),
                    thread_id: event_thread_id(&stored.event),
                    stored: stored.clone(),
                })
            })
            .collect();
        self.live.publish(&live);
    }

    /// Appends, projects and (with `effects`) enqueues events in one
    /// transaction, then publishes them. Upstream `write` / `writeWithEffects`.
    pub async fn write(
        &self,
        input: WriteInput,
    ) -> Result<Vec<OrchestrationV2StoredEvent>, SinkError> {
        let count = input.events.len();
        let command_id = input.command_id.clone();
        let effect_count = input.effects.len();
        let _lane = self.publish_lane.lock().await;
        let sink = self.clone();
        let appended = self
            .db
            .write(move |tx| sink.write_in(tx, input))
            .await
            .map_err(|cause| Self::write_error(count, command_id.as_ref(), cause))?;
        self.publish(&appended, effect_count);
        Ok(appended.typed)
    }

    async fn write_conditional<F>(
        &self,
        input: WriteInput,
        check: F,
    ) -> Result<ConditionalWriteResult, SinkError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<bool, PersistenceError> + Send + 'static,
    {
        let count = input.events.len();
        let command_id = input.command_id.clone();
        let effect_count = input.effects.len();
        let _lane = self.publish_lane.lock().await;
        let sink = self.clone();
        let appended = self
            .db
            .write(move |tx| {
                if !check(tx)? {
                    return Ok(None);
                }
                sink.write_in(tx, input).map(Some)
            })
            .await
            .map_err(|cause| Self::write_error(count, command_id.as_ref(), cause))?;
        match appended {
            None => Ok(ConditionalWriteResult {
                committed: false,
                stored_events: vec![],
            }),
            Some(appended) => {
                self.publish(&appended, effect_count);
                Ok(ConditionalWriteResult {
                    committed: true,
                    stored_events: appended.typed,
                })
            }
        }
    }

    pub async fn write_if_run_current(
        &self,
        input: RunCurrentWrite,
    ) -> Result<ConditionalWriteResult, SinkError> {
        let RunCurrentWrite {
            write,
            thread_id,
            run_id,
            active_attempt_id,
            expected_status,
        } = input;
        let expected_status = status_text(&expected_status);
        self.write_conditional(write, move |tx| {
            let current: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT status, json_extract(payload_json, '$.activeAttemptId') AS active_attempt_id FROM orchestration_v2_projection_runs WHERE run_id = ?1 AND thread_id = ?2 LIMIT 1",
                    params![run_id.as_str(), thread_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|err| PersistenceError::sql("EventSink.write-if-run-current", &err))?;
            Ok(current.is_some_and(|(status, attempt)| {
                status == expected_status && attempt.as_deref() == Some(active_attempt_id.as_str())
            }))
        })
        .await
    }

    pub async fn write_if_provider_thread_owner(
        &self,
        input: ProviderThreadOwnerWrite,
    ) -> Result<ConditionalWriteResult, SinkError> {
        let ProviderThreadOwnerWrite {
            write,
            provider_thread_id,
            run_id,
            active_attempt_id,
            expected_last_run_ordinal,
        } = input;
        self.write_conditional(write, move |tx| {
            let current: Option<(Option<String>, Option<i64>)> = tx
                .query_row(
                    "SELECT json_extract(r.payload_json, '$.activeAttemptId') AS active_attempt_id, p.last_run_ordinal FROM orchestration_v2_projection_provider_threads p JOIN orchestration_v2_projection_runs r ON r.run_id = ?1 AND r.thread_id = p.thread_id WHERE p.provider_thread_id = ?2 LIMIT 1",
                    params![run_id.as_str(), provider_thread_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|err| {
                    PersistenceError::sql("EventSink.write-if-provider-thread-owner", &err)
                })?;
            Ok(current.is_some_and(|(attempt, ordinal)| {
                attempt.as_deref() == Some(active_attempt_id.as_str())
                    && ordinal == i64::try_from(expected_last_run_ordinal).ok()
            }))
        })
        .await
    }

    /// Commits only while the stored provider thread still equals `expected`,
    /// the row its writer read, so a read-modify-write of the row cannot
    /// overwrite an update published in between. The whole row is compared
    /// because not every writer moves `updatedAt`.
    ///
    /// Compared as JSON values, not as text: key order and number spelling of
    /// the stored payload are the projection's business.
    pub async fn write_if_provider_thread_unchanged(
        &self,
        expected: OrchestrationV2ProviderThread,
        events: Vec<OrchestrationV2DomainEvent>,
    ) -> Result<ConditionalWriteResult, SinkError> {
        let count = events.len();
        let expected_json = serde_json::to_value(&expected).map_err(|err| {
            Self::write_error(
                count,
                None,
                decode_err("EventSink.provider-thread-json", err.to_string()),
            )
        })?;
        self.write_conditional(WriteInput::events(events), move |tx| {
            let current: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM orchestration_v2_projection_provider_threads WHERE provider_thread_id = ?1 LIMIT 1",
                    [expected.id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| {
                    PersistenceError::sql("EventSink.write-if-provider-thread-unchanged", &err)
                })?;
            Ok(current
                .and_then(|json| serde_json::from_str::<Value>(&json).ok())
                .is_some_and(|stored| stored == expected_json))
        })
        .await
    }

    fn existing_command_result(
        &self,
        tx: &Transaction<'_>,
        command_id: &CommandId,
    ) -> Result<CommandReceipt, PersistenceError> {
        get_thread_in(tx, command_id)?.ok_or_else(|| {
            decode_err(
                "EventSink.commit-command",
                format!("Command receipt {command_id} disappeared during its transaction."),
            )
        })
    }

    /// Reserves the command's receipt, appends and projects its events,
    /// enqueues its effects and finalizes the receipt, all in one transaction.
    /// A reused command id commits nothing and returns the recorded result.
    pub async fn commit_command(&self, input: CommandCommit) -> Result<CommitResult, SinkError> {
        let count = input.events.len();
        let command_id = input.command_id.clone();
        let effect_count = input.effects.len();
        let thread_id = input.thread_id.clone();
        let _lane = self.publish_lane.lock().await;
        let sink = self.clone();
        let outcome = self
            .db
            .write(move |tx| {
                let reserved = CommandReceipt {
                    command_id: input.command_id.clone(),
                    thread_id: input.thread_id.clone(),
                    command_type: input.command_type.clone(),
                    accepted_at: input.accepted_at,
                    result_sequence: 0,
                    status: ReceiptStatus::Accepted,
                    error: None,
                };
                if !insert_if_absent_in(tx, &AnyReceipt::Thread(reserved.clone()))? {
                    let receipt = sink.existing_command_result(tx, &input.command_id)?;
                    return Ok(CommitOutcome::Replayed(receipt));
                }
                let events = sink.normalize_events(tx, input.events)?;
                let appended = sink.append_and_apply(tx, Some(&input.command_id), events)?;
                let Some(last) = appended.typed.last() else {
                    return Err(decode_err(
                        "EventSink.commit-command",
                        format!("Command {} produced no orchestration events.", input.command_id),
                    ));
                };
                let result_sequence = last.sequence;
                EffectOutbox::enqueue_in(tx, &input.effects)?;
                let receipt = CommandReceipt {
                    result_sequence,
                    ..reserved
                };
                upsert_in(tx, &AnyReceipt::Thread(receipt.clone()))?;
                let cancelled = match &input.cancel_unsettled_effects {
                    Some(cancel) => EffectOutbox::cancel_unsettled_in(
                        tx,
                        &input.thread_id,
                        &cancel.effect_types,
                        &cancel.reason,
                    )?,
                    None => vec![],
                };
                Ok(CommitOutcome::Committed {
                    receipt,
                    appended,
                    cancelled,
                })
            })
            .await
            .map_err(|cause| Self::write_error(count, Some(&command_id), cause))?;
        match outcome {
            CommitOutcome::Replayed(receipt) => {
                let stored_events = self
                    .read_by_command_id(&command_id)
                    .await
                    .map_err(|error| match error {
                        SinkError::Stream { cause, .. } => {
                            Self::write_error(count, Some(&command_id), cause)
                        }
                        other => other,
                    })?;
                debug_assert_eq!(receipt.thread_id, thread_id);
                Ok(CommitResult {
                    receipt,
                    stored_events,
                    committed: false,
                    cancelled_effect_count: 0,
                })
            }
            CommitOutcome::Committed {
                receipt,
                appended,
                cancelled,
            } => {
                self.outbox.signal_cancellations(&cancelled);
                self.publish(&appended, effect_count);
                Ok(CommitResult {
                    receipt,
                    stored_events: appended.typed,
                    committed: true,
                    cancelled_effect_count: cancelled.len(),
                })
            }
        }
    }

    /// Records a rejected command (its result sequence is the thread's latest
    /// event), or returns the receipt its command id already has.
    pub async fn commit_rejected_command(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
        command_type: String,
        rejected_at: UtcDateTime,
        error: String,
    ) -> Result<CommandReceipt, SinkError> {
        let id = command_id.clone();
        self.db
            .write(move |tx| {
                let sequence: Option<i64> = tx
                    .query_row(
                        "SELECT MAX(sequence) FROM orchestration_events INDEXED BY idx_orchestration_events_agent_stream_sequence WHERE application_event_version = 2 AND aggregate_kind = 'thread' AND stream_id = ?1",
                        [thread_id.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(|err| PersistenceError::sql("EventSink.reject-sequence", &err))?;
                let receipt = CommandReceipt {
                    command_id: command_id.clone(),
                    thread_id,
                    command_type,
                    accepted_at: rejected_at,
                    result_sequence: u64::try_from(sequence.unwrap_or(0)).unwrap_or(0),
                    status: ReceiptStatus::Rejected,
                    error: Some(error),
                };
                if insert_if_absent_in(tx, &AnyReceipt::Thread(receipt.clone()))? {
                    return Ok(receipt);
                }
                Ok(get_thread_in(tx, &command_id)?.unwrap_or(receipt))
            })
            .await
            .map_err(|cause| Self::write_error(0, Some(&id), cause))
    }

    fn existing_project_receipt(
        tx: &Transaction<'_>,
        command_id: &CommandId,
    ) -> Result<ProjectCommandReceipt, PersistenceError> {
        get_project_in(tx, command_id)?.ok_or_else(|| {
            decode_err(
                "EventSink.project-command",
                format!("Command {command_id} was already used by a thread command."),
            )
        })
    }

    /// Appends a project event, folds it into its row and records the receipt
    /// in one transaction. A reused command id commits nothing and returns its
    /// receipt.
    pub async fn commit_project_command(
        &self,
        input: ProjectCommit,
    ) -> Result<ProjectCommitResult, SinkError> {
        let command_id = input.command_id.clone();
        let _lane = self.publish_lane.lock().await;
        let projections = Arc::clone(&self.projections);
        let (receipt, committed_event) = self
            .db
            .write(move |tx| {
                let reserved = ProjectCommandReceipt {
                    command_id: input.command_id.clone(),
                    project_id: input.project_id.clone(),
                    command_type: input.command_type,
                    accepted_at: input.accepted_at,
                    result_sequence: 0,
                    status: ReceiptStatus::Accepted,
                    error: None,
                };
                if !insert_if_absent_in(tx, &AnyReceipt::Project(reserved.clone()))? {
                    return Ok((
                        Self::existing_project_receipt(tx, &input.command_id)?,
                        None,
                    ));
                }
                let event = append_project_event_in(tx, &input.event)?;
                projections.apply_project_event(tx, &event)?;
                let receipt = ProjectCommandReceipt {
                    result_sequence: u64::try_from(event.sequence).unwrap_or(0),
                    ..reserved
                };
                upsert_in(tx, &AnyReceipt::Project(receipt.clone()))?;
                Ok((receipt, Some(event)))
            })
            .await
            .map_err(|cause| Self::write_error(1, Some(&command_id), cause))?;
        if let Some(event) = &committed_event {
            self.events
                .publish_committed(vec![ApplicationStoredEvent::Project(event.clone())]);
        }
        Ok(ProjectCommitResult {
            receipt,
            committed: committed_event.is_some(),
        })
    }

    /// Records a rejected project command, or returns the receipt its command
    /// id already has.
    pub async fn commit_rejected_project_command(
        &self,
        command_id: CommandId,
        project_id: ProjectId,
        command_type: String,
        rejected_at: UtcDateTime,
        error: String,
    ) -> Result<ProjectCommandReceipt, SinkError> {
        let id = command_id.clone();
        self.db
            .write(move |tx| {
                let sequence = latest_application_sequence_in(tx)?;
                let receipt = ProjectCommandReceipt {
                    command_id: command_id.clone(),
                    project_id,
                    command_type,
                    accepted_at: rejected_at,
                    result_sequence: sequence,
                    status: ReceiptStatus::Rejected,
                    error: Some(error),
                };
                if insert_if_absent_in(tx, &AnyReceipt::Project(receipt.clone()))? {
                    Ok(receipt)
                } else {
                    Self::existing_project_receipt(tx, &command_id)
                }
            })
            .await
            .map_err(|cause| Self::write_error(0, Some(&id), cause))
    }

    pub async fn latest_sequence(&self, thread_id: Option<ThreadId>) -> Result<i64, SinkError> {
        let for_error = thread_id.clone();
        self.events
            .latest_agent_sequence(thread_id)
            .await
            .map_err(|cause| SinkError::Stream {
                thread_id: for_error,
                after_sequence: None,
                cause,
            })
    }

    /// Every event the command wrote, in order.
    pub async fn read_by_command_id(
        &self,
        command_id: &CommandId,
    ) -> Result<Vec<OrchestrationV2StoredEvent>, SinkError> {
        let rows = self
            .events
            .read_agent_events(ReadAgentEvents {
                after_sequence: None,
                through_sequence: None,
                thread_id: None,
                command_id: Some(command_id.clone()),
                event_type: None,
                limit: None,
                skip_unknown_event_types: false,
            })
            .await
            .map_err(|cause| SinkError::Stream {
                thread_id: None,
                after_sequence: None,
                cause,
            })?;
        rows.iter()
            .map(|row| {
                stored_event_from_agent(row).map_err(|cause| SinkError::Stream {
                    thread_id: None,
                    after_sequence: None,
                    cause,
                })
            })
            .collect()
    }

    /// Replay from `after_sequence` up to the log's high-water mark at
    /// subscription time, then live events. The live subscription is taken
    /// before the high-water read, so an event committed in between is
    /// delivered by exactly one of the two halves. Internal workers use this:
    /// the live queue is unbounded and never drops events, and a type filter
    /// is applied before anything is queued.
    pub async fn stream(&self, options: StreamOptions) -> Result<SinkStream, SinkError> {
        let stream_error = |cause: PersistenceError| SinkError::Stream {
            thread_id: options.thread_id.clone(),
            after_sequence: options.after_sequence,
            cause,
        };
        let (subscription, rx) = self.live.subscribe(options.event_type.clone());
        let high_water = self
            .events
            .latest_agent_sequence(None)
            .await
            .map_err(stream_error)?;
        let after = options.after_sequence.unwrap_or(0);
        let state = StreamState {
            events: self.events.clone(),
            options,
            replay_after: after,
            high_water,
            live_after: high_water.max(after),
            replay_done: false,
            buffer: VecDeque::new(),
            rx,
            _subscription: subscription,
        };
        Ok(stream::unfold(state, StreamState::next).boxed())
    }
}

enum CommitOutcome {
    Replayed(CommandReceipt),
    Committed {
        receipt: CommandReceipt,
        appended: Appended,
        cancelled: Vec<String>,
    },
}

struct StreamState {
    events: OrchestrationEventStore,
    options: StreamOptions,
    replay_after: i64,
    high_water: i64,
    live_after: i64,
    replay_done: bool,
    buffer: VecDeque<OrchestrationV2StoredEvent>,
    rx: mpsc::UnboundedReceiver<Arc<LiveEvent>>,
    _subscription: LiveSubscription,
}

impl StreamState {
    fn matches(&self, thread_id: Option<&ThreadId>, kind: &str) -> bool {
        self.options
            .thread_id
            .as_ref()
            .is_none_or(|wanted| Some(wanted) == thread_id)
            && self.options.event_type.as_deref().is_none_or(|wanted| wanted == kind)
    }

    async fn next(mut self) -> Option<(Result<OrchestrationV2StoredEvent, SinkError>, Self)> {
        loop {
            if let Some(item) = self.buffer.pop_front() {
                return Some((Ok(item), self));
            }
            if !self.replay_done {
                // One page per turn (upstream `catchUp`): bounded memory for
                // a long replay.
                let page = self
                    .events
                    .read_agent_events(ReadAgentEvents {
                        after_sequence: Some(self.replay_after),
                        through_sequence: Some(self.high_water),
                        thread_id: self.options.thread_id.clone(),
                        command_id: None,
                        event_type: self.options.event_type.clone(),
                        limit: Some(CATCH_UP_PAGE_SIZE),
                        skip_unknown_event_types: false,
                    })
                    .await;
                let page = match page {
                    Ok(page) => page,
                    Err(cause) => {
                        self.replay_done = true;
                        self.rx.close();
                        let error = SinkError::Stream {
                            thread_id: self.options.thread_id.clone(),
                            after_sequence: self.options.after_sequence,
                            cause,
                        };
                        return Some((Err(error), self));
                    }
                };
                let page_len = page.len() as u64;
                let last = page.last().map(|row| row.sequence);
                for row in &page {
                    match stored_event_from_agent(row) {
                        Ok(stored) => self.buffer.push_back(stored),
                        Err(cause) => {
                            self.replay_done = true;
                            let error = SinkError::Stream {
                                thread_id: self.options.thread_id.clone(),
                                after_sequence: self.options.after_sequence,
                                cause,
                            };
                            return Some((Err(error), self));
                        }
                    }
                }
                match last {
                    Some(last) if page_len >= CATCH_UP_PAGE_SIZE && last < self.high_water => {
                        self.replay_after = last;
                    }
                    _ => self.replay_done = true,
                }
                continue;
            }
            let live = self.rx.recv().await?;
            if i64::try_from(live.stored.sequence).unwrap_or(i64::MAX) <= self.live_after
                || !self.matches(live.thread_id.as_ref(), &live.event_type)
            {
                continue;
            }
            return Some((Ok(live.stored.clone()), self));
        }
    }
}

fn latest_application_sequence_in(
    tx: &Transaction<'_>,
) -> Result<u64, PersistenceError> {
    let sequence: Option<i64> = tx
        .query_row(
            "SELECT MAX(sequence) AS sequence FROM orchestration_events INDEXED BY idx_orchestration_events_application_high_water WHERE aggregate_kind = 'project' OR (application_event_version = 2 AND aggregate_kind = 'thread')",
            [],
            |row| row.get(0),
        )
        .map_err(|err| PersistenceError::sql("EventSink.latest-application-sequence", &err))?;
    Ok(u64::try_from(sequence.unwrap_or(0)).unwrap_or(0))
}

fn status_text(status: &OrchestrationV2RunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn turn_item_run_id(item: &OrchestrationV2TurnItem) -> Option<&RunId> {
    macro_rules! run_id_of {
        ($($variant:ident),+ $(,)?) => {
            match item {
                $(OrchestrationV2TurnItem::$variant { run_id, .. } => run_id.as_ref(),)+
            }
        };
    }
    run_id_of!(
        Notification,
        UserMessage,
        AssistantMessage,
        Reasoning,
        ProposedPlan,
        TodoList,
        UserInputRequest,
        FileChange,
        CommandExecution,
        FileSearch,
        WebSearch,
        ApprovalRequest,
        Checkpoint,
        RunInterruptRequest,
        RunInterruptResult,
        SystemNotice,
        Error,
        Compaction,
        Handoff,
        Fork,
        ThreadCreated,
        SecretRequest,
        Subagent,
        DynamicTool,
    )
}

/// Mirrors `turin_persistence`'s private project-event insert and actor
/// inference exactly so a project command can append in the same transaction
/// as its receipt and projection row.
///
/// Deviation: persistence exposes only the async, self-transactional
/// `append_project_event`; a crate-public `append_project_event_in` would let
/// this copy go away (requested from the PM).
fn append_project_event_in(
    tx: &Transaction<'_>,
    event: &ProjectEventInput,
) -> Result<ProjectEvent, PersistenceError> {
    const OP: &str = "EventSink.project-event";
    if !matches!(
        event.event_type.as_str(),
        "project.created" | "project.meta-updated" | "project.deleted"
    ) {
        return Err(decode_err(
            OP,
            format!("unknown event type {}", event.event_type),
        ));
    }
    let actor = infer_actor_kind(event);
    let payload =
        serde_json::to_string(&event.payload).map_err(|err| decode_err(OP, err.to_string()))?;
    let metadata =
        serde_json::to_string(&event.metadata).map_err(|err| decode_err(OP, err.to_string()))?;
    let sequence: i64 = tx
        .query_row(
            "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json, application_event_version) VALUES (?1, 'project', ?2, COALESCE((SELECT stream_version + 1 FROM orchestration_events WHERE aggregate_kind = 'project' AND stream_id = ?2 ORDER BY stream_version DESC LIMIT 1), 0), ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 2) RETURNING sequence",
            params![
                event.event_id.as_str(),
                event.aggregate_id,
                event.event_type,
                event.occurred_at,
                event.command_id.as_ref().map(CommandId::as_str),
                event.causation_event_id.as_ref().map(EventId::as_str),
                event.correlation_id.as_ref().map(CommandId::as_str),
                actor,
                payload,
                metadata
            ],
            |row| row.get(0),
        )
        .map_err(|err| PersistenceError::sql(OP, &err))?;
    Ok(ProjectEvent {
        sequence,
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        aggregate_kind: AggregateKind::Project.as_str().to_owned(),
        aggregate_id: event.aggregate_id.clone(),
        occurred_at: event.occurred_at.clone(),
        command_id: event.command_id.clone(),
        causation_event_id: event.causation_event_id.clone(),
        correlation_id: event.correlation_id.clone(),
        payload: event.payload.clone(),
        metadata: event.metadata.clone(),
    })
}

fn infer_actor_kind(event: &ProjectEventInput) -> &'static str {
    if event
        .command_id
        .as_ref()
        .is_some_and(|id| id.as_str().starts_with("provider:"))
    {
        return "provider";
    }
    if event
        .command_id
        .as_ref()
        .is_some_and(|id| id.as_str().starts_with("server:"))
    {
        return "server";
    }
    if let Some(metadata) = event.metadata.as_object()
        && ["providerTurnId", "providerItemId", "adapterKey"]
            .iter()
            .any(|key| metadata.contains_key(*key))
    {
        return "provider";
    }
    if event.command_id.is_none() {
        "server"
    } else {
        "client"
    }
}

#[cfg(test)]
mod tests;
