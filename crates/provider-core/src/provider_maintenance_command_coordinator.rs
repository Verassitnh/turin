//! Port of upstream/apps/server/src/provider/providerMaintenanceCommandCoordinator.ts.
//!
//! Serializes provider maintenance commands. A target that is already running
//! refuses a second command at once; commands on different targets that share a
//! lock key queue behind each other.

use std::{collections::HashSet, future::Future, sync::Arc};

use parking_lot::Mutex;
use turin_shared::keyed_lock::KeyedLock;

use crate::rt::BoxFuture;

pub struct ProviderMaintenanceCommandCoordinator<E> {
    running_targets: Arc<Mutex<HashSet<String>>>,
    locks: KeyedLock<String>,
    make_already_running_error: Arc<dyn Fn(&str) -> E + Send + Sync>,
}

/// Clears the target when the command finishes, fails, or its caller is dropped.
struct RunningTarget {
    running_targets: Arc<Mutex<HashSet<String>>>,
    target_key: String,
}

impl Drop for RunningTarget {
    fn drop(&mut self) {
        self.running_targets.lock().remove(&self.target_key);
    }
}

impl<E> ProviderMaintenanceCommandCoordinator<E> {
    pub fn new(make_already_running_error: impl Fn(&str) -> E + Send + Sync + 'static) -> Self {
        Self {
            running_targets: Arc::new(Mutex::new(HashSet::new())),
            locks: KeyedLock::new(),
            make_already_running_error: Arc::new(make_already_running_error),
        }
    }

    /// Runs `run` under `lock_key` once `on_queued` (if any) has finished.
    /// Fails at once with the already-running error when `target_key` has a
    /// command in flight.
    pub async fn with_command_lock<A>(
        &self,
        target_key: &str,
        lock_key: &str,
        on_queued: Option<BoxFuture<'_, Result<(), E>>>,
        run: impl Future<Output = Result<A, E>>,
    ) -> Result<A, E> {
        let acquired = self.running_targets.lock().insert(target_key.to_owned());
        if !acquired {
            return Err((self.make_already_running_error)(target_key));
        }
        let _running = RunningTarget {
            running_targets: Arc::clone(&self.running_targets),
            target_key: target_key.to_owned(),
        };
        if let Some(on_queued) = on_queued {
            on_queued.await?;
        }
        self.locks.with_lock(lock_key.to_owned(), || run).await
    }
}

#[cfg(test)]
mod tests {
    //! Behavior the upstream coordinator relies on: a second command on a running
    //! target is refused, and the target frees itself after the command ends.

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum TestError {
        AlreadyRunning(String),
        Failed,
    }

    fn coordinator() -> ProviderMaintenanceCommandCoordinator<TestError> {
        ProviderMaintenanceCommandCoordinator::new(|target| {
            TestError::AlreadyRunning(target.to_owned())
        })
    }

    #[tokio::test]
    async fn refuses_a_second_command_on_a_running_target_and_frees_it_afterwards() {
        let coordinator = Arc::new(coordinator());
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                coordinator
                    .with_command_lock("codex", "codex", None, async move {
                        let _ = gate.await;
                        Ok::<_, TestError>(1)
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        let second = coordinator
            .with_command_lock("codex", "codex", None, async { Ok::<_, TestError>(2) })
            .await;
        assert_eq!(second, Err(TestError::AlreadyRunning("codex".to_owned())));

        release.send(()).expect("first command still waiting");
        assert_eq!(first.await.expect("join"), Ok(1));
        let third = coordinator
            .with_command_lock("codex", "codex", None, async { Ok::<_, TestError>(3) })
            .await;
        assert_eq!(third, Ok(3));
    }

    #[tokio::test]
    async fn a_failed_on_queued_step_releases_the_target_and_skips_the_run() {
        let coordinator = coordinator();
        let result = coordinator
            .with_command_lock(
                "claudeAgent",
                "claudeAgent",
                Some(Box::pin(async { Err(TestError::Failed) })),
                async { Ok::<(), TestError>(()) },
            )
            .await;
        assert_eq!(result, Err::<(), _>(TestError::Failed));
        let next = coordinator
            .with_command_lock("claudeAgent", "claudeAgent", None, async {
                Ok::<_, TestError>(7)
            })
            .await;
        assert_eq!(next, Ok(7));
    }
}
