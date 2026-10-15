//! Port of the `OpenCodeRuntimeError` half of
//! `upstream/packages/provider-opencode/src/server/OpenCodeRuntime.ts`.

use std::fmt;
use std::sync::Arc;

/// Tagged runtime failure. `detail` is the text callers show; it must not carry
/// a server URL, a password, or an authorization header.
#[derive(Clone)]
pub struct OpenCodeRuntimeError {
    pub operation: String,
    pub detail: String,
    pub cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

impl OpenCodeRuntimeError {
    pub fn new(operation: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            detail: detail.into(),
            cause: None,
        }
    }

    pub fn with_cause(mut self, cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Arc::new(cause));
        self
    }

    pub fn is_runtime_error(err: &(dyn std::error::Error + 'static)) -> bool {
        err.downcast_ref::<Self>().is_some()
    }
}

impl fmt::Debug for OpenCodeRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenCodeRuntimeError")
            .field("operation", &self.operation)
            .field("detail", &self.detail)
            .finish()
    }
}

impl fmt::Display for OpenCodeRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for OpenCodeRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| cause.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl PartialEq for OpenCodeRuntimeError {
    fn eq(&self, other: &Self) -> bool {
        self.operation == other.operation && self.detail == other.detail
    }
}

impl Eq for OpenCodeRuntimeError {}

/// Formats SDK and runtime failures the way `openCodeRuntimeErrorDetail` does.
/// Effect's generic "An error occurred in Effect.try" strings are not produced
/// here; those only exist on the TypeScript side.
pub fn open_code_runtime_error_detail(err: &(dyn std::error::Error + 'static)) -> String {
    if let Some(runtime) = err.downcast_ref::<OpenCodeRuntimeError>() {
        return runtime.detail.clone();
    }
    let message = err.to_string();
    let trimmed = message.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    message
}

/// The server sent nothing, not even a heartbeat, for [`crate::SILENT_STREAM_TIMEOUT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The OpenCode server stopped sending events.")]
pub struct SilentStreamError;

#[derive(Debug, thiserror::Error)]
pub enum EventStreamError {
    #[error("The OpenCode server stopped sending events.")]
    Silent,
    #[error("{0}")]
    Transport(String),
    #[error("OpenCode event stream returned HTTP {status}")]
    Status { status: u16, body: String },
}
