//! Crash recovery for the Fence shared-memory pool.
//!
//! After a host crash, some records may be in STATE_WRITING (partially written).
//! Recovery scans all slots, marks incomplete records as ABANDONED, and
//! advances `committed_tail` to skip over any gaps.
//!
//! Recovery is **idempotent**: running it N times produces the same state
//! as running it once. This means any host can run recovery at any time.

use fence_core::backend::{MemoryBackend, Ordering};
use fence_core::layout::{
    PoolGeometry, RECORD_META_SIZE, CACHELINE,
    HDR_COMMITTED_TAIL, HDR_RESERVE_TAIL,
    REC_STATE, REC_TERM, REC_INDEX, REC_DATA_LEN, REC_CHECKSUM,
    STATE_FREE, STATE_WRITING, STATE_COMMITTED, STATE_ABANDONED,
};
use fence_core::checksum;

/// Report produced by a recovery run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Number of records found in STATE_WRITING (now ABANDONED).
    pub abandoned_count: u64,
    /// The committed_tail value after recovery completes.
    pub committed_tail: u64,
    /// Total slots scanned.
    pub slots_scanned: u64,
}

/// Run crash recovery on the pool.
///
/// Algorithm:
/// 1. Read `reserve_tail` to know how many slots have ever been claimed.
/// 2. Scan slots [0, reserve_tail). For each:
///    - If STATE_WRITING → mark ABANDONED, flush.
///    - If STATE_COMMITTED → verify checksum. If invalid → mark ABANDONED.
/// 3. Advance committed_tail: starting from the current committed_tail,
///    walk forward through contiguous COMMITTED slots.
///
/// # Safety
///
/// Caller must ensure:
/// - The backend points to a validly initialized Fence pool.
/// - `geometry` matches the pool's actual dimensions.
pub unsafe fn recover(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
) -> RecoveryReport {
    let reserve_tail = backend.load_u64(HDR_RESERVE_TAIL, Ordering::Acquire);
    let slots_to_scan = std::cmp::min(reserve_tail, geometry.capacity as u64);

    let mut abandoned_count: u64 = 0;

    // Phase 1: Scan and fix broken records.
    for slot in 0..slots_to_scan as u32 {
        let rec_offset = geometry.record_offset(slot);
        let state = backend.load_u8(rec_offset + REC_STATE, Ordering::Acquire);

        match state {
            STATE_WRITING => {
                // Writer crashed mid-write. Mark abandoned.
                backend.store_u8(rec_offset + REC_STATE, STATE_ABANDONED, Ordering::Release);
                backend.flush(rec_offset, CACHELINE);
                abandoned_count += 1;
            }
            STATE_COMMITTED => {
                // Verify checksum integrity.
                let term = backend.load_u64(rec_offset + REC_TERM, Ordering::Relaxed);
                let index = backend.load_u64(rec_offset + REC_INDEX, Ordering::Relaxed);
                let data_len = {
                    let mut buf = [0u8; 4];
                    backend.read(rec_offset + REC_DATA_LEN, &mut buf);
                    u32::from_le_bytes(buf)
                };
                let stored_checksum =
                    backend.load_u64(rec_offset + REC_CHECKSUM, Ordering::Relaxed);

                // Read payload for checksum verification.
                let payload_offset = rec_offset + RECORD_META_SIZE;
                let payload_len = std::cmp::min(data_len as usize, geometry.payload_size as usize);
                let mut payload_buf = vec![0u8; payload_len];
                if payload_len > 0 {
                    backend.read(payload_offset, &mut payload_buf);
                }

                if !checksum::verify(term, index, &payload_buf, stored_checksum) {
                    // Checksum mismatch — torn write or corruption.
                    backend.store_u8(rec_offset + REC_STATE, STATE_ABANDONED, Ordering::Release);
                    backend.flush(rec_offset, CACHELINE);
                    abandoned_count += 1;
                }
            }
            STATE_FREE | STATE_ABANDONED => {
                // Nothing to do.
            }
            _ => {
                // Unknown state byte (corruption). Mark abandoned.
                backend.store_u8(rec_offset + REC_STATE, STATE_ABANDONED, Ordering::Release);
                backend.flush(rec_offset, CACHELINE);
                abandoned_count += 1;
            }
        }
    }

    // Phase 2: Advance committed_tail through contiguous COMMITTED slots.
    let committed_tail = advance_committed_tail(backend, geometry);

    RecoveryReport {
        abandoned_count,
        committed_tail,
        slots_scanned: slots_to_scan,
    }
}

/// Advance `committed_tail` forward through contiguous COMMITTED slots.
///
/// Starting from the current committed_tail, walk forward. For each slot
/// that is COMMITTED, advance. Stop at the first non-COMMITTED slot.
///
/// Uses CAS to handle concurrent advancement (another host may also
/// be running recovery or appending).
///
/// # Safety
///
/// Caller must ensure the backend is a valid initialized pool.
pub unsafe fn advance_committed_tail(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
) -> u64 {
    loop {
        let current_tail = backend.load_u64(HDR_COMMITTED_TAIL, Ordering::Acquire);

        // Find how far we can advance.
        let mut new_tail = current_tail;
        while new_tail < geometry.capacity as u64 {
            let slot = new_tail as u32;
            let rec_offset = geometry.record_offset(slot);
            let state = backend.load_u8(rec_offset + REC_STATE, Ordering::Acquire);
            if state != STATE_COMMITTED {
                break;
            }
            new_tail += 1;
        }

        if new_tail == current_tail {
            // No advancement possible.
            return current_tail;
        }

        // Try to CAS committed_tail forward.
        match backend.compare_exchange_u64(
            HDR_COMMITTED_TAIL,
            current_tail,
            new_tail,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return new_tail,
            Err(_) => {
                // Another writer/recoverer advanced it. Retry.
                continue;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fence_core::layout::{HDR_MAGIC, HDR_CAPACITY, HDR_RECORD_SIZE, HDR_MAX_HOSTS, HDR_EPOCH};
    use fence_core::layout::MAGIC;
    use fence_core::mmap_backend::MmapBackend;
    use tempfile::NamedTempFile;

    fn setup_pool(capacity: u32, payload_size: u32, max_hosts: u16) -> (MmapBackend, PoolGeometry, NamedTempFile) {
        let geometry = PoolGeometry::new(capacity, payload_size, max_hosts);
        let tmp = NamedTempFile::new().unwrap();
        let backend = MmapBackend::open(tmp.path(), geometry.total_size as usize).unwrap();

        // Initialize header.
        unsafe {
            backend.store_u64(HDR_MAGIC, MAGIC, Ordering::Relaxed);
            backend.store_u64(HDR_CAPACITY, capacity as u64, Ordering::Relaxed);
            backend.store_u64(HDR_RECORD_SIZE, geometry.record_size as u64, Ordering::Relaxed);
            backend.store_u64(HDR_MAX_HOSTS, max_hosts as u64, Ordering::Relaxed);
            backend.store_u64(HDR_COMMITTED_TAIL, 0, Ordering::Relaxed);
            backend.store_u64(HDR_RESERVE_TAIL, 0, Ordering::Relaxed);
            backend.store_u64(HDR_EPOCH, 1, Ordering::Relaxed);
        }

        (backend, geometry, tmp)
    }

    /// Write a valid committed record at the given slot.
    unsafe fn write_committed_record(
        backend: &MmapBackend,
        geometry: &PoolGeometry,
        slot: u32,
        term: u64,
        payload: &[u8],
    ) {
        let rec_offset = geometry.record_offset(slot);
        let index = slot as u64 + 1;

        // Write fields.
        backend.store_u64(rec_offset + REC_TERM, term, Ordering::Relaxed);
        backend.store_u64(rec_offset + REC_INDEX, index, Ordering::Relaxed);

        // Write data_len as LE bytes.
        let data_len_bytes = (payload.len() as u32).to_le_bytes();
        backend.write(rec_offset + REC_DATA_LEN, &data_len_bytes);

        // Write payload.
        if !payload.is_empty() {
            backend.write(rec_offset + RECORD_META_SIZE, payload);
        }

        // Compute and write checksum.
        let crc = checksum::compute(term, index, payload);
        backend.store_u64(rec_offset + REC_CHECKSUM, crc, Ordering::Relaxed);

        // Set state to COMMITTED.
        backend.store_u8(rec_offset + REC_STATE, STATE_COMMITTED, Ordering::Release);
    }

    #[test]
    fn empty_pool_recovery_is_noop() {
        let (backend, geometry, _tmp) = setup_pool(16, 64, 2);
        let report = unsafe { recover(&backend, &geometry) };
        assert_eq!(report.abandoned_count, 0);
        assert_eq!(report.committed_tail, 0);
        assert_eq!(report.slots_scanned, 0);
    }

    #[test]
    fn writing_state_becomes_abandoned() {
        let (backend, geometry, _tmp) = setup_pool(16, 64, 2);

        unsafe {
            // Simulate: reserve_tail = 2, slot 0 is WRITING, slot 1 is FREE.
            backend.store_u64(HDR_RESERVE_TAIL, 2, Ordering::Relaxed);
            let rec0 = geometry.record_offset(0);
            backend.store_u8(rec0 + REC_STATE, STATE_WRITING, Ordering::Relaxed);
        }

        let report = unsafe { recover(&backend, &geometry) };
        assert_eq!(report.abandoned_count, 1);
        assert_eq!(report.slots_scanned, 2);

        // Verify slot 0 is now ABANDONED.
        let state = unsafe {
            backend.load_u8(geometry.record_offset(0) + REC_STATE, Ordering::Relaxed)
        };
        assert_eq!(state, STATE_ABANDONED);
    }

    #[test]
    fn committed_tail_advances_through_committed_slots() {
        let (backend, geometry, _tmp) = setup_pool(16, 64, 2);

        unsafe {
            backend.store_u64(HDR_RESERVE_TAIL, 3, Ordering::Relaxed);
            write_committed_record(&backend, &geometry, 0, 1, b"slot0");
            write_committed_record(&backend, &geometry, 1, 1, b"slot1");
            write_committed_record(&backend, &geometry, 2, 1, b"slot2");
        }

        let report = unsafe { recover(&backend, &geometry) };
        assert_eq!(report.abandoned_count, 0);
        assert_eq!(report.committed_tail, 3);
    }

    #[test]
    fn committed_tail_stops_at_abandoned() {
        let (backend, geometry, _tmp) = setup_pool(16, 64, 2);

        unsafe {
            backend.store_u64(HDR_RESERVE_TAIL, 4, Ordering::Relaxed);
            write_committed_record(&backend, &geometry, 0, 1, b"ok");
            // slot 1 is WRITING → will become ABANDONED
            backend.store_u8(
                geometry.record_offset(1) + REC_STATE,
                STATE_WRITING,
                Ordering::Relaxed,
            );
            write_committed_record(&backend, &geometry, 2, 1, b"ok2");
            write_committed_record(&backend, &geometry, 3, 1, b"ok3");
        }

        let report = unsafe { recover(&backend, &geometry) };
        // Slot 1 was WRITING → abandoned
        assert_eq!(report.abandoned_count, 1);
        // committed_tail advances only through slot 0 (stops at abandoned slot 1)
        assert_eq!(report.committed_tail, 1);
    }

    #[test]
    fn corrupted_checksum_becomes_abandoned() {
        let (backend, geometry, _tmp) = setup_pool(16, 64, 2);

        unsafe {
            backend.store_u64(HDR_RESERVE_TAIL, 1, Ordering::Relaxed);
            write_committed_record(&backend, &geometry, 0, 1, b"data");
            // Corrupt the checksum.
            let rec0 = geometry.record_offset(0);
            backend.store_u64(rec0 + REC_CHECKSUM, 0xBADBADBAD, Ordering::Relaxed);
        }

        let report = unsafe { recover(&backend, &geometry) };
        assert_eq!(report.abandoned_count, 1);
        assert_eq!(report.committed_tail, 0); // Can't advance past corrupted slot 0.
    }

    #[test]
    fn recovery_is_idempotent() {
        let (backend, geometry, _tmp) = setup_pool(16, 64, 2);

        unsafe {
            backend.store_u64(HDR_RESERVE_TAIL, 3, Ordering::Relaxed);
            write_committed_record(&backend, &geometry, 0, 1, b"a");
            backend.store_u8(
                geometry.record_offset(1) + REC_STATE,
                STATE_WRITING,
                Ordering::Relaxed,
            );
            write_committed_record(&backend, &geometry, 2, 1, b"c");
        }

        let report1 = unsafe { recover(&backend, &geometry) };
        let report2 = unsafe { recover(&backend, &geometry) };

        // Second run should find nothing new to abandon.
        assert_eq!(report1.committed_tail, report2.committed_tail);
        assert_eq!(report2.abandoned_count, 0);
    }

    #[test]
    fn unknown_state_becomes_abandoned() {
        let (backend, geometry, _tmp) = setup_pool(16, 64, 2);

        unsafe {
            backend.store_u64(HDR_RESERVE_TAIL, 1, Ordering::Relaxed);
            let rec0 = geometry.record_offset(0);
            backend.store_u8(rec0 + REC_STATE, 0xFF, Ordering::Relaxed); // garbage state
        }

        let report = unsafe { recover(&backend, &geometry) };
        assert_eq!(report.abandoned_count, 1);
    }
}
