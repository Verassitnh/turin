//! Failures from the Cursor ACP adapter.
//!
//! Messages are safe to show. They do not include API keys, access tokens, or
//! cookie headers.

use std::fmt;

use turin_acp::AcpError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorOperation {
    Spawn,
    Initialize,
    Authenticate,
    SessionNew,
    SessionLoad,
    SessionList,
    Prompt,
    Cancel,
    Config,
    Models,
    Permission,
    UserInput,
    Plan,
    Usage,
    Settings,
}

impl CursorOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Initialize => "initialize",
            Self::Authenticate => "authenticate",
            Self::SessionNew => "session/new",
            Self::SessionLoad => "session/load",
            Self::SessionList => "session/list",
            Self::Prompt => "session/prompt",
            Self::Cancel => "session/cancel",
            Self::Config => "config",
            Self::Models => "models",
            Self::Permission => "permission",
            Self::UserInput => "user-input",
            Self::Plan => "plan",
            Self::Usage => "usage",
            Self::Settings => "settings",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CursorError {
    pub operation: CursorOperation,
    pub detail: String,
    pub not_signed_in: bool,
    pub unsupported: bool,
}

impl CursorError {
    pub fn new(operation: CursorOperation, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
            not_signed_in: false,
            unsupported: false,
        }
    }

    pub fn unsupported(operation: CursorOperation, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
            not_signed_in: false,
            unsupported: true,
        }
    }

    pub fn not_signed_in(operation: CursorOperation, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
            not_signed_in: true,
            unsupported: false,
        }
    }

    pub fn from_acp(operation: CursorOperation, error: &AcpError) -> Self {
        if crate::auth::is_auth_required_error(error) {
            return Self::not_signed_in(
                operation,
                "Not signed in. Run cursor-agent login, then sign in with cursor_login.",
            );
        }
        Self::new(operation, error.to_string())
    }
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Cursor {} failed. {}",
            self.operation.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for CursorError {}
