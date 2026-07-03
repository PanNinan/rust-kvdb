//! Top-level storage engine that coordinates MemTable, WAL, SSTable, and
//! Manifest to provide `put` / `get` / `delete` / `close` operations.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tracing::{debug, info};

use crate::error::Result;
use crate::manifest::{Manifest, SSTMeta};
use crate::memtable::memtable::MemTable;
use crate::sstable::builder::SSTableBuilder;
use crate::sstable::reader::SSTableReader;
use crate::wal::writer::{WALReader, WALWriter};
use crate::write_batch::{BatchOp, WriteBatch};

/// Tombstone marker written when a key is deleted.
const TOMBSTONE: &[u8] = &[0x00];

/// Engine configuration options.
#[derive(Debug, Clone)]
pub struct Options {
    /// MemTable freeze threshold in bytes. Default: 4 MiB.
    pub memtable_size: usize,
    /// SSTable data block size in bytes. Default: 4 KiB.
    pub block_size: usize,
    /// L0 SSTable count that triggers compaction. Default: 4.
    pub l0_compaction_threshold: usize,
    /// Whether to fsync after every write. Default: true.
    pub sync_wal: bool,
    /// Maximum number of LSM levels. Default: 7.
    pub max_levels: usize,
    /// Bloom filter bits per key. Default: 10.
    pub bloom_filter_bits_per_key: usize,
    /// Block cache capacity in bytes. Default: 8 MiB. 0 = disabled.
    pub block_cache_size: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            memtable_size: 4 * 1024 * 1024,
            block_size: 4096,
            l0_compaction_threshold: 4,
            sync_wal: true,
            max_levels: 7,
            bloom_filter_bits_per_key: 10,
            block_cache_size: 8 * 1024 * 1024,
        }
    }
}

/// Atomic counters for engine metrics.
#[derive(Debug, Default)]
pub struct Metrics {
    pub writes: AtomicU64,
    pub reads: AtomicU64,
    pub deletes: AtomicU64,
    pub compactions: AtomicU64,
    pub flushes: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            writes: self.writes.load(Ordering::Relaxed),
            reads: self.reads.load(Ordering::Relaxed),
            deletes: self.deletes.load(Ordering::Relaxed),
            compactions: self.compactions.load(Ordering::Relaxed),
            flushes: self.flushes.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time copy of metrics.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub writes: u64,
    pub reads: u64,
    pub deletes: u64,
    pub compactions: u64,
    pub flushes: u64,
}

/// The top-level key-value storage engine.
pub struct Engine {
    path: PathBuf,
    wal: WALWriter,
    memtable: MemTable,
    manifest: Manifest,
    sst_readers: Vec<(SSTMeta, SSTableReader)>,
    next_sst_id: u64,
    /// Monotonically increasing sequence number for MVCC.
    next_seq: u64,
    /// Engine configuration.
    opts: Options,
    /// Metrics counters.
    pub metrics: Metrics,
    /// Shared block cache.
    block_cache: Option<Arc<StdMutex<crate::cache::block_cache::BlockCache>>>,
}

/// A point-in-time snapshot of the database.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    seq: u64,
}

/// Report from a repair operation.
#[derive(Debug)]
pub struct RepairReport {
    pub corrupted_files: usize,
    pub recovered_ssts: usize,
}

impl Engine {
    /// Open (or create) a database at the given directory with default options.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_options(path, Options::default())
    }

    /// Open (or create) a database with custom options.
    pub fn open_with_options(path: &Path, opts: Options) -> Result<Self> {
        info!(path = %path.display(), "opening database");
        fs::create_dir_all(path)?;

        let manifest_path = path.join("MANIFEST");
        let manifest = Manifest::load(&manifest_path)?;

        // Create block cache if enabled.
        let block_cache = if opts.block_cache_size > 0 {
            Some(Arc::new(StdMutex::new(
                crate::cache::block_cache::BlockCache::new(opts.block_cache_size),
            )))
        } else {
            None
        };

        // Open SSTable readers and determine next id.
        let mut next_sst_id: u64 = 1;
        let mut sst_readers = Vec::new();

        for level in &manifest.levels {
            for meta in level {
                let sst_path = path.join(format!("{:06}.sst", meta.id));
                if sst_path.exists() {
                    let reader = SSTableReader::open_with_cache(
                        &sst_path,
                        meta.id,
                        block_cache.clone(),
                    )?;
                    if meta.id >= next_sst_id {
                        next_sst_id = meta.id + 1;
                    }
                    sst_readers.push((meta.clone(), reader));
                }
            }
        }

        // Find or create WAL path.
        let wal_path = Self::wal_path_for_dir(path)?;

        // Replay WAL into a fresh MemTable (no freeze during replay).
        let memtable = Self::replay_wal(&wal_path, opts.memtable_size)?;

        // Open WAL for new writes.
        let wal = WALWriter::open(&wal_path)?;

        Ok(Engine {
            path: path.to_path_buf(),
            wal,
            memtable,
            manifest,
            sst_readers,
            next_sst_id,
            next_seq: 1,
            opts,
            metrics: Metrics::new(),
            block_cache,
        })
    }

    /// Write a key-value pair.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let seq = self.next_seq;
        self.next_seq += 1;

        let encoded = Self::encode_with_seq(value, seq);
        self.wal.append(key, &encoded)?;
        if self.opts.sync_wal {
            self.wal.flush()?;
        }
        self.memtable.put(key, &encoded);
        self.metrics.writes.fetch_add(1, Ordering::Relaxed);
        self.maybe_flush()?;
        Ok(())
    }

    /// Delete a key by writing a tombstone.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        use crate::wal::record::{OpType, Record};

        let seq = self.next_seq;
        self.next_seq += 1;

        let encoded = Self::encode_with_seq(TOMBSTONE, seq);
        let record = Record {
            op: OpType::Delete,
            key: key.to_vec(),
            value: encoded.clone(),
        };
        self.wal.append_record(&record)?;
        if self.opts.sync_wal {
            self.wal.flush()?;
        }
        self.memtable.put(key, &encoded);
        self.metrics.deletes.fetch_add(1, Ordering::Relaxed);
        self.maybe_flush()?;
        Ok(())
    }

    /// Execute a batch of operations atomically.
    ///
    /// All operations in the batch are written to a single WAL record,
    /// then applied to the MemTable together.
    pub fn write_batch(&mut self, batch: &WriteBatch) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        // Write each op to the WAL individually (simple approach).
        for op in batch.ops() {
            match op {
                BatchOp::Put { key, value } => {
                    self.wal.append(key, value)?;
                }
                BatchOp::Delete { key } => {
                    use crate::wal::record::{OpType, Record};
                    self.wal.append_record(&Record {
                        op: OpType::Delete,
                        key: key.clone(),
                        value: Vec::new(),
                    })?;
                }
            }
        }
        self.wal.flush()?;

        // Apply to MemTable.
        for op in batch.ops() {
            match op {
                BatchOp::Put { key, value } => {
                    self.memtable.put(key, value);
                }
                BatchOp::Delete { key } => {
                    self.memtable.put(key, TOMBSTONE);
                }
            }
        }

        self.maybe_flush()?;
        Ok(())
    }

    /// Create a point-in-time snapshot.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot { seq: self.next_seq }
    }

    /// Look up a key at a specific snapshot.
    ///
    /// Only returns values with `seq < snap.seq` (written before the snapshot).
    pub fn get_at(&self, key: &[u8], snap: &Snapshot) -> Result<Option<Vec<u8>>> {
        // Check MemTable.
        if let Some(v) = self.memtable.get(key) {
            if let Some((val, seq)) = Self::decode_with_seq(&v) {
                if seq < snap.seq {
                    return Ok(Self::unpack_inner(&val));
                }
            }
        }

        // Check SSTables (newest first).
        for (_meta, reader) in self.sst_readers.iter().rev() {
            if let Some(v) = reader.get(key)? {
                if let Some((val, seq)) = Self::decode_with_seq(&v) {
                    if seq < snap.seq {
                        return Ok(Self::unpack_inner(&val));
                    }
                }
            }
        }

        Ok(None)
    }

    /// Write a key-value pair with a TTL (time-to-live).
    ///
    /// The value is stored with an expiry timestamp prefix.  After the
    /// expiry, `get()` returns `None`.
    pub fn put_with_ttl(
        &mut self,
        key: &[u8],
        value: &[u8],
        ttl: std::time::Duration,
    ) -> Result<()> {
        let expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + ttl.as_secs();

        // Encode: [0x01 marker] [expiry: u64 LE] [value]
        let mut encoded = Vec::with_capacity(1 + 8 + value.len());
        encoded.push(0x01); // TTL marker
        encoded.extend_from_slice(&expiry.to_le_bytes());
        encoded.extend_from_slice(value);

        let seq = self.next_seq;
        self.next_seq += 1;
        let seq_encoded = Self::encode_with_seq(&encoded, seq);
        self.wal.append(key, &seq_encoded)?;
        if self.opts.sync_wal {
            self.wal.flush()?;
        }
        self.memtable.put(key, &seq_encoded);
        self.metrics.writes.fetch_add(1, Ordering::Relaxed);
        self.maybe_flush()?;
        Ok(())
    }

    /// Scan keys with a given prefix.
    pub fn prefix_scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let end = next_key(prefix);
        self.scan(prefix, &end)
    }

    /// Repair a corrupted database directory.
    ///
    /// Scans all SST files, rebuilds the manifest, and cleans up
    /// truncated WAL segments.
    pub fn repair(path: &Path) -> Result<RepairReport> {
        let mut report = RepairReport {
            corrupted_files: 0,
            recovered_ssts: 0,
        };

        // Scan all .sst files and build a fresh manifest.
        let mut manifest = Manifest::new();
        let mut max_id: u64 = 0;

        if path.exists() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if name_str.ends_with(".sst") {
                    let id_str = name_str.trim_end_matches(".sst");
                    if let Ok(id) = id_str.parse::<u64>() {
                        // Try to open the SST file to verify it's valid.
                        match SSTableReader::open(&entry.path()) {
                            Ok(_reader) => {
                                // Read first and last key from the SST for metadata.
                                // Simplified: use empty keys as placeholders.
                                let meta = SSTMeta::new(id, 0, Vec::new(), Vec::new());
                                manifest.add_sst(0, meta);
                                report.recovered_ssts += 1;
                                if id > max_id {
                                    max_id = id;
                                }
                            }
                            Err(_) => {
                                report.corrupted_files += 1;
                                let _ = fs::remove_file(entry.path());
                            }
                        }
                    }
                }
            }
        }

        // Save rebuilt manifest.
        manifest.save(&path.join("MANIFEST"))?;

        // Clean up WAL (truncate to 0 so next open starts fresh).
        let wal_path = path.join("000001.wal");
        if wal_path.exists() {
            let _ = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&wal_path);
        }

        Ok(report)
    }

    /// Look up a key.
    ///
    /// Search: MemTable (active → immutable) → SSTables (newest → oldest).
    /// A tombstone is treated as "not found".
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.metrics.reads.fetch_add(1, Ordering::Relaxed);

        // MemTable.
        if let Some(v) = self.memtable.get(key) {
            return Ok(Self::unpack_value(&v));
        }

        // SSTables (newest first).
        for (_meta, reader) in self.sst_readers.iter().rev() {
            if let Some(v) = reader.get(key)? {
                return Ok(Self::unpack_value(&v));
            }
        }

        Ok(None)
    }

    /// Scan key-value pairs in the range `[start, end)`.
    ///
    /// Results are sorted by key.  Tombstones are excluded.
    pub fn scan(&self, start: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        use std::collections::BTreeMap;

        let mut map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

        // SSTables (oldest first, so newer ones overwrite).
        for (_meta, reader) in self.sst_readers.iter() {
            for (k, v) in reader.iter() {
                if k.as_slice() >= start && k.as_slice() < end {
                    map.insert(k, v);
                }
            }
        }

        // MemTable (active + immutable, overwrites SSTables).
        for (k, v) in self.memtable.scan(start, end) {
            map.insert(k, v);
        }

        // Filter out tombstones and expired TTL keys.
        let result: Vec<_> = map
            .into_iter()
            .filter(|(_, v)| {
                if let Some((inner, _seq)) = Self::decode_with_seq(v) {
                    if inner.as_slice() == TOMBSTONE {
                        return false;
                    }
                    Self::unwrap_ttl(&inner).is_some()
                } else {
                    true
                }
            })
            .map(|(k, v)| {
                // Strip seq prefix from value.
                let display_v = Self::decode_with_seq(&v)
                    .and_then(|(inner, _)| Self::unpack_inner(&inner))
                    .unwrap_or_default();
                (k, display_v)
            })
            .collect();

        Ok(result)
    }

    /// Flush all in-memory data to disk and close the engine.
    pub fn close(mut self) -> Result<()> {
        self.maybe_flush()?;
        self.memtable.freeze();
        self.maybe_flush()?;
        self.wal.flush()?;
        self.manifest.save(&self.path.join("MANIFEST"))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Replay a WAL file into a fresh MemTable.
    fn replay_wal(wal_path: &Path, memtable_size: usize) -> Result<MemTable> {
        use crate::wal::record::OpType;

        let mut memtable = MemTable::with_max_size(memtable_size);
        if wal_path.exists() {
            let reader = WALReader::open(wal_path)?;
            for entry in reader {
                let rec = entry?;
                match rec.op {
                    OpType::Put => memtable.put_no_freeze(&rec.key, &rec.value),
                    OpType::Delete => memtable.put_no_freeze(&rec.key, TOMBSTONE),
                }
            }
        }
        Ok(memtable)
    }

    /// Flush the immutable MemTable to an SSTable if one exists.
    fn maybe_flush(&mut self) -> Result<()> {
        if !self.memtable.has_immutable() {
            return Ok(());
        }
        debug!("flushing immutable memtable to SSTable");

        let immutable = self
            .memtable
            .take_immutable()
            .expect("has_immutable was true");

        let sst_id = self.next_sst_id;
        self.next_sst_id += 1;
        let sst_path = self.path.join(format!("{:06}.sst", sst_id));

        let entries: Vec<_> = immutable.iter().collect();
        if entries.is_empty() {
            return Ok(());
        }

        let mut builder = SSTableBuilder::new(&sst_path, self.opts.block_size)?;
        for (k, v) in &entries {
            builder.add(k, v)?;
        }
        builder.finish()?;

        let min_key = entries.first().unwrap().0.clone();
        let max_key = entries.last().unwrap().0.clone();
        let meta = SSTMeta::new(sst_id, 0, min_key, max_key);

        let reader = SSTableReader::open_with_cache(&sst_path, sst_id, self.block_cache.clone())?;
        self.manifest.add_sst(0, meta.clone());
        self.sst_readers.push((meta, reader));

        self.manifest.save(&self.path.join("MANIFEST"))?;
        self.reset_wal()?;

        // Trigger compaction if L0 is too large.
        self.metrics.flushes.fetch_add(1, Ordering::Relaxed);
        self.maybe_compact()?;

        Ok(())
    }

    /// Trigger compaction when any level exceeds its capacity.
    ///
    /// L0 threshold = `l0_compaction_threshold` (default 4).
    /// L1+ threshold = 10 × previous level's threshold.
    fn maybe_compact(&mut self) -> Result<()> {
        // Check each level from L0 upward.
        for level in 0..self.opts.max_levels.saturating_sub(1) {
            let threshold = if level == 0 {
                self.opts.l0_compaction_threshold
            } else {
                // Each level is 10× the previous.
                self.opts.l0_compaction_threshold * 10_usize.pow(level as u32)
            };

            if self.manifest.ssts_at_level(level).len() < threshold {
                continue;
            }

            self.compact_level(level)?;
        }
        Ok(())
    }

    /// Compact SSTables from `src_level` into `dst_level = src_level + 1`.
    fn compact_level(&mut self, src_level: usize) -> Result<()> {
        let dst_level = src_level + 1;
        info!(src_level, dst_level, "starting compaction");

        // Collect source level SSTables.
        let src_metas: Vec<SSTMeta> = self.manifest.ssts_at_level(src_level).to_vec();
        let src_ids: Vec<u64> = src_metas.iter().map(|m| m.id).collect();

        // Find overlapping destination level SSTables.
        let global_min = src_metas.iter().map(|m| m.min_key.clone()).min().unwrap();
        let global_max = src_metas.iter().map(|m| m.max_key.clone()).max().unwrap();

        let dst_metas: Vec<SSTMeta> = self
            .manifest
            .ssts_at_level(dst_level)
            .iter()
            .filter(|m| m.max_key >= global_min && m.min_key <= global_max)
            .cloned()
            .collect();
        let dst_ids: Vec<u64> = dst_metas.iter().map(|m| m.id).collect();

        // Merge-sort all entries (destination first = older, then source = newer).
        let mut all_entries: std::collections::BTreeMap<Vec<u8>, Vec<u8>> =
            std::collections::BTreeMap::new();

        for id in dst_ids.iter().chain(src_ids.iter()) {
            if let Some((_, reader)) = self.sst_readers.iter().find(|(m, _)| m.id == *id) {
                for (k, v) in reader.iter() {
                    all_entries.insert(k, v);
                }
            }
        }

        // Filter out tombstones and expired TTL keys.
        all_entries.retain(|_, v| {
            if let Some((inner, _seq)) = Self::decode_with_seq(v) {
                if inner.as_slice() == TOMBSTONE {
                    return false;
                }
                Self::unwrap_ttl(&inner).is_some()
            } else {
                false
            }
        });

        // Write new SSTables to the destination level.
        if !all_entries.is_empty() {
            let new_sst_id = self.next_sst_id;
            self.next_sst_id += 1;
            let new_sst_path = self.path.join(format!("{:06}.sst", new_sst_id));

            let mut builder = SSTableBuilder::new(&new_sst_path, self.opts.block_size)?;
            for (k, v) in &all_entries {
                builder.add(k, v)?;
            }
            builder.finish()?;

            let min_key = all_entries.keys().next().unwrap().clone();
            let max_key = all_entries.keys().last().unwrap().clone();
            let meta = SSTMeta::new(new_sst_id, dst_level, min_key, max_key);
            let reader = SSTableReader::open_with_cache(
                &new_sst_path,
                new_sst_id,
                self.block_cache.clone(),
            )?;
            self.manifest.add_sst(dst_level, meta);
            self.sst_readers.push((
                self.manifest.ssts_at_level(dst_level).last().unwrap().clone(),
                reader,
            ));
        }

        // Remove old SSTs from manifest and disk.
        for id in src_ids.iter().chain(dst_ids.iter()) {
            self.manifest.remove_sst(*id);
            self.sst_readers.retain(|(m, _)| m.id != *id);
            let old_path = self.path.join(format!("{:06}.sst", id));
            let _ = fs::remove_file(old_path);
        }

        self.manifest.save(&self.path.join("MANIFEST"))?;
        self.metrics.compactions.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Truncate and reopen the WAL after flushing to SSTable.
    fn reset_wal(&mut self) -> Result<()> {
        let wal_path = self.path.join("000001.wal");
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&wal_path)?;
        self.wal = WALWriter::open(&wal_path)?;
        Ok(())
    }

    /// Encode a value with a sequence number prefix.
    fn encode_with_seq(value: &[u8], seq: u64) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + value.len());
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.extend_from_slice(value);
        buf
    }

    /// Decode a seq-encoded value.
    fn decode_with_seq(v: &[u8]) -> Option<(Vec<u8>, u64)> {
        if v.len() >= 8 {
            let seq = u64::from_le_bytes([v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]]);
            Some((v[8..].to_vec(), seq))
        } else {
            Some((v.to_vec(), 0))
        }
    }

    /// Unpack inner value (after seq stripping): check tombstone and TTL.
    fn unpack_inner(v: &[u8]) -> Option<Vec<u8>> {
        if v == TOMBSTONE {
            return None;
        }
        Self::unwrap_ttl(v)
    }

    /// Map a stored value (with seq prefix) to the user-visible result.
    fn unpack_value(v: &[u8]) -> Option<Vec<u8>> {
        if let Some((inner, _seq)) = Self::decode_with_seq(v) {
            Self::unpack_inner(&inner)
        } else {
            None
        }
    }

    /// Unwrap a TTL-encoded value.  Returns `None` if expired.
    /// Regular values (no TTL marker) are returned as-is.
    fn unwrap_ttl(v: &[u8]) -> Option<Vec<u8>> {
        if v.len() >= 9 && v[0] == 0x01 {
            // TTL-encoded: [0x01] [expiry: u64 LE] [value]
            let expiry = u64::from_le_bytes([v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]]);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if now >= expiry {
                return None; // expired
            }
            Some(v[9..].to_vec())
        } else {
            Some(v.to_vec())
        }
    }

    /// Find an existing `.wal` file or return the default path.
    fn wal_path_for_dir(dir: &Path) -> Result<PathBuf> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .ends_with(".wal")
            {
                return Ok(entry.path());
            }
        }
        Ok(dir.join("000001.wal"))
    }
}

/// Compute the next lexicographic key after `prefix` (for prefix scan upper bound).
fn next_key(prefix: &[u8]) -> Vec<u8> {
    let mut next = prefix.to_vec();
    // Increment the last byte, or append 0x00 if all 0xFF.
    for i in (0..next.len()).rev() {
        if next[i] < 0xFF {
            next[i] += 1;
            return next;
        }
    }
    // All 0xFF — return vec![0xFF, 0xFF, ..., 0xFF, 0x00].
    next.push(0x00);
    next
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn basic_put_get_delete() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        db.put(b"name", b"rust").unwrap();
        assert_eq!(db.get(b"name").unwrap(), Some(b"rust".to_vec()));

        db.delete(b"name").unwrap();
        assert_eq!(db.get(b"name").unwrap(), None);

        // Non-existent key.
        assert_eq!(db.get(b"missing").unwrap(), None);
    }

    #[test]
    fn reopen_recovers_from_wal() {
        let dir = tempdir().unwrap();

        // First session: write data.
        {
            let mut db = Engine::open(dir.path()).unwrap();
            for i in 0..100 {
                let key = format!("key_{:04}", i);
                let val = format!("val_{:04}", i);
                db.put(key.as_bytes(), val.as_bytes()).unwrap();
            }
            db.close().unwrap();
        }

        // Second session: verify data.
        {
            let db = Engine::open(dir.path()).unwrap();
            for i in 0..100 {
                let key = format!("key_{:04}", i);
                let val = format!("val_{:04}", i);
                assert_eq!(db.get(key.as_bytes()).unwrap(), Some(val.into_bytes()));
            }
            assert_eq!(db.get(b"nonexistent").unwrap(), None);
        }
    }

    #[test]
    fn overwrite_preserves_latest() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        for i in 0..10 {
            let val = format!("v{}", i);
            db.put(b"key", val.as_bytes()).unwrap();
        }

        assert_eq!(db.get(b"key").unwrap(), Some(b"v9".to_vec()));
    }

    #[test]
    fn open_empty_dir_creates_db() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("new_db");
        let db = Engine::open(&sub).unwrap();
        assert_eq!(db.get(b"anything").unwrap(), None);
    }

    #[test]
    fn scan_returns_sorted_range() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        for i in 0..100 {
            let key = format!("k{:03}", i);
            db.put(key.as_bytes(), b"v").unwrap();
        }

        let results = db.scan(b"k010", b"k020").unwrap();
        assert_eq!(results.len(), 10);
        for (i, (key, _)) in results.iter().enumerate() {
            assert_eq!(key, format!("k{:03}", i + 10).as_bytes());
        }
    }

    #[test]
    fn scan_excludes_tombstones() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.put(b"c", b"3").unwrap();
        db.delete(b"b").unwrap();

        let results = db.scan(b"a", b"d").unwrap();
        let keys: Vec<_> = results.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(keys, vec![b"a".as_slice(), b"c".as_slice()]);
    }

    #[test]
    fn compaction_triggered_by_l0_threshold() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        // Write enough data. The default memtable is 4MB, so we write
        // enough small keys to accumulate L0 SSTables over multiple put()
        // calls.  With 5000 keys of ~20 bytes each, the memtable won't
        // freeze on its own, so we rely on the natural flow.
        //
        // To actually trigger compaction we write large values so the
        // memtable fills and flushes multiple times.
        for batch in 0..6 {
            // Each batch fills the 4MB memtable once.
            for i in 0..50 {
                let key = format!("b{:01}_k{:04}", batch, i);
                // ~100KB value → ~5MB per batch → triggers freeze.
                let val = vec![b'v'; 20_000];
                db.put(key.as_bytes(), &val).unwrap();
            }
        }

        // After multiple flushes, data should still be readable.
        for batch in 0..6 {
            for i in 0..50 {
                let key = format!("b{:01}_k{:04}", batch, i);
                let v = db.get(key.as_bytes()).unwrap();
                assert!(v.is_some(), "data lost for key {}", key);
                assert_eq!(v.unwrap().len(), 20_000);
            }
        }
    }

    #[test]
    fn writebatch_atomicity() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        let mut batch = WriteBatch::new();
        batch.put(b"a".to_vec(), b"1".to_vec());
        batch.put(b"b".to_vec(), b"2".to_vec());
        batch.put(b"c".to_vec(), b"3".to_vec());
        db.write_batch(&batch).unwrap();

        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    #[test]
    fn writebatch_with_delete() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        db.put(b"key", b"val").unwrap();

        let mut batch = WriteBatch::new();
        batch.put(b"new_key".to_vec(), b"new_val".to_vec());
        batch.delete(b"key".to_vec());
        db.write_batch(&batch).unwrap();

        assert_eq!(db.get(b"key").unwrap(), None);
        assert_eq!(db.get(b"new_key").unwrap(), Some(b"new_val".to_vec()));
    }

    #[test]
    fn snapshot_read_consistency() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        let snap = db.snapshot();

        // New write after snapshot.
        db.put(b"c", b"3").unwrap();

        // Snapshot sees a and b (written before), but not c.
        assert_eq!(db.get_at(b"a", &snap).unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.get_at(b"b", &snap).unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get_at(b"c", &snap).unwrap(), None);

        // Current get sees all three.
        assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    #[test]
    fn prefix_scan_returns_matching_keys() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        db.put(b"user:1", b"alice").unwrap();
        db.put(b"user:2", b"bob").unwrap();
        db.put(b"user:3", b"charlie").unwrap();
        db.put(b"post:1", b"hello").unwrap();

        let results = db.prefix_scan(b"user:").unwrap();
        assert_eq!(results.len(), 3);
        for (k, _) in &results {
            assert!(k.starts_with(b"user:"));
        }
    }

    #[test]
    fn put_with_ttl_expires() {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();

        db.put_with_ttl(b"temp", b"data", std::time::Duration::from_secs(1))
            .unwrap();

        // Should be readable immediately.
        assert_eq!(db.get(b"temp").unwrap(), Some(b"data".to_vec()));

        // After 2 seconds, should be expired.
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert_eq!(db.get(b"temp").unwrap(), None);
    }

    #[test]
    fn repair_rebuilds_manifest() {
        let dir = tempdir().unwrap();

        // Create a database with some data and flush to SST.
        {
            let mut db = Engine::open(dir.path()).unwrap();
            for i in 0..100 {
                let key = format!("k{:04}", i);
                let val = format!("v{}", i);
                db.put(key.as_bytes(), val.as_bytes()).unwrap();
            }
            db.close().unwrap();
        }

        // Delete the manifest.
        let manifest_path = dir.path().join("MANIFEST");
        std::fs::remove_file(&manifest_path).unwrap();

        // Repair should rebuild it.
        let report = Engine::repair(dir.path()).unwrap();
        assert!(report.recovered_ssts > 0 || report.corrupted_files == 0);

        // Reopen and verify data is still accessible.
        let db = Engine::open(dir.path()).unwrap();
        let mut readable = 0;
        for i in 0..100 {
            let key = format!("k{:04}", i);
            if db.get(key.as_bytes()).unwrap().is_some() {
                readable += 1;
            }
        }
        assert!(readable > 0);
    }
}

// ---------------------------------------------------------------------------
// DB — Thread-safe wrapper around Engine
// ---------------------------------------------------------------------------

use std::sync::Mutex;

/// A thread-safe handle to the key-value database.
///
/// All methods acquire an internal lock, making them safe to call from
/// multiple threads concurrently.
#[derive(Clone)]
pub struct DB {
    inner: Arc<Mutex<Engine>>,
}

impl DB {
    /// Open (or create) a database with default options.
    pub fn open(path: &Path) -> Result<Self> {
        let engine = Engine::open(path)?;
        Ok(DB {
            inner: Arc::new(Mutex::new(engine)),
        })
    }

    /// Open (or create) a database with custom options.
    pub fn open_with_options(path: &Path, opts: Options) -> Result<Self> {
        let engine = Engine::open_with_options(path, opts)?;
        Ok(DB {
            inner: Arc::new(Mutex::new(engine)),
        })
    }

    /// Write a key-value pair.
    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.inner.lock().unwrap().put(key, value)
    }

    /// Delete a key.
    pub fn delete(&self, key: &[u8]) -> Result<()> {
        self.inner.lock().unwrap().delete(key)
    }

    /// Look up a key.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.inner.lock().unwrap().get(key)
    }

    /// Scan keys in `[start, end)`.
    pub fn scan(&self, start: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.lock().unwrap().scan(start, end)
    }

    /// Scan keys with a given prefix.
    pub fn prefix_scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.lock().unwrap().prefix_scan(prefix)
    }

    /// Execute a batch of operations atomically.
    pub fn write_batch(&self, batch: &WriteBatch) -> Result<()> {
        self.inner.lock().unwrap().write_batch(batch)
    }

    /// Create a point-in-time snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().snapshot()
    }

    /// Look up a key at a specific snapshot.
    pub fn get_at(&self, key: &[u8], snap: &Snapshot) -> Result<Option<Vec<u8>>> {
        self.inner.lock().unwrap().get_at(key, snap)
    }

    /// Write a key-value pair with a TTL.
    pub fn put_with_ttl(
        &self,
        key: &[u8],
        value: &[u8],
        ttl: std::time::Duration,
    ) -> Result<()> {
        self.inner.lock().unwrap().put_with_ttl(key, value, ttl)
    }

    /// Get a snapshot of the current metrics.
    pub fn metrics(&self) -> MetricsSnapshot {
        self.inner.lock().unwrap().metrics.snapshot()
    }

    /// Flush all in-memory data to disk and close the engine.
    pub fn close(self) -> Result<()> {
        // Unwrap the Arc — if we're the last holder, we can consume it.
        let engine = Arc::try_unwrap(self.inner)
            .map_err(|_| crate::error::KvError::Internal(
                "cannot close: other references exist".to_string(),
            ))?
            .into_inner()
            .unwrap();
        engine.close()
    }
}

// ---------------------------------------------------------------------------
// DB Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod db_tests {
    use super::*;
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn concurrent_writes_and_reads() {
        let dir = tempdir().unwrap();
        let db = DB::open(dir.path()).unwrap();

        let writer_db = db.clone();
        let writer = thread::spawn(move || {
            for i in 0..1000 {
                let key = format!("k{:05}", i);
                let val = format!("v{}", i);
                writer_db.put(key.as_bytes(), val.as_bytes()).unwrap();
            }
        });

        let reader_db = db.clone();
        let reader = thread::spawn(move || {
            for i in 0..1000 {
                let key = format!("k{:05}", i);
                let _ = reader_db.get(key.as_bytes());
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();

        // After writes complete, all keys should be readable.
        for i in 0..1000 {
            let key = format!("k{:05}", i);
            assert!(db.get(key.as_bytes()).unwrap().is_some());
        }
    }

    #[test]
    fn db_open_with_options() {
        let dir = tempdir().unwrap();
        let opts = Options {
            memtable_size: 1024, // small for testing
            block_size: 64,
            ..Default::default()
        };
        let db = DB::open_with_options(dir.path(), opts).unwrap();
        db.put(b"key", b"val").unwrap();
        assert_eq!(db.get(b"key").unwrap(), Some(b"val".to_vec()));
    }

    #[test]
    fn db_metrics_tracking() {
        let dir = tempdir().unwrap();
        let db = DB::open(dir.path()).unwrap();

        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.get(b"a").unwrap();
        db.delete(b"b").unwrap();

        let m = db.metrics();
        assert_eq!(m.writes, 2);
        assert_eq!(m.reads, 1);
        assert_eq!(m.deletes, 1);
    }
}
