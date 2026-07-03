/// A key in the key-value store, wrapping raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(pub Vec<u8>);

/// A value in the key-value store, wrapping raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value(pub Vec<u8>);

/// Monotonically increasing sequence number for MVCC ordering.
/// Each write operation is assigned a unique sequence number so that
/// reads can see a consistent snapshot in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SequenceNumber(pub u64);

impl SequenceNumber {
    pub const MIN: Self = Self(0);
    pub const MAX: Self = Self(u64::MAX);

    /// Return the next sequence number.
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for SequenceNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "seq:{}", self.0)
    }
}

// Convenient conversions

impl From<Vec<u8>> for Key {
    fn from(v: Vec<u8>) -> Self {
        Key(v)
    }
}

impl From<&[u8]> for Key {
    fn from(s: &[u8]) -> Self {
        Key(s.to_vec())
    }
}

impl AsRef<[u8]> for Key {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value(v)
    }
}

impl From<&[u8]> for Value {
    fn from(s: &[u8]) -> Self {
        Value(s.to_vec())
    }
}

impl AsRef<[u8]> for Value {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<u64> for SequenceNumber {
    fn from(v: u64) -> Self {
        SequenceNumber(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ordering() {
        let k1 = Key(b"abc".to_vec());
        let k2 = Key(b"abd".to_vec());
        assert!(k1 < k2);
    }

    #[test]
    fn sequence_number_next() {
        let seq = SequenceNumber(42);
        assert_eq!(seq.next(), SequenceNumber(43));
    }

    #[test]
    fn sequence_number_display() {
        let seq = SequenceNumber(7);
        assert_eq!(format!("{}", seq), "seq:7");
    }

    #[test]
    fn key_from_slices() {
        let k: Key = b"hello".as_slice().into();
        assert_eq!(k.as_ref(), b"hello");
    }

    #[test]
    fn value_as_ref() {
        let v = Value(vec![1, 2, 3]);
        assert_eq!(v.as_ref(), &[1, 2, 3]);
    }
}
