//! Port of upstream/apps/web/src/components/DiffWorkerPoolProvider.tsx (pool lifecycle;
//! highlighting runs on the GPUI background executor instead of Pierre web workers).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
pub const DIFF_WORKER_IDLE_TTL_MS: u64 = 30_000;

struct PoolInner {
    consumers: AtomicUsize,
    idle_deadline_ms: Mutex<Option<u64>>,
}

#[derive(Clone)]
pub struct DiffWorkerPool {
    inner: Arc<PoolInner>,
}

impl DiffWorkerPool {
    pub fn shared() -> Self {
        static POOL: std::sync::OnceLock<DiffWorkerPool> = std::sync::OnceLock::new();
        POOL.get_or_init(|| Self {
            inner: Arc::new(PoolInner {
                consumers: AtomicUsize::new(0),
                idle_deadline_ms: Mutex::new(None),
            }),
        })
        .clone()
    }

    pub fn acquire(&self, now_ms: u64) {
        self.inner.consumers.fetch_add(1, Ordering::SeqCst);
        *self.inner.idle_deadline_ms.lock().unwrap() = None;
        let _ = now_ms;
    }

    pub fn release(&self, now_ms: u64) {
        if self.inner.consumers.fetch_sub(1, Ordering::SeqCst) == 1 {
            *self.inner.idle_deadline_ms.lock().unwrap() = Some(now_ms + DIFF_WORKER_IDLE_TTL_MS);
        }
    }

    pub fn should_shutdown_idle(&self, now_ms: u64) -> bool {
        self.inner.consumers.load(Ordering::SeqCst) == 0
            && self
                .inner
                .idle_deadline_ms
                .lock()
                .unwrap()
                .is_some_and(|deadline| now_ms >= deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_consumer_count_and_idle_ttl() {
        let pool = DiffWorkerPool {
            inner: Arc::new(PoolInner {
                consumers: AtomicUsize::new(0),
                idle_deadline_ms: Mutex::new(None),
            }),
        };
        pool.acquire(0);
        pool.acquire(0);
        assert_eq!(pool.inner.consumers.load(Ordering::SeqCst), 2);
        pool.release(100);
        assert_eq!(pool.inner.consumers.load(Ordering::SeqCst), 1);
        pool.release(100);
        assert!(pool.inner.idle_deadline_ms.lock().unwrap().is_some());
        assert!(!pool.should_shutdown_idle(100));
        assert!(pool.should_shutdown_idle(100 + DIFF_WORKER_IDLE_TTL_MS));
    }
}
