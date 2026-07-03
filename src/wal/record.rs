//! WAL record encoding and decoding.
//!
//! On-disk format (all integers little-endian):
//!
//! ```text
//! [len: u32] [crc32: u32] [op_type: u8] [key_len: u32] [key] [value_len: u32] [value]
//! ```
//!
//! - `len` = bytes after itself: 4 (crc) + 1 (op) + 4 (key_len) + key + 4 (value_len) + value
//! - `crc32` = CRC32 of op_type ‖ key_len ‖ key ‖ value_len ‖ value

use crate::error::{KvError, Result};

/// Operation type stored in each WAL record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpType {
    /// Write a key-value pair.
    Put = 0x01,
    /// Delete a key (tombstone).
    Delete = 0x02,
}

impl OpType {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0x01 => Ok(OpType::Put),
            0x02 => Ok(OpType::Delete),
            _ => Err(KvError::Corruption(format!("unknown op type: {}", v))),
        }
    }
}

/// A single key-value record stored in the WAL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub op: OpType,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

impl Record {
    /// Serialize this record into the on-disk format.
    pub fn encode(&self) -> Vec<u8> {
        let crc_payload = self.crc_payload();
        let crc = crc32fast::hash(&crc_payload);

        let record_len = 4 + crc_payload.len();
        let total_len = 4 + record_len;

        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(&(record_len as u32).to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&crc_payload);
        buf
    }

    /// Decode a record from the byte buffer that follows the `len` prefix.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        // Minimum: crc(4) + op(1) + key_len(4) + value_len(4) = 13
        if buf.len() < 13 {
            return Err(KvError::Corruption(format!(
                "WAL record too short: need at least 13 bytes, got {}",
                buf.len()
            )));
        }

        let stored_crc = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let actual_crc = crc32fast::hash(&buf[4..]);

        if stored_crc != actual_crc {
            return Err(KvError::ChecksumMismatch {
                expected: stored_crc,
                actual: actual_crc,
            });
        }

        // op_type at buf[4]
        let op = OpType::from_u8(buf[4])?;

        // key_len at buf[5..9]
        let key_len =
            u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
        let key_start = 9;
        let key_end = key_start + key_len;
        if buf.len() < key_end + 4 {
            return Err(KvError::Corruption("WAL record truncated in key".to_string()));
        }
        let key = buf[key_start..key_end].to_vec();

        // value_len at buf[key_end..key_end+4]
        let val_len = u32::from_le_bytes([
            buf[key_end],
            buf[key_end + 1],
            buf[key_end + 2],
            buf[key_end + 3],
        ]) as usize;
        let val_start = key_end + 4;
        let val_end = val_start + val_len;
        if buf.len() < val_end {
            return Err(KvError::Corruption(
                "WAL record truncated in value".to_string(),
            ));
        }
        let value = buf[val_start..val_end].to_vec();

        Ok(Record { op, key, value })
    }

    /// Return the bytes that the CRC checksum covers.
    pub fn crc_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(9 + self.key.len() + self.value.len());
        payload.push(self.op as u8);
        payload.extend_from_slice(&(self.key.len() as u32).to_le_bytes());
        payload.extend_from_slice(&self.key);
        payload.extend_from_slice(&(self.value.len() as u32).to_le_bytes());
        payload.extend_from_slice(&self.value);
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip() {
        let rec = Record {
            op: OpType::Put,
            key: b"hello".to_vec(),
            value: b"world".to_vec(),
        };
        let encoded = rec.encode();
        let decoded = Record::decode(&encoded[4..]).unwrap();
        assert_eq!(decoded, rec);
    }

    #[test]
    fn record_delete_roundtrip() {
        let rec = Record {
            op: OpType::Delete,
            key: b"rm".to_vec(),
            value: Vec::new(),
        };
        let encoded = rec.encode();
        let decoded = Record::decode(&encoded[4..]).unwrap();
        assert_eq!(decoded.op, OpType::Delete);
        assert_eq!(decoded.key, b"rm");
    }

    #[test]
    fn record_crc_detects_corruption() {
        let rec = Record {
            op: OpType::Put,
            key: b"key".to_vec(),
            value: b"val".to_vec(),
        };
        let mut encoded = rec.encode();
        encoded[10] ^= 0xff;
        let result = Record::decode(&encoded[4..]);
        assert!(matches!(result, Err(KvError::ChecksumMismatch { .. })));
    }

    #[test]
    fn record_truncated_returns_error() {
        let result = Record::decode(&[0u8, 1u8]);
        assert!(matches!(result, Err(KvError::Corruption(_))));
    }
}
