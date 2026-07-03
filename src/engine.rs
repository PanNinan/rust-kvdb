//! Top-level storage engine that coordinates MemTable, WAL, SSTable, and
//! Manifest to provide `put` / `get` / `delete` / `close` operations.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::manifest::{Manifest, SSTMeta};
use crate::memtable::memtable::MemTable;
use crate::sstable::builder::SSTableBuilder;
use crate::sstable::reader::SSTableReader;
use crate::wal::writer::{WALReader, WALWriter};

/// Tombstone marker written when a key is deleted.
const TOMBSTONE: &[u8] = &[0x00];

/// Default MemTable freeze threshold: 4 MiB.
const DEFAULT_MEMTABLE_SIZE: usize = 4 * 1024 * 1024;

/// Default SSTable data block size: 4 KiB.
const DEFAULT_BLOCK_SIZE: usize = 4096;

/// The top-level key-value storage engine.
pub struct Engine {
    path: PathBuf,
    wal: WALWriter,
    memtable: MemTable,
    manifest: Manifest,
    sst_readers: Vec<(SSTMeta, SSTableReader)>,
    next_sst_id: u64,
}

impl Engine {
    /// Open (or create) a database at the given directory.
    pub fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)?;

        let manifest_path = path.join("MANIFEST");
        let manifest = Manifest::load(&manifest_path)?;

        // Open SSTable readers and determine next id.
        let mut next_sst_id: u64 = 1;
        let mut sst_readers = Vec::new();

        for level in &manifest.levels {
            for meta in level {
                let sst_path = path.join(format!("{:06}.sst", meta.id));
                if sst_path.exists() {
                    let reader = SSTableReader::open(&sst_path)?;
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
        let memtable = Self::replay_wal(&wal_path)?;

        // Open WAL for new writes.
        let wal = WALWriter::open(&wal_path)?;

        Ok(Engine {
            path: path.to_path_buf(),
            wal,
            memtable,
            manifest,
            sst_readers,
            next_sst_id,
        })
    }

    /// Write a key-value pair.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.wal.append(key, value)?;
        self.wal.flush()?;

        self.memtable.put(key, value);
        self.maybe_flush()?;
        Ok(())
    }

    /// Delete a key by writing a tombstone.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.put(key, TOMBSTONE)
    }

    /// Look up a key.
    ///
    /// Search: MemTable (active → immutable) → SSTables (newest → oldest).
    /// A tombstone is treated as "not found".
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
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

        // Filter out tombstones.
        let result: Vec<_> = map
            .into_iter()
            .filter(|(_, v)| v.as_slice() != TOMBSTONE)
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
    fn replay_wal(wal_path: &Path) -> Result<MemTable> {
        let mut memtable = MemTable::with_max_size(DEFAULT_MEMTABLE_SIZE);
        if wal_path.exists() {
            let reader = WALReader::open(wal_path)?;
            for entry in reader {
                let (key, value) = entry?;
                memtable.put_no_freeze(&key, &value);
            }
        }
        Ok(memtable)
    }

    /// Flush the immutable MemTable to an SSTable if one exists.
    fn maybe_flush(&mut self) -> Result<()> {
        if !self.memtable.has_immutable() {
            return Ok(());
        }

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

        let mut builder = SSTableBuilder::new(&sst_path, DEFAULT_BLOCK_SIZE)?;
        for (k, v) in &entries {
            builder.add(k, v)?;
        }
        builder.finish()?;

        let min_key = entries.first().unwrap().0.clone();
        let max_key = entries.last().unwrap().0.clone();
        let meta = SSTMeta::new(sst_id, 0, min_key, max_key);

        let reader = SSTableReader::open(&sst_path)?;
        self.manifest.add_sst(0, meta.clone());
        self.sst_readers.push((meta, reader));

        self.manifest.save(&self.path.join("MANIFEST"))?;
        self.reset_wal()?;

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

    /// Map a stored value to the user-visible result.
    fn unpack_value(v: &[u8]) -> Option<Vec<u8>> {
        if v == TOMBSTONE {
            None
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
}
