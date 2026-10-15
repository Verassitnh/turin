//! Port of upstream/apps/server/src/persistence/Errors.ts

use std::fmt;

/// Correlation carried on persistence errors. Same four shapes as upstream's union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceErrorCorrelation {
    SessionId(String),
    CurrentSessionId(String),
    PairingLinkId(String),
    ThreadId(String),
}

impl fmt::Display for PersistenceErrorCorrelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionId(id) => write!(f, "sessionId={id}"),
            Self::CurrentSessionId(id) => write!(f, "currentSessionId={id}"),
            Self::PairingLinkId(id) => write!(f, "pairingLinkId={id}"),
            Self::ThreadId(id) => write!(f, "threadId={id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceSqlError {
    pub operation: String,
    pub detail: Option<String>,
    pub correlation: Option<PersistenceErrorCorrelation>,
}

impl PersistenceSqlError {
    pub fn with_correlation(mut self, correlation: PersistenceErrorCorrelation) -> Self {
        self.correlation = Some(correlation);
        self
    }

    pub fn message(&self) -> String {
        match &self.detail {
            None => format!("SQL error in {}", self.operation),
            Some(detail) => format!("SQL error in {}: {detail}", self.operation),
        }
    }
}

impl fmt::Display for PersistenceSqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceDecodeError {
    pub operation: String,
    pub issue: String,
    pub correlation: Option<PersistenceErrorCorrelation>,
}

impl PersistenceDecodeError {
    pub fn new(operation: impl Into<String>, issue: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            issue: issue.into(),
            correlation: None,
        }
    }

    pub fn with_correlation(mut self, correlation: PersistenceErrorCorrelation) -> Self {
        self.correlation = Some(correlation);
        self
    }

    pub fn message(&self) -> String {
        format!("Decode error in {}: {}", self.operation, self.issue)
    }
}

impl fmt::Display for PersistenceDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// `sqlite3_errstr` style detail: `SQLITE({code}) {description}`.
/// Non-SQLite failures leave the detail unset, matching upstream `describeSqlCause`.
pub fn sqlite_failure_detail(err: &rusqlite::Error) -> Option<String> {
    let rusqlite::Error::SqliteFailure(code, _) = err else {
        return None;
    };
    let extended = code.extended_code;
    let errstr = unsafe {
        let ptr = rusqlite::ffi::sqlite3_errstr(extended);
        if ptr.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    };
    Some(format!("SQLITE({extended}) {errstr}"))
}

pub fn sql_error(operation: impl Into<String>, err: &rusqlite::Error) -> PersistenceSqlError {
    PersistenceSqlError {
        operation: operation.into(),
        detail: sqlite_failure_detail(err),
        correlation: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationErrorKind {
    BadState,
    ImportError,
    Failed,
    Duplicates,
    Locked,
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("{}", .0.message())]
    Sql(PersistenceSqlError),
    #[error("{}", .0.message())]
    Decode(PersistenceDecodeError),
    #[error("{message}")]
    Migration {
        kind: MigrationErrorKind,
        message: String,
    },
    #[error(
        "Could not copy the V1 database at {source_path} to {destination_path}. The V1 database has not been migrated."
    )]
    V2DatabaseImport {
        source_path: String,
        destination_path: String,
        #[source]
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("persistence connection closed")]
    Closed,
}

impl PersistenceError {
    pub fn sql(operation: impl Into<String>, err: &rusqlite::Error) -> Self {
        Self::Sql(sql_error(operation, err))
    }

    pub fn migration(kind: MigrationErrorKind, message: impl Into<String>) -> Self {
        Self::Migration {
            kind,
            message: message.into(),
        }
    }

    pub fn is_decode(&self) -> bool {
        matches!(self, Self::Decode(_))
    }
}

/// Upstream error aliases. Callers can name the repository; the value is [`PersistenceError`].
pub type OrchestrationEventStoreError = PersistenceError;
/// Upstream error aliases. Callers can name the repository; the value is [`PersistenceError`].
pub type OrchestrationCommandReceiptRepositoryError = PersistenceError;
/// Upstream error aliases. Callers can name the repository; the value is [`PersistenceError`].
pub type ProviderSessionRuntimeRepositoryError = PersistenceError;
/// Upstream error aliases. Callers can name the repository; the value is [`PersistenceError`].
pub type PullRequestFilesViewedRepositoryError = PersistenceError;
/// Upstream error aliases. Callers can name the repository; the value is [`PersistenceError`].
pub type PullRequestReviewGuideRepositoryError = PersistenceError;
