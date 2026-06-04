//! CRC32C checksum computation and verification for Fence records.
//!
//! Each committed record stores a CRC32C covering (term || index || payload).
//! On read, the checksum is recomputed and compared to detect corruption
//! or torn writes.

use crc32fast::Hasher;

/// Compute the CRC32C checksum for a record.
///
/// The checksum covers: term (8 bytes LE) || index (8 bytes LE) || payload.
/// This ensures that swapping payloads between records or reordering records
/// produces a different checksum.
///
/// Returns the checksum as `u64` (upper 32 bits are zero) for consistent
/// storage in the 8-byte checksum field.
#[inline]
pub fn compute(term: u64, index: u64, payload: &[u8]) -> u64 {
    let mut hasher = Hasher::new();
    hasher.update(&term.to_le_bytes());
    hasher.update(&index.to_le_bytes());
    hasher.update(payload);
    hasher.finalize() as u64
}

/// Verify a record's checksum against an expected value.
///
/// Returns `true` if the computed checksum matches `expected`.
#[inline]
pub fn verify(term: u64, index: u64, payload: &[u8], expected: u64) -> bool {
    compute(term, index, payload) == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_deterministic() {
        let a = compute(1, 42, b"hello");
        let b = compute(1, 42, b"hello");
        assert_eq!(a, b);
    }

    #[test]
    fn different_payload_different_checksum() {
        let a = compute(1, 1, b"hello");
        let b = compute(1, 1, b"world");
        assert_ne!(a, b);
    }

    #[test]
    fn different_term_different_checksum() {
        let a = compute(1, 1, b"data");
        let b = compute(2, 1, b"data");
        assert_ne!(a, b);
    }

    #[test]
    fn different_index_different_checksum() {
        let a = compute(1, 1, b"data");
        let b = compute(1, 2, b"data");
        assert_ne!(a, b);
    }

    #[test]
    fn empty_payload_works() {
        let crc = compute(0, 0, b"");
        assert!(crc > 0 || crc == 0); // just ensure no panic
        // Verify roundtrip
        assert!(verify(0, 0, b"", crc));
    }

    #[test]
    fn verify_correct() {
        let crc = compute(5, 100, b"test payload");
        assert!(verify(5, 100, b"test payload", crc));
    }

    #[test]
    fn verify_wrong_checksum() {
        let crc = compute(5, 100, b"test payload");
        assert!(!verify(5, 100, b"test payload", crc + 1));
    }

    #[test]
    fn verify_wrong_payload() {
        let crc = compute(5, 100, b"test payload");
        assert!(!verify(5, 100, b"wrong payload", crc));
    }

    #[test]
    fn checksum_fits_in_u32_range() {
        // CRC32C output is 32 bits; upper 32 bits of our u64 should be zero
        let crc = compute(u64::MAX, u64::MAX, b"max values test");
        assert_eq!(crc >> 32, 0);
    }
}
