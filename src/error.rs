use std::fmt;

/// Unified error type for the key-value storage engine.
#[derive(Debug)]
pub enum KvError {
    /// I/O error from the underlying filesystem.
    Io(std::io::Error),
    /// Data corruption detected (e.g. CRC mismatch, truncated record).
    Corruption(String),
    /// The requested key was not found.
    KeyNotFound(Vec<u8>),
    /// An invalid argument was passed to an API call.
    InvalidArgument(String),
    /// The database has been closed and is no longer accepting operations.
    Closed,
    /// A WAL record's CRC32 does not match the stored checksum.
    ChecksumMismatch { expected: u32, actual: u32 },
    /// An operation was attempted that violates internal invariants.
    Internal(String),
}

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KvError::Io(err) => write!(f, "IO error: {}", err),
            KvError::Corruption(msg) => write!(f, "data corruption: {}", msg),
            KvError::KeyNotFound(key) => {
                write!(f, "key not found: {:?}", String::from_utf8_lossy(key))
            }
            KvError::InvalidArgument(msg) => write!(f, "invalid argument: {}", msg),
            KvError::Closed => write!(f, "database is closed"),
            KvError::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "checksum mismatch: expected 0x{:08x}, got 0x{:08x}",
                    expected, actual
                )
            }
            KvError::Internal(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for KvError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KvError::Io(err) => Some(err),
            _ => None,
        }
    }
}

/// Convenient conversion from `std::io::Error`.
impl From<std::io::Error> for KvError {
    fn from(err: std::io::Error) -> Self {
        KvError::Io(err)
    }
}

/// Type alias used throughout the engine.
pub type Result<T> = std::result::Result<T, KvError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn display_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = KvError::from(io_err);
        assert!(err.to_string().contains("IO error"));
        assert!(err.to_string().contains("file missing"));
    }

    #[test]
    fn display_corruption() {
        let err = KvError::Corruption("bad header byte".to_string());
        assert_eq!(err.to_string(), "data corruption: bad header byte");
    }

    #[test]
    fn display_key_not_found() {
        let err = KvError::KeyNotFound(b"mykey".to_vec());
        assert!(err.to_string().contains("mykey"));
    }

    #[test]
    fn display_checksum_mismatch() {
        let err = KvError::ChecksumMismatch {
            expected: 0xdeadbeef,
            actual: 0x00000000,
        };
        let s = err.to_string();
        assert!(s.contains("deadbeef"));
        assert!(s.contains("00000000"));
    }

    #[test]
    fn display_closed() {
        let err = KvError::Closed;
        assert_eq!(err.to_string(), "database is closed");
    }

    #[test]
    fn io_error_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no access");
        let err: KvError = io_err.into();
        assert!(matches!(err, KvError::Io(_)));
    }

    #[test]
    fn io_error_source_chain() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "root cause");
        let err = KvError::from(io_err);
        // source() should return the inner io::Error
        assert!(err.source().is_some());
    }

    #[test]
    fn non_io_error_has_no_source() {
        let err = KvError::Internal("oops".to_string());
        assert!(err.source().is_none());
    }

    #[test]
    fn result_alias_works() {
        fn returns_ok() -> Result<i32> {
            Ok(42)
        }
        fn returns_err() -> Result<i32> {
            Err(KvError::Closed)
        }
        assert_eq!(returns_ok().unwrap(), 42);
        assert!(returns_err().is_err());
    }
}
