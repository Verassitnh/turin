//! Conversions between the typed domain events services build and the
//! untyped rows `turin_persistence` stores (see PORTING.md Deviations:
//! persistence keeps `serde_json::Value` payloads).

use serde_json::{Map, Value, json};
use turin_contracts::orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2StoredEvent};
use turin_contracts::{EventId, NodeId, RawEventId, RunId, ThreadId, UtcDateTime};
use turin_persistence::{AgentEvent, PersistenceDecodeError, PersistenceError, StoredAgentEvent};

fn decode_err(issue: impl Into<String>) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(
        "EventSink.domain-event",
        issue,
    ))
}

fn string_field(value: &Value, name: &str) -> Result<String, PersistenceError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| decode_err(format!("missing {name}")))
}

fn optional_string_field(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(Value::as_str).map(str::to_owned)
}

/// The event as a stored row: the shared envelope fields lifted out, the rest
/// as the payload.
pub fn agent_event_from_domain(
    event: &OrchestrationV2DomainEvent,
) -> Result<AgentEvent, PersistenceError> {
    let value = serde_json::to_value(event).map_err(|err| decode_err(err.to_string()))?;
    let id = EventId::new(string_field(&value, "id")?).map_err(|err| decode_err(err.to_string()))?;
    let thread_id = ThreadId::new(string_field(&value, "threadId")?)
        .map_err(|err| decode_err(err.to_string()))?;
    let occurred_at: UtcDateTime = string_field(&value, "occurredAt")?
        .parse()
        .map_err(|err: turin_contracts::ContractError| decode_err(err.to_string()))?;
    let run_id = optional_string_field(&value, "runId")
        .map(|id| RunId::new(id).map_err(|err| decode_err(err.to_string())))
        .transpose()?;
    let node_id = optional_string_field(&value, "nodeId")
        .map(|id| NodeId::new(id).map_err(|err| decode_err(err.to_string())))
        .transpose()?;
    let raw_event_id = optional_string_field(&value, "rawEventId")
        .map(|id| RawEventId::new(id).map_err(|err| decode_err(err.to_string())))
        .transpose()?;
    Ok(AgentEvent {
        id,
        thread_id,
        event_type: string_field(&value, "type")?,
        occurred_at,
        payload: value.get("payload").cloned().unwrap_or(Value::Null),
        run_id,
        node_id,
        driver: optional_string_field(&value, "driver"),
        provider_instance_id: optional_string_field(&value, "providerInstanceId"),
        raw_event_id,
    })
}

/// Inverse of [`agent_event_from_domain`].
pub fn domain_event_from_agent(
    event: &AgentEvent,
) -> Result<OrchestrationV2DomainEvent, PersistenceError> {
    let mut object = Map::new();
    object.insert("type".into(), json!(event.event_type));
    object.insert("id".into(), json!(event.id.as_str()));
    object.insert("threadId".into(), json!(event.thread_id.as_str()));
    if let Some(run_id) = &event.run_id {
        object.insert("runId".into(), json!(run_id.as_str()));
    }
    if let Some(node_id) = &event.node_id {
        object.insert("nodeId".into(), json!(node_id.as_str()));
    }
    if let Some(driver) = &event.driver {
        object.insert("driver".into(), json!(driver));
    }
    if let Some(instance) = &event.provider_instance_id {
        object.insert("providerInstanceId".into(), json!(instance));
    }
    if let Some(raw) = &event.raw_event_id {
        object.insert("rawEventId".into(), json!(raw.as_str()));
    }
    object.insert("occurredAt".into(), json!(event.occurred_at.to_string()));
    object.insert("payload".into(), event.payload.clone());
    serde_json::from_value(Value::Object(object)).map_err(|err| decode_err(err.to_string()))
}

pub fn stored_event_from_agent(
    stored: &StoredAgentEvent,
) -> Result<OrchestrationV2StoredEvent, PersistenceError> {
    Ok(OrchestrationV2StoredEvent {
        sequence: u64::try_from(stored.sequence).map_err(|_| decode_err("negative sequence"))?,
        command_id: stored.command_id.clone(),
        event: domain_event_from_agent(&stored.event)?,
    })
}

/// The `type` literal of a domain event.
pub fn event_type(event: &OrchestrationV2DomainEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| optional_string_field(&value, "type"))
        .unwrap_or_default()
}

/// The thread a domain event belongs to.
pub fn event_thread_id(event: &OrchestrationV2DomainEvent) -> Option<ThreadId> {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| optional_string_field(&value, "threadId"))
        .and_then(|id| ThreadId::new(id).ok())
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Event builders shared by this crate's tests.

    use super::*;
    use turin_contracts::orchestration_v2::OrchestrationV2AppThread;

    pub fn thread_json(thread_id: &str) -> Value {
        json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": thread_id,
            "projectId": format!("project:{thread_id}"),
            "title": format!("Thread {thread_id}"),
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "activeProviderThreadId": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": thread_id},
            "forkedFrom": null,
            "createdAt": "2026-06-21T00:00:00.000Z",
            "updatedAt": "2026-06-21T00:00:00.000Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "lastVisitedAt": null,
            "deletedAt": null,
        })
    }

    pub fn thread(thread_id: &str) -> OrchestrationV2AppThread {
        serde_json::from_value(thread_json(thread_id)).expect("test thread decodes")
    }

    pub fn thread_event(kind: &str, event_id: &str, thread_id: &str) -> OrchestrationV2DomainEvent {
        serde_json::from_value(json!({
            "type": kind,
            "id": event_id,
            "threadId": thread_id,
            "providerInstanceId": "codex",
            "occurredAt": "2026-06-21T00:00:00.000Z",
            "payload": thread_json(thread_id),
        }))
        .expect("test event decodes")
    }

    pub fn thread_created(event_id: &str, thread_id: &str) -> OrchestrationV2DomainEvent {
        thread_event("thread.created", event_id, thread_id)
    }

    pub fn metadata_updated(
        event_id: &str,
        thread_id: &str,
        title: &str,
    ) -> OrchestrationV2DomainEvent {
        let mut payload = thread_json(thread_id);
        payload["title"] = json!(title);
        serde_json::from_value(json!({
            "type": "thread.metadata-updated",
            "id": event_id,
            "threadId": thread_id,
            "providerInstanceId": "codex",
            "occurredAt": "2026-06-21T00:00:00.000Z",
            "payload": payload,
        }))
        .expect("test event decodes")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn agent_row_round_trips_a_domain_event() {
        let event = thread_created("event:round-trip", "thread:round-trip");
        let row = agent_event_from_domain(&event).unwrap();
        assert_eq!(row.event_type, "thread.created");
        assert_eq!(row.thread_id.as_str(), "thread:round-trip");
        assert_eq!(row.provider_instance_id.as_deref(), Some("codex"));
        assert_eq!(row.payload["title"], "Thread thread:round-trip");
        assert_eq!(domain_event_from_agent(&row).unwrap(), event);
        assert_eq!(event_type(&event), "thread.created");
        assert_eq!(
            event_thread_id(&event).unwrap().as_str(),
            "thread:round-trip"
        );
    }
}
