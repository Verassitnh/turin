//! Port of upstream/packages/effect-codex-app-server/src/errors.ts
//! Also: Port of upstream/packages/effect-codex-app-server/src/_internal/shared.ts.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

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

#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolParseInfo {
    pub operation: &'static str,
    pub detail: String,
    pub method: Option<String>,
    pub request_id: Option<String>,
    pub payload_kind: Option<&'static str>,
    pub present_fields: Option<Vec<&'static str>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppServerError {
    Spawn(String),
    ProcessExited(Option<i32>),
    ProtocolParse(Box<ProtocolParseInfo>),
    Transport {
        operation: &'static str,
        detail: String,
    },
    Request {
        method: String,
        request_id: String,
        error: Box<ProtocolError>,
    },
    InputStreamEnded,
    IdentifierGeneration {
        purpose: String,
        detail: String,
    },
    InvalidPayload {
        method: String,
        detail: String,
    },
    Cancelled,
}

impl AppServerError {
    pub(crate) fn protocol_parse(operation: &'static str, detail: impl Into<String>) -> Self {
        Self::ProtocolParse(Box::new(ProtocolParseInfo {
            operation,
            detail: detail.into(),
            method: None,
            request_id: None,
            payload_kind: None,
            present_fields: None,
        }))
    }

    /// JSON-RPC `error.message` when this is a peer request failure.
    pub fn request_message(&self) -> Option<&str> {
        match self {
            Self::Request { error, .. } => Some(error.message.as_str()),
            _ => None,
        }
    }
}

impl fmt::Display for AppServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(detail) => {
                write!(f, "failed to spawn Codex App Server process: {detail}")
            }
            Self::ProcessExited(code) => {
                write!(f, "Codex App Server process exited with code {code:?}")
            }
            Self::ProtocolParse(info) => {
                let operation = info.operation;
                let method = info.method.as_deref();
                write!(
                    f,
                    "Codex App Server protocol operation '{operation}' failed"
                )?;
                if let Some(method) = method {
                    write!(f, " for method '{method}'")?;
                }
                f.write_str(".")
            }
            Self::Transport { operation, detail } => write!(
                f,
                "Codex App Server transport operation '{operation}' failed: {detail}"
            ),
            Self::Request {
                method,
                request_id,
                error,
            } => write!(
                f,
                "Codex App Server request {method} ({request_id}) failed: {error:?}"
            ),
            Self::InputStreamEnded => f.write_str("Codex App Server input stream ended"),
            Self::IdentifierGeneration { purpose, detail } => write!(
                f,
                "failed to generate Codex App Server identifier for {purpose}: {detail}"
            ),
            Self::InvalidPayload { method, detail } => {
                write!(f, "invalid Codex App Server payload for {method}: {detail}")
            }
            Self::Cancelled => f.write_str("Codex App Server request was cancelled"),
        }
    }
}

impl std::error::Error for AppServerError {}

pub(crate) fn payload_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Object(_) => "object",
    }
}
