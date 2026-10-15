//! Port of upstream/apps/server/src/orchestration-v2/EventStore.ts
//!
//! This facade maps the v2 service to the shared application event log.

use turin_contracts::{CommandId, ThreadId};
use turin_persistence::{AgentEvent, AppendAgentEvents, ApplicationStoredEvent, OrchestrationEventStore, PersistenceError, ProjectEvent, ProjectEventInput, ReadAgentEvents, StoredAgentEvent};

#[derive(Clone)]
pub struct EventStore { inner: OrchestrationEventStore }
impl EventStore {
    pub fn new(inner: OrchestrationEventStore) -> Self { Self { inner } }
    pub fn persistence(&self) -> &OrchestrationEventStore { &self.inner }
    pub async fn append(&self, command_id: Option<CommandId>, events: Vec<AgentEvent>) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
        self.inner.append_agent_events(AppendAgentEvents { command_id, events }).await
    }
    pub async fn append_project_event(&self, event: ProjectEventInput) -> Result<ProjectEvent, PersistenceError> {
        self.inner.append_project_event(event).await
    }
    pub async fn read(&self, query: ReadAgentEvents) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
        self.inner.read_agent_events(query).await
    }
    pub async fn read_by_command_id(&self, command_id: CommandId) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
        self.read(ReadAgentEvents { after_sequence: None, through_sequence: None, thread_id: None,
            command_id: Some(command_id), event_type: None, limit: None, skip_unknown_event_types: false }).await
    }
    pub async fn latest_sequence(&self, thread_id: Option<ThreadId>) -> Result<i64, PersistenceError> {
        self.inner.latest_agent_sequence(thread_id).await
    }
    pub async fn latest_application_sequence(&self) -> Result<i64, PersistenceError> {
        self.inner.latest_application_sequence().await
    }
    pub fn publish_committed(&self, events: Vec<ApplicationStoredEvent>) {
        self.inner.publish_committed(events);
    }
}
