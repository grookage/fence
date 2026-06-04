//! The Fence runtime engine — main API for append/read/trim/recover.
//!
//! `FenceRuntime` owns a `Box<dyn MemoryBackend>` and provides the
//! lock-free log protocol:
//! - **Append**: fetch_add to reserve a slot, write payload, CAS to commit.
//! - **Read**: load committed_tail, bounds-check, read + verify checksum.
//! - **Trim**: mark old records ABANDONED.
//! - **Recover**: fix partially-written records after a crash.

use fence_core::backend::{MemoryBackend, Ordering};
use fence_core::checksum;
use fence_core::error::FenceError;
use fence_core::layout::{
    PoolGeometry, CACHELINE, HEADER_SIZE, MAGIC, RECORD_META_SIZE,
    HDR_CAPACITY, HDR_COMMITTED_TAIL, HDR_EPOCH, HDR_MAGIC, HDR_MAX_HOSTS,
    HDR_RECORD_SIZE, HDR_RESERVE_TAIL,
    REC_CHECKSUM, REC_DATA_LEN, REC_INDEX, REC_STATE, REC_TERM, REC_WRITER_ID,
    STATE_ABANDONED, STATE_COMMITTED, STATE_FREE, STATE_WRITING,
};

use crate::compaction;
use crate::metrics::{self, PoolStats};
use crate::recovery::{self, RecoveryReport};

/// Configuration for opening/creating a Fence pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Path to the backing file (mmap) or device.
    pub path: std::path::PathBuf,
    /// Number of record slots.
    pub capacity: u32,
    /// Payload size per record (bytes).
    pub payload_size: u32,
    /// Maximum number of hosts sharing this pool.
    pub max_hosts: u16,
    /// This host's ID (0-based, must be < max_hosts).
    pub host_id: u16,
    /// If true, initialize a fresh pool (write header). If false, open existing.
    pub create: bool,
}

/// A single record read from the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The record's global sequence number.
    pub index: u64,
    /// The Raft/consensus term this record was written in.
    pub term: u64,
    /// The host that wrote this record.
    pub writer_id: u64,
    /// The payload data.
    pub data: Vec<u8>,
}

/// The Fence runtime — owns the backend and provides the log API.
///
/// Thread-safe: multiple threads can append/read concurrently.
/// The lock-free protocol ensures correctness without mutexes.
pub struct FenceRuntime {
    /// The memory backend (mmap, DAX, RDMA, etc.).
    backend: Box<dyn MemoryBackend>,
    /// Pool dimensions (computed from header or config).
    geometry: PoolGeometry,
    /// This host's ID.
    host_id: u16,
}

// FenceRuntime is Send + Sync because:
// - backend is Box<dyn MemoryBackend> which requires Send + Sync.
// - geometry and host_id are Copy types.
// The compiler auto-derives these, but we document the reasoning.

impl FenceRuntime {
    /// Open an existing pool or create a new one based on `config`.
    ///
    /// If `config.create` is true:
    /// - Creates the file, sizes it, writes the header.
    ///
    /// If `config.create` is false:
    /// - Opens the file, validates the magic number, reads geometry from header.
    ///
    /// # Errors
    ///
    /// Returns `FenceError::MmapFailed` if the file cannot be opened/mapped.
    /// Returns `FenceError::InvalidMagic` if opening an existing pool with wrong magic.
    pub fn open(config: PoolConfig) -> Result<Self, FenceError> {
        let geometry = PoolGeometry::new(config.capacity, config.payload_size, config.max_hosts);
        let backend = fence_core::open_mmap_backend(&config.path, geometry.total_size as usize)?;

        if config.create {
            // Initialize fresh pool header.
            unsafe {
                backend.store_u64(HDR_MAGIC, MAGIC, Ordering::Relaxed);
                backend.store_u64(HDR_CAPACITY, config.capacity as u64, Ordering::Relaxed);
                backend.store_u64(
                    HDR_RECORD_SIZE,
                    geometry.record_size as u64,
                    Ordering::Relaxed,
                );
                backend.store_u64(HDR_MAX_HOSTS, config.max_hosts as u64, Ordering::Relaxed);
                backend.store_u64(HDR_COMMITTED_TAIL, 0, Ordering::Relaxed);
                backend.store_u64(HDR_RESERVE_TAIL, 0, Ordering::Relaxed);
                backend.store_u64(HDR_EPOCH, 1, Ordering::Relaxed);
                // Flush the entire header to persistence.
                backend.flush(0, HEADER_SIZE);
            }
        } else {
            // Validate existing pool.
            let magic = unsafe { backend.load_u64(HDR_MAGIC, Ordering::Relaxed) };
            if magic != MAGIC {
                return Err(FenceError::InvalidMagic {
                    expected: MAGIC,
                    actual: magic,
                });
            }
        }

        Ok(Self {
            backend,
            geometry,
            host_id: config.host_id,
        })
    }

    /// Append a record to the log.
    ///
    /// Protocol:
    /// 1. `fetch_add(reserve_tail, 1)` — atomically claim a slot.
    /// 2. Check capacity — if slot >= capacity, return PoolFull.
    /// 3. Mark state = WRITING.
    /// 4. Write term, index, writer_id, data_len, payload.
    /// 5. Compute and write checksum.
    /// 6. Flush the entire record.
    /// 7. Mark state = COMMITTED, flush state.
    /// 8. Advance committed_tail (CAS loop).
    ///
    /// Returns the record's index (1-based: slot + 1).
    ///
    /// # Errors
    ///
    /// - `FenceError::PoolFull` if no slots remain.
    /// - `FenceError::PayloadTooLarge` if `data.len() > payload_size`.
    pub fn append(&self, term: u64, data: &[u8]) -> Result<u64, FenceError> {
        if data.len() > self.geometry.payload_size as usize {
            return Err(FenceError::PayloadTooLarge {
                max: self.geometry.payload_size,
                got: data.len(),
            });
        }

        // Step 1: Reserve a slot.
        let slot = unsafe {
            self.backend
                .fetch_add_u64(HDR_RESERVE_TAIL, 1, Ordering::AcqRel)
        };

        // Step 2: Bounds check.
        if slot >= self.geometry.capacity as u64 {
            // Undo the reservation (best effort — doesn't matter if it races).
            unsafe {
                self.backend
                    .fetch_add_u64(HDR_RESERVE_TAIL, u64::MAX, Ordering::Relaxed);
                // u64::MAX wraps to -1, effectively subtracting 1.
            }
            unsafe {
                metrics::increment_appends_failed(&*self.backend, &self.geometry, self.host_id);
            }
            return Err(FenceError::PoolFull {
                capacity: self.geometry.capacity,
                requested_slot: slot,
            });
        }

        let rec_offset = self.geometry.record_offset(slot as u32);
        let index = slot + 1; // 1-based index

        unsafe {
            // Step 3: Mark WRITING.
            self.backend
                .store_u8(rec_offset + REC_STATE, STATE_WRITING, Ordering::Relaxed);

            // Step 4: Write fields.
            self.backend
                .store_u64(rec_offset + REC_TERM, term, Ordering::Relaxed);
            self.backend
                .store_u64(rec_offset + REC_INDEX, index, Ordering::Relaxed);
            self.backend
                .store_u64(rec_offset + REC_WRITER_ID, self.host_id as u64, Ordering::Relaxed);

            let data_len_bytes = (data.len() as u32).to_le_bytes();
            self.backend.write(rec_offset + REC_DATA_LEN, &data_len_bytes);

            // Write payload.
            if !data.is_empty() {
                self.backend.write(rec_offset + RECORD_META_SIZE, data);
            }

            // Step 5: Compute and write checksum.
            let crc = checksum::compute(term, index, data);
            self.backend
                .store_u64(rec_offset + REC_CHECKSUM, crc, Ordering::Relaxed);

            // Step 6: Flush the entire record (metadata + payload).
            self.backend
                .flush(rec_offset, self.geometry.record_size as usize);

            // Step 7: Mark COMMITTED + flush state.
            self.backend
                .store_u8(rec_offset + REC_STATE, STATE_COMMITTED, Ordering::Release);
            self.backend.flush(rec_offset, CACHELINE);

            // Step 8: Advance committed_tail.
            self.advance_committed_tail(slot);

            // Metrics.
            metrics::increment_appends(&*self.backend, &self.geometry, self.host_id);
            metrics::increment_bytes_written(
                &*self.backend,
                &self.geometry,
                self.host_id,
                data.len() as u64,
            );
        }

        Ok(index)
    }

    /// Read a record by its 1-based index.
    ///
    /// Returns `Ok(Some(record))` if the record is COMMITTED and valid.
    /// Returns `Ok(None)` if the record is ABANDONED or FREE.
    /// Returns `Err(IndexOutOfBounds)` if index > committed_tail.
    /// Returns `Err(ChecksumMismatch)` if the checksum is invalid.
    pub fn read(&self, index: u64) -> Result<Option<Record>, FenceError> {
        if index == 0 {
            return Err(FenceError::IndexOutOfBounds {
                index,
                committed_tail: self.committed_tail(),
            });
        }

        let committed_tail = self.committed_tail();
        if index > committed_tail {
            return Err(FenceError::IndexOutOfBounds {
                index,
                committed_tail,
            });
        }

        let slot = (index - 1) as u32;
        let rec_offset = self.geometry.record_offset(slot);

        unsafe {
            let state = self.backend.load_u8(rec_offset + REC_STATE, Ordering::Acquire);

            match state {
                STATE_COMMITTED => {}
                STATE_ABANDONED | STATE_FREE => return Ok(None),
                _ => return Ok(None), // Unknown state, treat as missing.
            }

            // Read fields.
            let term = self.backend.load_u64(rec_offset + REC_TERM, Ordering::Relaxed);
            let stored_index = self.backend.load_u64(rec_offset + REC_INDEX, Ordering::Relaxed);
            let writer_id = self.backend.load_u64(rec_offset + REC_WRITER_ID, Ordering::Relaxed);

            let data_len = {
                let mut buf = [0u8; 4];
                self.backend.read(rec_offset + REC_DATA_LEN, &mut buf);
                u32::from_le_bytes(buf)
            };
            let stored_crc = self.backend.load_u64(rec_offset + REC_CHECKSUM, Ordering::Relaxed);

            // Read payload.
            let payload_len =
                std::cmp::min(data_len as usize, self.geometry.payload_size as usize);
            let mut data = vec![0u8; payload_len];
            if payload_len > 0 {
                self.backend.read(rec_offset + RECORD_META_SIZE, &mut data);
            }

            // Verify checksum.
            if !checksum::verify(term, stored_index, &data, stored_crc) {
                metrics::increment_checksum_failures(&*self.backend, &self.geometry, self.host_id);
                return Err(FenceError::ChecksumMismatch { index });
            }

            metrics::increment_reads(&*self.backend, &self.geometry, self.host_id);

            Ok(Some(Record {
                index: stored_index,
                term,
                writer_id,
                data,
            }))
        }
    }

    /// Read a contiguous range of records [start_index, end_index).
    ///
    /// Skips ABANDONED/FREE records (they appear as gaps in the result).
    /// Stops at ChecksumMismatch (returns error).
    pub fn read_range(&self, start_index: u64, end_index: u64) -> Result<Vec<Record>, FenceError> {
        let mut records = Vec::with_capacity((end_index - start_index) as usize);

        for idx in start_index..end_index {
            match self.read(idx)? {
                Some(record) => records.push(record),
                None => {} // Skip abandoned/free slots.
            }
        }

        Ok(records)
    }

    /// Get the current committed_tail (number of contiguous committed records from the start).
    pub fn committed_tail(&self) -> u64 {
        unsafe { self.backend.load_u64(HDR_COMMITTED_TAIL, Ordering::Acquire) }
    }

    /// Get the current reserve_tail (total slots ever claimed).
    pub fn reserve_tail(&self) -> u64 {
        unsafe { self.backend.load_u64(HDR_RESERVE_TAIL, Ordering::Acquire) }
    }

    /// Trim records with index < `up_to`.
    pub fn trim(&self, up_to: u64) -> Result<compaction::TrimResult, FenceError> {
        unsafe { compaction::trim(&*self.backend, &self.geometry, up_to) }
    }

    /// Run crash recovery.
    pub fn recover(&self) -> RecoveryReport {
        let report = unsafe { recovery::recover(&*self.backend, &self.geometry) };
        unsafe {
            metrics::increment_recovery_runs(&*self.backend, &self.geometry, self.host_id);
            if report.abandoned_count > 0 {
                metrics::increment_abandoned_found(
                    &*self.backend,
                    &self.geometry,
                    self.host_id,
                    report.abandoned_count,
                );
            }
        }
        report
    }

    /// Read aggregate pool statistics.
    pub fn stats(&self) -> PoolStats {
        unsafe { metrics::read_stats(&*self.backend, &self.geometry) }
    }

    /// Get the pool geometry.
    pub fn geometry(&self) -> &PoolGeometry {
        &self.geometry
    }

    /// Get this host's ID.
    pub fn host_id(&self) -> u16 {
        self.host_id
    }

    // ── Private helpers ──

    /// CAS loop to advance committed_tail after a successful commit.
    ///
    /// Always attempts to scan forward from the current committed_tail
    /// through contiguous COMMITTED slots. Multiple threads may attempt
    /// this concurrently — the CAS ensures only one succeeds per advancement.
    unsafe fn advance_committed_tail(&self, _our_slot: u64) {
        recovery::advance_committed_tail(&*self.backend, &self.geometry);
    }
}

impl Drop for FenceRuntime {
    fn drop(&mut self) {
        // The backend's Drop impl handles munmap/close.
        // Nothing else to clean up at the runtime level.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_runtime(capacity: u32, payload_size: u32) -> (FenceRuntime, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let config = PoolConfig {
            path: tmp.path().to_path_buf(),
            capacity,
            payload_size,
            max_hosts: 4,
            host_id: 0,
            create: true,
        };
        let runtime = FenceRuntime::open(config).unwrap();
        (runtime, tmp)
    }

    #[test]
    fn create_pool_sets_magic() {
        let (runtime, _tmp) = create_runtime(16, 64);
        let magic = unsafe { runtime.backend.load_u64(HDR_MAGIC, Ordering::Relaxed) };
        assert_eq!(magic, MAGIC);
    }

    #[test]
    fn append_single_record_roundtrip() {
        let (runtime, _tmp) = create_runtime(16, 128);
        let index = runtime.append(1, b"hello, fence!").unwrap();
        assert_eq!(index, 1);

        let record = runtime.read(1).unwrap().unwrap();
        assert_eq!(record.index, 1);
        assert_eq!(record.term, 1);
        assert_eq!(record.writer_id, 0);
        assert_eq!(record.data, b"hello, fence!");
    }

    #[test]
    fn append_multiple_records() {
        let (runtime, _tmp) = create_runtime(16, 64);

        let idx1 = runtime.append(1, b"first").unwrap();
        let idx2 = runtime.append(1, b"second").unwrap();
        let idx3 = runtime.append(2, b"third").unwrap();

        assert_eq!(idx1, 1);
        assert_eq!(idx2, 2);
        assert_eq!(idx3, 3);

        let r1 = runtime.read(1).unwrap().unwrap();
        let r2 = runtime.read(2).unwrap().unwrap();
        let r3 = runtime.read(3).unwrap().unwrap();

        assert_eq!(r1.data, b"first");
        assert_eq!(r2.data, b"second");
        assert_eq!(r3.data, b"third");
        assert_eq!(r3.term, 2);
    }

    #[test]
    fn append_pool_full() {
        let (runtime, _tmp) = create_runtime(2, 64);

        runtime.append(1, b"one").unwrap();
        runtime.append(1, b"two").unwrap();
        let result = runtime.append(1, b"three");

        assert!(matches!(result, Err(FenceError::PoolFull { capacity: 2, .. })));
    }

    #[test]
    fn append_payload_too_large() {
        let (runtime, _tmp) = create_runtime(16, 32);
        let big_payload = vec![0u8; 64];
        let result = runtime.append(1, &big_payload);

        assert!(matches!(
            result,
            Err(FenceError::PayloadTooLarge { max: 32, got: 64 })
        ));
    }

    #[test]
    fn read_out_of_bounds() {
        let (runtime, _tmp) = create_runtime(16, 64);
        runtime.append(1, b"data").unwrap();

        let result = runtime.read(5);
        assert!(matches!(result, Err(FenceError::IndexOutOfBounds { .. })));
    }

    #[test]
    fn read_index_zero_errors() {
        let (runtime, _tmp) = create_runtime(16, 64);
        let result = runtime.read(0);
        assert!(matches!(result, Err(FenceError::IndexOutOfBounds { .. })));
    }

    #[test]
    fn committed_tail_advances() {
        let (runtime, _tmp) = create_runtime(16, 64);
        assert_eq!(runtime.committed_tail(), 0);

        runtime.append(1, b"a").unwrap();
        assert_eq!(runtime.committed_tail(), 1);

        runtime.append(1, b"b").unwrap();
        assert_eq!(runtime.committed_tail(), 2);
    }

    #[test]
    fn read_range_works() {
        let (runtime, _tmp) = create_runtime(16, 64);
        runtime.append(1, b"one").unwrap();
        runtime.append(1, b"two").unwrap();
        runtime.append(1, b"three").unwrap();

        let records = runtime.read_range(1, 4).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].data, b"one");
        assert_eq!(records[1].data, b"two");
        assert_eq!(records[2].data, b"three");
    }

    #[test]
    fn trim_marks_records_abandoned() {
        let (runtime, _tmp) = create_runtime(16, 64);
        runtime.append(1, b"a").unwrap();
        runtime.append(1, b"b").unwrap();
        runtime.append(1, b"c").unwrap();

        let result = runtime.trim(2).unwrap();
        assert_eq!(result.trimmed_count, 2);

        // Records 1 and 2 should now be None (abandoned).
        assert_eq!(runtime.read(1).unwrap(), None);
        assert_eq!(runtime.read(2).unwrap(), None);
        // Record 3 should still be readable.
        assert!(runtime.read(3).unwrap().is_some());
    }

    #[test]
    fn trim_beyond_committed_fails() {
        let (runtime, _tmp) = create_runtime(16, 64);
        runtime.append(1, b"a").unwrap();

        let result = runtime.trim(5);
        assert!(matches!(
            result,
            Err(FenceError::TrimBeyondCommitted { .. })
        ));
    }

    #[test]
    fn recover_on_clean_pool() {
        let (runtime, _tmp) = create_runtime(16, 64);
        runtime.append(1, b"a").unwrap();
        runtime.append(1, b"b").unwrap();

        let report = runtime.recover();
        assert_eq!(report.abandoned_count, 0);
        assert_eq!(report.committed_tail, 2);
    }

    #[test]
    fn stats_after_operations() {
        let (runtime, _tmp) = create_runtime(16, 64);
        runtime.append(1, b"hello").unwrap();
        runtime.append(1, b"world").unwrap();
        runtime.read(1).unwrap();

        let stats = runtime.stats();
        assert_eq!(stats.appends_total, 2);
        assert_eq!(stats.bytes_written, 10); // "hello" + "world"
        assert_eq!(stats.reads_total, 1);
    }

    #[test]
    fn open_existing_pool() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // Create pool and write data.
        {
            let config = PoolConfig {
                path: path.clone(),
                capacity: 16,
                payload_size: 64,
                max_hosts: 4,
                host_id: 0,
                create: true,
            };
            let runtime = FenceRuntime::open(config).unwrap();
            runtime.append(1, b"persistent").unwrap();
        }

        // Reopen and verify data persists.
        {
            let config = PoolConfig {
                path: path.clone(),
                capacity: 16,
                payload_size: 64,
                max_hosts: 4,
                host_id: 1,
                create: false,
            };
            let runtime = FenceRuntime::open(config).unwrap();
            let record = runtime.read(1).unwrap().unwrap();
            assert_eq!(record.data, b"persistent");
            assert_eq!(runtime.host_id(), 1);
        }
    }

    #[test]
    fn open_invalid_magic_errors() {
        let tmp = NamedTempFile::new().unwrap();
        // Write garbage to the file.
        std::fs::write(tmp.path(), vec![0u8; 4096]).unwrap();

        let config = PoolConfig {
            path: tmp.path().to_path_buf(),
            capacity: 16,
            payload_size: 64,
            max_hosts: 4,
            host_id: 0,
            create: false,
        };
        let result = FenceRuntime::open(config);
        assert!(matches!(result, Err(FenceError::InvalidMagic { .. })));
    }

    #[test]
    fn empty_payload_roundtrip() {
        let (runtime, _tmp) = create_runtime(16, 64);
        let idx = runtime.append(1, b"").unwrap();
        let record = runtime.read(idx).unwrap().unwrap();
        assert_eq!(record.data, b"");
    }

    #[test]
    fn geometry_accessible() {
        let (runtime, _tmp) = create_runtime(32, 128);
        assert_eq!(runtime.geometry().capacity, 32);
        assert_eq!(runtime.geometry().payload_size, 128);
        assert_eq!(runtime.geometry().max_hosts, 4);
    }
}
