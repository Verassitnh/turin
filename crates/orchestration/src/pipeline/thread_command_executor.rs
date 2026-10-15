//! Port of upstream/apps/server/src/orchestration-v2/ThreadCommandExecutor.ts
//! and packages/shared/src/KeyedLock.ts.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::Mutex as AsyncMutex;
use turin_contracts::ThreadId;

#[derive(Clone, Default)]
pub struct ThreadCommandExecutor {
    locks: Arc<Mutex<HashMap<ThreadId, Weak<AsyncMutex<()>>>>>,
}

impl ThreadCommandExecutor {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn with_lock<T, F, Fut>(&self, thread_id: &ThreadId, action: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(|err| err.into_inner());
            locks.retain(|_, weak| weak.strong_count() > 0);
            locks
                .entry(thread_id.clone())
                .or_default()
                .upgrade()
                .unwrap_or_else(|| {
                    let lock = Arc::new(AsyncMutex::new(()));
                    locks.insert(thread_id.clone(), Arc::downgrade(&lock));
                    lock
                })
        };
        let _guard = lock.lock().await;
        action().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn serializes_commands_for_one_thread() {
        let executor = ThreadCommandExecutor::new();
        let thread = ThreadId::new("lock:test").unwrap();
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let executor = executor.clone();
            let thread = thread.clone();
            let entered = entered.clone();
            tokio::spawn(async move {
                executor
                    .with_lock(&thread, || async {
                        entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _ = rx.await;
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        let second = {
            let executor = executor.clone();
            let thread = thread.clone();
            let entered = entered.clone();
            tokio::spawn(async move {
                executor
                    .with_lock(&thread, || async {
                        entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 1);
        tx.send(()).unwrap();
        first.await.unwrap();
        second.await.unwrap();
        assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
