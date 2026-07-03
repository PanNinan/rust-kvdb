use crate::error::KvError;

/// Metadata for a single SSTable file
#[derive(Debug, Clone)]
pub struct SSTMeta {
    pub id: u64,
    pub level: usize,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
}

/// Manages the list of active SSTable files across all levels
#[derive(Debug)]
pub struct Manifest {
    /// SSTable metadata organized by level: levels[level] = Vec<SSTMeta>
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

    /// Load manifest from a file on disk
    pub fn load(_path: &std::path::Path) -> Result<Self, KvError> {
        // TODO: Phase 1.7
        Ok(Self::new())
    }

    /// Save manifest to a file on disk
    pub fn save(&self, _path: &std::path::Path) -> Result<(), KvError> {
        // TODO: Phase 1.7
        Ok(())
    }

    /// Add an SSTable entry to the given level
    pub fn add_sst(&mut self, level: usize, meta: SSTMeta) {
        while self.levels.len() <= level {
            self.levels.push(Vec::new());
        }
        self.levels[level].push(meta);
    }

    /// Get all SSTable metadata at a given level
    pub fn ssts_at_level(&self, level: usize) -> &[SSTMeta] {
        self.levels.get(level).map_or(&[], |v| v.as_slice())
    }
}
