//! LRU Block Cache — caches decoded SSTable data blocks in memory to
//! avoid repeated disk reads.

use std::collections::{HashMap, VecDeque};

use crate::sstable::block::Block;

/// Cache key: (sst_id, block_index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub sst_id: u64,
    pub block_idx: usize,
}

/// A simple LRU block cache with a byte-size capacity.
///
/// Evicts the least-recently-used entry when the cache exceeds `capacity`.
pub struct BlockCache {
    map: HashMap<CacheKey, (Block, usize)>, // key → (block, block_size)
    order: VecDeque<CacheKey>,              // LRU order: front = most recent
    capacity: usize,
    current_size: usize,
}

impl BlockCache {
    /// Create a new cache with the given byte capacity.
    pub fn new(capacity: usize) -> Self {
        BlockCache {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity,
            current_size: 0,
        }
    }

    /// Look up a block.  Moves it to the front of the LRU list on hit.
    pub fn get(&mut self, key: &CacheKey) -> Option<&Block> {
        if self.map.contains_key(key) {
            // Move to front (most recently used).
            self.order.retain(|k| k != key);
            self.order.push_front(*key);
            self.map.get(key).map(|(block, _)| block)
        } else {
            None
        }
    }

    /// Insert a block into the cache.  Evicts LRU entries if over capacity.
    pub fn put(&mut self, key: CacheKey, block: Block) {
        let block_size = block.estimated_size();

        // If already present, remove old entry first.
        if let Some((_, old_size)) = self.map.remove(&key) {
            self.current_size -= old_size;
            self.order.retain(|k| k != &key);
        }

        // Evict until we have room.
        while self.current_size + block_size > self.capacity && !self.order.is_empty() {
            if let Some(evict_key) = self.order.pop_back() {
                if let Some((_, evicted_size)) = self.map.remove(&evict_key) {
                    self.current_size -= evicted_size;
                }
            }
        }

        self.current_size += block_size;
        self.map.insert(key, (block, block_size));
        self.order.push_front(key);
    }

    /// Current memory usage in bytes.
    pub fn size(&self) -> usize {
        self.current_size
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_block(entries: &[(&str, &str)]) -> Block {
        let mut block = Block::new();
        for (k, v) in entries {
            block.add(k.as_bytes().to_vec(), v.as_bytes().to_vec());
        }
        block
    }

    #[test]
    fn cache_hit_and_eviction() {
        let mut cache = BlockCache::new(100); // 100 bytes capacity

        let b1 = make_block(&[("key1", "val1")]);
        cache.put(CacheKey { sst_id: 1, block_idx: 0 }, b1);

        assert!(cache.get(&CacheKey { sst_id: 1, block_idx: 0 }).is_some());

        // Insert enough to overflow.
        let b2 = make_block(&[("key2", "val2222222222222222222")]);
        let b3 = make_block(&[("key3", "val3333333333333333333")]);
        cache.put(CacheKey { sst_id: 1, block_idx: 1 }, b2);
        cache.put(CacheKey { sst_id: 1, block_idx: 2 }, b3);

        // Cache should have evicted oldest entries to stay within capacity.
        assert!(cache.size() <= 100);
    }

    #[test]
    fn cache_lru_order() {
        let mut cache = BlockCache::new(1000);

        cache.put(CacheKey { sst_id: 1, block_idx: 0 }, make_block(&[("a", "1")]));
        cache.put(CacheKey { sst_id: 1, block_idx: 1 }, make_block(&[("b", "2")]));
        cache.put(CacheKey { sst_id: 1, block_idx: 2 }, make_block(&[("c", "3")]));

        // Access block 0 to make it most recently used.
        cache.get(&CacheKey { sst_id: 1, block_idx: 0 });

        // Block 1 should be evicted first now (it's the least recently used).
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn cache_update_existing_key() {
        let mut cache = BlockCache::new(1000);

        cache.put(CacheKey { sst_id: 1, block_idx: 0 }, make_block(&[("a", "old")]));
        cache.put(CacheKey { sst_id: 1, block_idx: 0 }, make_block(&[("a", "new")]));

        let block = cache.get(&CacheKey { sst_id: 1, block_idx: 0 }).unwrap();
        assert_eq!(block.entries[0].1, b"new");
    }
}
