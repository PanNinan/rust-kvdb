//! SSTable builder: writes sorted key-value pairs to disk in the SSTable
//! file format.
//!
//! Layout:
//! ```text
//! [Data Block 0] [Data Block 1] ... [Index Block] [Bloom Filter] [Footer(8B)]
//! ```
//!
//! Footer = `index_offset:u32 LE` + `bloom_offset:u32 LE`.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::filter::bloom::BloomFilter;
use crate::sstable::block::Block;

/// Default target data block size: 4 KiB.
pub const DEFAULT_BLOCK_SIZE: usize = 4096;

/// Default bloom filter bits per key.
pub const DEFAULT_BITS_PER_KEY: usize = 10;

/// Builds an SSTable file from sorted key-value pairs.
pub struct SSTableBuilder {
    path: PathBuf,
    file: BufWriter<File>,
    current_block: Block,
    index_entries: Vec<(u32, Vec<u8>)>,
    /// All keys seen so far (collected for bloom filter construction).
    all_keys: Vec<Vec<u8>>,
    block_size: usize,
    bytes_written: u32,
}

impl SSTableBuilder {
    /// Create a new builder that will write to `path`.
    pub fn new(path: &Path, block_size: usize) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(SSTableBuilder {
            path: path.to_path_buf(),
            file: BufWriter::new(file),
            current_block: Block::new(),
            index_entries: Vec::new(),
            all_keys: Vec::new(),
            block_size,
            bytes_written: 0,
        })
    }

    /// Append a key-value pair.  Keys **must** arrive in sorted order.
    pub fn add(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        if self.current_block.estimated_size() > 0
            && self.current_block.estimated_size() + key.len() + value.len() + 8 > self.block_size
        {
            self.flush_block()?;
        }

        self.all_keys.push(key.to_vec());
        self.current_block.add(key.to_vec(), value.to_vec());
        Ok(())
    }

    /// Finalize the SSTable file.
    ///
    /// Writes the remaining data block, index block, bloom filter, and footer.
    pub fn finish(mut self) -> Result<()> {
        if !self.current_block.is_empty() {
            self.flush_block()?;
        }

        // Index block.
        let index_offset = self.bytes_written;
        self.write_index_block()?;

        // Bloom filter.
        let bloom_offset = self.bytes_written;
        let key_refs: Vec<&[u8]> = self.all_keys.iter().map(|k| k.as_slice()).collect();
        let bloom = BloomFilter::build(&key_refs, DEFAULT_BITS_PER_KEY);
        let bloom_data = bloom.encode();
        self.file.write_all(&bloom_data)?;
        self.bytes_written += bloom_data.len() as u32;

        // Footer: index_offset (4B) + bloom_offset (4B) = 8 bytes.
        self.file.write_all(&index_offset.to_le_bytes())?;
        self.file.write_all(&bloom_offset.to_le_bytes())?;

        self.file.flush()?;
        Ok(())
    }

    /// Return the path this builder is writing to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn flush_block(&mut self) -> Result<()> {
        let last_key = self.current_block.last_key().to_vec();
        let encoded = self.current_block.encode();

        self.file.write_all(&encoded)?;
        self.index_entries
            .push((self.bytes_written, last_key));
        self.bytes_written += encoded.len() as u32;

        self.current_block = Block::new();
        Ok(())
    }

    fn write_index_block(&mut self) -> Result<()> {
        self.file
            .write_all(&(self.index_entries.len() as u32).to_le_bytes())?;
        self.bytes_written += 4;

        for (offset, last_key) in &self.index_entries {
            self.file.write_all(&offset.to_le_bytes())?;
            self.file
                .write_all(&(last_key.len() as u32).to_le_bytes())?;
            self.file.write_all(last_key)?;
            self.bytes_written += 4 + 4 + last_key.len() as u32;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sstable_build_single_block() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("single.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        builder.add(b"aaa", b"v1").unwrap();
        builder.add(b"bbb", b"v2").unwrap();
        builder.add(b"ccc", b"v3").unwrap();
        builder.finish().unwrap();

        let data = std::fs::read(&path).unwrap();
        assert!(data.len() > 16); // at least some data + 8-byte footer
    }

    #[test]
    fn sstable_build_multiple_blocks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.sst");

        let mut builder = SSTableBuilder::new(&path, 64).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let val = format!("value_{:04}", i);
            builder.add(key.as_bytes(), val.as_bytes()).unwrap();
        }
        builder.finish().unwrap();

        let data = std::fs::read(&path).unwrap();
        let len = data.len();
        // Footer is now 8 bytes: index_offset + bloom_offset.
        let index_offset = u32::from_le_bytes([data[len - 8], data[len - 7], data[len - 6], data[len - 5]]);
        assert!(index_offset > 0);
        assert!((index_offset as usize) < len - 8);
    }

    #[test]
    fn builder_finish_writes_valid_footer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("footer.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        builder.add(b"a", b"1").unwrap();
        builder.add(b"b", b"2").unwrap();
        builder.finish().unwrap();

        let data = std::fs::read(&path).unwrap();
        let len = data.len();
        let index_offset = u32::from_le_bytes([data[len - 8], data[len - 7], data[len - 6], data[len - 5]]);
        let bloom_offset = u32::from_le_bytes([data[len - 4], data[len - 3], data[len - 2], data[len - 1]]);

        assert!(index_offset > 0);
        assert!(bloom_offset >= index_offset);
        assert!((bloom_offset as usize) < len - 8);
    }
}
