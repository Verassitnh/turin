//! Port of upstream/apps/server/src/workspace/workspaceLease.ts
//!
//! One permit per cwd, plus a user count so the semaphore is dropped only
//! after the last waiter is gone. The map lock is `parking_lot` because the
//! guard releases that count from `Drop` (which cannot `.await`); the permit
//! itself is a `tokio` semaphore. Share one [`WorkspaceLease`] in the process
//! the way upstream shares its module-level map.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

struct LeaseEntry {
    semaphore: Arc<Semaphore>,
    users: usize,
}

/// Coordinates checkout removal and startup across tasks using the same cwd.
#[derive(Clone)]
pub struct WorkspaceLease {
    inner: Arc<Mutex<HashMap<String, LeaseEntry>>>,
}

/// Decrements the user count. Created before the permit wait so cancel during
/// `acquire` still removes the cwd entry.
struct UserCountGuard {
    cwd: String,
    inner: Arc<Mutex<HashMap<String, LeaseEntry>>>,
}

impl Drop for UserCountGuard {
    fn drop(&mut self) {
        let mut leases = self.inner.lock();
        let Some(entry) = leases.get_mut(&self.cwd) else {
            return;
        };
        entry.users = entry.users.saturating_sub(1);
        if entry.users == 0 {
            leases.remove(&self.cwd);
        }
    }
}

/// Holds the cwd permit. Dropping it — including on task cancel — releases the
/// permit and then the user count.
#[must_use = "dropping the guard releases the workspace lease"]
pub struct WorkspaceLeaseGuard {
    permit: Option<OwnedSemaphorePermit>,
    users: Option<UserCountGuard>,
}

impl Drop for WorkspaceLeaseGuard {
    fn drop(&mut self) {
        // The permit goes first. Otherwise a waiter can miss this semaphore
        // and take a freshly inserted one while we are still inside the cwd.
        self.permit.take();
        self.users.take();
    }
}

impl WorkspaceLease {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn acquire(&self, cwd: impl Into<String>) -> WorkspaceLeaseGuard {
        let cwd = cwd.into();
        let semaphore = {
            let mut leases = self.inner.lock();
            let entry = leases.entry(cwd.clone()).or_insert_with(|| LeaseEntry {
                semaphore: Arc::new(Semaphore::new(1)),
                users: 0,
            });
            entry.users += 1;
            Arc::clone(&entry.semaphore)
        };
        let users = UserCountGuard {
            cwd,
            inner: Arc::clone(&self.inner),
        };
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .expect("workspace lease semaphore stays open");
        WorkspaceLeaseGuard {
            permit: Some(permit),
            users: Some(users),
        }
    }

    /// Runs `f` only after the cwd permit is held. The permit is released if
    /// `f`'s future is cancelled.
    pub async fn with_lease<T, F, Fut>(&self, cwd: impl Into<String>, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _guard = self.acquire(cwd).await;
        f().await
    }
}

impl Default for WorkspaceLease {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::WorkspaceLease;

    #[tokio::test]
    async fn serializes_holders_of_the_same_cwd() {
        let leases = WorkspaceLease::new();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first = leases.clone();
        let entered_holder = Arc::clone(&entered);
        let release_holder = Arc::clone(&release);
        let holder = tokio::spawn(async move {
            first
                .with_lease("cwd", || async move {
                    entered_holder.notify_one();
                    release_holder.notified().await;
                })
                .await;
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        let second = tokio::spawn({
            let leases = leases.clone();
            async move {
                leases.with_lease("cwd", || async {}).await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!second.is_finished());
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap();
        holder.await.unwrap();
    }

    #[tokio::test]
    async fn releases_the_lease_when_the_holder_is_cancelled() {
        let leases = WorkspaceLease::new();
        let child = leases.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let holder = tokio::spawn(async move {
            child
                .with_lease("/work", || async move {
                    let _ = tx.send(());
                    std::future::pending::<()>().await;
                })
                .await;
        });
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        holder.abort();
        let _ = holder.await;
        tokio::time::timeout(
            Duration::from_secs(2),
            leases.with_lease("/work", || async {}),
        )
        .await
        .expect("cancelled holder must release the cwd lease");
    }

    #[tokio::test]
    async fn different_cwds_do_not_block_each_other() {
        let leases = WorkspaceLease::new();
        let release = Arc::new(tokio::sync::Notify::new());
        let release_holder = Arc::clone(&release);
        let holder = tokio::spawn({
            let leases = leases.clone();
            async move {
                leases
                    .with_lease("a", || async move {
                        release_holder.notified().await;
                    })
                    .await;
            }
        });
        tokio::time::timeout(Duration::from_secs(2), leases.with_lease("b", || async {}))
            .await
            .unwrap();
        release.notify_one();
        holder.await.unwrap();
    }
}
