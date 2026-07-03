//! SSTable reader: loads an SSTable file into memory and provides
//! random lookup (`get`) and sequential iteration (`iter`).
//!
//! The entire file is read into a `Vec<u8>` (Phase 1 — no mmap dependency).
//! The footer and index block are parsed once on `open()`.

use std::fs;
use std::path::Path;

use crate::error::{KvError, Result};
use crate::sstable::block::Block;

/// An opened SSTable file ready for reads.
pub struct SSTableReader {
    /// Raw file bytes (entire SSTable).
    data: Vec<u8>,
    /// Parsed index: `(block_offset, last_key)` for each data block.
    index: Vec<(u32, Vec<u8>)>,
    /// Byte offset where the index block starts (= end of last data block).
    index_offset: u32,
}

impl SSTableReader {
    /// Open an SSTable file and parse its footer + index block.
    pub fn open(path: &Path) -> Result<Self> {
        let data = fs::read(path).map_err(KvError::Io)?;
        if data.len() < 4 {
            return Err(KvError::Corruption("SSTable file too short".to_string()));
        }

        // Read footer: last 4 bytes = index_offset.
        let footer_start = data.len() - 4;
        let index_offset = u32::from_le_bytes([
            data[footer_start],
            data[footer_start + 1],
            data[footer_start + 2],
            data[footer_start + 3],
        ]);

        // Parse index block.
        let index = Self::parse_index(&data, index_offset as usize)?;

        Ok(SSTableReader {
            data,
            index,
            index_offset,
        })
    }

    /// Look up a key.  Returns `Ok(Some(value))` if found, `Ok(None)` if not.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        // Binary search the index for the data block that may contain `key`.
        let block_idx = match self.index.binary_search_by(|entry| entry.1.as_slice().cmp(key)) {
            Ok(i) => i,                   // exact match on last_key
            Err(i) => {
                if i < self.index.len() {
                    i // first block whose last_key >= key
                } else {
                    // key is beyond the last block's last_key → not found
                    return Ok(None);
                }
            }
        };

        let block = self.load_block(block_idx)?;

        // Binary search within the block.
        match block
            .entries
            .binary_search_by(|entry| entry.0.as_slice().cmp(key))
        {
            Ok(i) => Ok(Some(block.entries[i].1.clone())),
            Err(_) => Ok(None),
        }
    }

    /// Return an iterator over all entries in key order.
    pub fn iter(&self) -> SSTableIterator<'_> {
        SSTableIterator {
            reader: self,
            block_idx: 0,
            entry_idx: 0,
            current_block: None,
        }
    }

    /// Number of data blocks in this SSTable.
    pub fn num_blocks(&self) -> usize {
        self.index.len()
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Parse the index block starting at `offset`.
    fn parse_index(data: &[u8], offset: usize) -> Result<Vec<(u32, Vec<u8>)>> {
        if data.len() < offset + 4 {
            return Err(KvError::Corruption("index block too short".to_string()));
        }

        let num = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;

        let mut entries = Vec::with_capacity(num);
        let mut pos = offset + 4;

        for _ in 0..num {
            if data.len() < pos + 8 {
                return Err(KvError::Corruption(
                    "index entry truncated".to_string(),
                ));
            }
            let block_offset = u32::from_le_bytes([
                data[pos],
                data[pos + 1],
                data[pos + 2],
                data[pos + 3],
            ]);
            pos += 4;

            let key_len = u32::from_le_bytes([
                data[pos],
                data[pos + 1],
                data[pos + 2],
                data[pos + 3],
            ]) as usize;
            pos += 4;

            if data.len() < pos + key_len {
                return Err(KvError::Corruption(
                    "index entry key truncated".to_string(),
                ));
            }
            let last_key = data[pos..pos + key_len].to_vec();
            pos += key_len;

            entries.push((block_offset, last_key));
        }

        Ok(entries)
    }

    /// Load and decode a data block by index position.
    ///
    /// The block spans from `index[block_idx].0` to the next block's offset
    /// (or `index_offset` if it's the last block).
    fn load_block(&self, block_idx: usize) -> Result<Block> {
        let start = self.index[block_idx].0 as usize;
        let end = if block_idx + 1 < self.index.len() {
            self.index[block_idx + 1].0 as usize
        } else {
            self.index_offset as usize
        };

        if end > self.data.len() || start > end {
            return Err(KvError::Corruption(format!(
                "block {} offset out of range: {}..{}",
                block_idx, start, end
            )));
        }

        Block::decode(&self.data[start..end])
    }

    /// Load a block and return a reference to it (used by iterator).
    fn load_block_cached(&self, block_idx: usize) -> Result<Block> {
        self.load_block(block_idx)
    }
}

// ---------------------------------------------------------------------------
// SSTableIterator
// ---------------------------------------------------------------------------

/// Sequential iterator over all entries in an SSTable.
///
/// Yields `(key, value)` pairs in sorted key order across all data blocks.
pub struct SSTableIterator<'a> {
    reader: &'a SSTableReader,
    block_idx: usize,
    entry_idx: usize,
    current_block: Option<Block>,
}

impl<'a> Iterator for SSTableIterator<'a> {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Load the current block if we don't have one yet.
            if self.current_block.is_none() {
                if self.block_idx >= self.reader.index.len() {
                    return None; // exhausted all blocks
                }
                self.current_block = Some(self.reader.load_block_cached(self.block_idx).ok()?);
                self.entry_idx = 0;
            }

            let block = self.current_block.as_ref().unwrap();
            if self.entry_idx < block.entries.len() {
                let (k, v) = &block.entries[self.entry_idx];
                self.entry_idx += 1;
                return Some((k.clone(), v.clone()));
            }

            // Move to the next block.
            self.block_idx += 1;
            self.current_block = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sstable::builder::SSTableBuilder;
    use tempfile::tempdir;

    #[test]
    fn sstable_build_and_read_single_block() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("single.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        builder.add(b"aaa", b"v1").unwrap();
        builder.add(b"bbb", b"v2").unwrap();
        builder.add(b"ccc", b"v3").unwrap();
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        assert_eq!(reader.get(b"bbb").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(reader.get(b"aaa").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(reader.get(b"ccc").unwrap(), Some(b"v3".to_vec()));
        // Missing key.
        assert_eq!(reader.get(b"xxx").unwrap(), None);
    }

    #[test]
    fn sstable_multiple_data_blocks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi_block.sst");

        // 64-byte blocks → many blocks for 100 entries.
        let mut builder = SSTableBuilder::new(&path, 64).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let val = format!("value_{:04}", i);
            builder.add(key.as_bytes(), val.as_bytes()).unwrap();
        }
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        assert!(reader.num_blocks() > 1, "expected multiple blocks");

        // Spot-check lookups across different blocks.
        assert_eq!(
            reader.get(b"key_0000").unwrap(),
            Some(b"value_0000".to_vec())
        );
        assert_eq!(
            reader.get(b"key_0050").unwrap(),
            Some(b"value_0050".to_vec())
        );
        assert_eq!(
            reader.get(b"key_0099").unwrap(),
            Some(b"value_0099".to_vec())
        );
        assert_eq!(reader.get(b"missing").unwrap(), None);
    }

    #[test]
    fn sstable_iterator_returns_sorted_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("iter.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        for i in 0..50 {
            builder.add(format!("k{:03}", i).as_bytes(), b"v").unwrap();
        }
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        let keys: Vec<_> = reader.iter().map(|(k, _)| k).collect();
        // Entries should arrive in the order they were written (sorted
        // internally per block, and blocks written in order).
        for w in keys.windows(2) {
            assert!(w[0] <= w[1], "keys not sorted: {:?} > {:?}", w[0], w[1]);
        }
        assert_eq!(keys.len(), 50);
    }

    #[test]
    fn sstable_reader_open_nonexistent() {
        let result = SSTableReader::open(Path::new("/no/such/file.sst"));
        assert!(result.is_err());
    }
}
