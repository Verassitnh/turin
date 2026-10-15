//! Port of upstream/apps/server/src/orchestration-v2/EventStore.ts
//!
//! This facade maps the v2 service to the shared application event log. The
//! sink composes appends inside its own transaction through
//! `OrchestrationEventStore::append_agent_events_in`; these async methods are
//! for callers outside a write.

use super::domain_event::{agent_event_from_domain, stored_event_from_agent};
use turin_contracts::orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2StoredEvent};
use turin_contracts::{CommandId, ThreadId};
use turin_persistence::{
    AppendAgentEvents, ApplicationStoredEvent, OrchestrationEventStore, PersistenceError,
    ProjectEvent, ProjectEventInput, ReadAgentEvents,
};

/// `EventStoreAppendEventsError` / `EventStoreReadEventsError`.
#[derive(Debug, thiserror::Error)]
pub enum EventStoreError {
    #[error("Failed to append {event_count} agent orchestration event(s).")]
    Append {
        event_count: usize,
        #[source]
        cause: PersistenceError,
    },
    #[error("{}", read_message(.thread_id))]
    Read {
        after_sequence: Option<i64>,
        thread_id: Option<ThreadId>,
        #[source]
        cause: PersistenceError,
    },
}

fn read_message(thread_id: &Option<ThreadId>) -> String {
    match thread_id {
        None => "Failed to read agent orchestration events.".to_owned(),
        Some(id) => format!("Failed to read agent orchestration events for thread {id}."),
    }
}

/// `read` options.
#[derive(Debug, Clone, Default)]
pub struct ReadOptions {
    pub after_sequence: Option<i64>,
    pub through_sequence: Option<i64>,
    pub thread_id: Option<ThreadId>,
    pub event_type: Option<String>,
    pub limit: Option<u64>,
}

#[derive(Clone)]
pub struct EventStore {
    inner: OrchestrationEventStore,
}

impl EventStore {
    pub fn new(inner: OrchestrationEventStore) -> Self {
        Self { inner }
    }

    pub fn persistence(&self) -> &OrchestrationEventStore {
        &self.inner
    }

    pub async fn append(
        &self,
        command_id: Option<CommandId>,
        events: Vec<OrchestrationV2DomainEvent>,
    ) -> Result<Vec<OrchestrationV2StoredEvent>, EventStoreError> {
        let event_count = events.len();
        let append_error = |cause| EventStoreError::Append { event_count, cause };
        let rows = events
            .iter()
            .map(agent_event_from_domain)
            .collect::<Result<Vec<_>, _>>()
            .map_err(append_error)?;
        let stored = self
            .inner
            .append_agent_events(AppendAgentEvents {
                command_id,
                events: rows,
            })
            .await
            .map_err(append_error)?;
        stored
            .iter()
            .map(stored_event_from_agent)
            .collect::<Result<_, _>>()
            .map_err(append_error)
    }

    pub async fn append_project_event(
        &self,
        event: ProjectEventInput,
    ) -> Result<ProjectEvent, EventStoreError> {
        self.inner
            .append_project_event(event)
            .await
            .map_err(|cause| EventStoreError::Append {
                event_count: 1,
                cause,
            })
    }

    pub async fn read(
        &self,
        options: ReadOptions,
    ) -> Result<Vec<OrchestrationV2StoredEvent>, EventStoreError> {
        let read_error = |cause| EventStoreError::Read {
            after_sequence: options.after_sequence,
            thread_id: options.thread_id.clone(),
            cause,
        };
        let rows = self
            .inner
            .read_agent_events(ReadAgentEvents {
                after_sequence: options.after_sequence,
                through_sequence: options.through_sequence,
                thread_id: options.thread_id.clone(),
                command_id: None,
                event_type: options.event_type.clone(),
                limit: options.limit,
                skip_unknown_event_types: false,
            })
            .await
            .map_err(read_error)?;
        rows.iter()
            .map(stored_event_from_agent)
            .collect::<Result<_, _>>()
            .map_err(read_error)
    }

    pub async fn read_by_command_id(
        &self,
        command_id: CommandId,
    ) -> Result<Vec<OrchestrationV2StoredEvent>, EventStoreError> {
        let read_error = |cause| EventStoreError::Read {
            after_sequence: None,
            thread_id: None,
            cause,
        };
        let rows = self
            .inner
            .read_agent_events(ReadAgentEvents {
                after_sequence: None,
                through_sequence: None,
                thread_id: None,
                command_id: Some(command_id),
                event_type: None,
                limit: None,
                skip_unknown_event_types: false,
            })
            .await
            .map_err(read_error)?;
        rows.iter()
            .map(stored_event_from_agent)
            .collect::<Result<_, _>>()
            .map_err(read_error)
    }

    pub async fn latest_sequence(
        &self,
        thread_id: Option<ThreadId>,
    ) -> Result<i64, EventStoreError> {
        let for_error = thread_id.clone();
        self.inner
            .latest_agent_sequence(thread_id)
            .await
            .map_err(|cause| EventStoreError::Read {
                after_sequence: None,
                thread_id: for_error,
                cause,
            })
    }

    /// Latest sequence across project and V2 thread events.
    pub async fn latest_application_sequence(&self) -> Result<i64, EventStoreError> {
        self.inner
            .latest_application_sequence()
            .await
            .map_err(|cause| EventStoreError::Read {
                after_sequence: None,
                thread_id: None,
                cause,
            })
    }

    pub fn publish_committed(&self, events: Vec<ApplicationStoredEvent>) {
        self.inner.publish_committed(events);
    }
}
