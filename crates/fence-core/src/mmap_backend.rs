//! File-backed mmap implementation of `MemoryBackend`.
//!
//! Used for development (macOS ARM) and single-machine deployment.
//! Maps a file into the process address space and implements all
//! backend operations via pointer arithmetic + atomics.

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8};

use crate::backend::{MemoryBackend, Ordering};
use crate::error::FenceError;
use crate::persistence;

/// A memory backend backed by a file-based mmap.
///
/// The file is created/opened, sized via `ftruncate`, and mapped with
/// `MAP_SHARED` so that multiple processes can share the same pool
/// (simulating CXL shared memory on a single machine).
pub struct MmapBackend {
    /// Raw pointer to the start of the mapped region.
    ptr: *mut u8,
    /// Size of the mapped region in bytes.
    len: usize,
    /// File descriptor (kept open for the lifetime of the mapping).
    fd: i32,
}

// SAFETY: The mmap'd region is MAP_SHARED and accessed only through atomic
// operations and volatile reads/writes. The raw pointer does not alias any
// Rust references. Multiple threads can safely call the backend methods
// concurrently because:
// - Atomic operations on u64/u8 are inherently thread-safe.
// - read/write use copy_nonoverlapping on disjoint regions (callers ensure
//   exclusive ownership of record slots via the reserve protocol).
unsafe impl Send for MmapBackend {}
unsafe impl Sync for MmapBackend {}

impl MmapBackend {
    /// Open (or create) a file at `path` and mmap it with the given `size`.
    ///
    /// If the file is smaller than `size`, it is extended via `ftruncate`.
    /// The mapping uses `MAP_SHARED` + `PROT_READ | PROT_WRITE`.
    pub fn open(path: &Path, size: usize) -> Result<Self, FenceError> {
        use libc::{
            c_int, close, ftruncate, mmap, open, MAP_FAILED, MAP_SHARED, O_CREAT, O_RDWR,
            PROT_READ, PROT_WRITE,
        };
        use std::ffi::CString;

        let c_path = CString::new(
            path.to_str()
                .ok_or_else(|| FenceError::MmapFailed {
                    detail: "path contains invalid UTF-8".into(),
                })?,
        )
        .map_err(|_| FenceError::MmapFailed {
            detail: "path contains null byte".into(),
        })?;

        unsafe {
            // SAFETY: c_path is a valid null-terminated C string.
            let fd: c_int = open(c_path.as_ptr(), O_RDWR | O_CREAT, 0o644);
            if fd < 0 {
                return Err(FenceError::MmapFailed {
                    detail: format!("open() failed: {}", std::io::Error::last_os_error()),
                });
            }

            // SAFETY: fd is a valid file descriptor.
            if ftruncate(fd, size as libc::off_t) != 0 {
                close(fd);
                return Err(FenceError::MmapFailed {
                    detail: format!("ftruncate() failed: {}", std::io::Error::last_os_error()),
                });
            }

            // SAFETY: fd is valid, size > 0, flags are correct.
            let ptr = mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            );

            if ptr == MAP_FAILED {
                close(fd);
                return Err(FenceError::MmapFailed {
                    detail: format!("mmap() failed: {}", std::io::Error::last_os_error()),
                });
            }

            Ok(Self {
                ptr: ptr as *mut u8,
                len: size,
                fd,
            })
        }
    }
}

impl MemoryBackend for MmapBackend {
    unsafe fn read(&self, offset: usize, dst: &mut [u8]) {
        debug_assert!(offset + dst.len() <= self.len);
        // SAFETY: Caller ensures offset + dst.len() <= self.len.
        // copy_nonoverlapping is used because src and dst never overlap
        // (dst is a caller-owned buffer, src is mmap'd memory).
        let src = self.ptr.add(offset);
        std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), dst.len());
    }

    unsafe fn write(&self, offset: usize, src: &[u8]) {
        debug_assert!(offset + src.len() <= self.len);
        // SAFETY: Caller ensures offset + src.len() <= self.len.
        // The writer has exclusive ownership of this record slot
        // (guaranteed by the reserve protocol in the engine).
        let dst = self.ptr.add(offset);
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
    }

    unsafe fn load_u64(&self, offset: usize, ordering: Ordering) -> u64 {
        debug_assert!(offset + 8 <= self.len);
        debug_assert!(offset % 8 == 0, "load_u64: offset must be 8-byte aligned");
        // SAFETY: Caller ensures offset is aligned and within bounds.
        // AtomicU64::from_ptr is sound because the pointer is aligned,
        // valid, and the mmap lives for the process lifetime.
        let ptr = self.ptr.add(offset) as *mut u64;
        AtomicU64::from_ptr(ptr).load(ordering)
    }

    unsafe fn store_u64(&self, offset: usize, value: u64, ordering: Ordering) {
        debug_assert!(offset + 8 <= self.len);
        debug_assert!(offset % 8 == 0, "store_u64: offset must be 8-byte aligned");
        // SAFETY: Same as load_u64.
        let ptr = self.ptr.add(offset) as *mut u64;
        AtomicU64::from_ptr(ptr).store(value, ordering);
    }

    unsafe fn fetch_add_u64(&self, offset: usize, delta: u64, ordering: Ordering) -> u64 {
        debug_assert!(offset + 8 <= self.len);
        debug_assert!(offset % 8 == 0, "fetch_add_u64: offset must be 8-byte aligned");
        // SAFETY: Same as load_u64.
        let ptr = self.ptr.add(offset) as *mut u64;
        AtomicU64::from_ptr(ptr).fetch_add(delta, ordering)
    }

    unsafe fn compare_exchange_u64(
        &self,
        offset: usize,
        expected: u64,
        desired: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        debug_assert!(offset + 8 <= self.len);
        debug_assert!(offset % 8 == 0, "compare_exchange_u64: offset must be 8-byte aligned");
        // SAFETY: Same as load_u64.
        let ptr = self.ptr.add(offset) as *mut u64;
        AtomicU64::from_ptr(ptr).compare_exchange(expected, desired, success, failure)
    }

    unsafe fn load_u8(&self, offset: usize, ordering: Ordering) -> u8 {
        debug_assert!(offset < self.len);
        // SAFETY: Caller ensures offset is within bounds.
        let ptr = self.ptr.add(offset) as *mut u8;
        AtomicU8::from_ptr(ptr).load(ordering)
    }

    unsafe fn store_u8(&self, offset: usize, value: u8, ordering: Ordering) {
        debug_assert!(offset < self.len);
        // SAFETY: Caller ensures offset is within bounds.
        let ptr = self.ptr.add(offset) as *mut u8;
        AtomicU8::from_ptr(ptr).store(value, ordering);
    }

    unsafe fn flush(&self, offset: usize, len: usize) {
        debug_assert!(offset + len <= self.len);
        // SAFETY: Caller ensures the range is within the mapping.
        let addr = self.ptr.add(offset) as *const u8;
        persistence::flush_range(addr, len);
    }

    fn size(&self) -> usize {
        self.len
    }
}

impl Drop for MmapBackend {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: self.ptr and self.len were set by a successful mmap call.
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
            // SAFETY: self.fd is a valid file descriptor from a successful open call.
            libc::close(self.fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_backend(size: usize) -> (MmapBackend, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let backend = MmapBackend::open(tmp.path(), size).unwrap();
        (backend, tmp)
    }

    #[test]
    fn read_write_roundtrip() {
        let (backend, _tmp) = create_backend(4096);
        let data = b"hello, fence!";
        unsafe {
            backend.write(0, data);
            let mut buf = vec![0u8; data.len()];
            backend.read(0, &mut buf);
            assert_eq!(&buf, data);
        }
    }

    #[test]
    fn read_write_at_offset() {
        let (backend, _tmp) = create_backend(4096);
        let data = b"offset test";
        unsafe {
            backend.write(1024, data);
            let mut buf = vec![0u8; data.len()];
            backend.read(1024, &mut buf);
            assert_eq!(&buf, data);
        }
    }

    #[test]
    fn atomic_u64_store_load() {
        let (backend, _tmp) = create_backend(4096);
        unsafe {
            backend.store_u64(0, 0xDEAD_BEEF_CAFE_BABE, Ordering::SeqCst);
            let val = backend.load_u64(0, Ordering::SeqCst);
            assert_eq!(val, 0xDEAD_BEEF_CAFE_BABE);
        }
    }

    #[test]
    fn atomic_fetch_add() {
        let (backend, _tmp) = create_backend(4096);
        unsafe {
            backend.store_u64(64, 10, Ordering::SeqCst);
            let prev = backend.fetch_add_u64(64, 5, Ordering::SeqCst);
            assert_eq!(prev, 10);
            let curr = backend.load_u64(64, Ordering::SeqCst);
            assert_eq!(curr, 15);
        }
    }

    #[test]
    fn atomic_compare_exchange_success() {
        let (backend, _tmp) = create_backend(4096);
        unsafe {
            backend.store_u64(0, 42, Ordering::SeqCst);
            let result =
                backend.compare_exchange_u64(0, 42, 99, Ordering::SeqCst, Ordering::SeqCst);
            assert_eq!(result, Ok(42));
            assert_eq!(backend.load_u64(0, Ordering::SeqCst), 99);
        }
    }

    #[test]
    fn atomic_compare_exchange_failure() {
        let (backend, _tmp) = create_backend(4096);
        unsafe {
            backend.store_u64(0, 42, Ordering::SeqCst);
            let result =
                backend.compare_exchange_u64(0, 100, 99, Ordering::SeqCst, Ordering::SeqCst);
            assert_eq!(result, Err(42));
            // Value unchanged
            assert_eq!(backend.load_u64(0, Ordering::SeqCst), 42);
        }
    }

    #[test]
    fn atomic_u8_store_load() {
        let (backend, _tmp) = create_backend(4096);
        unsafe {
            backend.store_u8(7, 0xAB, Ordering::SeqCst);
            let val = backend.load_u8(7, Ordering::SeqCst);
            assert_eq!(val, 0xAB);
        }
    }

    #[test]
    fn flush_does_not_panic() {
        let (backend, _tmp) = create_backend(4096);
        unsafe {
            backend.write(0, b"some data to flush");
            backend.flush(0, 64);
        }
    }

    #[test]
    fn size_matches() {
        let (backend, _tmp) = create_backend(8192);
        assert_eq!(backend.size(), 8192);
    }

    #[test]
    fn multiple_regions_independent() {
        let (backend, _tmp) = create_backend(4096);
        unsafe {
            backend.write(0, b"region A");
            backend.write(2048, b"region B");

            let mut buf_a = vec![0u8; 8];
            let mut buf_b = vec![0u8; 8];
            backend.read(0, &mut buf_a);
            backend.read(2048, &mut buf_b);

            assert_eq!(&buf_a, b"region A");
            assert_eq!(&buf_b, b"region B");
        }
    }
}
