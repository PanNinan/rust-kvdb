//! Multi-way merge iterator — merges multiple sorted iterators into a
//! single sorted stream.
//!
//! Used for compaction and SCAN operations.

use std::collections::BinaryHeap;
use std::cmp::Reverse;

/// Type alias for the boxed iterators used as merge sources.
type SourceIter = Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>;

/// A merged iterator that yields entries from multiple sorted sources
/// in key order.  When keys are equal, the source with the lowest index
/// wins (i.e. the most recent source should be placed first).
pub struct MergeIterator {
    heap: BinaryHeap<Reverse<HeapEntry>>,
    sources: Vec<SourceIter>,
}

#[derive(Debug, Eq, PartialEq)]
struct HeapEntry {
    key: Vec<u8>,
    value: Vec<u8>,
    source: usize,
}

// Manual Ord impl: compare by key (lexicographic), then by source index.
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key
            .cmp(&other.key)
            .then_with(|| self.source.cmp(&other.source))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl MergeIterator {
    /// Create a new merge iterator from a list of sorted iterators.
    ///
    /// Sources are indexed 0..N.  On key ties, the lower-indexed source wins.
    /// Place the most recent source (e.g. MemTable) at index 0.
    pub fn new(sources: Vec<SourceIter>) -> Self {
        let mut heap = BinaryHeap::new();

        // Seed the heap with one entry per non-empty source.
        let mut sources = sources;
        for (idx, source) in sources.iter_mut().enumerate() {
            if let Some((key, value)) = source.next() {
                heap.push(Reverse(HeapEntry {
                    key,
                    value,
                    source: idx,
                }));
            }
        }

        MergeIterator { heap, sources }
    }
}

impl Iterator for MergeIterator {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        let Reverse(entry) = self.heap.pop()?;
        // Advance the source that produced this entry.
        if let Some((key, value)) = self.sources[entry.source].next() {
            self.heap.push(Reverse(HeapEntry {
                key,
                value,
                source: entry.source,
            }));
        }
        Some((entry.key, entry.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_iter(data: Vec<(&str, &str)>) -> Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>> {
        let owned: Vec<(Vec<u8>, Vec<u8>)> = data
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect();
        Box::new(owned.into_iter())
    }

    #[test]
    fn merge_two_iterators() {
        let s1 = vec_iter(vec![("a", "1"), ("c", "3"), ("e", "5")]);
        let s2 = vec_iter(vec![("b", "2"), ("d", "4"), ("f", "6")]);

        let merged: Vec<_> = MergeIterator::new(vec![s1, s2])
            .map(|(k, v)| (String::from_utf8(k).unwrap(), String::from_utf8(v).unwrap()))
            .collect();

        assert_eq!(
            merged,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "3".into()),
                ("d".into(), "4".into()),
                ("e".into(), "5".into()),
                ("f".into(), "6".into()),
            ]
        );
    }

    #[test]
    fn merge_deduplicates_first_source_wins() {
        // Same key "x" in both sources — source 0 comes first in sort order.
        // The merge iterator yields all entries; dedup is the consumer's job.
        let s1 = vec_iter(vec![("x", "new"), ("y", "2")]);
        let s2 = vec_iter(vec![("x", "old"), ("z", "3")]);

        let merged: Vec<_> = MergeIterator::new(vec![s1, s2])
            .map(|(k, v)| (String::from_utf8(k).unwrap(), String::from_utf8(v).unwrap()))
            .collect();

        // All 4 entries are yielded; source 0's "x" comes first.
        assert_eq!(merged[0], ("x".into(), "new".into()));
        assert_eq!(merged[1], ("x".into(), "old".into()));
        assert_eq!(merged.len(), 4); // x(new), x(old), y, z
    }

    #[test]
    fn merge_empty_sources() {
        let s1 = vec_iter(vec![]);
        let s2 = vec_iter(vec![]);
        let merged: Vec<_> = MergeIterator::new(vec![s1, s2]).collect();
        assert!(merged.is_empty());
    }

    #[test]
    fn merge_single_source() {
        let s1 = vec_iter(vec![("a", "1"), ("b", "2")]);
        let merged: Vec<_> = MergeIterator::new(vec![s1]).collect();
        assert_eq!(merged.len(), 2);
    }
}
