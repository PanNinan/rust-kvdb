//! SSTable builder: writes sorted key-value pairs to disk in the SSTable
//! file format.
//!
//! The builder accumulates entries into an in-memory `Block`.  When the
//! block reaches `block_size` bytes it is flushed to disk and a new block
//! starts.  `finish()` writes the final block, the index block, and the
//! footer.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::sstable::block::Block;

/// Default target data block size: 4 KiB.
pub const DEFAULT_BLOCK_SIZE: usize = 4096;

/// Builds an SSTable file from sorted key-value pairs.
///
/// # Usage
///
/// ```ignore
/// let mut builder = SSTableBuilder::new(&path, 4096)?;
/// builder.add(b"key1", b"val1")?;
/// builder.add(b"key2", b"val2")?;
/// builder.finish()?;
/// ```
pub struct SSTableBuilder {
    path: PathBuf,
    file: BufWriter<File>,
    current_block: Block,
    /// (block_offset_on_disk, last_key_of_that_block)
    index_entries: Vec<(u32, Vec<u8>)>,
    block_size: usize,
    bytes_written: u32,
}

impl SSTableBuilder {
    /// Create a new builder that will write to `path`.
    ///
    /// `block_size` controls the target size of each data block.
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
            block_size,
            bytes_written: 0,
        })
    }

    /// Append a key-value pair.  Keys **must** arrive in sorted order.
    ///
    /// When the current data block reaches `block_size`, it is flushed to
    /// disk before the new entry is added.
    pub fn add(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        // Flush the current block if it's full.
        if self.current_block.estimated_size() > 0
            && self.current_block.estimated_size() + key.len() + value.len() + 8 > self.block_size
        {
            self.flush_block()?;
        }

        self.current_block.add(key.to_vec(), value.to_vec());
        Ok(())
    }

    /// Finalize the SSTable file.
    ///
    /// Writes the last data block (if non-empty), the index block, and
    /// the footer.  After this call the builder is consumed.
    pub fn finish(mut self) -> Result<()> {
        // Flush any remaining entries.
        if !self.current_block.is_empty() {
            self.flush_block()?;
        }

        // Write index block.
        let index_offset = self.bytes_written;
        self.write_index_block()?;

        // Write footer: index_offset (4 bytes, LE).
        self.file.write_all(&index_offset.to_le_bytes())?;

        // Flush BufWriter to the underlying file.
        self.file.flush()?;

        Ok(())
    }

    /// Return the path this builder is writing to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Serialize the current block, write it to disk, record it in the index,
    /// and clear the block.
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

    /// Write the index block to disk.
    ///
    /// Format:
    /// ```text
    /// [num_entries: u32 LE]
    /// for each entry:
    ///     [block_offset: u32 LE]
    ///     [last_key_len: u32 LE]
    ///     [last_key: [u8]]
    /// ```
    fn write_index_block(&mut self) -> Result<()> {
        self.file
            .write_all(&(self.index_entries.len() as u32).to_le_bytes())?;

        for (offset, last_key) in &self.index_entries {
            self.file.write_all(&offset.to_le_bytes())?;
            self.file
                .write_all(&(last_key.len() as u32).to_le_bytes())?;
            self.file.write_all(last_key)?;
        }

        Ok(())
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
    fn sstable_build_single_block() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("single.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        builder.add(b"aaa", b"v1").unwrap();
        builder.add(b"bbb", b"v2").unwrap();
        builder.add(b"ccc", b"v3").unwrap();
        builder.finish().unwrap();

        // Verify the file exists and has content.
        let data = std::fs::read(&path).unwrap();
        assert!(!data.is_empty());
        // Footer is 4 bytes, index block is non-trivial, data block is non-trivial.
        assert!(data.len() > 12);
    }

    #[test]
    fn sstable_build_multiple_blocks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.sst");

        // Use a tiny block size (64 bytes) to force many blocks.
        let mut builder = SSTableBuilder::new(&path, 64).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let val = format!("value_{:04}", i);
            builder.add(key.as_bytes(), val.as_bytes()).unwrap();
        }
        builder.finish().unwrap();

        // Parse footer to check index_offset is sane.
        let data = std::fs::read(&path).unwrap();
        let footer_start = data.len() - 4;
        let index_offset = u32::from_le_bytes([
            data[footer_start],
            data[footer_start + 1],
            data[footer_start + 2],
            data[footer_start + 3],
        ]) as usize;

        // Index offset must be within the file and after the data blocks.
        assert!(index_offset > 0);
        assert!(index_offset < data.len() - 4);
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
        let index_offset = u32::from_le_bytes([data[len - 4], data[len - 3], data[len - 2], data[len - 1]]);

        // The index block starts after all data blocks.
        assert!(index_offset > 0);
        assert!((index_offset as usize) < len - 4);
    }
}
