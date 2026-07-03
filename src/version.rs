use crate::manifest::SSTMeta;

/// Tracks which SSTable files belong to each level of the LSM tree.
///
/// This is a placeholder — full implementation in Phase 2 (Compaction).
#[derive(Debug, Clone)]
pub struct Version {
    /// SSTable metadata organized by level.
    pub levels: Vec<Vec<SSTMeta>>,
}

impl Default for Version {
    fn default() -> Self {
        Self::new()
    }
}

impl Version {
    pub fn new() -> Self {
        Self { levels: Vec::new() }
    }
}
