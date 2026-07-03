//! WriteBatch — atomic batch of put/delete operations.

/// A single operation in a write batch.
#[derive(Debug, Clone)]
pub enum BatchOp {
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        key: Vec<u8>,
    },
}

/// A batch of key-value operations that will be applied atomically.
///
/// All operations in the batch are written to the WAL before being
/// applied to the MemTable, ensuring atomicity.
#[derive(Debug, Clone, Default)]
pub struct WriteBatch {
    ops: Vec<BatchOp>,
}

impl WriteBatch {
    /// Create a new, empty batch.
    pub fn new() -> Self {
        WriteBatch { ops: Vec::new() }
    }

    /// Add a put operation to the batch.
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.ops.push(BatchOp::Put { key, value });
    }

    /// Add a delete operation to the batch.
    pub fn delete(&mut self, key: Vec<u8>) {
        self.ops.push(BatchOp::Delete { key });
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Number of operations in the batch.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Iterate over the operations.
    pub fn ops(&self) -> &[BatchOp] {
        &self.ops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writebatch_basic() {
        let mut batch = WriteBatch::new();
        assert!(batch.is_empty());

        batch.put(b"a".to_vec(), b"1".to_vec());
        batch.put(b"b".to_vec(), b"2".to_vec());
        batch.delete(b"c".to_vec());

        assert_eq!(batch.len(), 3);
        assert!(!batch.is_empty());
    }
}
