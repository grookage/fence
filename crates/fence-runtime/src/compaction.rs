//! Log compaction (trim) for the Fence pool.
//!
//! Trimming marks records as ABANDONED up to a given index, freeing
//! them logically. In Phase 1 (flat, non-circular log), trimmed records
//! simply remain as ABANDONED skeletons. Circular reuse comes in Phase 5.

use fence_core::backend::{MemoryBackend, Ordering};
use fence_core::error::FenceError;
use fence_core::layout::{
    PoolGeometry, CACHELINE, HDR_COMMITTED_TAIL, REC_STATE, STATE_ABANDONED, STATE_COMMITTED,
};

/// Result of a trim operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimResult {
    /// Number of records actually trimmed (transitioned to ABANDONED).
    pub trimmed_count: u64,
}

/// Trim (mark as ABANDONED) all records with index < `up_to`.
///
/// Only records in STATE_COMMITTED are transitioned. Records that are
/// already FREE or ABANDONED are skipped. Records in STATE_WRITING are
/// left alone (they'll be handled by recovery).
///
/// # Errors
///
/// Returns `FenceError::TrimBeyondCommitted` if `up_to > committed_tail`.
///
/// # Safety
///
/// Caller must ensure the backend is a validly initialized pool.
pub unsafe fn trim(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    up_to: u64,
) -> Result<TrimResult, FenceError> {
    let committed_tail = backend.load_u64(HDR_COMMITTED_TAIL, Ordering::Acquire);

    if up_to > committed_tail {
        return Err(FenceError::TrimBeyondCommitted {
            requested: up_to,
            committed_tail,
        });
    }

    let mut trimmed_count: u64 = 0;

    for slot in 0..up_to as u32 {
        let rec_offset = geometry.record_offset(slot);
        let state = backend.load_u8(rec_offset + REC_STATE, Ordering::Acquire);

        if state == STATE_COMMITTED {
            backend.store_u8(rec_offset + REC_STATE, STATE_ABANDONED, Ordering::Release);
            backend.flush(rec_offset, CACHELINE);
            trimmed_count += 1;
        }
    }

    Ok(TrimResult { trimmed_count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fence_core::layout::{
        HDR_MAGIC, HDR_CAPACITY, HDR_RECORD_SIZE, HDR_MAX_HOSTS, HDR_RESERVE_TAIL, HDR_EPOCH,
        MAGIC, RECORD_META_SIZE, REC_TERM, REC_INDEX, REC_DATA_LEN, REC_CHECKSUM,
    };
    use fence_core::checksum;
    use fence_core::mmap_backend::MmapBackend;
    use tempfile::NamedTempFile;

    fn setup_pool(capacity: u32, payload_size: u32) -> (MmapBackend, PoolGeometry, NamedTempFile) {
        let geometry = PoolGeometry::new(capacity, payload_size, 2);
        let tmp = NamedTempFile::new().unwrap();
        let backend = MmapBackend::open(tmp.path(), geometry.total_size as usize).unwrap();

        unsafe {
            backend.store_u64(HDR_MAGIC, MAGIC, Ordering::Relaxed);
            backend.store_u64(HDR_CAPACITY, capacity as u64, Ordering::Relaxed);
            backend.store_u64(HDR_RECORD_SIZE, geometry.record_size as u64, Ordering::Relaxed);
            backend.store_u64(HDR_MAX_HOSTS, 2, Ordering::Relaxed);
            backend.store_u64(HDR_COMMITTED_TAIL, 0, Ordering::Relaxed);
            backend.store_u64(HDR_RESERVE_TAIL, 0, Ordering::Relaxed);
            backend.store_u64(HDR_EPOCH, 1, Ordering::Relaxed);
        }

        (backend, geometry, tmp)
    }

    unsafe fn write_committed_record(
        backend: &MmapBackend,
        geometry: &PoolGeometry,
        slot: u32,
        payload: &[u8],
    ) {
        let rec_offset = geometry.record_offset(slot);
        let term = 1u64;
        let index = slot as u64 + 1;

        backend.store_u64(rec_offset + REC_TERM, term, Ordering::Relaxed);
        backend.store_u64(rec_offset + REC_INDEX, index, Ordering::Relaxed);
        let data_len_bytes = (payload.len() as u32).to_le_bytes();
        backend.write(rec_offset + REC_DATA_LEN, &data_len_bytes);
        if !payload.is_empty() {
            backend.write(rec_offset + RECORD_META_SIZE, payload);
        }
        let crc = checksum::compute(term, index, payload);
        backend.store_u64(rec_offset + REC_CHECKSUM, crc, Ordering::Relaxed);
        backend.store_u8(rec_offset + REC_STATE, STATE_COMMITTED, Ordering::Release);
    }

    #[test]
    fn trim_zero_is_noop() {
        let (backend, geometry, _tmp) = setup_pool(8, 64);
        let result = unsafe { trim(&backend, &geometry, 0) }.unwrap();
        assert_eq!(result.trimmed_count, 0);
    }

    #[test]
    fn trim_committed_records() {
        let (backend, geometry, _tmp) = setup_pool(8, 64);

        unsafe {
            backend.store_u64(HDR_RESERVE_TAIL, 4, Ordering::Relaxed);
            backend.store_u64(HDR_COMMITTED_TAIL, 4, Ordering::Relaxed);
            for slot in 0..4u32 {
                write_committed_record(&backend, &geometry, slot, b"data");
            }
        }

        // Trim first 2 records.
        let result = unsafe { trim(&backend, &geometry, 2) }.unwrap();
        assert_eq!(result.trimmed_count, 2);

        // Verify trimmed slots are ABANDONED.
        for slot in 0..2u32 {
            let state = unsafe {
                backend.load_u8(geometry.record_offset(slot) + REC_STATE, Ordering::Relaxed)
            };
            assert_eq!(state, STATE_ABANDONED);
        }

        // Verify remaining slots are still COMMITTED.
        for slot in 2..4u32 {
            let state = unsafe {
                backend.load_u8(geometry.record_offset(slot) + REC_STATE, Ordering::Relaxed)
            };
            assert_eq!(state, STATE_COMMITTED);
        }
    }

    #[test]
    fn trim_beyond_committed_errors() {
        let (backend, geometry, _tmp) = setup_pool(8, 64);

        unsafe {
            backend.store_u64(HDR_COMMITTED_TAIL, 3, Ordering::Relaxed);
        }

        let result = unsafe { trim(&backend, &geometry, 5) };
        assert!(matches!(
            result,
            Err(FenceError::TrimBeyondCommitted {
                requested: 5,
                committed_tail: 3
            })
        ));
    }

    #[test]
    fn trim_skips_already_abandoned() {
        let (backend, geometry, _tmp) = setup_pool(8, 64);

        unsafe {
            backend.store_u64(HDR_RESERVE_TAIL, 3, Ordering::Relaxed);
            backend.store_u64(HDR_COMMITTED_TAIL, 3, Ordering::Relaxed);
            write_committed_record(&backend, &geometry, 0, b"a");
            // Slot 1 is already ABANDONED.
            backend.store_u8(
                geometry.record_offset(1) + REC_STATE,
                STATE_ABANDONED,
                Ordering::Relaxed,
            );
            write_committed_record(&backend, &geometry, 2, b"c");
        }

        let result = unsafe { trim(&backend, &geometry, 3) }.unwrap();
        // Only slots 0 and 2 were COMMITTED; slot 1 was already ABANDONED.
        assert_eq!(result.trimmed_count, 2);
    }
}
