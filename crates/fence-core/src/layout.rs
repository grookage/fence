//! Memory layout definitions for the Fence shared-memory pool.
//!
//! This module is the single source of truth for the binary format.
//! Compile-time constants define the fixed structure. Runtime-configurable
//! dimensions (capacity, record_size, max_hosts) live in the pool header
//! and are read on open.

/// Magic number written at byte 0 of every valid Fence pool.
/// ASCII encoding of "FENCE_LO". Used to detect uninitialized or foreign data.
pub const MAGIC: u64 = 0x46454E43455F4C4F;

/// Size of the pool header in bytes. Exactly one cacheline.
/// The header occupies bytes [0, 64) of the pool.
pub const HEADER_SIZE: usize = 64;

/// Size of the fixed metadata portion of each record. One cacheline.
/// Contains: state, term, index, writer_id, data_len, checksum, padding.
/// The variable-size payload follows immediately after this.
pub const RECORD_META_SIZE: usize = 64;

/// x86 cacheline width in bytes. All structures are aligned to this boundary
/// to prevent false sharing between CPU cores and across CXL hosts.
pub const CACHELINE: usize = 64;

/// Record Lifecycle states are below.
/// While Rust enum looks elegant, we don't use it here, because this value lives in shared memory that any host can write to.
/// If a bug or hardware corruption writes 0xFF into the state byte, and we read it as a Rust enum, that's instant undefined behavior —
/// Rust assumes an enum always holds a valid variant. The compiler generates code that relies on this assumption.
/// With plain u8, reading 0xFF from shared memory is just... the number 255. Our code handles it explicitly.

/// Record slot is empty. Never been written to, or has been zeroed by compaction.
pub const STATE_FREE: u8 = 0;

/// A writer has claimed this slot and is actively writing payload data.
/// If the writer crashes in this state, recovery will mark it ABANDONED.
pub const STATE_WRITING: u8 = 1;

/// Payload is fully written, checksum is valid, data is flushed.
/// Readers may safely read this record.
pub const STATE_COMMITTED: u8 = 2;

/// The writer crashed mid-write, or this slot was reclaimed by trim/compaction.
/// Readers skip this slot. The data is garbage or zeroed.
pub const STATE_ABANDONED: u8 = 3;

// ─── Pool header offsets ───────────────
// CxlLogHeader layout (64 bytes total):
//   [0..8)   magic
//   [8..16)  capacity
//   [16..24) record_size
//   [24..32) max_hosts
//   [32..40) committed_tail  (atomic)
//   [40..48) reserve_tail    (atomic)
//   [48..56) epoch
//  [56..64) _padding

/// Byte offset of `magic` within the pool header.
pub const HDR_MAGIC: usize = 0;

/// Byte offset of `capacity` (number of record slots) within the pool header.
pub const HDR_CAPACITY: usize = 8;

/// Byte offset of `record_size` (bytes per record) within the pool header.
pub const HDR_RECORD_SIZE: usize = 16;

/// Byte offset of `max_hosts` within the pool header.
pub const HDR_MAX_HOSTS: usize = 24;

/// Byte offset of `committed_tail` within the pool header.
/// This field is accessed atomically (load/store/CAS via MemoryBackend).
pub const HDR_COMMITTED_TAIL: usize = 32;

/// Byte offset of `reserve_tail` within the pool header.
/// This field is accessed atomically (fetch_add via MemoryBackend).
pub const HDR_RESERVE_TAIL: usize = 40;

/// Byte offset of `epoch` within the pool header.
pub const HDR_EPOCH: usize = 48;

// ─── Record Field Offsets (relative to record start) ───────────────
//
// Each record is a contiguous block of `record_size` bytes.
// The first 64 bytes (RECORD_META_SIZE) are fixed-layout metadata.
// The remaining bytes are the variable-size payload.
//
// Metadata layout (first 64 bytes of each record):
//   [0..1)   state        (u8, accessed atomically)
//   [1..8)   _pad1        (7 bytes alignment padding)
//   [8..16)  term         (u64)
//   [16..24) index        (u64)
//   [24..32) writer_id    (u64)
//   [32..36) data_len     (u32)
//   [36..40) _pad2        (4 bytes alignment padding)
//   [40..48) checksum     (u64)
//   [48..64) _pad3        (16 bytes padding to fill cacheline)
//
// Payload starts at byte 64 (RECORD_META_SIZE) from the record start.
//
// Padding rationale:
//   _pad1: state is 1 byte, but term (u64) must be 8-byte aligned.
//   _pad2: data_len is 4 bytes ending at offset 36; checksum (u64) must start at 8-byte boundary (40).
//   _pad3: after checksum ends at byte 48, 16 unused bytes fill to 64 so payload starts on a cacheline boundary.
//
// These are RELATIVE offsets (within a record). The engine computes absolute
// offsets as: HEADER_SIZE + (slot * record_size) + REC_<field>.

/// Byte offset of `state` within a record. 1 byte (u8), accessed atomically.
/// Holds one of: STATE_FREE, STATE_WRITING, STATE_COMMITTED, STATE_ABANDONED.
pub const REC_STATE: usize = 0;

/// Byte offset of `term` within a record. 8 bytes (u64).
/// The Raft/consensus term this record was written in.
pub const REC_TERM: usize = 8;

/// Byte offset of `index` within a record. 8 bytes (u64).
/// The global sequence number of this record (slot + 1).
pub const REC_INDEX: usize = 16;

/// Byte offset of `writer_id` within a record. 8 bytes (u64).
/// Identifies which host wrote this record. Used for recovery attribution.
pub const REC_WRITER_ID: usize = 24;

/// Byte offset of `data_len` within a record. 4 bytes (u32).
/// The actual number of payload bytes written (may be less than payload_max).
pub const REC_DATA_LEN: usize = 32;

/// Byte offset of `checksum` within a record. 8 bytes (u64).
/// CRC32C computed over (term || index || payload[0..data_len]).
/// Used by readers and recovery to detect torn writes or corruption.
pub const REC_CHECKSUM: usize = 40;

/// Byte offset where the payload begins within a record.
/// Equals RECORD_META_SIZE (64). Payload extends for `data_len` bytes
/// (up to a maximum of `payload_max` = record_size - RECORD_META_SIZE).
pub const REC_PAYLOAD: usize = RECORD_META_SIZE;

// ─── RecordMeta Struct (for compile-time layout verification) ──────
//
// This struct is NOT used for memory access at runtime. All field access
// goes through MemoryBackend + offset constants (REC_STATE, REC_TERM, etc.).
//
// It exists solely so we can write compile-time assertions:
//   assert_eq!(offset_of!(RecordMeta, term), REC_TERM);
//   assert_eq!(size_of::<RecordMeta>(), RECORD_META_SIZE);

/// Mirror of the 64-byte record metadata layout.
/// Used only in tests to verify offset constants match the actual layout.
/// Size =  state(1) + _pad1(7) + term(8) + index(8) + writer_id(8) + data_len(4) + _pad2(4) + checksum(8) + _pad3(16) = 64 bytes
#[repr(C, align(64))]
#[allow(dead_code)]
pub struct RecordMeta {
    /// Record lifecycle state (STATE_FREE / WRITING / COMMITTED / ABANDONED).
    pub state: u8,
    /// Alignment padding. Ensures `term` starts at offset 8.
    pub _pad1: [u8; 7],
    /// Raft/consensus term this record was written in.
    pub term: u64,
    /// Global sequence number (slot + 1).
    pub index: u64,
    /// Identifies which host wrote this record.
    pub writer_id: u64,
    /// Actual number of payload bytes (may be less than payload_max).
    pub data_len: u32,
    /// Alignment padding. Ensures `checksum` starts at offset 40.
    pub _pad2: [u8; 4],
    /// CRC32C of (term || index || payload[0..data_len]).
    pub checksum: u64,
    /// Padding to fill the struct to exactly 64 bytes (one cacheline).
    pub _pad3: [u8; 16],
}

// ─── MetricsBlock Struct (per-host counters) ───────────────────────
//
// One MetricsBlock per host, stored in a contiguous array at the end
// of the pool (after all record slots). Each block is one cacheline
// to prevent false sharing between hosts' counters.
//
// Pool layout: [Header (64B)] [Records...] [MetricsBlock[0]] [MetricsBlock[1]] ...
//
// All fields are accessed atomically with Relaxed ordering (metrics are
// best-effort — losing a count to a race is acceptable).

/// Per-host metrics counters. 64 bytes (one cacheline).
/// Each host increments only its own block. Readers scan all blocks to aggregate.
/// Size = 7 fields * 8 bytes + 8 padding = 64 bytes.
#[repr(C, align(64))]
#[allow(dead_code)]
pub struct MetricsBlock {
    /// Total successful appends from this host.
    pub appends_total: u64,
    /// Failed appends (pool full, payload too large).
    pub appends_failed: u64,
    /// Total payload bytes written by this host.
    pub bytes_written: u64,
    /// Total reads performed by this host.
    pub reads_total: u64,
    /// Records that failed checksum validation on read.
    pub checksum_failures: u64,
    /// Number of times this host ran crash recovery.
    pub recovery_runs: u64,
    /// ABANDONED records found during last recovery run.
    pub abandoned_found: u64,
    /// Padding to fill to exactly 64 bytes (one cacheline).
    pub _padding: [u8; 8],
}

// ─── PoolGeometry (runtime-configured pool dimensions) ─────────────
//
// Created once at pool init — either from config (new pool) or by reading
// the pool header (existing pool). Passed by value/reference to all
// offset calculations. This is the "ruler" for the pool.

/// Runtime-configured pool dimensions. Immutable after construction.
///
/// All offset calculations go through this struct — no hardcoded sizes
/// leak into the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolGeometry {
    /// Number of record slots in the pool.
    pub capacity: u32,
    /// Payload bytes per record (does NOT include the 64-byte metadata).
    pub payload_size: u32,
    /// Maximum number of hosts that can share this pool.
    pub max_hosts: u16,
    /// Total size of one record: RECORD_META_SIZE + payload_size.
    /// Stored to avoid repeated casts on every offset calc.
    pub record_size: u32,
    /// Total pool size in bytes (header + records + metrics array).
    pub total_size: u64,
}

impl PoolGeometry {
    /// Construct a new `PoolGeometry` from the three user-configured values.
    /// Derives `record_size` and `total_size` automatically.
    ///
    /// # Panics
    /// Panics if `payload_size` is 0 or `capacity` is 0 — these are programming
    /// errors caught at startup, not runtime conditions.
    pub fn new(capacity: u32, payload_size: u32, max_hosts: u16) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(payload_size > 0, "payload_size must be > 0");
        assert!(max_hosts > 0, "max_hosts must be > 0");

        let record_size = RECORD_META_SIZE as u32 + payload_size;
        let records_region = record_size as u64 * capacity as u64;
        let metrics_region = CACHELINE as u64 * max_hosts as u64;
        let total_size = HEADER_SIZE as u64 + records_region + metrics_region;

        Self {
            capacity,
            payload_size,
            max_hosts,
            record_size,
            total_size,
        }
    }

    /// Absolute byte offset of the start of record `slot` from pool byte 0.
    /// Returns: HEADER_SIZE + slot * record_size.
    #[inline]
    pub fn record_offset(&self, slot: u32) -> usize {
        HEADER_SIZE + (slot as usize) * (self.record_size as usize)
    }

    /// Absolute byte offset of the payload within record `slot`.
    /// Returns: record_offset(slot) + RECORD_META_SIZE.
    #[inline]
    pub fn payload_offset(&self, slot: u32) -> usize {
        self.record_offset(slot) + RECORD_META_SIZE
    }

    /// Absolute byte offset of `MetricsBlock[host_id]` from pool byte 0.
    /// Metrics array starts after all record slots.
    #[inline]
    pub fn metrics_offset(&self, host_id: u16) -> usize {
        let records_end = HEADER_SIZE + (self.capacity as usize) * (self.record_size as usize);
        records_end + (host_id as usize) * CACHELINE
    }

    /// Maximum payload bytes that fit in one record.
    /// Same as `payload_size` (provided for API clarity when reading code).
    #[inline]
    pub fn payload_max(&self) -> u32 {
        self.payload_size
    }
}

// ─── Compile-time assertion tests ──────────────────────────────────
//
// These tests verify that our hand-written offset constants match the
// actual struct layout produced by the compiler. If we ever change a
// field size or reorder fields, these tests catch the mismatch.

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    // ── RecordMeta layout assertions ──

    #[test]
    fn record_meta_size_is_64() {
        assert_eq!(size_of::<RecordMeta>(), RECORD_META_SIZE);
    }

    #[test]
    fn record_meta_offsets_match_constants() {
        assert_eq!(offset_of!(RecordMeta, state), REC_STATE);
        assert_eq!(offset_of!(RecordMeta, term), REC_TERM);
        assert_eq!(offset_of!(RecordMeta, index), REC_INDEX);
        assert_eq!(offset_of!(RecordMeta, writer_id), REC_WRITER_ID);
        assert_eq!(offset_of!(RecordMeta, data_len), REC_DATA_LEN);
        assert_eq!(offset_of!(RecordMeta, checksum), REC_CHECKSUM);
    }

    // ── MetricsBlock layout assertions ──

    #[test]
    fn metrics_block_size_is_64() {
        assert_eq!(size_of::<MetricsBlock>(), CACHELINE);
    }

    // ── PoolGeometry basic calculations ──

    #[test]
    fn geometry_record_offset() {
        let g = PoolGeometry::new(1024, 192, 4);
        // record_size = 64 + 192 = 256
        assert_eq!(g.record_size, 256);
        // slot 0 starts right after header
        assert_eq!(g.record_offset(0), HEADER_SIZE);
        // slot 1 starts at 64 + 256 = 320
        assert_eq!(g.record_offset(1), HEADER_SIZE + 256);
    }

    #[test]
    fn geometry_payload_offset() {
        let g = PoolGeometry::new(1024, 192, 4);
        // payload of slot 0 = HEADER_SIZE + RECORD_META_SIZE = 128
        assert_eq!(g.payload_offset(0), HEADER_SIZE + RECORD_META_SIZE);
    }

    #[test]
    fn geometry_metrics_offset() {
        let g = PoolGeometry::new(1024, 192, 4);
        // records end at: 64 + 1024 * 256 = 262208
        let records_end = HEADER_SIZE + 1024 * 256;
        assert_eq!(g.metrics_offset(0), records_end);
        assert_eq!(g.metrics_offset(1), records_end + CACHELINE);
        assert_eq!(g.metrics_offset(3), records_end + 3 * CACHELINE);
    }

    #[test]
    fn geometry_total_size() {
        let g = PoolGeometry::new(1024, 192, 4);
        // header(64) + records(1024*256) + metrics(4*64)
        let expected = 64u64 + (1024 * 256) + (4 * 64);
        assert_eq!(g.total_size, expected);
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn geometry_rejects_zero_capacity() {
        PoolGeometry::new(0, 192, 4);
    }

    #[test]
    #[should_panic(expected = "payload_size must be > 0")]
    fn geometry_rejects_zero_payload() {
        PoolGeometry::new(1024, 0, 4);
    }

    #[test]
    #[should_panic(expected = "max_hosts must be > 0")]
    fn geometry_rejects_zero_hosts() {
        PoolGeometry::new(1024, 192, 0);
    }
}
