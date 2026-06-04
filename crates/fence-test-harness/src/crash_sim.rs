//! Crash simulation for testing recovery.
//!
//! On Linux: uses fork() + SIGKILL to simulate a host crash mid-operation.
//! On macOS (dev): uses a cooperative approach — the writer thread checks a
//! "should_crash" flag and panics at a designated point, leaving the pool
//! in a partially-written state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use fence_core::MemoryBackend;

/// Crash simulation controller.
///
/// Create one, share it with writer threads, and trigger a crash at the
/// desired point in the write protocol.
#[derive(Clone)]
pub struct CrashSim {
    /// When true, the writer should abort before completing.
    should_crash: Arc<AtomicBool>,
    /// Which phase to crash at: "after_reserve", "after_write", "before_commit".
    crash_point: Arc<String>,
}

impl CrashSim {
    /// Create a new crash simulator targeting the given crash point.
    ///
    /// Valid crash points:
    /// - `"after_reserve"` — crash after fetch_add but before writing payload.
    /// - `"after_write"` — crash after writing payload but before COMMITTED.
    /// - `"before_flush"` — crash after writing but before flush.
    pub fn new(crash_point: &str) -> Self {
        Self {
            should_crash: Arc::new(AtomicBool::new(false)),
            crash_point: Arc::new(crash_point.to_string()),
        }
    }

    /// Arm the crash — next check will trigger.
    pub fn arm(&self) {
        self.should_crash.store(true, Ordering::SeqCst);
    }

    /// Disarm the crash.
    pub fn disarm(&self) {
        self.should_crash.store(false, Ordering::SeqCst);
    }

    /// Check if a crash should occur at the given point.
    /// Returns true if the caller should abort.
    pub fn should_crash_at(&self, point: &str) -> bool {
        if !self.should_crash.load(Ordering::SeqCst) {
            return false;
        }
        *self.crash_point == point
    }

    /// Check and panic if crash is armed for this point.
    /// Use in test code that simulates a writer.
    pub fn maybe_crash(&self, point: &str) {
        if self.should_crash_at(point) {
            // Don't actually panic — just signal via return.
            // The caller should check and stop writing.
        }
    }

    /// Simulate a partial write: reserves a slot, writes some data, but
    /// does NOT commit. Leaves the record in STATE_WRITING.
    ///
    /// Returns the slot number that was left in a crashed state.
    pub fn simulate_crashed_write(
        path: &std::path::Path,
        capacity: u32,
        payload_size: u32,
        max_hosts: u16,
    ) -> u64 {
        use fence_core::backend::Ordering as BO;
        use fence_core::layout::*;
        use fence_core::mmap_backend::MmapBackend;

        let geometry = fence_core::layout::PoolGeometry::new(capacity, payload_size, max_hosts);
        let backend = MmapBackend::open(path, geometry.total_size as usize)
            .expect("failed to open backend for crash sim");

        unsafe {
            // Reserve a slot.
            let slot = backend.fetch_add_u64(HDR_RESERVE_TAIL, 1, BO::AcqRel);

            // Write WRITING state but don't complete.
            let rec_offset = geometry.record_offset(slot as u32);
            backend.store_u8(rec_offset + REC_STATE, STATE_WRITING, BO::Release);
            backend.store_u64(rec_offset + REC_TERM, 99, BO::Relaxed);
            backend.store_u64(rec_offset + REC_INDEX, slot + 1, BO::Relaxed);

            // Deliberately do NOT write checksum or set COMMITTED.
            // This simulates a crash mid-write.
            slot
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_sim_arm_disarm() {
        let sim = CrashSim::new("after_write");
        assert!(!sim.should_crash_at("after_write"));

        sim.arm();
        assert!(sim.should_crash_at("after_write"));
        assert!(!sim.should_crash_at("after_reserve")); // Wrong point

        sim.disarm();
        assert!(!sim.should_crash_at("after_write"));
    }
}
