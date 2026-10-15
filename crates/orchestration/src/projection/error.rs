//! Errors of the projection store (upstream `ProjectionStoreApplyEventError`,
//! `ProjectionStoreSetupError`, `ProjectionStoreThreadNotFoundError`,
//! `ProjectionStoreReadError`).

use turin_contracts::ThreadId;
use turin_persistence::{PersistenceDecodeError, PersistenceError};

/// Underlying failure of a store operation: a SQL error, a payload that does
/// not decode, or a broken invariant. Rendered into the typed error below.
pub type Cause = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum ProjectionStoreError {
    #[error("Failed to apply orchestration projection event {event_type}.")]
    ApplyEvent {
        event_type: String,
        #[source]
        cause: Cause,
    },
    #[error("Failed to initialize orchestration projection store.")]
    Setup {
        #[source]
        cause: Cause,
    },
    #[error("No orchestration projection exists for thread {thread_id}.")]
    ThreadNotFound { thread_id: ThreadId },
    #[error("Failed to read orchestration projection for thread {thread_id}.")]
    Read {
        thread_id: ThreadId,
        #[source]
        cause: Cause,
    },
    /// The database handle itself failed (closed, task panicked). Upstream has
    /// no equivalent because the SQL client cannot fail that way.
    #[error("The orchestration database is unavailable.")]
    Database(#[source] PersistenceError),
}

impl ProjectionStoreError {
    pub fn read(thread_id: &ThreadId, cause: impl Into<Cause>) -> Self {
        Self::Read {
            thread_id: thread_id.clone(),
            cause: cause.into(),
        }
    }

    pub fn setup(cause: impl Into<Cause>) -> Self {
        Self::Setup {
            cause: cause.into(),
        }
    }

    pub fn is_thread_not_found(&self) -> bool {
        matches!(self, Self::ThreadNotFound { .. })
    }
}

/// `isProjectionStoreThreadNotFoundError(cause) ? cause : new ProjectionStoreReadError(...)`:
/// keep a typed store error, wrap anything else.
pub(crate) fn keep_or_read(thread_id: &ThreadId, error: ReadFailure) -> ProjectionStoreError {
    match error {
        ReadFailure::Store(error) => error,
        ReadFailure::Cause(cause) => ProjectionStoreError::read(thread_id, cause),
    }
}

/// A failure inside a read body: either already a typed store error to pass
/// through, or a raw cause to be wrapped with the thread id.
#[derive(Debug)]
pub(crate) enum ReadFailure {
    Store(ProjectionStoreError),
    Cause(Cause),
}

impl From<ProjectionStoreError> for ReadFailure {
    fn from(error: ProjectionStoreError) -> Self {
        Self::Store(error)
    }
}

macro_rules! cause_from {
    ($($t:ty),+ $(,)?) => {$(
        impl From<$t> for ReadFailure {
            fn from(error: $t) -> Self {
                Self::Cause(Box::new(error))
            }
        }
    )+};
}
cause_from!(rusqlite::Error, serde_json::Error, PersistenceError);

impl From<Cause> for ReadFailure {
    fn from(cause: Cause) -> Self {
        Self::Cause(cause)
    }
}

impl From<String> for ReadFailure {
    fn from(message: String) -> Self {
        Self::Cause(message.into())
    }
}

impl From<&str> for ReadFailure {
    fn from(message: &str) -> Self {
        Self::Cause(message.into())
    }
}

/// Lets a store error cross a `Db::write` closure, which can only return a
/// [`PersistenceError`]. The message keeps the typed error's text and cause.
impl From<ProjectionStoreError> for PersistenceError {
    fn from(error: ProjectionStoreError) -> Self {
        use std::error::Error as _;
        let mut message = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        match error {
            ProjectionStoreError::Database(error) => error,
            _ => PersistenceError::Decode(PersistenceDecodeError::new("ProjectionStore", message)),
        }
    }
}
