use std::collections::HashMap;
use std::hash::Hash;

// Search pages hold thread summaries and details, so 64 entries bound memory while
// retaining enough adjacent pages and recent queries for normal navigation.
pub(crate) const SEARCH_PAGE_CACHE_CAPACITY: usize = 64;

// Thread details are much smaller than complete search pages. A 4,096-entry limit
// keeps a useful cross-query working set without allowing old revisions to accumulate.
pub(crate) const THREAD_DETAIL_CACHE_CAPACITY: usize = 4_096;

#[derive(Debug)]
struct CacheEntry<V> {
    value: V,
    last_used: u64,
}

/// A fixed-entry-count cache with least-recently-used eviction.
///
/// Lookups and replacements refresh recency. Insertion only scans the map when a
/// full cache needs to evict an entry; ordinary lookups remain constant-time.
#[derive(Debug)]
pub(crate) struct BoundedLruCache<K, V> {
    capacity: usize,
    entries: HashMap<K, CacheEntry<V>>,
    recency: u64,
}

impl<K, V> BoundedLruCache<K, V>
where
    K: Eq + Hash,
{
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "cache capacity must be nonzero");
        let capacity_as_u64 =
            u64::try_from(capacity).expect("cache capacity must fit in the recency counter");
        assert!(
            capacity_as_u64 < u64::MAX,
            "cache capacity must leave room to advance the recency counter"
        );
        Self {
            capacity,
            entries: HashMap::with_capacity(capacity),
            recency: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        // Check first so misses do not consume recency stamps. This costs a second
        // hash lookup on a hit, but keeps the counter tied to actual cache use and
        // leaves overflow/rebase behavior straightforward.
        if !self.entries.contains_key(key) {
            return None;
        }
        let recency = self.next_recency();
        let entry = self
            .entries
            .get_mut(key)
            .expect("cache entry found immediately before recency update");
        entry.last_used = recency;
        Some(&entry.value)
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let recency = self.next_recency();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = recency;
            return Some(std::mem::replace(&mut entry.value, value));
        }

        if self.entries.len() == self.capacity {
            self.evict_lru();
        }
        self.entries.insert(
            key,
            CacheEntry {
                value,
                last_used: recency,
            },
        );
        debug_assert!(self.entries.len() <= self.capacity);
        None
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.recency = 0;
    }

    fn evict_lru(&mut self) {
        let Some(oldest_recency) = self.entries.values().map(|entry| entry.last_used).min() else {
            return;
        };
        let previous_len = self.entries.len();
        self.entries
            .retain(|_, entry| entry.last_used != oldest_recency);
        debug_assert_eq!(self.entries.len(), previous_len - 1);
    }

    fn next_recency(&mut self) -> u64 {
        if let Some(next) = self.recency.checked_add(1) {
            self.recency = next;
            return next;
        }

        self.rebase_recency();
        let next = self
            .recency
            .checked_add(1)
            .expect("rebased cache recency must have room to advance");
        self.recency = next;
        next
    }

    fn rebase_recency(&mut self) {
        let mut stamps = self
            .entries
            .values()
            .map(|entry| entry.last_used)
            .collect::<Vec<_>>();
        stamps.sort_unstable();
        stamps.dedup();

        for entry in self.entries.values_mut() {
            let rank = stamps
                .binary_search(&entry.last_used)
                .expect("cache recency stamp must be present")
                + 1;
            entry.last_used =
                u64::try_from(rank).expect("cache recency rank must fit in the counter");
        }
        self.recency =
            u64::try_from(stamps.len()).expect("cache entry count must fit in the counter");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_cache_capacities_bound_local_instances() {
        for capacity in [SEARCH_PAGE_CACHE_CAPACITY, THREAD_DETAIL_CACHE_CAPACITY] {
            let mut cache = BoundedLruCache::new(capacity);
            for key in 0..=capacity {
                cache.insert(key, key * 2);
                assert!(cache.len() <= capacity);
            }
            assert_eq!(cache.len(), capacity);
            assert_eq!(cache.get(&0), None);
            assert_eq!(cache.get(&capacity), Some(&(capacity * 2)));
        }
    }

    #[test]
    fn hit_refreshes_recency_before_eviction() {
        let mut cache = BoundedLruCache::new(2);
        cache.insert("first", 1);
        cache.insert("second", 2);

        assert_eq!(cache.get(&"first"), Some(&1));
        cache.insert("third", 3);

        assert_eq!(cache.get(&"second"), None);
        assert_eq!(cache.get(&"first"), Some(&1));
        assert_eq!(cache.get(&"third"), Some(&3));
    }

    #[test]
    fn miss_does_not_advance_recency() {
        let mut cache = BoundedLruCache::new(2);
        cache.insert("present", 1);
        let before = cache.recency;

        assert_eq!(cache.get(&"missing"), None);

        assert_eq!(cache.recency, before);
    }

    #[test]
    fn replacement_does_not_grow_and_refreshes_recency() {
        let mut cache = BoundedLruCache::new(2);
        assert_eq!(cache.insert("first", 1), None);
        assert_eq!(cache.insert("second", 2), None);

        assert_eq!(cache.insert("first", 10), Some(1));
        assert_eq!(cache.len(), 2);
        cache.insert("third", 3);

        assert_eq!(cache.get(&"first"), Some(&10));
        assert_eq!(cache.get(&"second"), None);
        assert_eq!(cache.get(&"third"), Some(&3));
    }

    #[test]
    fn clear_removes_entries_and_resets_recency() {
        let mut cache = BoundedLruCache::new(2);
        cache.insert("first", 1);
        cache.insert("second", 2);
        assert!(cache.recency > 0);

        cache.clear();

        assert_eq!(cache.len(), 0);
        assert_eq!(cache.recency, 0);
        assert_eq!(cache.get(&"first"), None);
    }

    #[test]
    fn repeated_hits_preserve_true_lru_order() {
        let mut cache = BoundedLruCache::new(3);
        cache.insert('a', 1);
        cache.insert('b', 2);
        cache.insert('c', 3);
        assert_eq!(cache.get(&'a'), Some(&1));
        assert_eq!(cache.get(&'b'), Some(&2));

        cache.insert('d', 4);

        assert_eq!(cache.get(&'c'), None);
        assert_eq!(cache.get(&'a'), Some(&1));
        assert_eq!(cache.get(&'b'), Some(&2));
        assert_eq!(cache.get(&'d'), Some(&4));
    }

    #[test]
    fn recency_overflow_rebases_without_changing_lru_order() {
        let mut cache = BoundedLruCache::new(3);
        cache.insert('a', 1);
        cache.insert('b', 2);
        cache.insert('c', 3);
        assert_eq!(cache.get(&'a'), Some(&1));
        cache.recency = u64::MAX;

        assert_eq!(cache.get(&'b'), Some(&2));
        cache.insert('d', 4);

        assert_eq!(cache.get(&'c'), None);
        assert_eq!(cache.get(&'a'), Some(&1));
        assert_eq!(cache.get(&'b'), Some(&2));
        assert_eq!(cache.get(&'d'), Some(&4));
    }

    #[test]
    #[should_panic(expected = "cache capacity must be nonzero")]
    fn zero_capacity_is_rejected() {
        let _ = BoundedLruCache::<u8, u8>::new(0);
    }
}
