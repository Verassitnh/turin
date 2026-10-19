//! Port of upstream/packages/effect-acp/src/errors.ts
//! Also: Port of upstream/packages/effect-acp/src/_internal/shared.ts.

use std::fmt;

use serde_json::{Value, json};

/// JSON-RPC id. Numbers and strings are distinct: `"1"` does not complete request `1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl RequestId {
    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Number(number) => number.as_i64().map(Self::Number),
            Value::String(text) => Some(Self::String(text.clone())),
            _ => None,
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Self::Number(number) => Value::Number((*number).into()),
            Self::String(text) => Value::String(text.clone()),
        }
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::String(text) => write!(f, "{text}"),
        }
    }
}

/// Lossless map key for callback tables. Mirrors `acpRequestIdentity`.
pub fn acp_request_identity(request_id: &RequestId) -> String {
    const PREFIX: &str = "$t3:jsonrpc:";
    match request_id {
        RequestId::Number(number) => format!("{PREFIX}number:{number}"),
        RequestId::String(text) if text.starts_with(PREFIX) => format!("{PREFIX}string:{text}"),
        RequestId::String(text) => text.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolParseOperation {
    EncodeMessage,
    DecodeWireMessage,
    DecodeNotificationPayload,
}

impl ProtocolParseOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EncodeMessage => "encode-message",
            Self::DecodeWireMessage => "decode-wire-message",
            Self::DecodeNotificationPayload => "decode-notification-payload",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOperation {
    DecodeExtensionRequestPayload,
    EncodeExtensionResponse,
    HandleRequest,
    HandleExtensionRequest,
    ReceiveResponse,
    ReceiveStreamingResponse,
}

impl RequestOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DecodeExtensionRequestPayload => "decode-extension-request-payload",
            Self::EncodeExtensionResponse => "encode-extension-response",
            Self::HandleRequest => "handle-request",
            Self::HandleExtensionRequest => "handle-extension-request",
            Self::ReceiveResponse => "receive-response",
            Self::ReceiveStreamingResponse => "receive-streaming-response",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportOperation {
    CallRpc,
    ReadInputStream,
    ReadProcessExitStatus,
}

impl TransportOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallRpc => "call-rpc",
            Self::ReadInputStream => "read-input-stream",
            Self::ReadProcessExitStatus => "read-process-exit-status",
        }
    }
}

/// Schema failure summary. The rejected value is never copied into this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaIssue {
    pub issue_count: u64,
    pub issue_kinds: Vec<String>,
    pub maximum_path_depth: u64,
}

impl SchemaIssue {
    pub fn invalid_type(path_depth: u64) -> Self {
        Self {
            issue_count: 1,
            issue_kinds: vec!["InvalidType".to_owned(), "Pointer".to_owned()],
            maximum_path_depth: path_depth,
        }
    }

    pub fn to_data(&self) -> Value {
        json!({
            "issueCount": self.issue_count,
            "issueKinds": self.issue_kinds,
            "maximumPathDepth": self.maximum_path_depth,
        })
    }
}

impl fmt::Display for SchemaIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "schema validation failed ({} issues)", self.issue_count)
    }
}

impl std::error::Error for SchemaIssue {}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{}", self.message())]
pub struct AcpSpawnError {
    pub command: Option<String>,
    pub cause: String,
}

impl AcpSpawnError {
    pub fn message(&self) -> String {
        match &self.command {
            Some(command) => format!("Failed to spawn ACP process for command: {command}"),
            None => "Failed to spawn ACP process".to_owned(),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{}", self.message())]
pub struct AcpProcessExitedError {
    pub code: Option<i32>,
    pub pid: Option<i64>,
    pub stderr: Option<String>,
    pub cause: Option<String>,
}

impl AcpProcessExitedError {
    pub fn message(&self) -> String {
        let base = match self.code {
            None => "ACP process exited".to_owned(),
            Some(code) => format!("ACP process exited with code {code}"),
        };
        match self
            .stderr
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(excerpt) => format!("{base}\n{excerpt}"),
            None => base,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{}", self.message())]
pub struct AcpProtocolParseError {
    pub operation: ProtocolParseOperation,
    pub method: Option<String>,
    pub request_id: Option<RequestId>,
    pub issue_count: Option<u64>,
    pub issue_kinds: Option<Vec<String>>,
    pub maximum_path_depth: Option<u64>,
    pub cause: Option<String>,
}

impl AcpProtocolParseError {
    pub fn message(&self) -> String {
        match &self.method {
            Some(method) => format!(
                "ACP protocol operation '{}' failed for method '{method}'.",
                self.operation.as_str()
            ),
            None => format!(
                "ACP protocol operation '{}' failed.",
                self.operation.as_str()
            ),
        }
    }

    pub fn from_schema(
        operation: ProtocolParseOperation,
        method: &str,
        issue: &SchemaIssue,
    ) -> Self {
        Self {
            operation,
            method: Some(method.to_owned()),
            request_id: None,
            issue_count: Some(issue.issue_count),
            issue_kinds: Some(issue.issue_kinds.clone()),
            maximum_path_depth: Some(issue.maximum_path_depth),
            cause: Some(issue.to_string()),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{}", self.message())]
pub struct AcpTransportError {
    pub operation: Option<TransportOperation>,
    pub method: Option<String>,
    /// Diagnostic kept off the public message. Upstream `detail` is not interpolated.
    pub detail: Option<String>,
    pub pid: Option<i64>,
    pub cause: Option<String>,
}

impl AcpTransportError {
    pub fn message(&self) -> String {
        let method = self
            .method
            .as_ref()
            .map(|method| format!(" for method {method}"))
            .unwrap_or_default();
        match self.operation {
            Some(operation) => {
                format!(
                    "ACP transport operation {} failed{method}.",
                    operation.as_str()
                )
            }
            None => "ACP transport operation failed.".to_owned(),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{}", self.error_message)]
pub struct AcpRequestError {
    pub code: i64,
    pub error_message: String,
    pub data: Option<Value>,
    pub method: Option<String>,
    pub request_id: Option<RequestId>,
    pub operation: Option<RequestOperation>,
    pub issue_count: Option<u64>,
    pub issue_kinds: Option<Vec<String>>,
    pub maximum_path_depth: Option<u64>,
    pub cause: Option<String>,
}

impl AcpRequestError {
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(-32700, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(-32600, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("Method not found: {method}"))
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(-32603, message)
    }

    pub fn auth_required(message: impl Into<String>) -> Self {
        Self::new(-32000, message)
    }

    pub fn resource_not_found(message: impl Into<String>) -> Self {
        Self::new(-32002, message)
    }

    pub fn from_protocol_error(
        code: i64,
        message: impl Into<String>,
        data: Option<Value>,
        method: &str,
        request_id: RequestId,
    ) -> Self {
        Self {
            code,
            error_message: message.into(),
            data,
            method: Some(method.to_owned()),
            request_id: Some(request_id),
            operation: Some(RequestOperation::ReceiveResponse),
            issue_count: None,
            issue_kinds: None,
            maximum_path_depth: None,
            cause: None,
        }
    }

    pub fn invalid_extension_payload(method: &str, issue: &SchemaIssue) -> Self {
        Self {
            code: -32602,
            error_message: format!("Invalid payload for ACP extension method '{method}'."),
            data: Some(issue.to_data()),
            method: Some(method.to_owned()),
            request_id: None,
            operation: Some(RequestOperation::DecodeExtensionRequestPayload),
            issue_count: Some(issue.issue_count),
            issue_kinds: Some(issue.issue_kinds.clone()),
            maximum_path_depth: Some(issue.maximum_path_depth),
            cause: Some(issue.to_string()),
        }
    }

    pub fn from_handler(error: AcpError, method: &str, extension: bool) -> Self {
        if let AcpError::Request(request) = error {
            return *request;
        }
        let (message, operation) = if extension {
            (
                format!("ACP extension request handler failed for method '{method}'"),
                RequestOperation::HandleExtensionRequest,
            )
        } else {
            (
                format!("ACP request handler failed for method '{method}'"),
                RequestOperation::HandleRequest,
            )
        };
        Self {
            code: -32603,
            error_message: message,
            data: None,
            method: Some(method.to_owned()),
            request_id: None,
            operation: Some(operation),
            issue_count: None,
            issue_kinds: None,
            maximum_path_depth: None,
            cause: Some(error.to_string()),
        }
    }

    pub fn to_protocol_error(&self) -> Value {
        let mut error = json!({
            "code": self.code,
            "message": self.error_message,
        });
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        error
    }

    pub fn method_unavailable(message: impl Into<String>) -> Self {
        Self::new(-32601, message)
    }

    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            error_message: message.into(),
            data: None,
            method: None,
            request_id: None,
            operation: None,
            issue_count: None,
            issue_kinds: None,
            maximum_path_depth: None,
            cause: None,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum AcpError {
    #[error(transparent)]
    Request(Box<AcpRequestError>),
    #[error(transparent)]
    Spawn(Box<AcpSpawnError>),
    #[error(transparent)]
    ProcessExited(Box<AcpProcessExitedError>),
    #[error(transparent)]
    ProtocolParse(Box<AcpProtocolParseError>),
    #[error(transparent)]
    Transport(Box<AcpTransportError>),
    #[error("ACP input stream ended.")]
    InputStreamEnded,
}

impl From<AcpRequestError> for AcpError {
    fn from(error: AcpRequestError) -> Self {
        Self::Request(Box::new(error))
    }
}

impl From<AcpSpawnError> for AcpError {
    fn from(error: AcpSpawnError) -> Self {
        Self::Spawn(Box::new(error))
    }
}

impl From<AcpProcessExitedError> for AcpError {
    fn from(error: AcpProcessExitedError) -> Self {
        Self::ProcessExited(Box::new(error))
    }
}

impl From<AcpProtocolParseError> for AcpError {
    fn from(error: AcpProtocolParseError) -> Self {
        Self::ProtocolParse(Box::new(error))
    }
}

impl From<AcpTransportError> for AcpError {
    fn from(error: AcpTransportError) -> Self {
        Self::Transport(Box::new(error))
    }
}

impl AcpError {
    pub fn transport(detail: impl Into<String>) -> Self {
        Self::from(AcpTransportError {
            operation: None,
            method: None,
            detail: Some(detail.into()),
            pid: None,
            cause: None,
        })
    }

    pub fn is_request(&self) -> bool {
        matches!(self, Self::Request(_))
    }
}

/// Walk `path` and require a JSON number. The rejected value is not copied.
pub fn expect_number_at(value: &Value, path: &[&str]) -> Result<(), SchemaIssue> {
    let mut cursor = value;
    for (index, segment) in path.iter().enumerate() {
        let Some(next) = cursor.get(*segment) else {
            return Err(SchemaIssue {
                issue_count: 1,
                issue_kinds: vec!["MissingKey".to_owned(), "Pointer".to_owned()],
                maximum_path_depth: (index + 1) as u64,
            });
        };
        cursor = next;
    }
    if cursor.is_number() {
        Ok(())
    } else {
        Err(SchemaIssue::invalid_type(path.len() as u64))
    }
}
