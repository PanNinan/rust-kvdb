//! Bloom filter — a space-efficient probabilistic data structure for
//! set-membership testing.
//!
//! Uses double-hashing: `h(i) = h1 + i * h2` so only two hash computations
//! are needed regardless of the number of hash functions.

use crate::error::{KvError, Result};

/// A Bloom filter for testing whether a key *may* be in a set.
///
/// False positives are possible; false negatives are not.
#[derive(Debug, Clone)]
pub struct BloomFilter {
    /// Bit array stored as a vector of u64 words.
    bits: Vec<u64>,
    /// Number of hash functions.
    num_hashes: usize,
    /// Total number of bits (bits.len() * 64).
    num_bits: usize,
}

impl BloomFilter {
    /// Build a Bloom filter from the given keys.
    ///
    /// `bits_per_key` controls the false-positive rate:
    /// 10 bits/key ≈ 1% FP, 14 bits/key ≈ 0.1% FP.
    pub fn build(keys: &[&[u8]], bits_per_key: usize) -> Self {
        let n = keys.len();
        let num_bits = (n * bits_per_key).max(64);
        let num_words = num_bits.div_ceil(64);
        let num_hashes = Self::optimal_hashes(bits_per_key);

        let mut bits = vec![0u64; num_words];

        for key in keys {
            let (h1, h2) = Self::double_hash(key);
            for i in 0..num_hashes {
                let idx = h1.wrapping_add((i as u32).wrapping_mul(h2));
                let pos = (idx as usize) % num_bits;
                bits[pos / 64] |= 1u64 << (pos % 64);
            }
        }

        BloomFilter {
            bits,
            num_hashes,
            num_bits,
        }
    }

    /// Test whether the key *may* be in the set.
    ///
    /// Returns `true` if the key might be present (could be a false positive).
    /// Returns `false` if the key is definitely not present.
    pub fn may_contain(&self, key: &[u8]) -> bool {
        if self.num_bits == 0 {
            return false;
        }
        let (h1, h2) = Self::double_hash(key);
        for i in 0..self.num_hashes {
            let idx = h1.wrapping_add((i as u32).wrapping_mul(h2));
            let pos = (idx as usize) % self.num_bits;
            if self.bits[pos / 64] & (1u64 << (pos % 64)) == 0 {
                return false;
            }
        }
        true
    }

    /// Serialize the bloom filter to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12 + self.bits.len() * 8);
        buf.extend_from_slice(&(self.num_hashes as u32).to_le_bytes());
        buf.extend_from_slice(&(self.num_bits as u32).to_le_bytes());
        buf.extend_from_slice(&(self.bits.len() as u32).to_le_bytes());
        for word in &self.bits {
            buf.extend_from_slice(&word.to_le_bytes());
        }
        buf
    }

    /// Deserialize a bloom filter from bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 12 {
            return Err(KvError::Corruption("bloom filter too short".to_string()));
        }
        let num_hashes = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let num_bits = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let num_words = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

        let expected = 12 + num_words * 8;
        if data.len() < expected {
            return Err(KvError::Corruption("bloom filter data truncated".to_string()));
        }

        let mut bits = Vec::with_capacity(num_words);
        for i in 0..num_words {
            let offset = 12 + i * 8;
            let word = u64::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
            bits.push(word);
        }

        Ok(BloomFilter {
            bits,
            num_hashes,
            num_bits,
        })
    }

    /// Compute two independent hashes using FNV-1a-like mixing.
    fn double_hash(key: &[u8]) -> (u32, u32) {
        let h1 = Self::fnv1a(key);
        // Second hash: mix h1 with a different seed.
        let h2 = Self::fnv1a_with_seed(key, 0x811c_9dc5 ^ 0x0100_0193);
        (h1, h2.wrapping_mul(2).wrapping_add(1)) // h2 must be odd for full-period LCG
    }

    /// FNV-1a hash.
    fn fnv1a(key: &[u8]) -> u32 {
        Self::fnv1a_with_seed(key, 0x811c_9dc5)
    }

    fn fnv1a_with_seed(key: &[u8], seed: u32) -> u32 {
        let mut h = seed;
        for &b in key {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    /// Optimal number of hash functions for a given bits-per-key ratio.
    fn optimal_hashes(bits_per_key: usize) -> usize {
        let k = (bits_per_key as f64 * std::f64::consts::LN_2).round() as usize;
        k.clamp(1, 30)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_no_false_negatives() {
        let keys: Vec<Vec<u8>> = (0..10000)
            .map(|i| format!("key_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let filter = BloomFilter::build(&refs, 10);

        for key in &keys {
            assert!(
                filter.may_contain(key),
                "false negative for existing key"
            );
        }
    }

    #[test]
    fn bloom_false_positive_rate() {
        let keys: Vec<Vec<u8>> = (0..10000)
            .map(|i| format!("key_{}", i).into_bytes())
            .collect();
        let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let filter = BloomFilter::build(&refs, 10);

        let mut false_positives = 0;
        let test_count = 100_000;
        for i in 0..test_count {
            let probe = format!("probe_{}", i);
            if filter.may_contain(probe.as_bytes()) {
                false_positives += 1;
            }
        }
        let fp_rate = false_positives as f64 / test_count as f64;
        assert!(
            fp_rate < 0.02,
            "false positive rate too high: {:.4}",
            fp_rate
        );
    }

    #[test]
    fn bloom_encode_decode_roundtrip() {
        let keys: Vec<&[u8]> = vec![b"hello", b"world", b"foo", b"bar"];
        let filter = BloomFilter::build(&keys, 10);
        let encoded = filter.encode();
        let decoded = BloomFilter::decode(&encoded).unwrap();

        for key in &keys {
            assert!(decoded.may_contain(key));
        }
    }

    #[test]
    fn bloom_empty_set() {
        let filter = BloomFilter::build(&[], 10);
        assert!(!filter.may_contain(b"anything"));
    }
}
