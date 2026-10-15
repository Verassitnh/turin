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
    state: Mutex<State<K, V>>,
}

struct State<K, V> {
    /// Least recently used first; a hit moves its entry to the back.
    entries: IndexMap<K, Entry<V>>,
    /// Bumped by every `invalidate`. A lookup that started before one must not
    /// store its result (see `insert_if_unchanged`).
    epoch: u64,
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
            state: Mutex::new(State {
                entries: IndexMap::new(),
                epoch: 0,
            }),
        }
    }

    /// A live entry, marked most recently used. An expired entry is dropped.
    pub fn get(&self, key: &K) -> Option<V> {
        let now = (self.clock)();
        let mut state = self.state.lock();
        let entry = state.entries.shift_remove(key)?;
        if now >= entry.expires_at {
            return None;
        }
        let value = entry.value.clone();
        state.entries.insert(key.clone(), entry);
        Some(value)
    }

    /// Whether a live entry exists. Unlike `get`, leaves LRU order alone.
    pub fn contains(&self, key: &K) -> bool {
        let now = (self.clock)();
        self.state
            .lock()
            .entries
            .get(key)
            .is_some_and(|entry| now < entry.expires_at)
    }

    /// A zero TTL stores nothing, matching upstream `timeToLive` returning
    /// `Duration.zero` for failed lookups.
    pub fn insert(&self, key: K, value: V, ttl: Duration) {
        self.store(key, value, ttl);
    }

    /// The epoch to pass to `insert_if_unchanged`. Read it before the lookup
    /// whose result will be stored.
    pub fn epoch(&self) -> u64 {
        self.state.lock().epoch
    }

    /// Stores like `insert`, unless an `invalidate` ran since `epoch` was read.
    /// Upstream `Cache` drops an invalidated entry, so a lookup that completes
    /// after the invalidation never refills it. Returns whether it stored.
    ///
    /// The epoch is cache-wide, so an invalidation of any key also drops a
    /// concurrent result for other keys. That only costs one more lookup.
    pub fn insert_if_unchanged(&self, epoch: u64, key: K, value: V, ttl: Duration) -> bool {
        let mut state = self.state.lock();
        if state.epoch != epoch {
            return false;
        }
        Self::store_locked(
            &self.capacity,
            &mut state.entries,
            key,
            value,
            ttl,
            (self.clock)(),
        );
        true
    }

    fn store(&self, key: K, value: V, ttl: Duration) {
        let mut state = self.state.lock();
        Self::store_locked(
            &self.capacity,
            &mut state.entries,
            key,
            value,
            ttl,
            (self.clock)(),
        );
    }

    fn store_locked(
        capacity: &usize,
        entries: &mut IndexMap<K, Entry<V>>,
        key: K,
        value: V,
        ttl: Duration,
        now: Instant,
    ) {
        if ttl.is_zero() {
            return;
        }
        let expires_at = now + ttl;
        entries.shift_remove(&key);
        entries.insert(key, Entry { value, expires_at });
        while entries.len() > *capacity {
            entries.shift_remove_index(0);
        }
    }

    pub fn invalidate(&self, key: &K) {
        let mut state = self.state.lock();
        state.entries.shift_remove(key);
        state.epoch += 1;
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::TtlCache;
    use super::test_support::TestClock;

    #[test]
    fn insert_if_unchanged_refuses_a_result_from_before_an_invalidation() {
        let clock = TestClock::new();
        let cache = TtlCache::new(8, clock.clock());
        let before = cache.epoch();
        cache.invalidate(&"root");

        assert!(!cache.insert_if_unchanged(before, "root", 1, Duration::from_secs(60)));
        assert_eq!(cache.get(&"root"), None);

        let after = cache.epoch();
        assert!(cache.insert_if_unchanged(after, "root", 2, Duration::from_secs(60)));
        assert_eq!(cache.get(&"root"), Some(2));
    }
}
