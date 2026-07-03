//! Skip List — a probabilistic sorted data structure used as the core
//! of the in-memory MemTable.
//!
//! All `unsafe` pointer operations are confined to this module.
//! The public API is entirely safe.

use std::ptr;

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

/// A single node in the skip list.
///
/// `next[i]` points to the next node at level `i`. A `null` pointer means
/// there is no successor at that level (end of the lane).
struct Node {
    key: Vec<u8>,
    value: Vec<u8>,
    next: Vec<*mut Node>,
}

impl Node {
    /// Allocate a new node on the heap and return a raw pointer to it.
    fn new(key: Vec<u8>, value: Vec<u8>, level: usize) -> *mut Node {
        Box::into_raw(Box::new(Node {
            key,
            value,
            next: vec![ptr::null_mut(); level + 1],
        }))
    }

    /// # Safety
    /// `node` must be a valid, heap-allocated pointer returned by `Node::new`.
    unsafe fn free(node: *mut Node) {
        drop(Box::from_raw(node));
    }

    /// Get a shared reference to the node at `next[level]`.
    ///
    /// # Safety
    /// `self` must be valid (i.e. `this` came from a non-null, properly
    /// aligned pointer that was previously returned by `Node::new`).
    unsafe fn next_at(this: *const Node, level: usize) -> *mut Node {
        (&(*this).next)[level]
    }

    /// Set `next[level]` to `target`.
    ///
    /// # Safety
    /// `this` must be a valid, live, mutable pointer.
    unsafe fn set_next(this: *mut Node, level: usize, target: *mut Node) {
        (&mut (*this).next)[level] = target;
    }
}

// ---------------------------------------------------------------------------
// SkipList
// ---------------------------------------------------------------------------

/// A probabilistic sorted map backed by a skip list.
///
/// Keys are `&[u8]` byte slices sorted lexicographically.
/// Inserting an existing key updates the value (upsert semantics).
pub struct SkipList {
    head: *mut Node,
    max_level: usize,
    size: usize, // estimated memory usage in bytes
}

// SkipList is not Sync because it uses raw pointers without interior
// mutability.  It is safe to move between threads (Send) since we never
// share it across threads in Phase 1.
unsafe impl Send for SkipList {}

impl SkipList {
    /// Create a new, empty skip list.
    ///
    /// `max_level` controls the maximum height of the tower (typical: 16).
    /// Each level halves the expected number of nodes, giving O(log n) lookups.
    pub fn new(max_level: usize) -> Self {
        assert!(max_level > 0, "max_level must be at least 1");
        let head = Node::new(Vec::new(), Vec::new(), max_level - 1);
        SkipList {
            head,
            max_level,
            size: std::mem::size_of::<Self>(),
        }
    }

    /// Insert or update a key-value pair.
    ///
    /// If the key already exists its value is overwritten.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) {
        let level = self.random_level();
        let new_node = Node::new(key.to_vec(), value.to_vec(), level);

        // Track the predecessor at each level.
        let mut update: Vec<*mut Node> = vec![self.head; self.max_level];
        let mut current = self.head;

        unsafe {
            // Walk from the highest level down to find the insertion point.
            for i in (0..self.max_level).rev() {
                while !Node::next_at(current, i).is_null()
                    && (&*Node::next_at(current, i)).key.as_slice() < key
                {
                    current = Node::next_at(current, i);
                }
                update[i] = current;
            }

            // The level-0 successor — may be the same key (upsert).
            let existing = Node::next_at(current, 0);

            if !existing.is_null() && (&*existing).key.as_slice() == key {
                // Upsert: overwrite value in-place, then free the new node.
                (&mut *existing).value = value.to_vec();
                Node::free(new_node);
                return;
            }

            // Splice the new node into each lane up to `level`.
            for (i, pred) in update.iter().enumerate().take(level + 1) {
                Node::set_next(new_node, i, Node::next_at(*pred, i));
                Node::set_next(*pred, i, new_node);
            }
        }

        // Update size tracking:
        // key + value + Vec overhead per level + node header
        self.size += key.len()
            + value.len()
            + (level + 1) * std::mem::size_of::<*mut Node>()
            + std::mem::size_of::<Node>()
            + 2 * std::mem::size_of::<Vec<u8>>(); // key/value Vec overhead
    }

    /// Look up a key and return a copy of its value, or `None`.
    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut current = self.head;

        unsafe {
            for i in (0..self.max_level).rev() {
                while !Node::next_at(current, i).is_null()
                    && (&*Node::next_at(current, i)).key.as_slice() < key
                {
                    current = Node::next_at(current, i);
                }
            }

            let candidate = Node::next_at(current, 0);
            if !candidate.is_null() && (&*candidate).key.as_slice() == key {
                Some((&*candidate).value.clone())
            } else {
                None
            }
        }
    }

    /// Return an iterator over key-value pairs where `start <= key < end`.
    ///
    /// Both bounds are byte-slice comparisons.  Pass `b""` for the start to
    /// begin at the very first key, and `b"\xff"` (or similar) for the end
    /// to include everything.
    pub fn scan<'a>(&self, start: &'a [u8], end: &'a [u8]) -> ScanIterator<'a> {
        let mut current = self.head;

        unsafe {
            // Navigate to the first node >= start.
            for i in (0..self.max_level).rev() {
                while !Node::next_at(current, i).is_null()
                    && (&*Node::next_at(current, i)).key.as_slice() < start
                {
                    current = Node::next_at(current, i);
                }
            }
            // current is the last node whose key < start, so next[0] is the
            // first node >= start (if any).
            current = Node::next_at(current, 0);
        }

        ScanIterator { current, end }
    }

    /// Return an iterator over all key-value pairs in sorted order.
    pub fn iter(&self) -> FullIterator {
        let start = unsafe { Node::next_at(self.head, 0) };
        FullIterator { current: start }
    }

    /// Estimated memory usage of this skip list in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Pick a random level for a new node.
    ///
    /// Uses a simple xorshift64 PRNG (thread-local) to avoid pulling in an
    /// external random crate.  Each level has ~1/2 probability, capped at
    /// `max_level - 1`.
    fn random_level(&self) -> usize {
        use std::cell::Cell;

        thread_local! {
            static STATE: Cell<u64> = const { Cell::new(0xdeadbeef_cafebabe) };
        }

        STATE.with(|s| {
            let mut v = s.get();
            // xorshift64
            v ^= v << 13;
            v ^= v >> 7;
            v ^= v << 17;
            s.set(v);

            // Count trailing zero bits in a non-zero value to pick level.
            // Each bit has 1/2 probability → geometric distribution.
            let bits = v | 1; // ensure non-zero
            let trailing = bits.trailing_zeros() as usize;
            trailing.min(self.max_level - 1)
        })
    }
}

// ---------------------------------------------------------------------------
// Drop — walk level-0 and free every node
// ---------------------------------------------------------------------------

impl Drop for SkipList {
    fn drop(&mut self) {
        unsafe {
            let mut current = Node::next_at(self.head, 0);
            // Free the sentinel head.
            Node::free(self.head);

            // Walk the bottom lane and free each data node.
            while !current.is_null() {
                let next = Node::next_at(current, 0);
                Node::free(current);
                current = next;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ScanIterator
// ---------------------------------------------------------------------------

/// An iterator that yields `(key, value)` pairs from a skip list scan.
pub struct ScanIterator<'a> {
    current: *const Node,
    end: &'a [u8],
}

impl<'a> Iterator for ScanIterator<'a> {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if self.current.is_null() {
                return None;
            }

            let node = &*self.current;

            // Stop when we reach or pass the exclusive end bound.
            if node.key.as_slice() >= self.end {
                return None;
            }

            let result = (node.key.clone(), node.value.clone());
            self.current = Node::next_at(self.current, 0);
            Some(result)
        }
    }
}

// ---------------------------------------------------------------------------
// FullIterator — yields all entries without an upper bound
// ---------------------------------------------------------------------------

/// An unbounded iterator over all entries in a skip list.
pub struct FullIterator {
    current: *const Node,
}

impl Iterator for FullIterator {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if self.current.is_null() {
                return None;
            }
            let node = &*self.current;
            let result = (node.key.clone(), node.value.clone());
            self.current = Node::next_at(self.current, 0);
            Some(result)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skiplist_insert_and_get() {
        let mut list = SkipList::new(16);
        list.insert(b"key1", b"value1");
        assert_eq!(list.get(b"key1"), Some(b"value1".to_vec()));
    }

    #[test]
    fn skiplist_get_nonexistent_returns_none() {
        let mut list = SkipList::new(16);
        list.insert(b"key1", b"value1");
        assert_eq!(list.get(b"missing"), None);
    }

    #[test]
    fn skiplist_update_existing_key() {
        let mut list = SkipList::new(16);
        list.insert(b"key1", b"v1");
        list.insert(b"key1", b"v2");
        assert_eq!(list.get(b"key1"), Some(b"v2".to_vec()));
    }

    #[test]
    fn skiplist_scan_range() {
        let mut list = SkipList::new(16);
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            list.insert(key.as_bytes(), format!("val_{}", i).as_bytes());
        }
        let results: Vec<_> = list.scan(b"key_0020", b"key_0030").collect();
        assert_eq!(results.len(), 10);
        assert_eq!(results[0].0, b"key_0020");
        assert_eq!(results[9].0, b"key_0029");
    }

    #[test]
    fn skiplist_ordering_is_sorted() {
        let mut list = SkipList::new(16);
        let keys: Vec<String> = (0..1000).map(|i| format!("key_{:06}", i)).collect();
        // Insert in reverse order.
        for key in keys.iter().rev() {
            list.insert(key.as_bytes(), b"v");
        }
        let collected: Vec<_> = list.scan(b"", b"\xff").map(|(k, _)| k).collect();
        for w in collected.windows(2) {
            assert!(w[0] <= w[1], "keys not sorted");
        }
        assert_eq!(collected.len(), 1000);
    }

    #[test]
    fn skiplist_size_tracking() {
        let mut list = SkipList::new(16);
        let before = list.size();
        list.insert(b"key", b"value");
        assert!(list.size() > before, "size should increase after insert");
    }

    #[test]
    fn skiplist_empty_scan() {
        let list = SkipList::new(16);
        let results: Vec<_> = list.scan(b"a", b"z").collect();
        assert!(results.is_empty());
    }

    #[test]
    fn skiplist_scan_open_ended() {
        let mut list = SkipList::new(16);
        list.insert(b"a", b"1");
        list.insert(b"b", b"2");
        list.insert(b"c", b"3");

        // Scan from "b" onwards with a high end bound.
        let results: Vec<_> = list.scan(b"b", b"\xff").collect();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"b");
        assert_eq!(results[1].0, b"c");
    }
}
