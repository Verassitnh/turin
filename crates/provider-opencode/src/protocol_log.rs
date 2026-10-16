//! Port of `makeOpenCodeProtocolLogger` and `formatOpenCodeProtocolLogPayload` in
//! `upstream/packages/provider-opencode/src/server/adapter.ts`, with the
//! structural summary from provider-core's `nativeProtocolLogging.ts`.
//!
//! Upstream writes these lines through an injected NDJSON logger. Here the seam is
//! [`ProtocolLog`]; [`NdjsonProtocolLog`] is the file sink. The summary keeps
//! lengths, counts and method names, never the payload values, because prompts and
//! tool output pass through these requests.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};

/// The SDK protocol name upstream stamps on each line (`OPENCODE_SDK_PROTOCOL`).
pub const OPENCODE_SDK_PROTOCOL: &str = "opencode-sdk.sse";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMessageKind {
    Request,
    Response,
    Notification,
    Error,
}

#[derive(Debug, Clone)]
pub struct ProtocolEvent {
    pub direction: ProtocolDirection,
    pub message_kind: ProtocolMessageKind,
    /// The operation name, such as `session.create`. Sanitised by the sink.
    pub method: String,
    pub payload: Value,
}

/// Where protocol events go. Recording must not fail or block the adapter.
pub trait ProtocolLog: Send + Sync {
    fn record(&self, event: ProtocolEvent);
}

/// `structuralProtocolMethod`: a method name that is short and identifier-like is kept,
/// anything else becomes `unknown`.
pub fn structural_protocol_method(value: &str) -> String {
    let well_formed = value.len() <= 128
        && value.starts_with(|c: char| c.is_ascii_alphabetic())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'));
    if well_formed {
        value.to_owned()
    } else {
        "unknown".to_owned()
    }
}

/// `summarizeNativeProtocolPayload`: structure only.
pub fn summarize_payload(payload: &Value) -> Value {
    match payload {
        Value::Null => json!({ "valueType": "null" }),
        Value::String(text) => json!({ "valueType": "string", "byteLength": text.len() }),
        Value::Array(items) => json!({ "valueType": "array", "itemCount": items.len() }),
        Value::Object(fields) => {
            let mut summary = json!({ "valueType": "object", "fieldCount": fields.len() });
            if let Some(tag) = fields.get("tag").and_then(Value::as_str) {
                summary["method"] = json!(structural_protocol_method(tag));
            }
            summary
        }
        Value::Bool(_) => json!({ "valueType": "boolean" }),
        Value::Number(_) => json!({ "valueType": "number" }),
    }
}

/// Appends one JSON object per line to a file. Write errors are dropped, as
/// upstream's logger catches them, so a full disk never stops a turn.
pub struct NdjsonProtocolLog {
    file: Mutex<File>,
}

impl NdjsonProtocolLog {
    pub fn open(path: &Path) -> std::io::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Arc::new(Self {
            file: Mutex::new(file),
        }))
    }

    /// The line this sink writes for `event`, without the newline.
    pub fn line(event: &ProtocolEvent, observed_at: &str) -> String {
        json!({
            "observedAt": observed_at,
            "event": {
                "kind": "protocol",
                "protocol": OPENCODE_SDK_PROTOCOL,
                "provider": "opencode",
                "createdAt": observed_at,
                "payload": {
                    "direction": match event.direction {
                        ProtocolDirection::Incoming => "incoming",
                        ProtocolDirection::Outgoing => "outgoing",
                    },
                    "messageKind": match event.message_kind {
                        ProtocolMessageKind::Request => "request",
                        ProtocolMessageKind::Response => "response",
                        ProtocolMessageKind::Notification => "notification",
                        ProtocolMessageKind::Error => "error",
                    },
                    "method": structural_protocol_method(&event.method),
                    "payload": summarize_payload(&event.payload),
                },
            },
        })
        .to_string()
    }
}

impl ProtocolLog for NdjsonProtocolLog {
    fn record(&self, event: ProtocolEvent) {
        let observed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut line = Self::line(&event, &observed_at);
        line.push('\n');
        let _ = self.file.lock().write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_names_are_kept_only_when_identifier_like() {
        assert_eq!(
            structural_protocol_method("session.create"),
            "session.create"
        );
        assert_eq!(structural_protocol_method("bad method!"), "unknown");
        assert_eq!(structural_protocol_method("1starts-with-digit"), "unknown");
        assert_eq!(structural_protocol_method(&"a".repeat(129)), "unknown");
    }

    #[test]
    fn the_summary_has_structure_and_no_values() {
        let secret = "sk-live-do-not-log";
        let summary = summarize_payload(&json!({"prompt": secret, "tag": "message.updated"}));
        let text = summary.to_string();
        assert!(!text.contains(secret), "{text}");
        assert_eq!(summary["fieldCount"], 2);
        assert_eq!(summary["method"], "message.updated");
        assert_eq!(summarize_payload(&json!("abc"))["byteLength"], 3);
        assert_eq!(summarize_payload(&json!([1, 2]))["itemCount"], 2);
    }

    #[test]
    fn a_line_carries_the_upstream_envelope_and_a_summary() {
        let event = ProtocolEvent {
            direction: ProtocolDirection::Outgoing,
            message_kind: ProtocolMessageKind::Request,
            method: "session.prompt".to_owned(),
            payload: json!({"text": "hello"}),
        };
        let line: Value =
            serde_json::from_str(&NdjsonProtocolLog::line(&event, "2026-10-15T12:00:00.000Z"))
                .unwrap();
        assert_eq!(line["event"]["protocol"], OPENCODE_SDK_PROTOCOL);
        assert_eq!(line["event"]["payload"]["direction"], "outgoing");
        assert_eq!(line["event"]["payload"]["messageKind"], "request");
        assert_eq!(line["event"]["payload"]["method"], "session.prompt");
        assert_eq!(line["event"]["payload"]["payload"]["fieldCount"], 1);
        assert!(!line.to_string().contains("hello"));
    }

    #[test]
    fn the_file_sink_appends_one_line_per_event() {
        let dir = std::env::temp_dir().join(format!(
            "turin-oc-protocol-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("opencode.ndjson");
        let log = NdjsonProtocolLog::open(&path).unwrap();
        for method in ["session.create", "session.prompt"] {
            log.record(ProtocolEvent {
                direction: ProtocolDirection::Outgoing,
                message_kind: ProtocolMessageKind::Request,
                method: method.to_owned(),
                payload: Value::Null,
            });
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(
            text.lines()
                .all(|line| serde_json::from_str::<Value>(line).is_ok())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
