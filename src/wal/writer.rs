//! WAL writer and reader.
//!
//! `WALWriter` appends key-value records to a log file with optional fsync.
//! `WALReader` iterates over all records in a WAL file, stopping on
//! corruption or truncation (simulating crash recovery).

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::error::{KvError, Result};
use crate::wal::record::{OpType, Record};

// ---------------------------------------------------------------------------
// WALWriter
// ---------------------------------------------------------------------------

/// Append-only WAL writer backed by a `BufWriter<File>`.
pub struct WALWriter {
    file: BufWriter<File>,
}

impl WALWriter {
    /// Open (or create) a WAL file and seek to its end so subsequent
    /// `append()` calls extend the log.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        Ok(WALWriter {
            file: BufWriter::new(file),
        })
    }

    /// Append a key-value record to the WAL and flush the internal buffer.
    pub fn append(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let record = Record {
            op: OpType::Put,
            key: key.to_vec(),
            value: value.to_vec(),
        };
        let encoded = record.encode();
        self.file.write_all(&encoded)?;
        Ok(())
    }

    /// Append a record with an explicit operation type.
    pub fn append_record(&mut self, record: &Record) -> Result<()> {
        let encoded = record.encode();
        self.file.write_all(&encoded)?;
        Ok(())
    }

    /// Flush the internal `BufWriter` and issue `fsync` so that all
    /// previously appended records are durable.
    pub fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_data()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// WALReader
// ---------------------------------------------------------------------------

/// Sequential WAL reader that yields `(key, value)` pairs.
///
/// When a corrupted or truncated record is encountered the iterator stops
/// gracefully — this is the expected behaviour during crash recovery.
pub struct WALReader {
    reader: BufReader<File>,
}

impl WALReader {
    /// Open a WAL file for reading.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        Ok(WALReader {
            reader: BufReader::new(file),
        })
    }
}

impl Iterator for WALReader {
    type Item = Result<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut len_buf = [0u8; 4];
        match self.reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(e) => return Some(Err(KvError::Io(e))),
        }

        let record_len = u32::from_le_bytes(len_buf) as usize;
        let mut record_buf = vec![0u8; record_len];
        match self.reader.read_exact(&mut record_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(e) => return Some(Err(KvError::Io(e))),
        }

        Some(Record::decode(&record_buf))
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
    fn wal_append_and_read_back() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let mut writer = WALWriter::open(&path).unwrap();
        writer.append(b"key1", b"value1").unwrap();
        writer.append(b"key2", b"value2").unwrap();
        writer.flush().unwrap();

        let entries: Vec<_> = WALReader::open(&path).unwrap().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].as_ref().unwrap().key, b"key1");
        assert_eq!(entries[0].as_ref().unwrap().value, b"value1");
        assert_eq!(entries[1].as_ref().unwrap().key, b"key2");
        assert_eq!(entries[1].as_ref().unwrap().value, b"value2");
    }

    #[test]
    fn wal_crc_detects_corruption() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("corrupt.wal");

        let mut writer = WALWriter::open(&path).unwrap();
        writer.append(b"key", b"value").unwrap();
        writer.flush().unwrap();

        // Corrupt a byte in the middle of the file.
        let mut data = std::fs::read(&path).unwrap();
        data[8] ^= 0xff;
        std::fs::write(&path, &data).unwrap();

        let mut reader = WALReader::open(&path).unwrap();
        let result = reader.next().unwrap();
        assert!(
            matches!(result, Err(KvError::ChecksumMismatch { .. })),
            "expected ChecksumMismatch, got {:?}",
            result
        );
    }

    #[test]
    fn wal_recovery_after_truncation() {
        // Simulate a crash by truncating the last few bytes of the file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("truncated.wal");

        let mut writer = WALWriter::open(&path).unwrap();
        writer.append(b"a", b"1").unwrap();
        writer.append(b"b", b"2").unwrap();
        writer.flush().unwrap();

        // Chop off the last 3 bytes (corrupts the second record).
        let len = std::fs::metadata(&path).unwrap().len();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(len - 3).unwrap();

        let entries: Vec<_> = WALReader::open(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        // Should recover the first record; the second is lost.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, b"a");
        assert_eq!(entries[0].value, b"1");
    }

    #[test]
    fn wal_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.wal");

        // Create an empty file.
        File::create(&path).unwrap();

        let entries: Vec<_> = WALReader::open(&path).unwrap().collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn wal_multiple_appends() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.wal");

        let mut writer = WALWriter::open(&path).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let val = format!("val_{:04}", i);
            writer.append(key.as_bytes(), val.as_bytes()).unwrap();
        }
        writer.flush().unwrap();

        let entries: Vec<_> = WALReader::open(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 100);
        for (i, rec) in entries.iter().enumerate() {
            assert_eq!(rec.key, format!("key_{:04}", i).as_bytes());
            assert_eq!(rec.value, format!("val_{:04}", i).as_bytes());
        }
    }
}
