//! Shared error types for the Fence engine.
//!
//! All fallible operations return `Result<T, FenceError>`. Error variants
//! carry context data (offsets, sizes, etc.) for debugging without
//! heap-allocating format strings on the hot path.

use std::fmt;

/// Errors that can occur during Fence operations.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FenceError {
    /// Pool has no remaining slots for a new record.
    PoolFull {
        capacity: u32,
        requested_slot: u64,
    },

    /// Payload exceeds the pool's configured maximum.
    PayloadTooLarge {
        max: u32,
        got: usize,
    },

    /// Record at the given index has an invalid checksum (corruption or torn write).
    ChecksumMismatch {
        index: u64,
    },

    /// The pool file/region could not be memory-mapped.
    MmapFailed {
        detail: String,
    },

    /// The pool header contains an invalid magic number.
    InvalidMagic {
        expected: u64,
        actual: u64,
    },

    /// An I/O error occurred during backend operations.
    Io {
        detail: String,
    },

    /// The requested record index is out of bounds.
    IndexOutOfBounds {
        index: u64,
        committed_tail: u64,
    },

    /// Trim was requested past the committed tail.
    TrimBeyondCommitted {
        requested: u64,
        committed_tail: u64,
    },
}

impl fmt::Display for FenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PoolFull { capacity, requested_slot } => {
                write!(f, "pool full: capacity {capacity}, requested slot {requested_slot}")
            }
            Self::PayloadTooLarge { max, got } => {
                write!(f, "payload too large: max {max} bytes, got {got}")
            }
            Self::ChecksumMismatch { index } => {
                write!(f, "checksum mismatch at index {index}")
            }
            Self::MmapFailed { detail } => {
                write!(f, "mmap failed: {detail}")
            }
            Self::InvalidMagic { expected, actual } => {
                write!(f, "invalid magic: expected 0x{expected:016X}, got 0x{actual:016X}")
            }
            Self::Io { detail } => {
                write!(f, "i/o error: {detail}")
            }
            Self::IndexOutOfBounds { index, committed_tail } => {
                write!(f, "index {index} out of bounds (committed_tail: {committed_tail})")
            }
            Self::TrimBeyondCommitted { requested, committed_tail } => {
                write!(f, "trim to {requested} exceeds committed_tail {committed_tail}")
            }
        }
    }
}

impl std::error::Error for FenceError {}

impl From<std::io::Error> for FenceError {
    fn from(e: std::io::Error) -> Self {
        Self::Io { detail: e.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_pool_full() {
        let e = FenceError::PoolFull { capacity: 1024, requested_slot: 1024 };
        assert_eq!(e.to_string(), "pool full: capacity 1024, requested slot 1024");
    }

    #[test]
    fn display_payload_too_large() {
        let e = FenceError::PayloadTooLarge { max: 192, got: 256 };
        assert_eq!(e.to_string(), "payload too large: max 192 bytes, got 256");
    }

    #[test]
    fn display_checksum_mismatch() {
        let e = FenceError::ChecksumMismatch { index: 42 };
        assert_eq!(e.to_string(), "checksum mismatch at index 42");
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let fence_err: FenceError = io_err.into();
        assert!(matches!(fence_err, FenceError::Io { .. }));
    }
}
