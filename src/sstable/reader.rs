//! SSTable reader: loads an SSTable file into memory and provides
//! random lookup (`get`) and sequential iteration (`iter`).
//!
//! Uses the bloom filter to skip the index search when a key is definitely
//! not present.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::cache::block_cache::{BlockCache, CacheKey};
use crate::error::{KvError, Result};
use crate::filter::bloom::BloomFilter;
use crate::sstable::block::Block;

/// An opened SSTable file ready for reads.
pub struct SSTableReader {
    data: Vec<u8>,
    index: Vec<(u32, Vec<u8>)>,
    index_offset: u32,
    bloom: BloomFilter,
    /// This SSTable's unique id (used as cache key).
    sst_id: u64,
    /// Shared block cache (optional).
    cache: Option<Arc<Mutex<BlockCache>>>,
}

impl SSTableReader {
    /// Open an SSTable file and parse its footer, index block, and bloom filter.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_cache(path, 0, None)
    }

    /// Open with an SST id and optional shared block cache.
    pub fn open_with_cache(
        path: &Path,
        sst_id: u64,
        cache: Option<Arc<Mutex<BlockCache>>>,
    ) -> Result<Self> {
        let data = fs::read(path).map_err(KvError::Io)?;
        if data.len() < 8 {
            return Err(KvError::Corruption("SSTable file too short".to_string()));
        }

        let len = data.len();
        let index_offset = u32::from_le_bytes([
            data[len - 8],
            data[len - 7],
            data[len - 6],
            data[len - 5],
        ]);
        let bloom_offset = u32::from_le_bytes([
            data[len - 4],
            data[len - 3],
            data[len - 2],
            data[len - 1],
        ]);

        let index = Self::parse_index(&data, index_offset as usize)?;

        // Bloom filter spans [bloom_offset .. start of footer).
        let bloom_end = len - 8;
        let bloom_data = &data[bloom_offset as usize..bloom_end];
        let bloom = BloomFilter::decode(bloom_data)?;

        Ok(SSTableReader {
            data,
            index,
            index_offset,
            bloom,
            sst_id,
            cache,
        })
    }

    /// Look up a key.  Returns `Ok(Some(value))` if found, `Ok(None)` if not.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if !self.bloom.may_contain(key) {
            return Ok(None);
        }

        let block_idx = match self
            .index
            .binary_search_by(|entry| entry.1.as_slice().cmp(key))
        {
            Ok(i) => i,
            Err(i) => {
                if i < self.index.len() {
                    i
                } else {
                    return Ok(None);
                }
            }
        };

        let block = self.load_block(block_idx)?;

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
            let block_offset =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;

            let key_len =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
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

    fn load_block(&self, block_idx: usize) -> Result<Block> {
        // Check cache first.
        if let Some(ref cache) = self.cache {
            let key = CacheKey {
                sst_id: self.sst_id,
                block_idx,
            };
            if let Some(block) = cache.lock().unwrap().get(&key) {
                return Ok(block.clone());
            }
        }

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

        let block = Block::decode(&self.data[start..end])?;

        // Populate cache.
        if let Some(ref cache) = self.cache {
            let key = CacheKey {
                sst_id: self.sst_id,
                block_idx,
            };
            cache.lock().unwrap().put(key, block.clone());
        }

        Ok(block)
    }
}

/// Sequential iterator over all entries in an SSTable.
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
            if self.current_block.is_none() {
                if self.block_idx >= self.reader.index.len() {
                    return None;
                }
                self.current_block = Some(self.reader.load_block(self.block_idx).ok()?);
                self.entry_idx = 0;
            }

            let block = self.current_block.as_ref().unwrap();
            if self.entry_idx < block.entries.len() {
                let (k, v) = &block.entries[self.entry_idx];
                self.entry_idx += 1;
                return Some((k.clone(), v.clone()));
            }

            self.block_idx += 1;
            self.current_block = None;
        }
    }
}

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
        assert_eq!(reader.get(b"xxx").unwrap(), None);
    }

    #[test]
    fn sstable_multiple_data_blocks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi_block.sst");

        let mut builder = SSTableBuilder::new(&path, 64).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let val = format!("value_{:04}", i);
            builder.add(key.as_bytes(), val.as_bytes()).unwrap();
        }
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        assert!(reader.num_blocks() > 1);

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
        for w in keys.windows(2) {
            assert!(w[0] <= w[1]);
        }
        assert_eq!(keys.len(), 50);
    }

    #[test]
    fn sstable_reader_open_nonexistent() {
        let result = SSTableReader::open(Path::new("/no/such/file.sst"));
        assert!(result.is_err());
    }

    #[test]
    fn bloom_filter_skips_index_for_missing_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bloom.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        for i in 0..1000 {
            builder
                .add(format!("key_{:04}", i).as_bytes(), b"v")
                .unwrap();
        }
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        // Missing key should be filtered by bloom without loading any block.
        assert_eq!(reader.get(b"not_in_sst").unwrap(), None);
    }
}
