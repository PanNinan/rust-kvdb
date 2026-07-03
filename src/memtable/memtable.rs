//! MemTable — wraps a SkipList with dual-buffer (active + immutable) semantics.
//!
//! Writes go to the `active` skip list. When its estimated size exceeds
//! `max_size` bytes, the active list is frozen into `immutable` and a fresh
//! active list is created.  At most one immutable exists at a time — the
//! engine (Step 1.8) is responsible for flushing it to an SSTable before
//! the next freeze.

use std::collections::BTreeMap;

use crate::memtable::skiplist::SkipList;

/// Default freeze threshold: 4 MiB.
const DEFAULT_MAX_SIZE: usize = 4 * 1024 * 1024;

/// An in-memory write buffer backed by a skip list.
///
/// Supports a single `immutable` snapshot that is waiting to be flushed
/// to disk as an SSTable.
pub struct MemTable {
    active: SkipList,
    immutable: Option<SkipList>,
    max_size: usize,
}

impl MemTable {
    /// Create a new, empty MemTable with the default 4 MiB threshold.
    pub fn new() -> Self {
        Self::with_max_size(DEFAULT_MAX_SIZE)
    }

    /// Create a new MemTable with a custom freeze threshold.
    pub fn with_max_size(max_size: usize) -> Self {
        MemTable {
            active: SkipList::new(16),
            immutable: None,
            max_size,
        }
    }

    /// Write a key-value pair into the active MemTable.
    ///
    /// If the active list exceeds `max_size` after the insert it is frozen
    /// into the immutable slot and a new active list is created.
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.active.insert(key, value);

        if self.active.size() > self.max_size {
            self.freeze();
        }
    }

    /// Insert into the active list without triggering a freeze.
    ///
    /// This is used during WAL replay at startup so that the MemTable
    /// doesn't try to flush mid-recovery.
    pub fn put_no_freeze(&mut self, key: &[u8], value: &[u8]) {
        self.active.insert(key, value);
    }

    /// Look up a key.  The active list is searched first (most recent writes),
    /// then the immutable list.
    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        if let Some(v) = self.active.get(key) {
            return Some(v);
        }
        if let Some(ref imm) = self.immutable {
            return imm.get(key);
        }
        None
    }

    /// Scan key-value pairs in the range `[start, end)`.
    ///
    /// Results from both active and immutable are merged; when a key exists
    /// in both, the active value (more recent) wins.
    pub fn scan(&self, start: &[u8], end: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

        // Insert immutable entries first (they are older).
        if let Some(ref imm) = self.immutable {
            for (k, v) in imm.scan(start, end) {
                map.insert(k, v);
            }
        }

        // Active entries overwrite immutable (same key → active wins).
        for (k, v) in self.active.scan(start, end) {
            map.insert(k, v);
        }

        map.into_iter().collect()
    }

    /// Freeze the active list into the immutable slot.
    ///
    /// Returns `true` if a freeze actually happened (active was non-empty
    /// and immutable was `None`).
    pub fn freeze(&mut self) -> bool {
        // Nothing to freeze if immutable is already occupied.
        if self.immutable.is_some() {
            return false;
        }

        let old_active = std::mem::replace(&mut self.active, SkipList::new(16));
        // Only freeze if there's something in the list.
        if old_active.size() <= std::mem::size_of::<SkipList>() {
            // The list is effectively empty — don't waste an immutable slot.
            // Put it back and signal no freeze.
            self.active = old_active;
            return false;
        }

        self.immutable = Some(old_active);
        true
    }

    /// Return the estimated size of the active MemTable in bytes.
    pub fn active_size(&self) -> usize {
        self.active.size()
    }

    /// Return the estimated size of the immutable MemTable, if one exists.
    pub fn immutable_size(&self) -> Option<usize> {
        self.immutable.as_ref().map(|s| s.size())
    }

    /// Whether an immutable MemTable is awaiting flush.
    pub fn has_immutable(&self) -> bool {
        self.immutable.is_some()
    }

    /// Take the immutable MemTable out, leaving `None` in its place.
    ///
    /// The engine calls this when it's ready to flush the frozen data
    /// to an SSTable.
    pub fn take_immutable(&mut self) -> Option<SkipList> {
        self.immutable.take()
    }
}

impl Default for MemTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memtable_put_and_get() {
        let mut mt = MemTable::with_max_size(1024);
        mt.put(b"hello", b"world");
        assert_eq!(mt.get(b"hello"), Some(b"world".to_vec()));
    }

    #[test]
    fn memtable_get_nonexistent() {
        let mt = MemTable::with_max_size(1024);
        assert_eq!(mt.get(b"nope"), None);
    }

    #[test]
    fn memtable_overwrite_key() {
        let mut mt = MemTable::with_max_size(1024);
        mt.put(b"k", b"v1");
        mt.put(b"k", b"v2");
        assert_eq!(mt.get(b"k"), Some(b"v2".to_vec()));
    }

    #[test]
    fn memtable_freeze_on_threshold() {
        // Use a very small threshold so a single insert triggers freeze.
        let mut mt = MemTable::with_max_size(20);
        mt.put(b"a", b"1");
        // After insert, if size > 20, freeze happened.
        // The key should still be readable (it moved to immutable).
        assert!(mt.has_immutable() || mt.active_size() <= 20);
    }

    #[test]
    fn memtable_get_from_immutable() {
        let mut mt = MemTable::with_max_size(1);
        // This will exceed threshold and freeze.
        mt.put(b"key_in_immutable", b"old_value");
        assert!(mt.has_immutable());

        // New write goes to active.
        mt.put(b"key_in_active", b"new_value");

        // Both keys should be readable.
        assert_eq!(mt.get(b"key_in_immutable"), Some(b"old_value".to_vec()));
        assert_eq!(mt.get(b"key_in_active"), Some(b"new_value".to_vec()));
    }

    #[test]
    fn memtable_scan_merges() {
        let mut mt = MemTable::with_max_size(1);

        // Force freeze: these go to immutable.
        mt.put(b"aaa", b"from_imm");
        mt.put(b"ccc", b"from_imm");

        // Now write to active (new active list).
        mt.put(b"bbb", b"from_active");
        // Overwrite a key that's in immutable — active should win.
        mt.put(b"ccc", b"from_active");

        let results = mt.scan(b"", b"\xff");
        let map: BTreeMap<_, _> = results.into_iter().collect();

        assert_eq!(map.get(b"aaa".as_slice()), Some(&b"from_imm".to_vec()));
        assert_eq!(map.get(b"bbb".as_slice()), Some(&b"from_active".to_vec()));
        assert_eq!(map.get(b"ccc".as_slice()), Some(&b"from_active".to_vec()));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn memtable_active_size_tracking() {
        let mut mt = MemTable::with_max_size(1024 * 1024);
        let before = mt.active_size();
        mt.put(b"key", b"value");
        assert!(mt.active_size() > before);
    }

    #[test]
    fn memtable_take_immutable() {
        let mut mt = MemTable::with_max_size(1);
        mt.put(b"k", b"v");
        assert!(mt.has_immutable());

        let taken = mt.take_immutable();
        assert!(taken.is_some());
        assert!(!mt.has_immutable());
    }
}
