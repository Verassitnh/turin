//! Port of upstream/apps/server/src/provider/Errors.ts.
//!
//! `ProviderDriverError` itself lives in [`crate::errors`].

use thiserror::Error;

/// Lookup against the instance registry failed.
///
/// The driver may be registered, but no instance with the requested id has
/// been bootstrapped — typically because the persisted instance id refers to
/// an instance the user removed from settings, or because routing is asked
/// for an instance before the registry has finished its first reload.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("No provider instance bound to id '{instance_id}'")]
pub struct ProviderInstanceNotFoundError {
    pub instance_id: String,
    pub cause: Option<String>,
}

/// The session's working directory no longer exists on disk, so no provider
/// process can start in it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "This thread's workspace folder no longer exists or is not a directory: {cwd}. Restore the folder at this path before retrying."
)]
pub struct ProviderWorkspaceMissingError {
    pub thread_id: String,
    pub cwd: String,
}
