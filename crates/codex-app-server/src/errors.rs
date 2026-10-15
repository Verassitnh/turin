//! Port of upstream/packages/effect-codex-app-server/src/errors.ts
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProtocolError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ProtocolError {
    pub fn parse_error(message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data,
        }
    }
    pub fn invalid_request(message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data,
        }
    }
    pub fn invalid_params(message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data,
        }
    }
    pub fn internal_error(message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data,
        }
    }
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {method}"),
            data: None,
        }
    }
    pub fn overloaded() -> Self {
        Self {
            code: -32001,
            message: "Too many Codex requests are already active.".into(),
            data: None,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum AppServerError {
    #[error("failed to spawn Codex App Server process: {0}")]
    Spawn(String),
    #[error("Codex App Server process exited with code {0:?}")]
    ProcessExited(Option<i32>),
    #[error("Codex App Server protocol operation '{operation}' failed: {detail}")]
    ProtocolParse {
        operation: &'static str,
        detail: String,
    },
    #[error("Codex App Server transport operation '{operation}' failed: {detail}")]
    Transport {
        operation: &'static str,
        detail: String,
    },
    #[error("Codex App Server request {method} ({request_id}) failed: {error:?}")]
    Request {
        method: String,
        request_id: String,
        error: Box<ProtocolError>,
    },
    #[error("Codex App Server input stream ended")]
    InputStreamEnded,
    #[error("failed to generate Codex App Server identifier for {purpose}: {detail}")]
    IdentifierGeneration { purpose: String, detail: String },
    #[error("invalid Codex App Server payload for {method}: {detail}")]
    InvalidPayload { method: String, detail: String },
    #[error("Codex App Server request was cancelled")]
    Cancelled,
}
