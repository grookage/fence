//! The `MemoryBackend` trait — the core abstraction that makes Fence
//! deployable on mmap, DAX, RDMA, or CXL without changing engine code.
//!
//! All shared-memory access goes through this trait. The engine never
//! touches raw pointers directly — it calls `backend.read(offset, buf)`,
//! `backend.store_u64(offset, value, ordering)`, etc.
//!
//! This is analogous to a Java interface:
//! ```java
//! public interface MemoryBackend {
//!     void read(long offset, byte[] dst);
//!     void write(long offset, byte[] src);
//!     long loadU64(long offset, AccessMode mode);
//!     // ...
//! }
//! ```

pub use std::sync::atomic::Ordering;

use crate::error::FenceError;

/// Trait abstracting all shared-memory access.
///
/// Implementations exist for:
/// - `MmapBackend`: file-backed mmap (dev + single-machine)
/// - Future: DAX backend, RDMA backend, CXL device backend
///
/// # Safety
///
/// All methods are `unsafe` because:
/// - The caller must ensure `offset` and `offset + len` are within bounds.
/// - The caller must ensure proper atomic ordering for concurrent access.
/// - The underlying memory may be shared with other processes/hosts.
///
/// Implementations must be `Send + Sync` (the runtime is shared across threads).
pub trait MemoryBackend: Send + Sync {
    /// Read `dst.len()` bytes from `offset` into `dst`.
    ///
    /// # Safety
    /// Caller must ensure `offset + dst.len() <= self.size()`.
    unsafe fn read(&self, offset: usize, dst: &mut [u8]);

    /// Write `src` bytes to `offset`.
    ///
    /// # Safety
    /// Caller must ensure `offset + src.len() <= self.size()`.
    unsafe fn write(&self, offset: usize, src: &[u8]);

    /// Atomically load a `u64` at `offset` with the given ordering.
    ///
    /// # Safety
    /// Caller must ensure `offset` is 8-byte aligned and `offset + 8 <= self.size()`.
    unsafe fn load_u64(&self, offset: usize, ordering: Ordering) -> u64;

    /// Atomically store a `u64` at `offset` with the given ordering.
    ///
    /// # Safety
    /// Caller must ensure `offset` is 8-byte aligned and `offset + 8 <= self.size()`.
    unsafe fn store_u64(&self, offset: usize, value: u64, ordering: Ordering);

    /// Atomically add `delta` to the `u64` at `offset`. Returns the previous value.
    ///
    /// # Safety
    /// Caller must ensure `offset` is 8-byte aligned and `offset + 8 <= self.size()`.
    unsafe fn fetch_add_u64(&self, offset: usize, delta: u64, ordering: Ordering) -> u64;

    /// Atomic compare-exchange on the `u64` at `offset`.
    ///
    /// If the current value equals `expected`, stores `desired` and returns `Ok(expected)`.
    /// Otherwise returns `Err(actual_value)`.
    ///
    /// # Safety
    /// Caller must ensure `offset` is 8-byte aligned and `offset + 8 <= self.size()`.
    unsafe fn compare_exchange_u64(
        &self,
        offset: usize,
        expected: u64,
        desired: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64>;

    /// Atomically load a `u8` at `offset` with the given ordering.
    ///
    /// # Safety
    /// Caller must ensure `offset < self.size()`.
    unsafe fn load_u8(&self, offset: usize, ordering: Ordering) -> u8;

    /// Atomically store a `u8` at `offset` with the given ordering.
    ///
    /// # Safety
    /// Caller must ensure `offset < self.size()`.
    unsafe fn store_u8(&self, offset: usize, value: u8, ordering: Ordering);

    /// Flush the byte range `[offset, offset + len)` to the persistence domain.
    ///
    /// On x86-64 with ADR: executes CLWB + SFENCE.
    /// On other platforms: may be a no-op.
    ///
    /// # Safety
    /// Caller must ensure `offset + len <= self.size()`.
    unsafe fn flush(&self, offset: usize, len: usize);

    /// Total size of the memory region in bytes.
    fn size(&self) -> usize;
}

/// Open a file-backed mmap pool. Returns a boxed MemoryBackend.
///
/// This is a convenience constructor that creates an `MmapBackend`.
/// For production, callers may use other backend implementations.
pub fn open_mmap_backend(
    path: &std::path::Path,
    size: usize,
) -> Result<Box<dyn MemoryBackend>, FenceError> {
    let backend = crate::mmap_backend::MmapBackend::open(path, size)?;
    Ok(Box::new(backend))
}
