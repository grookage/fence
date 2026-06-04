//! Assertion helpers for verifying pool state in tests.

use fence_core::backend::Ordering;
use fence_core::layout::*;
use fence_core::MemoryBackend;

/// Assert that a record at `slot` is in the expected state.
pub unsafe fn assert_record_state(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    slot: u32,
    expected_state: u8,
) {
    let rec_offset = geometry.record_offset(slot);
    let actual = backend.load_u8(rec_offset + REC_STATE, Ordering::Acquire);
    assert_eq!(
        actual, expected_state,
        "slot {slot}: expected state {expected_state}, got {actual}"
    );
}

/// Assert that committed_tail equals the expected value.
pub unsafe fn assert_committed_tail(backend: &dyn MemoryBackend, expected: u64) {
    let actual = backend.load_u64(HDR_COMMITTED_TAIL, Ordering::Acquire);
    assert_eq!(
        actual, expected,
        "committed_tail: expected {expected}, got {actual}"
    );
}

/// Assert that reserve_tail equals the expected value.
pub unsafe fn assert_reserve_tail(backend: &dyn MemoryBackend, expected: u64) {
    let actual = backend.load_u64(HDR_RESERVE_TAIL, Ordering::Acquire);
    assert_eq!(
        actual, expected,
        "reserve_tail: expected {expected}, got {actual}"
    );
}

/// Assert that the pool header has a valid magic number.
pub unsafe fn assert_valid_magic(backend: &dyn MemoryBackend) {
    let magic = backend.load_u64(HDR_MAGIC, Ordering::Relaxed);
    assert_eq!(
        magic, MAGIC,
        "invalid magic: expected 0x{:016X}, got 0x{magic:016X}",
        MAGIC
    );
}

/// Assert that N contiguous records from slot 0 are COMMITTED.
pub unsafe fn assert_committed_range(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    count: u32,
) {
    for slot in 0..count {
        assert_record_state(backend, geometry, slot, STATE_COMMITTED);
    }
}
