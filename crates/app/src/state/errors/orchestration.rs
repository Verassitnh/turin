//! Port of upstream/packages/client-runtime/src/errors/orchestration.ts
//!
//! `Schema.is(OrchestrationDispatchCommandError)` over `unknown` is a
//! downcast of the error that reached the caller.

use std::error::Error;

use turin_contracts::orchestration_dispatch::{
    BootstrapThreadDisposition, OrchestrationDispatchCommandError,
};

fn bootstrap_thread_disposition(
    error: &(dyn Error + 'static),
) -> Option<BootstrapThreadDisposition> {
    match error.downcast_ref::<OrchestrationDispatchCommandError>()? {
        OrchestrationDispatchCommandError::OrchestrationDispatchCommandError {
            bootstrap_thread_disposition,
            ..
        } => *bootstrap_thread_disposition,
    }
}

/// The dispatch failed after its bootstrap thread had been removed.
pub fn was_bootstrap_thread_deleted(error: &(dyn Error + 'static)) -> bool {
    bootstrap_thread_disposition(error) == Some(BootstrapThreadDisposition::Deleted)
}

/// The dispatch failed before its bootstrap thread was ever created.
pub fn was_bootstrap_thread_not_created(error: &(dyn Error + 'static)) -> bool {
    bootstrap_thread_disposition(error) == Some(BootstrapThreadDisposition::NotCreated)
}

#[cfg(test)]
mod tests {
    //! Port of upstream/packages/client-runtime/src/errors/orchestration.test.ts

    use turin_contracts::base_schemas::TrimmedNonEmptyString;

    use super::*;

    fn dispatch_error(
        message: &str,
        disposition: Option<BootstrapThreadDisposition>,
    ) -> OrchestrationDispatchCommandError {
        OrchestrationDispatchCommandError::OrchestrationDispatchCommandError {
            message: TrimmedNonEmptyString::new(message).unwrap(),
            cause: None,
            bootstrap_thread_disposition: disposition,
        }
    }

    fn connection_lost() -> std::io::Error {
        std::io::Error::other("connection lost")
    }

    #[test]
    fn was_bootstrap_thread_deleted_accepts_only_a_confirmed_deleted_bootstrap_thread() {
        assert!(was_bootstrap_thread_deleted(&dispatch_error(
            "Failed to create worktree.",
            Some(BootstrapThreadDisposition::Deleted)
        )));
        assert!(!was_bootstrap_thread_deleted(&dispatch_error(
            "Failed to create worktree.",
            None
        )));
        assert!(!was_bootstrap_thread_deleted(&connection_lost()));
    }

    #[test]
    fn was_bootstrap_thread_not_created_accepts_only_a_confirmed_never_created_bootstrap_thread() {
        let not_created = dispatch_error(
            "A separate worktree requires a base commit.",
            Some(BootstrapThreadDisposition::NotCreated),
        );
        assert!(was_bootstrap_thread_not_created(&not_created));
        assert!(!was_bootstrap_thread_deleted(&not_created));
        assert!(!was_bootstrap_thread_not_created(&dispatch_error(
            "Failed to create worktree.",
            Some(BootstrapThreadDisposition::Deleted)
        )));
        assert!(!was_bootstrap_thread_not_created(&dispatch_error(
            "Failed to create worktree.",
            None
        )));
        assert!(!was_bootstrap_thread_not_created(&connection_lost()));
    }
}
