//! Replacement for upstream's `Cache.makeWith` (capacity-bounded LRU whose
//! entries carry their own time-to-live). Lookups are not single-flight:
//! concurrent misses for one key each compute the value.

use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use parking_lot::Mutex;

/// Time source for TTL checks. Upstream tests drive Effect's `TestClock`; the
/// port injects a clock instead.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(Instant::now)
}

pub struct TtlCache<K, V> {
    capacity: usize,
    clock: Clock,
    /// Least recently used first; a hit moves its entry to the back.
    entries: Mutex<IndexMap<K, Entry<V>>>,
}

struct Entry<V> {
    value: V,
    expires_at: Instant,
}

impl<K: Hash + Eq + Clone, V: Clone> TtlCache<K, V> {
    pub fn new(capacity: usize, clock: Clock) -> Self {
        Self {
            capacity,
            clock,
            entries: Mutex::new(IndexMap::new()),
        }
    }

    /// A live entry, marked most recently used. An expired entry is dropped.
    pub fn get(&self, key: &K) -> Option<V> {
        let now = (self.clock)();
        let mut entries = self.entries.lock();
        let entry = entries.shift_remove(key)?;
        if now >= entry.expires_at {
            return None;
        }
        let value = entry.value.clone();
        entries.insert(key.clone(), entry);
        Some(value)
    }

    /// A zero TTL stores nothing, matching upstream `timeToLive` returning
    /// `Duration.zero` for failed lookups.
    pub fn insert(&self, key: K, value: V, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }
        let expires_at = (self.clock)() + ttl;
        let mut entries = self.entries.lock();
        entries.shift_remove(&key);
        entries.insert(key, Entry { value, expires_at });
        while entries.len() > self.capacity {
            entries.shift_remove_index(0);
        }
    }

    pub fn invalidate(&self, key: &K) {
        self.entries.lock().shift_remove(key);
    }
}

#[cfg(test)]
pub mod test_support {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use parking_lot::Mutex;

    use super::Clock;

    /// Upstream `TestClock`: time only moves when a test adjusts it.
    pub struct TestClock {
        base: Instant,
        offset: Mutex<Duration>,
    }

    impl TestClock {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset: Mutex::new(Duration::ZERO),
            })
        }

        pub fn adjust(&self, by: Duration) {
            *self.offset.lock() += by;
        }

        pub fn clock(self: &Arc<Self>) -> Clock {
            let this = Arc::clone(self);
            Arc::new(move || this.base + *this.offset.lock())
        }
    }
}
