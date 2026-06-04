//! Per-host metrics tracking for the Fence pool.
//!
//! Each host maintains a 64-byte `MetricsBlock` in shared memory.
//! These functions provide safe wrappers for incrementing counters
//! and reading aggregate statistics.
//!
//! All counter updates use `Relaxed` ordering — metrics are best-effort.
//! Losing a count to a race is acceptable; adding synchronization overhead
//! to the hot path is not.

use fence_core::backend::{MemoryBackend, Ordering};
use fence_core::layout::PoolGeometry;

/// Aggregate pool statistics (summed across all hosts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub appends_total: u64,
    pub appends_failed: u64,
    pub bytes_written: u64,
    pub reads_total: u64,
    pub checksum_failures: u64,
    pub recovery_runs: u64,
    pub abandoned_found: u64,
}

// ─── Offset helpers for MetricsBlock fields ────────────────────────
// MetricsBlock layout: 7 x u64 fields + 8 bytes padding = 64 bytes.
// Field offsets within one MetricsBlock:
const METRICS_APPENDS_TOTAL: usize = 0;
const METRICS_APPENDS_FAILED: usize = 8;
const METRICS_BYTES_WRITTEN: usize = 16;
const METRICS_READS_TOTAL: usize = 24;
const METRICS_CHECKSUM_FAILURES: usize = 32;
const METRICS_RECOVERY_RUNS: usize = 40;
const METRICS_ABANDONED_FOUND: usize = 48;

/// Increment the `appends_total` counter for `host_id`.
///
/// # Safety
/// Caller must ensure the backend is properly initialized and `host_id < max_hosts`.
#[inline]
pub unsafe fn increment_appends(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    host_id: u16,
) {
    let offset = geometry.metrics_offset(host_id) + METRICS_APPENDS_TOTAL;
    backend.fetch_add_u64(offset, 1, Ordering::Relaxed);
}

/// Increment the `appends_failed` counter for `host_id`.
///
/// # Safety
/// Caller must ensure the backend is properly initialized and `host_id < max_hosts`.
#[inline]
pub unsafe fn increment_appends_failed(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    host_id: u16,
) {
    let offset = geometry.metrics_offset(host_id) + METRICS_APPENDS_FAILED;
    backend.fetch_add_u64(offset, 1, Ordering::Relaxed);
}

/// Increment the `bytes_written` counter for `host_id` by `n`.
///
/// # Safety
/// Caller must ensure the backend is properly initialized and `host_id < max_hosts`.
#[inline]
pub unsafe fn increment_bytes_written(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    host_id: u16,
    n: u64,
) {
    let offset = geometry.metrics_offset(host_id) + METRICS_BYTES_WRITTEN;
    backend.fetch_add_u64(offset, n, Ordering::Relaxed);
}

/// Increment the `reads_total` counter for `host_id`.
///
/// # Safety
/// Caller must ensure the backend is properly initialized and `host_id < max_hosts`.
#[inline]
pub unsafe fn increment_reads(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    host_id: u16,
) {
    let offset = geometry.metrics_offset(host_id) + METRICS_READS_TOTAL;
    backend.fetch_add_u64(offset, 1, Ordering::Relaxed);
}

/// Increment the `checksum_failures` counter for `host_id`.
///
/// # Safety
/// Caller must ensure the backend is properly initialized and `host_id < max_hosts`.
#[inline]
pub unsafe fn increment_checksum_failures(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    host_id: u16,
) {
    let offset = geometry.metrics_offset(host_id) + METRICS_CHECKSUM_FAILURES;
    backend.fetch_add_u64(offset, 1, Ordering::Relaxed);
}

/// Increment the `recovery_runs` counter for `host_id`.
///
/// # Safety
/// Caller must ensure the backend is properly initialized and `host_id < max_hosts`.
#[inline]
pub unsafe fn increment_recovery_runs(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    host_id: u16,
) {
    let offset = geometry.metrics_offset(host_id) + METRICS_RECOVERY_RUNS;
    backend.fetch_add_u64(offset, 1, Ordering::Relaxed);
}

/// Increment the `abandoned_found` counter for `host_id` by `n`.
///
/// # Safety
/// Caller must ensure the backend is properly initialized and `host_id < max_hosts`.
#[inline]
pub unsafe fn increment_abandoned_found(
    backend: &dyn MemoryBackend,
    geometry: &PoolGeometry,
    host_id: u16,
    n: u64,
) {
    let offset = geometry.metrics_offset(host_id) + METRICS_ABANDONED_FOUND;
    backend.fetch_add_u64(offset, n, Ordering::Relaxed);
}

/// Read and aggregate metrics across all hosts.
///
/// Scans each host's `MetricsBlock` and sums the counters.
///
/// # Safety
/// Caller must ensure the backend is properly initialized.
pub unsafe fn read_stats(backend: &dyn MemoryBackend, geometry: &PoolGeometry) -> PoolStats {
    let mut stats = PoolStats::default();

    for host_id in 0..geometry.max_hosts {
        let base = geometry.metrics_offset(host_id);
        stats.appends_total += backend.load_u64(base + METRICS_APPENDS_TOTAL, Ordering::Relaxed);
        stats.appends_failed += backend.load_u64(base + METRICS_APPENDS_FAILED, Ordering::Relaxed);
        stats.bytes_written += backend.load_u64(base + METRICS_BYTES_WRITTEN, Ordering::Relaxed);
        stats.reads_total += backend.load_u64(base + METRICS_READS_TOTAL, Ordering::Relaxed);
        stats.checksum_failures +=
            backend.load_u64(base + METRICS_CHECKSUM_FAILURES, Ordering::Relaxed);
        stats.recovery_runs += backend.load_u64(base + METRICS_RECOVERY_RUNS, Ordering::Relaxed);
        stats.abandoned_found +=
            backend.load_u64(base + METRICS_ABANDONED_FOUND, Ordering::Relaxed);
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use fence_core::mmap_backend::MmapBackend;
    use tempfile::NamedTempFile;

    fn setup() -> (MmapBackend, PoolGeometry, NamedTempFile) {
        let geometry = PoolGeometry::new(16, 64, 4);
        let tmp = NamedTempFile::new().unwrap();
        let backend = MmapBackend::open(tmp.path(), geometry.total_size as usize).unwrap();
        (backend, geometry, tmp)
    }

    #[test]
    fn initial_stats_are_zero() {
        let (backend, geometry, _tmp) = setup();
        let stats = unsafe { read_stats(&backend, &geometry) };
        assert_eq!(stats, PoolStats::default());
    }

    #[test]
    fn increment_appends_counted() {
        let (backend, geometry, _tmp) = setup();
        unsafe {
            increment_appends(&backend, &geometry, 0);
            increment_appends(&backend, &geometry, 0);
            increment_appends(&backend, &geometry, 1);
        }
        let stats = unsafe { read_stats(&backend, &geometry) };
        assert_eq!(stats.appends_total, 3);
    }

    #[test]
    fn increment_bytes_written_accumulated() {
        let (backend, geometry, _tmp) = setup();
        unsafe {
            increment_bytes_written(&backend, &geometry, 0, 100);
            increment_bytes_written(&backend, &geometry, 2, 200);
        }
        let stats = unsafe { read_stats(&backend, &geometry) };
        assert_eq!(stats.bytes_written, 300);
    }

    #[test]
    fn all_counters_independent() {
        let (backend, geometry, _tmp) = setup();
        unsafe {
            increment_appends(&backend, &geometry, 0);
            increment_appends_failed(&backend, &geometry, 0);
            increment_bytes_written(&backend, &geometry, 0, 50);
            increment_reads(&backend, &geometry, 0);
            increment_checksum_failures(&backend, &geometry, 0);
            increment_recovery_runs(&backend, &geometry, 0);
            increment_abandoned_found(&backend, &geometry, 0, 3);
        }
        let stats = unsafe { read_stats(&backend, &geometry) };
        assert_eq!(stats.appends_total, 1);
        assert_eq!(stats.appends_failed, 1);
        assert_eq!(stats.bytes_written, 50);
        assert_eq!(stats.reads_total, 1);
        assert_eq!(stats.checksum_failures, 1);
        assert_eq!(stats.recovery_runs, 1);
        assert_eq!(stats.abandoned_found, 3);
    }

    #[test]
    fn metrics_per_host_isolation() {
        let (backend, geometry, _tmp) = setup();
        unsafe {
            increment_appends(&backend, &geometry, 0);
            increment_appends(&backend, &geometry, 0);
            increment_appends(&backend, &geometry, 3);
        }
        // Verify host 0's block directly
        let host0_base = geometry.metrics_offset(0);
        let host0_appends =
            unsafe { backend.load_u64(host0_base + METRICS_APPENDS_TOTAL, Ordering::Relaxed) };
        assert_eq!(host0_appends, 2);

        let host3_base = geometry.metrics_offset(3);
        let host3_appends =
            unsafe { backend.load_u64(host3_base + METRICS_APPENDS_TOTAL, Ordering::Relaxed) };
        assert_eq!(host3_appends, 1);
    }
}
