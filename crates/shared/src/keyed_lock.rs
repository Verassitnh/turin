//! Port of upstream/packages/shared/src/KeyedLock.ts
//!
//! Effect `Semaphore.withPermit` is a FIFO permit. `tokio::sync::Semaphore` is
//! fair, so waiters for one key run first-in, first-out. The map of locks is a
//! sync mutex: it is never held across `.await`.

use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

struct LockEntry {
    semaphore: Arc<Semaphore>,
    users: usize,
}

/// Mutual exclusion per key. A key's lock exists only while someone holds or
/// waits on it. Not reentrant: taking a key while already holding it deadlocks.
pub struct KeyedLock<K> {
    locks: Arc<Mutex<IndexMap<K, LockEntry>>>,
}

struct LockGuard<K: Eq + Hash> {
    key: K,
    permit: Option<OwnedSemaphorePermit>,
    locks: Arc<Mutex<IndexMap<K, LockEntry>>>,
}

impl<K: Eq + Hash> Drop for LockGuard<K> {
    fn drop(&mut self) {
        // Release the permit before forgetting the key, matching
        // `acquireUseRelease`: the next waiter can enter, and a key with no
        // holders or waiters disappears.
        self.permit.take();
        let mut locks = self.locks.lock();
        let Some(entry) = locks.get_mut(&self.key) else {
            return;
        };
        if entry.users <= 1 {
            locks.shift_remove(&self.key);
        } else {
            entry.users -= 1;
        }
    }
}

impl<K> KeyedLock<K>
where
    K: Eq + Hash + Clone,
{
    pub fn new() -> Self {
        Self {
            locks: Arc::new(Mutex::new(IndexMap::new())),
        }
    }

    /// Runs `effect` once no other holder of `key` is running.
    pub async fn with_lock<T, F, Fut>(&self, key: K, effect: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let semaphore = {
            let mut locks = self.locks.lock();
            let entry = locks.entry(key.clone()).or_insert_with(|| LockEntry {
                semaphore: Arc::new(Semaphore::new(1)),
                users: 0,
            });
            entry.users += 1;
            Arc::clone(&entry.semaphore)
        };
        let mut guard = LockGuard {
            key,
            permit: None,
            locks: Arc::clone(&self.locks),
        };
        guard.permit = Some(
            semaphore
                .acquire_owned()
                .await
                .expect("keyed lock semaphore stays open"),
        );
        let result = effect().await;
        drop(guard);
        result
    }

    /// The keys someone holds or waits on right now, in insertion order.
    pub fn active_keys(&self) -> Vec<K> {
        self.locks.lock().keys().cloned().collect()
    }
}

impl<K: Eq + Hash + Clone> Default for KeyedLock<K> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Mutex as AsyncMutex, oneshot};

    #[tokio::test]
    async fn runs_one_holder_of_a_key_at_a_time() {
        let lock = Arc::new(KeyedLock::new());
        let events = Arc::new(AsyncMutex::new(Vec::new()));
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(AsyncMutex::new(Some(release_rx)));

        let first_lock = Arc::clone(&lock);
        let first_events = Arc::clone(&events);
        let first_release = Arc::clone(&release_rx);
        let first = tokio::spawn(async move {
            first_lock
                .with_lock("key", || async {
                    first_events.lock().await.push("first:start");
                    let _ = entered_tx.send(());
                    let rx = first_release.lock().await.take().expect("release");
                    let _ = rx.await;
                    first_events.lock().await.push("first:end");
                })
                .await;
        });
        entered_rx.await.expect("entered");

        let second_lock = Arc::clone(&lock);
        let second_events = Arc::clone(&events);
        let second = tokio::spawn(async move {
            second_lock
                .with_lock("key", || async {
                    second_events.lock().await.push("second");
                })
                .await;
        });
        let third_lock = Arc::clone(&lock);
        let third_events = Arc::clone(&events);
        let third = tokio::spawn(async move {
            third_lock
                .with_lock("key", || async {
                    third_events.lock().await.push("third");
                })
                .await;
        });
        tokio::task::yield_now().await;
        assert_eq!(events.lock().await.as_slice(), ["first:start"]);
        let _ = release_tx.send(());
        first.await.unwrap();
        second.await.unwrap();
        third.await.unwrap();
        assert_eq!(
            events.lock().await.as_slice(),
            ["first:start", "first:end", "second", "third"]
        );
    }

    #[tokio::test]
    async fn runs_holders_of_different_keys_concurrently() {
        let lock = Arc::new(KeyedLock::<&str>::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(AsyncMutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for key in ["a", "b"] {
            let lock = Arc::clone(&lock);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            tasks.push(tokio::spawn(async move {
                lock.with_lock(key, || {
                    let entered = Arc::clone(&entered);
                    let release = Arc::clone(&release);
                    async move {
                        let notified = release.notified();
                        tokio::pin!(notified);
                        entered.lock().await.push(key);
                        notified.await;
                    }
                })
                .await;
            }));
        }
        while entered.lock().await.len() < 2 {
            tokio::task::yield_now().await;
        }
        let mut both = entered.lock().await.clone();
        both.sort_unstable();
        assert_eq!(both, ["a", "b"]);
        release.notify_waiters();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn drops_a_key_once_holders_and_waiters_are_gone() {
        let lock = Arc::new(KeyedLock::new());
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let holder_lock = Arc::clone(&lock);
        let holder = tokio::spawn(async move {
            holder_lock
                .with_lock("key", || async {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                })
                .await;
        });
        entered_rx.await.unwrap();
        let abandoned_lock = Arc::clone(&lock);
        let abandoned = tokio::spawn(async move {
            abandoned_lock.with_lock("key", || async {}).await;
        });
        let waiting_lock = Arc::clone(&lock);
        let waiting = tokio::spawn(async move {
            waiting_lock.with_lock("key", || async {}).await;
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(lock.active_keys(), vec!["key"]);
        abandoned.abort();
        let _ = abandoned.await;
        assert_eq!(lock.active_keys(), vec!["key"]);
        let _ = release_tx.send(());
        holder.await.unwrap();
        waiting.await.unwrap();
        let failed = lock
            .with_lock("other", || async { Err::<(), _>("boom") })
            .await;
        assert_eq!(failed, Err("boom"));
        assert!(lock.active_keys().is_empty());
    }

    #[tokio::test]
    async fn keeps_working_after_earlier_holders_finish() {
        // Upstream checks the lock outlives the scope that built it. A Rust lock
        // has no build scope, so the equivalent is reuse after earlier holders.
        let lock = KeyedLock::new();
        assert_eq!(lock.with_lock("key", || async { "first" }).await, "first");
        assert_eq!(lock.with_lock("key", || async { "ran" }).await, "ran");
    }

    #[tokio::test]
    async fn releases_a_key_when_its_holder_fails() {
        let lock = KeyedLock::new();
        let failed: Result<(), &str> = lock.with_lock("other", || async { Err("boom") }).await;
        assert_eq!(failed, Err("boom"));
        assert!(lock.active_keys().is_empty());
    }
}
