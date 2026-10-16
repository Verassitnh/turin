//! Port of upstream/packages/provider-acp/src/server/nativeLogging.ts
//!
//! Bounded native NDJSON diagnostics for ACP request and protocol traffic.

use std::sync::Arc;

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use turin_contracts::ThreadId;
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_provider_core::native_protocol_logging::{
    structural_protocol_method, summarize_native_protocol_payload,
};
use turin_provider_core::provider_event_loggers::EventNdjsonLogger;
use turin_provider_core::rt::{BoxFuture, boxed};
use uuid::Uuid;

use crate::session_runtime::{RequestLogEvent, RequestLogStatus};

const TRANSIENT_PROTOCOL_UPDATES: [&str; 2] = ["agent_message_chunk", "agent_thought_chunk"];

/// Port of upstream `EffectAcpProtocol.AcpProtocolLogEvent`.
#[derive(Debug, Clone)]
pub struct AcpProtocolLogEvent {
    pub direction: AcpProtocolLogDirection,
    pub stage: AcpProtocolLogStage,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpProtocolLogDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpProtocolLogStage {
    Raw,
    Decoded,
    DecodeFailed,
}

/// Upstream `AcpSessionRequestLogEvent` failure metadata (Effect `Cause`).
#[derive(Debug, Clone)]
pub struct AcpSessionRequestLogFailure {
    pub error_tag: String,
    pub reason_count: usize,
}

/// Upstream `AcpSessionRequestLogEvent`.
#[derive(Debug, Clone)]
pub struct AcpSessionRequestLogEvent {
    pub method: String,
    pub payload: Value,
    pub status: RequestLogStatus,
    pub result: Option<Value>,
    pub failure: Option<AcpSessionRequestLogFailure>,
}

#[derive(Clone)]
pub struct AcpProtocolLoggingOptions {
    pub log_incoming: bool,
    pub log_outgoing: bool,
    pub logger: Arc<dyn Fn(AcpProtocolLogEvent) -> BoxFuture<'static, ()> + Send + Sync>,
}

#[derive(Clone)]
pub struct AcpNativeLoggerHooks {
    pub request_logger:
        Option<Arc<dyn Fn(AcpSessionRequestLogEvent) -> BoxFuture<'static, ()> + Send + Sync>>,
    pub protocol_logging: Option<AcpProtocolLoggingOptions>,
}

pub struct MakeAcpNativeLoggerInput {
    pub native_event_logger: Option<EventNdjsonLogger>,
    pub provider: ProviderDriverKind,
    pub thread_id: ThreadId,
    pub verbose_protocol_logging: bool,
    /// When set, writer defects are reported here instead of only `tracing::warn!`.
    pub diagnostic_warning_sink: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}

fn format_request_log_payload(event: &AcpSessionRequestLogEvent) -> Value {
    let mut payload = json!({
        "method": structural_protocol_method(&event.method),
        "status": request_status_wire(event.status),
        "request": summarize_native_protocol_payload(&event.payload),
    });
    if let Some(result) = &event.result {
        payload["result"] = summarize_native_protocol_payload(result);
    }
    if let Some(failure) = &event.failure {
        payload["errorTag"] = json!(failure.error_tag);
        payload["reasonCount"] = json!(failure.reason_count);
    }
    payload
}

fn request_status_wire(status: RequestLogStatus) -> &'static str {
    match status {
        RequestLogStatus::Started => "started",
        RequestLogStatus::Succeeded => "succeeded",
        RequestLogStatus::Failed => "failed",
    }
}

fn format_acp_protocol_log_payload(event: &AcpProtocolLogEvent) -> Value {
    json!({
        "direction": protocol_direction_wire(event.direction),
        "stage": protocol_stage_wire(event.stage),
        "payload": summarize_native_protocol_payload(&event.payload),
    })
}

fn protocol_direction_wire(direction: AcpProtocolLogDirection) -> &'static str {
    match direction {
        AcpProtocolLogDirection::Incoming => "incoming",
        AcpProtocolLogDirection::Outgoing => "outgoing",
    }
}

fn protocol_stage_wire(stage: AcpProtocolLogStage) -> &'static str {
    match stage {
        AcpProtocolLogStage::Raw => "raw",
        AcpProtocolLogStage::Decoded => "decoded",
        AcpProtocolLogStage::DecodeFailed => "decode_failed",
    }
}

fn is_transient_protocol_message(message: &Value) -> bool {
    let Some(record) = message.as_object() else {
        return false;
    };
    let method = record
        .get("tag")
        .or_else(|| record.get("method"))
        .and_then(Value::as_str);
    if method != Some("session/update") {
        return false;
    }
    let payload = record
        .get("payload")
        .or_else(|| record.get("params"))
        .and_then(Value::as_object);
    let Some(payload) = payload else {
        return false;
    };
    let update = payload.get("update").and_then(Value::as_object);
    let Some(update) = update else {
        return false;
    };
    update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .is_some_and(|update_type| TRANSIENT_PROTOCOL_UPDATES.contains(&update_type))
}

fn raw_chunk_contains_only_transient_messages(payload: &str) -> bool {
    let mut lines: Vec<&str> = payload.split('\n').collect();
    let remainder = lines.pop().unwrap_or("");
    if !remainder.trim().is_empty() {
        return false;
    }
    let mut messages = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        messages.push(message);
    }
    !messages.is_empty() && messages.iter().all(is_transient_protocol_message)
}

fn filter_transient_protocol_log(event: AcpProtocolLogEvent) -> Option<AcpProtocolLogEvent> {
    if event.direction != AcpProtocolLogDirection::Incoming {
        return Some(event);
    }
    if event.stage == AcpProtocolLogStage::Raw
        && let Some(payload) = event.payload.as_str()
    {
        return if raw_chunk_contains_only_transient_messages(payload) {
            None
        } else {
            Some(event)
        };
    }
    if event.stage != AcpProtocolLogStage::Decoded {
        return Some(event);
    }
    if let Some(messages) = event.payload.as_array() {
        let payload: Vec<Value> = messages
            .iter()
            .filter(|message| !is_transient_protocol_message(message))
            .cloned()
            .collect();
        return if payload.is_empty() {
            None
        } else {
            Some(AcpProtocolLogEvent {
                payload: Value::Array(payload),
                ..event
            })
        };
    }
    if is_transient_protocol_message(&event.payload) {
        None
    } else {
        Some(event)
    }
}

fn log_writer_failure(input: &MakeAcpNativeLoggerInput, error_tag: &str, reason_count: usize) {
    let fields = json!({
        "errorTag": error_tag,
        "reasonCount": reason_count,
        "provider": input.provider.as_str(),
        "threadId": input.thread_id.as_str(),
    });
    if let Some(sink) = &input.diagnostic_warning_sink {
        sink(fields);
    } else {
        tracing::warn!(target: "turin_acp::native_logging", ?fields, "Failed to write native ACP event log.");
    }
}

async fn write_native_acp_log(input: &MakeAcpNativeLoggerInput, kind: &str, payload: Value) {
    let Some(logger) = input.native_event_logger.as_ref() else {
        return;
    };
    let observed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let envelope = json!({
        "observedAt": observed_at,
        "event": {
            "id": Uuid::new_v4().to_string(),
            "kind": kind,
            "provider": input.provider.as_str(),
            "createdAt": observed_at,
            "threadId": input.thread_id.as_str(),
            "payload": payload,
        },
    });
    let write = (logger.write)(envelope, Some(input.thread_id.clone()));
    match tokio::spawn(write).await {
        Ok(()) => {}
        Err(join_error) if join_error.is_panic() => log_writer_failure(input, "Die", 1),
        Err(_) => {}
    }
}

/// Upstream `makeAcpNativeLoggerFactory`.
pub fn make_acp_native_logger_hooks(input: MakeAcpNativeLoggerInput) -> AcpNativeLoggerHooks {
    let input = Arc::new(input);
    let request_input = Arc::clone(&input);
    let request_logger = input.native_event_logger.as_ref().map(|_| {
        Arc::new(move |event: AcpSessionRequestLogEvent| {
            let input = Arc::clone(&request_input);
            boxed(async move {
                let payload = format_request_log_payload(&event);
                write_native_acp_log(&input, "request", payload).await;
            })
        }) as Arc<dyn Fn(AcpSessionRequestLogEvent) -> BoxFuture<'static, ()> + Send + Sync>
    });

    let protocol_logging = input.native_event_logger.as_ref().and_then(|_| {
        if !input.verbose_protocol_logging {
            return None;
        }
        let protocol_input = Arc::clone(&input);
        Some(AcpProtocolLoggingOptions {
            log_incoming: true,
            log_outgoing: true,
            logger: Arc::new(move |event: AcpProtocolLogEvent| {
                let input = Arc::clone(&protocol_input);
                boxed(async move {
                    let Some(filtered) = filter_transient_protocol_log(event) else {
                        return;
                    };
                    let payload = format_acp_protocol_log_payload(&filtered);
                    write_native_acp_log(&input, "protocol", payload).await;
                })
            }),
        })
    });

    AcpNativeLoggerHooks {
        request_logger,
        protocol_logging,
    }
}

/// Bridges session-runtime [`RequestLogEvent`] hooks to native NDJSON logging.
pub fn request_logger_from_hooks(
    hooks: &AcpNativeLoggerHooks,
) -> Option<Arc<dyn Fn(RequestLogEvent) + Send + Sync>> {
    let logger = hooks.request_logger.as_ref()?.clone();
    Some(Arc::new(move |event: RequestLogEvent| {
        let logger = Arc::clone(&logger);
        let failure = event
            .error
            .as_ref()
            .map(|error| AcpSessionRequestLogFailure {
                error_tag: if error.contains("AcpRequestError") {
                    "AcpRequestError".into()
                } else {
                    "Error".into()
                },
                reason_count: 1,
            });
        let mapped = AcpSessionRequestLogEvent {
            method: event.method,
            payload: event.payload,
            status: event.status,
            result: event.result,
            failure,
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(logger(mapped));
        } else {
            futures::executor::block_on(logger(mapped));
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use turin_contracts::provider_instance::ProviderDriverKind;

    fn mock_logger(
        write: Arc<dyn Fn(Value, Option<ThreadId>) -> BoxFuture<'static, ()> + Send + Sync>,
    ) -> EventNdjsonLogger {
        EventNdjsonLogger {
            file_path: "/tmp/provider-native.ndjson".into(),
            write,
            close: Arc::new(|| boxed(async {})),
        }
    }

    fn cursor() -> ProviderDriverKind {
        ProviderDriverKind::new("cursor").expect("cursor")
    }

    fn thread() -> ThreadId {
        ThreadId::new("thread-1").expect("thread")
    }

    #[tokio::test]
    async fn records_bounded_request_and_protocol_diagnostics_without_raw_payloads() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_write = Arc::clone(&records);
        let hooks = make_acp_native_logger_hooks(MakeAcpNativeLoggerInput {
            native_event_logger: Some(mock_logger(Arc::new(move |event, _thread| {
                records_write.lock().expect("lock").push(event);
                boxed(async {})
            }))),
            provider: cursor(),
            thread_id: thread(),
            verbose_protocol_logging: true,
            diagnostic_warning_sink: None,
        });
        let request_logger = hooks.request_logger.as_ref().expect("request logger");
        let protocol_logger = hooks
            .protocol_logging
            .as_ref()
            .expect("protocol logging")
            .logger
            .clone();
        let secret = "secret-token-value";

        request_logger(AcpSessionRequestLogEvent {
            method: "session/prompt".into(),
            payload: json!({ "prompt": secret, "sessionId": secret }),
            status: RequestLogStatus::Failed,
            result: None,
            failure: Some(AcpSessionRequestLogFailure {
                error_tag: "AcpRequestError".into(),
                reason_count: 1,
            }),
        })
        .await;

        protocol_logger(AcpProtocolLogEvent {
            direction: AcpProtocolLogDirection::Incoming,
            stage: AcpProtocolLogStage::Raw,
            payload: json!(format!("{{\"token\":\"{secret}\"}}")),
        })
        .await;

        protocol_logger(AcpProtocolLogEvent {
            direction: AcpProtocolLogDirection::Outgoing,
            stage: AcpProtocolLogStage::Decoded,
            payload: json!({
                "_tag": "Request",
                "tag": "session/prompt",
                "payload": { "prompt": secret },
            }),
        })
        .await;

        let serialized = serde_json::to_string(&*records.lock().expect("lock")).expect("serialize");
        assert!(!serialized.contains(secret));
        assert!(serialized.contains("\"method\":\"session/prompt\""));
        assert!(serialized.contains("\"errorTag\":\"AcpRequestError\""));
        assert!(serialized.contains("\"reasonCount\":1"));
        assert!(serialized.contains("\"valueType\":\"string\""));
        assert!(serialized.contains("\"messageTag\":\"Request\""));
    }

    #[tokio::test]
    async fn keeps_request_diagnostics_without_enabling_full_protocol_logging() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_write = Arc::clone(&records);
        let hooks = make_acp_native_logger_hooks(MakeAcpNativeLoggerInput {
            native_event_logger: Some(mock_logger(Arc::new(move |event, _thread| {
                records_write.lock().expect("lock").push(event);
                boxed(async {})
            }))),
            provider: ProviderDriverKind::new("grok").expect("grok"),
            thread_id: thread(),
            verbose_protocol_logging: false,
            diagnostic_warning_sink: None,
        });
        assert!(hooks.protocol_logging.is_none());
        let request_logger = hooks.request_logger.as_ref().expect("request logger");
        request_logger(AcpSessionRequestLogEvent {
            method: "session/prompt".into(),
            payload: json!({}),
            status: RequestLogStatus::Started,
            result: None,
            failure: None,
        })
        .await;
        assert_eq!(records.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn drops_transient_acp_chunks_before_formatting_verbose_protocol_logs() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_write = Arc::clone(&records);
        let hooks = make_acp_native_logger_hooks(MakeAcpNativeLoggerInput {
            native_event_logger: Some(mock_logger(Arc::new(move |event, _thread| {
                records_write.lock().expect("lock").push(event);
                boxed(async {})
            }))),
            provider: cursor(),
            thread_id: thread(),
            verbose_protocol_logging: true,
            diagnostic_warning_sink: None,
        });
        let protocol_logger = hooks
            .protocol_logging
            .as_ref()
            .expect("protocol logging")
            .logger
            .clone();

        for update_type in ["agent_message_chunk", "agent_thought_chunk"] {
            protocol_logger(AcpProtocolLogEvent {
                direction: AcpProtocolLogDirection::Incoming,
                stage: AcpProtocolLogStage::Raw,
                payload: json!(format!(
                    "{{\"method\":\"session/update\",\"params\":{{\"update\":{{\"sessionUpdate\":\"{update_type}\"}}}}}}\n"
                )),
            })
            .await;
            protocol_logger(AcpProtocolLogEvent {
                direction: AcpProtocolLogDirection::Incoming,
                stage: AcpProtocolLogStage::Decoded,
                payload: json!([{
                    "_tag": "Request",
                    "tag": "session/update",
                    "payload": { "update": { "sessionUpdate": update_type } },
                }]),
            })
            .await;
        }
        assert_eq!(records.lock().expect("lock").len(), 0);

        protocol_logger(AcpProtocolLogEvent {
            direction: AcpProtocolLogDirection::Incoming,
            stage: AcpProtocolLogStage::Decoded,
            payload: json!([{
                "_tag": "Request",
                "tag": "session/update",
                "payload": { "update": { "sessionUpdate": "tool_call" } },
            }]),
        })
        .await;
        assert_eq!(records.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn keeps_mixed_and_incomplete_raw_diagnostics() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_write = Arc::clone(&records);
        let hooks = make_acp_native_logger_hooks(MakeAcpNativeLoggerInput {
            native_event_logger: Some(mock_logger(Arc::new(move |event, _thread| {
                records_write.lock().expect("lock").push(event);
                boxed(async {})
            }))),
            provider: cursor(),
            thread_id: thread(),
            verbose_protocol_logging: true,
            diagnostic_warning_sink: None,
        });
        let protocol_logger = hooks
            .protocol_logging
            .as_ref()
            .expect("protocol logging")
            .logger
            .clone();

        let transient = json!({
            "method": "session/update",
            "params": { "update": { "sessionUpdate": "agent_message_chunk" } },
        });
        let lifecycle = json!({ "method": "session/new", "params": {} });

        protocol_logger(AcpProtocolLogEvent {
            direction: AcpProtocolLogDirection::Incoming,
            stage: AcpProtocolLogStage::Raw,
            payload: json!(format!(
                "{}\n{}\n",
                serde_json::to_string(&transient).expect("transient"),
                serde_json::to_string(&lifecycle).expect("lifecycle"),
            )),
        })
        .await;

        protocol_logger(AcpProtocolLogEvent {
            direction: AcpProtocolLogDirection::Incoming,
            stage: AcpProtocolLogStage::Raw,
            payload: json!(serde_json::to_string(&transient).expect("transient")),
        })
        .await;

        protocol_logger(AcpProtocolLogEvent {
            direction: AcpProtocolLogDirection::Incoming,
            stage: AcpProtocolLogStage::Raw,
            payload: json!(format!(
                "{}\n{{malformed}}\n",
                serde_json::to_string(&transient).expect("transient"),
            )),
        })
        .await;

        assert_eq!(records.lock().expect("lock").len(), 3);
    }

    #[tokio::test]
    async fn filters_transient_entries_from_mixed_decoded_batches() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_write = Arc::clone(&records);
        let hooks = make_acp_native_logger_hooks(MakeAcpNativeLoggerInput {
            native_event_logger: Some(mock_logger(Arc::new(move |event, _thread| {
                records_write.lock().expect("lock").push(event);
                boxed(async {})
            }))),
            provider: ProviderDriverKind::new("grok").expect("grok"),
            thread_id: thread(),
            verbose_protocol_logging: true,
            diagnostic_warning_sink: None,
        });
        let protocol_logger = hooks
            .protocol_logging
            .as_ref()
            .expect("protocol logging")
            .logger
            .clone();

        protocol_logger(AcpProtocolLogEvent {
            direction: AcpProtocolLogDirection::Incoming,
            stage: AcpProtocolLogStage::Decoded,
            payload: json!([
                {
                    "_tag": "Request",
                    "tag": "session/update",
                    "payload": { "update": { "sessionUpdate": "agent_thought_chunk" } },
                },
                {
                    "_tag": "Request",
                    "tag": "session/new",
                    "payload": {},
                },
            ]),
        })
        .await;

        let serialized = serde_json::to_string(&*records.lock().expect("lock")).expect("serialize");
        assert_eq!(records.lock().expect("lock").len(), 1);
        assert!(serialized.contains("\"itemCount\":1"));
    }

    #[tokio::test]
    async fn logs_structural_tag_when_the_native_writer_defects() {
        let secret = "secret-writer-failure";
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warnings_sink = Arc::clone(&warnings);
        let hooks = make_acp_native_logger_hooks(MakeAcpNativeLoggerInput {
            native_event_logger: Some(mock_logger(Arc::new(move |_event, _thread| {
                let secret = secret.to_owned();
                boxed(async move {
                    panic!("{secret}");
                })
            }))),
            provider: cursor(),
            thread_id: thread(),
            verbose_protocol_logging: false,
            diagnostic_warning_sink: Some(Arc::new(move |value| {
                warnings_sink.lock().expect("lock").push(value);
            })),
        });
        let request_logger = hooks.request_logger.as_ref().expect("request logger");
        request_logger(AcpSessionRequestLogEvent {
            method: "session/prompt".into(),
            payload: json!({}),
            status: RequestLogStatus::Started,
            result: None,
            failure: None,
        })
        .await;

        let serialized =
            serde_json::to_string(&*warnings.lock().expect("lock")).expect("serialize");
        assert!(!serialized.contains(secret));
        assert!(serialized.contains("\"errorTag\":\"Die\""));
        assert!(serialized.contains("\"reasonCount\":1"));
    }

    #[tokio::test]
    async fn preserves_native_writer_interruption() {
        let hooks = make_acp_native_logger_hooks(MakeAcpNativeLoggerInput {
            native_event_logger: Some(mock_logger(Arc::new(|_event, _thread| {
                boxed(async { std::future::pending::<()>().await })
            }))),
            provider: cursor(),
            thread_id: thread(),
            verbose_protocol_logging: false,
            diagnostic_warning_sink: None,
        });
        let request_logger = hooks
            .request_logger
            .as_ref()
            .expect("request logger")
            .clone();
        let join = tokio::spawn(async move {
            request_logger(AcpSessionRequestLogEvent {
                method: "session/prompt".into(),
                payload: json!({}),
                status: RequestLogStatus::Started,
                result: None,
                failure: None,
            })
            .await;
        });
        join.abort();
        assert!(join.await.is_err());
    }
}
