//! Port of upstream/apps/server/src/orchestration-v2/EventSink.ts
//!
//! Transaction seam with T13a: its projection store implements
//! [ProjectionApplier]. The sink invokes it synchronously inside the same
//! `Db::write` transaction as event append, receipt upsert, and effect enqueue.
//! The trait must not perform async work. The sink publishes only after commit.

use super::effect_outbox::{EffectOutbox, PendingEffect};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;
use tokio::sync::Mutex;
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_contracts::orchestration_v2::OrchestrationV2ProviderThread;
use turin_contracts::{
    CommandId, EventId, NodeId, ProjectId, ProviderThreadId, RunAttemptId, RunId, ThreadId,
    UtcDateTime,
};
use turin_persistence::{
    AgentEvent, AggregateKind, AppendAgentEvents, ApplicationStoredEvent, Db,
    OrchestrationCommandReceipt, OrchestrationEventStore, PersistenceDecodeError, PersistenceError,
    ProjectEvent, ProjectEventInput, ReadAgentEvents, ReceiptStatus, StoredAgentEvent,
    get_by_command_id, insert_if_absent, upsert, utc_millis_now,
};

/// T13a's read side implements this against the same transaction connection.
/// `normalize_event` performs turn-item ordinal normalization; `apply_agent`
/// folds every stored event. Failed application aborts the event and receipt.
pub trait ProjectionApplier: Send + Sync {
    fn normalize_event(
        &self,
        conn: &Connection,
        event: AgentEvent,
        run_ordinal: Option<i64>,
    ) -> Result<AgentEvent, PersistenceError>;
    fn apply_agent(
        &self,
        conn: &Connection,
        event: &StoredAgentEvent,
    ) -> Result<(), PersistenceError>;
    fn apply_project(
        &self,
        conn: &Connection,
        event: &ProjectEvent,
    ) -> Result<(), PersistenceError>;
    fn schema_version(&self) -> i64 {
        2
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("Failed to write {event_count} orchestration V2 event(s).")]
    Write {
        event_count: usize,
        command_id: Option<CommandId>,
        #[source]
        cause: PersistenceError,
    },
    #[error("Command id {0} belongs to a different aggregate")]
    AggregateMismatch(CommandId),
    #[error("Command {0} produced no orchestration events")]
    EmptyCommand(CommandId),
}

#[derive(Debug, Clone)]
pub struct CommitResult {
    pub receipt: OrchestrationCommandReceipt,
    pub stored_events: Vec<StoredAgentEvent>,
    pub committed: bool,
    pub cancelled_effect_count: usize,
}

#[derive(Debug, Clone)]
pub struct ProjectCommitResult {
    pub receipt: OrchestrationCommandReceipt,
    pub committed: bool,
}

#[derive(Debug, Clone)]
pub struct ConditionalWriteResult {
    pub committed: bool,
    pub stored_events: Vec<StoredAgentEvent>,
}

#[derive(Debug, Clone)]
pub struct CancelUnsettled {
    pub effect_types: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct CommandCommit {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub command_type: String,
    pub accepted_at: UtcDateTime,
    pub events: Vec<AgentEvent>,
    pub effects: Vec<PendingEffect>,
    pub cancel_unsettled_effects: Option<CancelUnsettled>,
}

#[derive(Clone)]
pub struct EventSink {
    db: Db,
    events: OrchestrationEventStore,
    outbox: EffectOutbox,
    projections: Arc<dyn ProjectionApplier>,
    /// Acquired before writing and released after publish, preserving sequence
    /// order even if a later writer is scheduled immediately after commit.
    publish_lane: Arc<Mutex<()>>,
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
            publish_lane: Arc::new(Mutex::new(())),
        }
    }

    fn write_error(
        event_count: usize,
        command_id: Option<CommandId>,
        cause: PersistenceError,
    ) -> SinkError {
        SinkError::Write {
            event_count,
            command_id,
            cause,
        }
    }

    fn append_and_apply(
        &self,
        conn: &Connection,
        command_id: Option<CommandId>,
        events: Vec<AgentEvent>,
        effects: &[PendingEffect],
        guard_cancellations: bool,
    ) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
        let events = if guard_cancellations {
            guard_pending_user_input_cancellations(conn, events)?
        } else {
            events
        };
        let mut ordinals = std::collections::HashMap::new();
        for event in &events {
            if matches!(event.event_type.as_str(), "run.created" | "run.updated") {
                if let (Some(id), Some(ordinal)) = (
                    event.payload.get("id").and_then(|v| v.as_str()),
                    event.payload.get("ordinal").and_then(|v| v.as_i64()),
                ) {
                    ordinals.insert(id.to_owned(), ordinal);
                }
            }
        }
        let mut normalized = Vec::with_capacity(events.len());
        for event in events {
            if event.event_type == "turn-item.updated" {
                let run_ordinal = event
                    .payload
                    .get("runId")
                    .and_then(|v| v.as_str())
                    .and_then(|id| ordinals.get(id))
                    .copied();
                normalized.push(self.projections.normalize_event(conn, event, run_ordinal)?);
            } else {
                normalized.push(event);
            }
        }
        let stored = self.events.append_agent_events_in(
            conn,
            &AppendAgentEvents {
                command_id,
                events: normalized,
            },
        )?;
        for event in &stored {
            self.projections.apply_agent(conn, event)?;
        }
        if let Some(last) = stored.last() {
            conn.execute(
                "INSERT INTO orchestration_v2_projection_metadata (projection_name, schema_version, last_sequence, updated_at) VALUES ('thread-projections', ?1, ?2, ?3) ON CONFLICT(projection_name) DO UPDATE SET schema_version = excluded.schema_version, last_sequence = excluded.last_sequence, updated_at = excluded.updated_at",
                params![self.projections.schema_version(), last.sequence, utc_millis_now()],
            ).map_err(|err| PersistenceError::sql("EventSink.projection-metadata", &err))?;
        }
        EffectOutbox::enqueue_in(conn, effects)?;
        Ok(stored)
    }

    fn publish(&self, stored: &[StoredAgentEvent], effect_count: usize) {
        if effect_count > 0 {
            self.outbox.notify_available(effect_count);
        }
        if !stored.is_empty() {
            self.events.publish_committed(
                stored
                    .iter()
                    .cloned()
                    .map(ApplicationStoredEvent::Agent)
                    .collect(),
            );
        }
    }

    pub async fn write(
        &self,
        command_id: Option<CommandId>,
        events: Vec<AgentEvent>,
        guard_cancellations: bool,
    ) -> Result<Vec<StoredAgentEvent>, SinkError> {
        self.write_with_effects(command_id, events, vec![], guard_cancellations)
            .await
    }

    pub async fn write_with_effects(
        &self,
        command_id: Option<CommandId>,
        events: Vec<AgentEvent>,
        effects: Vec<PendingEffect>,
        guard_cancellations: bool,
    ) -> Result<Vec<StoredAgentEvent>, SinkError> {
        let _lane = self.publish_lane.lock().await;
        let sink = self.clone();
        let count = events.len();
        let effects_count = effects.len();
        let error_id = command_id.clone();
        let stored = self
            .db
            .write(move |tx| {
                sink.append_and_apply(tx, command_id, events, &effects, guard_cancellations)
            })
            .await
            .map_err(|cause| Self::write_error(count, error_id, cause))?;
        self.publish(&stored, effects_count);
        Ok(stored)
    }

    pub async fn write_domain_events(
        &self,
        command_id: Option<CommandId>,
        events: Vec<OrchestrationV2DomainEvent>,
        effects: Vec<PendingEffect>,
        guard_cancellations: bool,
    ) -> Result<Vec<StoredAgentEvent>, SinkError> {
        let count = events.len();
        let decoded = events
            .into_iter()
            .map(agent_event_from_domain)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|cause| Self::write_error(count, command_id.clone(), cause))?;
        self.write_with_effects(command_id, decoded, effects, guard_cancellations)
            .await
    }

    async fn write_conditional<F>(
        &self,
        command_id: Option<CommandId>,
        events: Vec<AgentEvent>,
        effects: Vec<PendingEffect>,
        guard_cancellations: bool,
        check: F,
    ) -> Result<ConditionalWriteResult, SinkError>
    where
        F: FnOnce(&Connection) -> Result<bool, PersistenceError> + Send + 'static,
    {
        let _lane = self.publish_lane.lock().await;
        let sink = self.clone();
        let count = events.len();
        let effect_count = effects.len();
        let error_id = command_id.clone();
        let result = self
            .db
            .write(move |tx| {
                if !check(tx)? {
                    return Ok(ConditionalWriteResult {
                        committed: false,
                        stored_events: vec![],
                    });
                }
                let stored =
                    sink.append_and_apply(tx, command_id, events, &effects, guard_cancellations)?;
                Ok(ConditionalWriteResult {
                    committed: true,
                    stored_events: stored,
                })
            })
            .await
            .map_err(|cause| Self::write_error(count, error_id, cause))?;
        if result.committed {
            self.publish(&result.stored_events, effect_count);
        }
        Ok(result)
    }

    pub async fn write_if_run_current(
        &self,
        command_id: Option<CommandId>,
        thread_id: ThreadId,
        run_id: RunId,
        active_attempt_id: RunAttemptId,
        expected_status: &str,
        events: Vec<AgentEvent>,
        effects: Vec<PendingEffect>,
        guard_cancellations: bool,
    ) -> Result<ConditionalWriteResult, SinkError> {
        let expected_status = expected_status.to_owned();
        self.write_conditional(command_id, events, effects, guard_cancellations, move |tx| {
            let current: Option<(String, Option<String>)> = tx.query_row(
                "SELECT status, json_extract(payload_json, '$.activeAttemptId') FROM orchestration_v2_projection_runs WHERE run_id = ?1 AND thread_id = ?2 LIMIT 1",
                params![run_id.as_str(), thread_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().map_err(|err| PersistenceError::sql("EventSink.write-if-run-current", &err))?;
            Ok(current.is_some_and(|(status, attempt)| status == expected_status && attempt.as_deref() == Some(active_attempt_id.as_str())))
        }).await
    }

    pub async fn write_if_provider_thread_owner(
        &self,
        command_id: Option<CommandId>,
        provider_thread_id: ProviderThreadId,
        run_id: RunId,
        active_attempt_id: RunAttemptId,
        expected_last_run_ordinal: i64,
        events: Vec<AgentEvent>,
        guard_cancellations: bool,
    ) -> Result<ConditionalWriteResult, SinkError> {
        self.write_conditional(command_id, events, vec![], guard_cancellations, move |tx| {
            let current: Option<(Option<String>, Option<i64>)> = tx.query_row(
                "SELECT json_extract(r.payload_json, '$.activeAttemptId'), p.last_run_ordinal FROM orchestration_v2_projection_provider_threads p JOIN orchestration_v2_projection_runs r ON r.run_id = ?1 AND r.thread_id = p.thread_id WHERE p.provider_thread_id = ?2 LIMIT 1",
                params![run_id.as_str(), provider_thread_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().map_err(|err| PersistenceError::sql("EventSink.write-if-provider-thread-owner", &err))?;
            Ok(current.is_some_and(|(attempt, ordinal)| attempt.as_deref() == Some(active_attempt_id.as_str()) && ordinal == Some(expected_last_run_ordinal)))
        }).await
    }

    pub async fn write_if_provider_thread_unchanged(
        &self,
        expected: OrchestrationV2ProviderThread,
        events: Vec<AgentEvent>,
    ) -> Result<ConditionalWriteResult, SinkError> {
        let expected_json = serde_json::to_string(&expected).map_err(|err| {
            Self::write_error(
                events.len(),
                None,
                decode_err("EventSink.provider-thread-json", &err.to_string()),
            )
        })?;
        self.write_conditional(None, events, vec![], false, move |tx| {
            let current: Option<String> = tx.query_row(
                "SELECT payload_json FROM orchestration_v2_projection_provider_threads WHERE provider_thread_id = ?1 LIMIT 1",
                [expected.id.as_str()], |row| row.get(0),
            ).optional().map_err(|err| PersistenceError::sql("EventSink.write-if-provider-thread-unchanged", &err))?;
            Ok(current.as_deref() == Some(expected_json.as_str()))
        }).await
    }

    pub async fn commit_command(&self, input: CommandCommit) -> Result<CommitResult, SinkError> {
        let _lane = self.publish_lane.lock().await;
        let sink = self.clone();
        let count = input.events.len();
        let id = input.command_id.clone();
        let effects_count = input.effects.len();
        let (receipt, stored_events, committed, cancelled) = self
            .db
            .write(move |tx| {
                let reserved = OrchestrationCommandReceipt {
                    command_id: input.command_id.clone(),
                    aggregate_kind: AggregateKind::Thread,
                    aggregate_id: input.thread_id.to_string(),
                    command_type: input.command_type.clone(),
                    accepted_at: input.accepted_at.to_string(),
                    result_sequence: 0,
                    status: ReceiptStatus::Accepted,
                    error: None,
                };
                if !insert_if_absent(tx, &reserved)? {
                    let existing = get_by_command_id(tx, &input.command_id)?.ok_or_else(|| {
                        decode_err("EventSink.commit-command", "receipt disappeared")
                    })?;
                    if existing.aggregate_kind != AggregateKind::Thread {
                        return Err(decode_err(
                            "EventSink.commit-command",
                            "command id belongs to project",
                        ));
                    }
                    return Ok((existing, vec![], false, vec![]));
                }
                let stored = sink.append_and_apply(
                    tx,
                    Some(input.command_id.clone()),
                    input.events,
                    &input.effects,
                    false,
                )?;
                let Some(last) = stored.last() else {
                    return Err(decode_err(
                        "EventSink.commit-command",
                        "command produced no events",
                    ));
                };
                let receipt = OrchestrationCommandReceipt {
                    result_sequence: last.sequence,
                    ..reserved
                };
                upsert(tx, &receipt)?;
                let cancelled = match input.cancel_unsettled_effects {
                    Some(cancel) => EffectOutbox::cancel_unsettled_in(
                        tx,
                        &input.thread_id,
                        &cancel.effect_types,
                        &cancel.reason,
                    )?,
                    None => vec![],
                };
                Ok((receipt, stored, true, cancelled))
            })
            .await
            .map_err(|cause| Self::write_error(count, Some(id.clone()), cause))?;
        let stored_events = if committed {
            stored_events
        } else {
            self.events
                .read_agent_events(ReadAgentEvents {
                    after_sequence: None,
                    through_sequence: None,
                    thread_id: None,
                    command_id: Some(id.clone()),
                    event_type: None,
                    limit: None,
                    skip_unknown_event_types: false,
                })
                .await
                .map_err(|cause| Self::write_error(count, Some(id.clone()), cause))?
        };
        self.outbox.signal_cancellations(&cancelled);
        if committed {
            self.publish(&stored_events, effects_count);
        }
        Ok(CommitResult {
            receipt,
            stored_events,
            committed,
            cancelled_effect_count: cancelled.len(),
        })
    }

    pub async fn commit_rejected_command(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
        command_type: String,
        error: String,
        rejected_at: UtcDateTime,
    ) -> Result<OrchestrationCommandReceipt, SinkError> {
        let id = command_id.clone();
        self.db.write(move |tx| {
            let sequence: Option<i64> = tx.query_row(
                "SELECT MAX(sequence) FROM orchestration_events WHERE application_event_version = 2 AND aggregate_kind = 'thread' AND stream_id = ?1",
                [thread_id.as_str()], |row| row.get(0),
            ).map_err(|err| PersistenceError::sql("EventSink.reject-sequence", &err))?;
            let receipt = OrchestrationCommandReceipt {
                command_id: command_id.clone(), aggregate_kind: AggregateKind::Thread,
                aggregate_id: thread_id.to_string(), command_type, accepted_at: rejected_at.to_string(),
                result_sequence: sequence.unwrap_or(0), status: ReceiptStatus::Rejected, error: Some(error),
            };
            if insert_if_absent(tx, &receipt)? { Ok(receipt) }
            else { get_by_command_id(tx, &command_id)?.ok_or_else(|| decode_err("EventSink.reject", "receipt disappeared")) }
        }).await.map_err(|cause| Self::write_error(0, Some(id), cause))
    }

    pub async fn commit_project_command(
        &self,
        command_id: CommandId,
        project_id: ProjectId,
        command_type: String,
        accepted_at: UtcDateTime,
        event: ProjectEventInput,
    ) -> Result<ProjectCommitResult, SinkError> {
        let _lane = self.publish_lane.lock().await;
        let id = command_id.clone();
        let projections = Arc::clone(&self.projections);
        let (receipt, committed_event) = self
            .db
            .write(move |tx| {
                if event.aggregate_id != project_id.as_str() {
                    return Err(decode_err(
                        "EventSink.project-command",
                        "event aggregate does not match project",
                    ));
                }
                let reserved = OrchestrationCommandReceipt {
                    command_id: command_id.clone(),
                    aggregate_kind: AggregateKind::Project,
                    aggregate_id: project_id.to_string(),
                    command_type,
                    accepted_at: accepted_at.to_string(),
                    result_sequence: 0,
                    status: ReceiptStatus::Accepted,
                    error: None,
                };
                if !insert_if_absent(tx, &reserved)? {
                    let existing = get_by_command_id(tx, &command_id)?.ok_or_else(|| {
                        decode_err("EventSink.project-command", "receipt disappeared")
                    })?;
                    if existing.aggregate_kind != AggregateKind::Project {
                        return Err(decode_err(
                            "EventSink.project-command",
                            "command id belongs to thread",
                        ));
                    }
                    return Ok((existing, None));
                }
                let stored = append_project_event_in(tx, &event)?;
                projections.apply_project(tx, &stored)?;
                let receipt = OrchestrationCommandReceipt {
                    result_sequence: stored.sequence,
                    ..reserved
                };
                upsert(tx, &receipt)?;
                Ok((receipt, Some(stored)))
            })
            .await
            .map_err(|cause| Self::write_error(1, Some(id), cause))?;
        if let Some(event) = &committed_event {
            self.events
                .publish_committed(vec![ApplicationStoredEvent::Project(event.clone())]);
        }
        Ok(ProjectCommitResult {
            receipt,
            committed: committed_event.is_some(),
        })
    }

    pub async fn commit_rejected_project_command(
        &self,
        command_id: CommandId,
        project_id: ProjectId,
        command_type: String,
        error: String,
        rejected_at: UtcDateTime,
    ) -> Result<OrchestrationCommandReceipt, SinkError> {
        let id = command_id.clone();
        self.db.write(move |tx| {
            let sequence: Option<i64> = tx.query_row(
                "SELECT MAX(sequence) FROM orchestration_events WHERE aggregate_kind = 'project' OR (application_event_version = 2 AND aggregate_kind = 'thread')",
                [], |row| row.get(0),
            ).map_err(|err| PersistenceError::sql("EventSink.reject-project-sequence", &err))?;
            let receipt = OrchestrationCommandReceipt {
                command_id: command_id.clone(), aggregate_kind: AggregateKind::Project,
                aggregate_id: project_id.to_string(), command_type, accepted_at: rejected_at.to_string(),
                result_sequence: sequence.unwrap_or(0), status: ReceiptStatus::Rejected, error: Some(error),
            };
            if insert_if_absent(tx, &receipt)? { Ok(receipt) }
            else {
                let existing = get_by_command_id(tx, &command_id)?.ok_or_else(|| decode_err("EventSink.reject-project", "receipt disappeared"))?;
                if existing.aggregate_kind != AggregateKind::Project { return Err(decode_err("EventSink.reject-project", "command id belongs to thread")); }
                Ok(existing)
            }
        }).await.map_err(|cause| Self::write_error(0, Some(id), cause))
    }
}

fn append_project_event_in(
    conn: &Connection,
    event: &ProjectEventInput,
) -> Result<ProjectEvent, PersistenceError> {
    if !matches!(
        event.event_type.as_str(),
        "project.created" | "project.meta-updated" | "project.deleted"
    ) {
        return Err(decode_err(
            "EventSink.project-event",
            "unknown project event type",
        ));
    }
    let actor = if event
        .command_id
        .as_ref()
        .is_some_and(|id| id.as_str().starts_with("provider:"))
        || event.metadata.as_object().is_some_and(|map| {
            ["providerTurnId", "providerItemId", "adapterKey"]
                .iter()
                .any(|key| map.contains_key(*key))
        }) {
        "provider"
    } else if event
        .command_id
        .as_ref()
        .is_some_and(|id| id.as_str().starts_with("server:"))
        || event.command_id.is_none()
    {
        "server"
    } else {
        "client"
    };
    let payload = serde_json::to_string(&event.payload)
        .map_err(|err| decode_err("EventSink.project-event", &err.to_string()))?;
    let metadata = serde_json::to_string(&event.metadata)
        .map_err(|err| decode_err("EventSink.project-event", &err.to_string()))?;
    let sequence: i64 = conn.query_row(
        "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json, application_event_version) VALUES (?1, 'project', ?2, COALESCE((SELECT stream_version + 1 FROM orchestration_events WHERE aggregate_kind = 'project' AND stream_id = ?2 ORDER BY stream_version DESC LIMIT 1), 0), ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 2) RETURNING sequence",
        params![event.event_id.as_str(), event.aggregate_id, event.event_type, event.occurred_at,
            event.command_id.as_ref().map(CommandId::as_str), event.causation_event_id.as_ref().map(EventId::as_str),
            event.correlation_id.as_ref().map(CommandId::as_str), actor, payload, metadata],
        |row| row.get(0),
    ).map_err(|err| PersistenceError::sql("EventSink.project-event", &err))?;
    Ok(ProjectEvent {
        sequence,
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        aggregate_kind: "project".into(),
        aggregate_id: event.aggregate_id.clone(),
        occurred_at: event.occurred_at.clone(),
        command_id: event.command_id.clone(),
        causation_event_id: event.causation_event_id.clone(),
        correlation_id: event.correlation_id.clone(),
        payload: event.payload.clone(),
        metadata: event.metadata.clone(),
    })
}

fn decode_err(operation: &str, issue: &str) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(operation, issue))
}

pub fn agent_event_from_domain(
    event: OrchestrationV2DomainEvent,
) -> Result<AgentEvent, PersistenceError> {
    let value = serde_json::to_value(event)
        .map_err(|err| decode_err("EventSink.domain-event", &err.to_string()))?;
    let field = |name: &str| {
        value
            .get(name)
            .cloned()
            .ok_or_else(|| decode_err("EventSink.domain-event", &format!("missing {name}")))
    };
    let parse = |name: &str| -> Result<String, PersistenceError> {
        field(name)?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| decode_err("EventSink.domain-event", &format!("invalid {name}")))
    };
    let parse_optional = |name: &str| -> Option<String> {
        value.get(name).and_then(|v| v.as_str()).map(str::to_owned)
    };
    Ok(AgentEvent {
        id: EventId::new(&parse("id")?)
            .map_err(|err| decode_err("EventSink.domain-event", &err.to_string()))?,
        thread_id: ThreadId::new(&parse("threadId")?)
            .map_err(|err| decode_err("EventSink.domain-event", &err.to_string()))?,
        event_type: parse("type")?,
        occurred_at: serde_json::from_value(field("occurredAt")?)
            .map_err(|err| decode_err("EventSink.domain-event", &err.to_string()))?,
        payload: field("payload")?,
        run_id: parse_optional("runId")
            .map(|id| {
                RunId::new(&id)
                    .map_err(|err| decode_err("EventSink.domain-event", &err.to_string()))
            })
            .transpose()?,
        node_id: parse_optional("nodeId")
            .map(|id| {
                NodeId::new(&id)
                    .map_err(|err| decode_err("EventSink.domain-event", &err.to_string()))
            })
            .transpose()?,
        driver: parse_optional("driver"),
        provider_instance_id: parse_optional("providerInstanceId"),
        raw_event_id: parse_optional("rawEventId")
            .map(|id| {
                turin_contracts::RawEventId::new(&id)
                    .map_err(|err| decode_err("EventSink.domain-event", &err.to_string()))
            })
            .transpose()?,
    })
}

fn guard_pending_user_input_cancellations(
    conn: &Connection,
    events: Vec<AgentEvent>,
) -> Result<Vec<AgentEvent>, PersistenceError> {
    let mut stale_requests = std::collections::HashSet::new();
    let mut stale_nodes = std::collections::HashSet::new();
    for event in &events {
        if event.event_type != "runtime-request.updated"
            || event.payload.get("kind").and_then(|v| v.as_str()) != Some("user_input")
            || event.payload.get("status").and_then(|v| v.as_str()) != Some("cancelled")
        {
            continue;
        }
        let Some(id) = event.payload.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let current: Option<String> = conn.query_row(
            "SELECT payload_json FROM orchestration_v2_projection_runtime_requests WHERE runtime_request_id = ?1 AND thread_id = ?2",
            params![id, event.thread_id.as_str()], |row| row.get(0),
        ).optional().map_err(|err| PersistenceError::sql("EventSink.guard-cancellation", &err))?;
        let valid = current
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .is_some_and(|request| {
                request.get("status").and_then(|v| v.as_str()) == Some("pending")
                    && request.get("kind").and_then(|v| v.as_str()) == Some("user_input")
                    && request.get("providerTurnId") == event.payload.get("providerTurnId")
                    && request
                        .get("responseCapability")
                        .and_then(|v| v.get("type"))
                        .and_then(|v| v.as_str())
                        != Some("message")
            });
        if !valid {
            stale_requests.insert(id.to_owned());
            if let Some(node_id) = event.payload.get("nodeId").and_then(|v| v.as_str()) {
                stale_nodes.insert(node_id.to_owned());
            }
        }
    }
    Ok(events
        .into_iter()
        .filter(|event| match event.event_type.as_str() {
            "runtime-request.updated" => {
                event.payload.get("status").and_then(|v| v.as_str()) != Some("cancelled")
                    || !event
                        .payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| stale_requests.contains(id))
            }
            "node.updated" => {
                event.payload.get("status").and_then(|v| v.as_str()) != Some("cancelled")
                    || !event
                        .payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| stale_nodes.contains(id))
            }
            "turn-item.updated" => {
                !(event.payload.get("type").and_then(|v| v.as_str()) == Some("user_input_request")
                    && event.payload.get("status").and_then(|v| v.as_str()) == Some("cancelled")
                    && event
                        .payload
                        .get("requestId")
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| stale_requests.contains(id)))
            }
            _ => true,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestProjection {
        fail: bool,
    }
    impl ProjectionApplier for TestProjection {
        fn normalize_event(
            &self,
            _conn: &Connection,
            event: AgentEvent,
            _ordinal: Option<i64>,
        ) -> Result<AgentEvent, PersistenceError> {
            Ok(event)
        }
        fn apply_agent(
            &self,
            conn: &Connection,
            event: &StoredAgentEvent,
        ) -> Result<(), PersistenceError> {
            if self.fail {
                return Err(decode_err("TestProjection.apply", "projection failed"));
            }
            conn.execute(
                "INSERT INTO test_projection (event_id) VALUES (?1)",
                [event.event.id.as_str()],
            )
            .map_err(|err| PersistenceError::sql("TestProjection.apply", &err))?;
            Ok(())
        }
        fn apply_project(
            &self,
            _conn: &Connection,
            _event: &ProjectEvent,
        ) -> Result<(), PersistenceError> {
            Ok(())
        }
    }

    fn make_event(id: &str, thread: &ThreadId) -> AgentEvent {
        AgentEvent {
            id: EventId::new(id).unwrap(),
            thread_id: thread.clone(),
            event_type: "thread.created".into(),
            occurred_at: UtcDateTime::now(),
            payload: serde_json::json!({"id":thread.as_str()}),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
        }
    }
    async fn setup(fail: bool) -> (Db, EventSink) {
        let db = Db::open_in_memory().await.unwrap();
        db.write(|conn| {
            conn.execute_batch("CREATE TABLE test_projection (event_id TEXT PRIMARY KEY);")
                .map_err(|err| PersistenceError::sql("test.setup", &err))
        })
        .await
        .unwrap();
        let events = OrchestrationEventStore::new(db.clone());
        let outbox = EffectOutbox::new(db.clone());
        let sink = EventSink::new(events, outbox, Arc::new(TestProjection { fail }));
        (db, sink)
    }

    #[tokio::test]
    async fn command_receipt_event_projection_and_effect_commit_together() {
        let (db, sink) = setup(false).await;
        let command_id = CommandId::new("cmd:sink:test").unwrap();
        let thread_id = ThreadId::new("thread:sink:test").unwrap();
        let effect = PendingEffect {
            id: "effect:sink:test".into(),
            command_id: command_id.clone(),
            thread_id: thread_id.clone(),
            request: serde_json::json!({"type":"terminal.cleanup"}),
            available_at: None,
        };
        let input = CommandCommit {
            command_id: command_id.clone(),
            thread_id: thread_id.clone(),
            command_type: "thread.create".into(),
            accepted_at: UtcDateTime::now(),
            events: vec![make_event("event:sink:test", &thread_id)],
            effects: vec![effect],
            cancel_unsettled_effects: None,
        };
        let first = sink.commit_command(input.clone()).await.unwrap();
        assert!(first.committed);
        assert!(first.receipt.result_sequence > 0);
        assert_eq!(first.stored_events.len(), 1);
        let repeated = sink.commit_command(input).await.unwrap();
        assert!(!repeated.committed);
        assert_eq!(repeated.receipt, first.receipt);
        assert_eq!(repeated.stored_events, first.stored_events);
        let counts = db
            .read(|conn| {
                let one = |table: &str| -> Result<i64, PersistenceError> {
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .map_err(|err| PersistenceError::sql("test.count", &err))
                };
                Ok((
                    one("orchestration_events")?,
                    one("orchestration_command_receipts")?,
                    one("orchestration_v2_effect_outbox")?,
                    one("test_projection")?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(counts, (1, 1, 1, 1));
    }

    #[tokio::test]
    async fn projection_failure_rolls_back_receipt_event_and_effect() {
        let (db, sink) = setup(true).await;
        let command_id = CommandId::new("cmd:sink:rollback").unwrap();
        let thread_id = ThreadId::new("thread:sink:rollback").unwrap();
        let result = sink
            .commit_command(CommandCommit {
                command_id,
                thread_id: thread_id.clone(),
                command_type: "thread.create".into(),
                accepted_at: UtcDateTime::now(),
                events: vec![make_event("event:sink:rollback", &thread_id)],
                effects: vec![],
                cancel_unsettled_effects: None,
            })
            .await;
        assert!(result.is_err());
        let counts = db
            .read(|conn| {
                let events: i64 = conn
                    .query_row("SELECT COUNT(*) FROM orchestration_events", [], |row| {
                        row.get(0)
                    })
                    .map_err(|err| PersistenceError::sql("test.count", &err))?;
                let receipts: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM orchestration_command_receipts",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|err| PersistenceError::sql("test.count", &err))?;
                Ok((events, receipts))
            })
            .await
            .unwrap();
        assert_eq!(counts, (0, 0));
    }
}
