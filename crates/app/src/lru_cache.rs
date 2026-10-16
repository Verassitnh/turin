//! Port of upstream/apps/web/src/lib/lruCache.ts
//!
//! An LRU bounded by both entry count and approximate byte size. Entries
//! larger than the whole memory budget are never stored.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

struct CacheEntry<V> {
    value: V,
    approximate_size: usize,
    /// Recency tick; the smallest tick is the oldest entry.
    tick: u64,
}

pub struct LruCache<K, V> {
    entries: HashMap<K, CacheEntry<V>>,
    /// Insertion order by tick, standing in for the insertion-ordered `Map`
    /// upstream relies on (`promote` is delete + set).
    order: BTreeMap<u64, K>,
    next_tick: u64,
    total_size: usize,
    max_entries: usize,
    max_memory_bytes: usize,
}

impl<K: Hash + Eq + Clone, V> LruCache<K, V> {
    pub fn new(max_entries: usize, max_memory_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_tick: 0,
            total_size: 0,
            max_entries,
            max_memory_bytes,
        }
    }

    /// Returns the value and promotes it to most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let tick = self.take_tick();
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.tick);
        entry.tick = tick;
        self.order.insert(tick, key.clone());
        Some(&entry.value)
    }

    pub fn set(&mut self, key: K, value: V, approximate_size: usize) {
        if approximate_size > self.max_memory_bytes {
            return;
        }

        if let Some(existing) = self.entries.remove(&key) {
            self.total_size -= existing.approximate_size;
            self.order.remove(&existing.tick);
        }

        self.evict_if_needed(approximate_size);
        let tick = self.take_tick();
        self.order.insert(tick, key.clone());
        self.entries.insert(
            key,
            CacheEntry {
                value,
                approximate_size,
                tick,
            },
        );
        self.total_size += approximate_size;
    }

    // `clear` (and the size accessors) are upstream's API; the highlight cache
    // does not clear itself.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.total_size = 0;
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn take_tick(&mut self) -> u64 {
        let tick = self.next_tick;
        self.next_tick += 1;
        tick
    }

    fn evict_if_needed(&mut self, incoming_size: usize) {
        while (self.entries.len() >= self.max_entries
            || self.total_size + incoming_size > self.max_memory_bytes)
            && !self.entries.is_empty()
        {
            let Some((_, oldest_key)) = self.order.pop_first() else {
                break;
            };
            if let Some(oldest_entry) = self.entries.remove(&oldest_key) {
                self.total_size -= oldest_entry.approximate_size;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(max_entries: usize, max_memory_bytes: usize) -> LruCache<String, String> {
        LruCache::new(max_entries, max_memory_bytes)
    }

    fn get(cache: &mut LruCache<String, String>, key: &str) -> Option<String> {
        cache.get(&key.to_string()).cloned()
    }

    fn set(cache: &mut LruCache<String, String>, key: &str, value: &str, size: usize) {
        cache.set(key.to_string(), value.to_string(), size);
    }

    #[test]
    fn returns_none_for_missing_keys() {
        let mut cache = cache(2, 100);
        assert_eq!(get(&mut cache, "missing"), None);
    }

    #[test]
    fn evicts_oldest_by_max_entries() {
        let mut cache = cache(2, 1_000);
        set(&mut cache, "a", "A", 10);
        set(&mut cache, "b", "B", 10);
        set(&mut cache, "c", "C", 10);

        assert_eq!(get(&mut cache, "a"), None);
        assert_eq!(get(&mut cache, "b").as_deref(), Some("B"));
        assert_eq!(get(&mut cache, "c").as_deref(), Some("C"));
    }

    #[test]
    fn promotes_on_get_and_evicts_least_recently_used() {
        let mut cache = cache(2, 1_000);
        set(&mut cache, "a", "A", 10);
        set(&mut cache, "b", "B", 10);
        assert_eq!(get(&mut cache, "a").as_deref(), Some("A"));

        set(&mut cache, "c", "C", 10);
        assert_eq!(get(&mut cache, "a").as_deref(), Some("A"));
        assert_eq!(get(&mut cache, "b"), None);
        assert_eq!(get(&mut cache, "c").as_deref(), Some("C"));
    }

    #[test]
    fn evicts_by_memory_budget() {
        let mut cache = cache(10, 25);
        set(&mut cache, "a", "A", 10);
        set(&mut cache, "b", "B", 10);
        set(&mut cache, "c", "C", 10);

        assert_eq!(get(&mut cache, "a"), None);
        assert_eq!(get(&mut cache, "b").as_deref(), Some("B"));
        assert_eq!(get(&mut cache, "c").as_deref(), Some("C"));
    }

    #[test]
    fn does_not_cache_entries_larger_than_the_memory_budget() {
        let mut cache = cache(2, 25);
        set(&mut cache, "a", "A", 10);
        set(&mut cache, "oversized", "X", 30);

        assert_eq!(get(&mut cache, "a").as_deref(), Some("A"));
        assert_eq!(get(&mut cache, "oversized"), None);
    }

    #[test]
    fn preserves_an_existing_entry_when_an_oversized_replacement_is_rejected() {
        let mut cache = cache(2, 25);
        set(&mut cache, "a", "A", 10);
        set(&mut cache, "a", "oversized", 30);

        assert_eq!(get(&mut cache, "a").as_deref(), Some("A"));
    }

    #[test]
    fn replacing_an_entry_releases_its_size_and_clear_empties_the_cache() {
        let mut cache = cache(10, 25);
        set(&mut cache, "a", "A", 20);
        set(&mut cache, "a", "A2", 20);
        set(&mut cache, "b", "B", 5);
        assert_eq!(cache.len(), 2);
        assert_eq!(get(&mut cache, "a").as_deref(), Some("A2"));

        cache.clear();
        assert!(cache.is_empty());
        set(&mut cache, "c", "C", 25);
        assert_eq!(get(&mut cache, "c").as_deref(), Some("C"));
    }
}
