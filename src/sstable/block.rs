//! SSTable data block representation.
//!
//! A Block is a sorted, in-memory collection of key-value pairs that is
//! serialized to a contiguous byte sequence on disk.  No prefix compression
//! is used in Phase 1 (simpler, correct baseline).

/// An in-memory data block containing sorted key-value pairs.
#[derive(Debug, Clone)]
pub struct Block {
    /// Sorted entries: (key, value).
    pub entries: Vec<(Vec<u8>, Vec<u8>)>,
    /// Running total of encoded byte size (keys + values + per-entry headers).
    encoded_size: usize,
}

impl Block {
    /// Per-entry fixed overhead: key_len(4) + value_len(4) = 8 bytes.
    const ENTRY_HEADER: usize = 8;

    /// Create an empty block.
    pub fn new() -> Self {
        Block {
            entries: Vec::new(),
            encoded_size: 0,
        }
    }

    /// Append an entry.  Callers must ensure keys arrive in sorted order.
    pub fn add(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.encoded_size += Self::ENTRY_HEADER + key.len() + value.len();
        self.entries.push((key, value));
    }

    /// Whether the block is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entries in the block.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Estimated encoded size in bytes (same as `encode().len()`).
    pub fn estimated_size(&self) -> usize {
        self.encoded_size
    }

    /// Return the last key in the block (block must be non-empty).
    pub fn last_key(&self) -> &[u8] {
        &self.entries.last().expect("block is empty").0
    }

    /// Serialize the block with prefix compression.
    ///
    /// ```text
    /// [num_entries: u32 LE]
    /// for each entry:
    ///     [shared_len: u16 LE] [suffix_len: u16 LE] [suffix: [u8]]
    ///     [value_len: u32 LE] [value: [u8]]
    /// ```
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + self.encoded_size);
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());

        let mut prev_key: &[u8] = &[];
        for (key, value) in &self.entries {
            // Compute shared prefix length with previous key.
            let shared = Self::prefix_shared_len(prev_key, key);
            let suffix = &key[shared..];

            buf.extend_from_slice(&(shared as u16).to_le_bytes());
            buf.extend_from_slice(&(suffix.len() as u16).to_le_bytes());
            buf.extend_from_slice(suffix);
            buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
            buf.extend_from_slice(value);

            prev_key = key;
        }

        buf
    }

    /// Deserialize a prefix-compressed block from its on-disk bytes.
    pub fn decode(buf: &[u8]) -> crate::error::Result<Self> {
        if buf.len() < 4 {
            return Err(crate::error::KvError::Corruption(
                "block too short".to_string(),
            ));
        }

        let num_entries = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let mut entries = Vec::with_capacity(num_entries);
        let mut offset = 4;
        let mut prev_key: Vec<u8> = Vec::new();

        for _ in 0..num_entries {
            if buf.len() < offset + 4 {
                return Err(crate::error::KvError::Corruption(
                    "block truncated in prefix header".to_string(),
                ));
            }
            let shared = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
            let suffix_len = u16::from_le_bytes([buf[offset + 2], buf[offset + 3]]) as usize;
            offset += 4;

            if buf.len() < offset + suffix_len {
                return Err(crate::error::KvError::Corruption(
                    "block truncated in key suffix".to_string(),
                ));
            }
            // Reconstruct full key: prev_key[..shared] + suffix
            let mut key = Vec::with_capacity(shared + suffix_len);
            key.extend_from_slice(&prev_key[..shared]);
            key.extend_from_slice(&buf[offset..offset + suffix_len]);
            offset += suffix_len;

            if buf.len() < offset + 4 {
                return Err(crate::error::KvError::Corruption(
                    "block truncated in value_len".to_string(),
                ));
            }
            let value_len =
                u32::from_le_bytes([buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]])
                    as usize;
            offset += 4;

            if buf.len() < offset + value_len {
                return Err(crate::error::KvError::Corruption(
                    "block truncated in value".to_string(),
                ));
            }
            let value = buf[offset..offset + value_len].to_vec();
            offset += value_len;

            prev_key = key.clone();
            entries.push((key, value));
        }

        let mut block = Block::new();
        block.entries = entries;
        block.encoded_size = offset - 4;
        Ok(block)
    }

    fn prefix_shared_len(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
    }
}

impl Default for Block {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_encode_decode_roundtrip() {
        let mut block = Block::new();
        block.add(b"apple".to_vec(), b"red".to_vec());
        block.add(b"banana".to_vec(), b"yellow".to_vec());
        block.add(b"cherry".to_vec(), b"dark_red".to_vec());

        let encoded = block.encode();
        let decoded = Block::decode(&encoded).unwrap();

        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded.entries[0], (b"apple".to_vec(), b"red".to_vec()));
        assert_eq!(decoded.entries[1], (b"banana".to_vec(), b"yellow".to_vec()));
        assert_eq!(decoded.entries[2], (b"cherry".to_vec(), b"dark_red".to_vec()));
    }

    #[test]
    fn block_empty_roundtrip() {
        let block = Block::new();
        let encoded = block.encode();
        let decoded = Block::decode(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn block_size_tracking() {
        let mut block = Block::new();
        assert_eq!(block.estimated_size(), 0);

        block.add(b"k".to_vec(), b"v".to_vec());
        // 8 (header) + 1 (key) + 1 (value) = 10
        assert_eq!(block.estimated_size(), 10);
    }

    #[test]
    fn block_last_key() {
        let mut block = Block::new();
        block.add(b"a".to_vec(), b"1".to_vec());
        block.add(b"c".to_vec(), b"3".to_vec());
        assert_eq!(block.last_key(), b"c");
    }
}
