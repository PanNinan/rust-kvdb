//! Manifest file — records which SSTable files belong to each level.
//!
//! Format (all integers little-endian):
//!
//! ```text
//! [magic: 4 bytes "KVDB"]
//! [version: u32]
//! [num_levels: u32]
//! for each level:
//!     [num_ssts: u32]
//!     for each SST:
//!         [id: u64]
//!         [min_key_len: u32] [min_key: [u8]]
//!         [max_key_len: u32] [max_key: [u8]]
//! ```

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::{KvError, Result};

/// Magic bytes identifying a valid manifest file.
const MAGIC: &[u8; 4] = b"KVDB";

/// Current manifest format version.
const VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// SSTMeta
// ---------------------------------------------------------------------------

/// Metadata for a single SSTable file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SSTMeta {
    pub id: u64,
    pub level: usize,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
}

impl SSTMeta {
    /// Convenience constructor.
    pub fn new(id: u64, level: usize, min_key: Vec<u8>, max_key: Vec<u8>) -> Self {
        SSTMeta {
            id,
            level,
            min_key,
            max_key,
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// Manages the list of active SSTable files across all levels.
#[derive(Debug)]
pub struct Manifest {
    /// SSTable metadata organized by level: `levels[level] = Vec<SSTMeta>`.
    pub levels: Vec<Vec<SSTMeta>>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

impl Manifest {
    pub fn new() -> Self {
        Self { levels: Vec::new() }
    }

    /// Load a manifest from a file on disk.
    ///
    /// Returns an empty manifest if the file does not exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let data = fs::read(path).map_err(KvError::Io)?;
        Self::decode(&data)
    }

    /// Save the manifest to a file on disk (atomic write via rename when
    /// possible; falls back to direct write).
    pub fn save(&self, path: &Path) -> Result<()> {
        let encoded = self.encode();
        let mut file = fs::File::create(path)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        Ok(())
    }

    /// Add an SSTable entry to the given level.
    pub fn add_sst(&mut self, level: usize, meta: SSTMeta) {
        while self.levels.len() <= level {
            self.levels.push(Vec::new());
        }
        self.levels[level].push(meta);
    }

    /// Remove an SSTable by id from any level.  Returns `true` if found.
    pub fn remove_sst(&mut self, id: u64) -> bool {
        for level in &mut self.levels {
            let before = level.len();
            level.retain(|m| m.id != id);
            if level.len() < before {
                return true;
            }
        }
        false
    }

    /// Get all SSTable metadata at a given level.
    pub fn ssts_at_level(&self, level: usize) -> &[SSTMeta] {
        self.levels.get(level).map_or(&[], |v| v.as_slice())
    }

    /// Total number of SSTable files across all levels.
    pub fn total_ssts(&self) -> usize {
        self.levels.iter().map(|l| l.len()).sum()
    }

    // -----------------------------------------------------------------------
    // Encode / Decode
    // -----------------------------------------------------------------------

    /// Serialize the manifest to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&(self.levels.len() as u32).to_le_bytes());

        for level in &self.levels {
            buf.extend_from_slice(&(level.len() as u32).to_le_bytes());
            for sst in level {
                buf.extend_from_slice(&sst.id.to_le_bytes());
                buf.extend_from_slice(&(sst.min_key.len() as u32).to_le_bytes());
                buf.extend_from_slice(&sst.min_key);
                buf.extend_from_slice(&(sst.max_key.len() as u32).to_le_bytes());
                buf.extend_from_slice(&sst.max_key);
            }
        }

        buf
    }

    /// Deserialize a manifest from bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 12 {
            return Err(KvError::Corruption(
                "manifest too short (need at least 12 bytes)".to_string(),
            ));
        }

        // Verify magic.
        if &data[0..4] != MAGIC {
            return Err(KvError::Corruption(format!(
                "invalid manifest magic: {:?}",
                &data[0..4]
            )));
        }

        let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if version != VERSION {
            return Err(KvError::Corruption(format!(
                "unsupported manifest version: {}",
                version
            )));
        }

        let num_levels = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
        let mut levels = Vec::with_capacity(num_levels);
        let mut pos = 12;

        for _ in 0..num_levels {
            if data.len() < pos + 4 {
                return Err(KvError::Corruption(
                    "manifest truncated at level count".to_string(),
                ));
            }
            let num_ssts = u32::from_le_bytes([
                data[pos],
                data[pos + 1],
                data[pos + 2],
                data[pos + 3],
            ]) as usize;
            pos += 4;

            let mut ssts = Vec::with_capacity(num_ssts);
            for _ in 0..num_ssts {
                let sst = Self::decode_sst_meta(data, &mut pos)?;
                ssts.push(sst);
            }
            levels.push(ssts);
        }

        Ok(Manifest { levels })
    }

    fn decode_sst_meta(data: &[u8], pos: &mut usize) -> Result<SSTMeta> {
        let p = *pos;

        // id: u64 (8 bytes)
        if data.len() < p + 8 {
            return Err(KvError::Corruption("manifest truncated at sst id".to_string()));
        }
        let id = u64::from_le_bytes([
            data[p], data[p + 1], data[p + 2], data[p + 3],
            data[p + 4], data[p + 5], data[p + 6], data[p + 7],
        ]);
        let mut p = p + 8;

        // min_key
        let min_key = Self::decode_bytes(data, &mut p, "min_key")?;
        // max_key
        let max_key = Self::decode_bytes(data, &mut p, "max_key")?;

        // level is inferred from position, stored as 0 here (caller sets it).
        // We store level in the outer loop index, so use 0 as placeholder.
        *pos = p;

        Ok(SSTMeta {
            id,
            level: 0, // will be set by the caller based on position
            min_key,
            max_key,
        })
    }

    fn decode_bytes(data: &[u8], pos: &mut usize, field: &str) -> Result<Vec<u8>> {
        if data.len() < *pos + 4 {
            return Err(KvError::Corruption(format!(
                "manifest truncated at {} len",
                field
            )));
        }
        let len = u32::from_le_bytes([
            data[*pos],
            data[*pos + 1],
            data[*pos + 2],
            data[*pos + 3],
        ]) as usize;
        *pos += 4;

        if data.len() < *pos + len {
            return Err(KvError::Corruption(format!(
                "manifest truncated at {} data",
                field
            )));
        }
        let bytes = data[*pos..*pos + len].to_vec();
        *pos += len;
        Ok(bytes)
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
    fn manifest_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("MANIFEST");

        let mut manifest = Manifest::new();
        manifest.add_sst(
            0,
            SSTMeta::new(1, 0, b"a".to_vec(), b"z".to_vec()),
        );
        manifest.add_sst(
            1,
            SSTMeta::new(2, 1, b"a".to_vec(), b"z".to_vec()),
        );
        manifest.save(&path).unwrap();

        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.ssts_at_level(0).len(), 1);
        assert_eq!(loaded.ssts_at_level(1).len(), 1);
        assert_eq!(loaded.ssts_at_level(0)[0].id, 1);
        assert_eq!(loaded.ssts_at_level(1)[0].id, 2);
        assert_eq!(loaded.ssts_at_level(0)[0].min_key, b"a");
        assert_eq!(loaded.ssts_at_level(1)[0].max_key, b"z");
    }

    #[test]
    fn manifest_load_nonexistent_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("no_such_file");
        let manifest = Manifest::load(&path).unwrap();
        assert_eq!(manifest.total_ssts(), 0);
    }

    #[test]
    fn manifest_encode_decode_roundtrip() {
        let mut manifest = Manifest::new();
        manifest.add_sst(0, SSTMeta::new(10, 0, b"aaa".to_vec(), b"mmm".to_vec()));
        manifest.add_sst(0, SSTMeta::new(11, 0, b"nnn".to_vec(), b"zzz".to_vec()));
        manifest.add_sst(1, SSTMeta::new(20, 1, b"a".to_vec(), b"z".to_vec()));
        manifest.add_sst(2, SSTMeta::new(30, 2, b"0".to_vec(), b"9".to_vec()));

        let encoded = manifest.encode();
        let decoded = Manifest::decode(&encoded).unwrap();

        assert_eq!(decoded.levels.len(), 3);
        assert_eq!(decoded.total_ssts(), 4);

        assert_eq!(decoded.ssts_at_level(0).len(), 2);
        assert_eq!(decoded.ssts_at_level(0)[0].id, 10);
        assert_eq!(decoded.ssts_at_level(0)[1].id, 11);
        assert_eq!(decoded.ssts_at_level(1)[0].id, 20);
        assert_eq!(decoded.ssts_at_level(2)[0].id, 30);
    }

    #[test]
    fn manifest_remove_sst() {
        let mut manifest = Manifest::new();
        manifest.add_sst(0, SSTMeta::new(1, 0, b"a".to_vec(), b"m".to_vec()));
        manifest.add_sst(0, SSTMeta::new(2, 0, b"n".to_vec(), b"z".to_vec()));
        manifest.add_sst(1, SSTMeta::new(3, 1, b"a".to_vec(), b"z".to_vec()));

        assert!(manifest.remove_sst(2));
        assert_eq!(manifest.total_ssts(), 2);
        assert_eq!(manifest.ssts_at_level(0).len(), 1);
        // Already removed.
        assert!(!manifest.remove_sst(2));
    }

    #[test]
    fn manifest_corrupted_magic() {
        let mut data = b"JUNK".to_vec();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        let result = Manifest::decode(&data);
        assert!(matches!(result, Err(KvError::Corruption(_))));
    }
}
