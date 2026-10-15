//! Port of upstream/packages/shared/src/KeyedLock.ts.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

struct LockEntry {
    semaphore: Arc<Semaphore>,
    users: usize,
}

/// Mutual exclusion per key. A key's lock exists only while someone holds or
/// waits on it, so locks for keys that come and go never accumulate. Not
/// reentrant: taking a key while already holding it deadlocks.
pub struct KeyedLock<K> {
    locks: Mutex<HashMap<K, LockEntry>>,
}

/// Holding one releases the key. Dropping it is the release, even on cancel.
pub struct KeyedLockGuard<'a, K: Hash + Eq + Clone> {
    lock: &'a KeyedLock<K>,
    key: K,
    _permit: OwnedSemaphorePermit,
}

impl<K: Hash + Eq + Clone> Default for KeyedLock<K> {
    fn default() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Hash + Eq + Clone> KeyedLock<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for the key. Queued waiters go first-in, first-out.
    pub async fn lock(&self, key: K) -> KeyedLockGuard<'_, K> {
        let semaphore = {
            let mut locks = self.locks.lock();
            let entry = locks.entry(key.clone()).or_insert_with(|| LockEntry {
                semaphore: Arc::new(Semaphore::new(1)),
                users: 0,
            });
            entry.users += 1;
            Arc::clone(&entry.semaphore)
        };
        let permit = semaphore
            .acquire_owned()
            .await
            .expect("KeyedLock semaphores are never closed");
        KeyedLockGuard {
            lock: self,
            key,
            _permit: permit,
        }
    }

    /// The keys someone holds or waits on right now.
    pub fn active_keys(&self) -> Vec<K> {
        self.locks.lock().keys().cloned().collect()
    }

    fn release(&self, key: &K) {
        let mut locks = self.locks.lock();
        let Some(entry) = locks.get_mut(key) else {
            return;
        };
        if entry.users == 1 {
            locks.remove(key);
        } else {
            entry.users -= 1;
        }
    }
}

impl<K: Hash + Eq + Clone> Drop for KeyedLockGuard<'_, K> {
    fn drop(&mut self) {
        // The permit drops after this, so the next waiter wakes to a live entry.
        self.lock.release(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use pretty_assertions::assert_eq;

    use super::KeyedLock;

    #[tokio::test]
    async fn serializes_holders_of_one_key_and_drops_idle_keys() {
        let locks = Arc::new(KeyedLock::<String>::new());
        let first = locks.lock("a".to_owned()).await;
        assert_eq!(locks.active_keys(), vec!["a".to_owned()]);

        let waiter = {
            let locks = Arc::clone(&locks);
            tokio::spawn(async move {
                let _second = locks.lock("a".to_owned()).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!waiter.is_finished());

        // Another key is never blocked by the first.
        let other = locks.lock("b".to_owned()).await;
        drop(first);
        waiter.await.expect("waiter finishes");
        assert_eq!(locks.active_keys(), vec!["b".to_owned()]);
        drop(other);
        assert!(locks.active_keys().is_empty());
    }
}
