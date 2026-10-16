//! Process-group termination errors for ACP session runtime.

use std::fmt;

use thiserror::Error;

/// Port of upstream `AcpProcessGroupTerminationError`.
#[derive(Debug, Error, Clone)]
#[error("{detail}")]
pub struct AcpProcessGroupTerminationError {
    pub detail: String,
    pub pid: Option<i32>,
    pub exit_code: Option<i32>,
    pub cause: Option<String>,
}

impl AcpProcessGroupTerminationError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            pid: None,
            exit_code: None,
            cause: None,
        }
    }

    pub fn with_cause(detail: impl Into<String>, cause: impl fmt::Display) -> Self {
        Self {
            detail: detail.into(),
            pid: None,
            exit_code: None,
            cause: Some(cause.to_string()),
        }
    }
}
