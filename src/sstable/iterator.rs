//! SSTable iterator for sequential scanning.
//!
//! In Phase 1 the iterator lives in `reader.rs` as `SSTableIterator`.
//! This module will host more advanced iterators (e.g. merge iterators
//! for compaction) in Phase 2.
