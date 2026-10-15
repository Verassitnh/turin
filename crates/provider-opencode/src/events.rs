//! SSE event envelopes for both OpenCode generations.
//!
//! v1 frames are `{id?, type, properties}`. v2 frames are
//! `{id, created?, type, durable?, location?, metadata?, data}`. `durable` is
//! the server's aggregate cursor object when the event is stored, not a
//! boolean. A frame this build cannot read as that envelope is kept only when
//! it is an execution start or end with `data.sessionID`, matching
//! `OpenCode2Client.ts`.

use serde::Deserialize;
use serde_json::Value;

use crate::sse::SseFrame;

const EXECUTION_ENDS: [&str; 3] = [
    "session.execution.succeeded",
    "session.execution.failed",
    "session.execution.interrupted",
];

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct V1Event {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub properties: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct V2Event {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<serde_json::Number>,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default = "empty_object")]
    pub data: Value,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Debug, Clone, PartialEq)]
pub enum V2StreamEvent {
    Event(Box<V2Event>),
    /// The execution ended, but the frame was not a full envelope.
    UnreadableExecutionEnded {
        execution_type: String,
        session_id: String,
    },
    /// The execution started, but the frame was not a full envelope.
    UnreadableExecutionStarted {
        session_id: String,
    },
}

pub fn decode_v1_event(frame: &SseFrame) -> Option<V1Event> {
    if frame.data.is_empty() {
        return None;
    }
    serde_json::from_str(&frame.data).ok()
}

pub fn decode_v2_event(frame: &SseFrame) -> Option<V2StreamEvent> {
    if frame.data.is_empty() {
        return None;
    }
    if let Ok(event) = serde_json::from_str::<V2Event>(&frame.data) {
        return Some(V2StreamEvent::Event(Box::new(event)));
    }
    let envelope: MinEnvelope = serde_json::from_str(&frame.data).ok()?;
    let event_type = envelope.event_type.chars().take(80).collect::<String>();
    let session_id = envelope.data.and_then(|data| data.session_id);
    if is_execution_end(&event_type) {
        let session_id = session_id?;
        tracing::warn!(
            event_type = %event_type,
            "Ended an OpenCode execution from an event this build cannot decode."
        );
        return Some(V2StreamEvent::UnreadableExecutionEnded {
            execution_type: event_type,
            session_id,
        });
    }
    if event_type == "session.execution.started" {
        let session_id = session_id?;
        tracing::debug!("Read an OpenCode execution start this build cannot decode.");
        return Some(V2StreamEvent::UnreadableExecutionStarted { session_id });
    }
    tracing::debug!(event_type = %event_type, "Skipped an OpenCode event this build cannot decode.");
    None
}

fn is_execution_end(event_type: &str) -> bool {
    EXECUTION_ENDS.contains(&event_type)
}

#[derive(Deserialize)]
struct MinEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    data: Option<MinData>,
}

#[derive(Deserialize)]
struct MinData {
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_event_uses_properties() {
        let frame = SseFrame {
            data: r#"{"id":"1","type":"server.connected","properties":{}}"#.to_string(),
            event: None,
            id: None,
        };
        let event = decode_v1_event(&frame).unwrap();
        assert_eq!(event.event_type, "server.connected");
        assert_eq!(event.properties, Value::Object(serde_json::Map::new()));
    }

    #[test]
    fn v2_event_keeps_data_and_optional_durable_object() {
        let frame = SseFrame {
            data: r#"{"id":"e1","created":10,"type":"session.execution.started","durable":{"aggregateID":"s","seq":1,"version":1},"data":{"sessionID":"s"}}"#.to_string(),
            event: None,
            id: None,
        };
        match decode_v2_event(&frame).unwrap() {
            V2StreamEvent::Event(event) => {
                assert_eq!(event.event_type, "session.execution.started");
                assert!(event.durable.is_some());
                assert_eq!(event.data["sessionID"], "s");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn server_connected_does_not_require_created() {
        let frame = SseFrame {
            data: r#"{"id":"e1","type":"server.connected","data":{}}"#.to_string(),
            event: None,
            id: None,
        };
        match decode_v2_event(&frame).unwrap() {
            V2StreamEvent::Event(event) => assert!(event.created.is_none()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_execution_end_still_ends_the_turn() {
        let frame = SseFrame {
            data: r#"{"type":"session.execution.failed","data":{"sessionID":"sess"}}"#.to_string(),
            event: None,
            id: None,
        };
        match decode_v2_event(&frame).unwrap() {
            V2StreamEvent::UnreadableExecutionEnded {
                execution_type,
                session_id,
            } => {
                assert_eq!(execution_type, "session.execution.failed");
                assert_eq!(session_id, "sess");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn other_unreadable_frames_are_skipped() {
        let frame = SseFrame {
            data: r#"{"type":"session.step.streamed"}"#.to_string(),
            event: None,
            id: None,
        };
        assert!(decode_v2_event(&frame).is_none());
    }
}
