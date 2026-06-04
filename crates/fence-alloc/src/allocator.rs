//! The region allocator — manages named regions in shared memory.
//!
//! Provides:
//! - `init()`: initialize a fresh allocator pool.
//! - `open()`: open an existing allocator pool.
//! - `create_region()`: allocate a named region.
//! - `free_region()`: release a region.
//! - `find_region()`: look up a region by name.
//! - `list_regions()`: enumerate all active regions.
//! - `recover()`: fix partially-transitioned regions after a crash.

use fence_core::backend::Ordering;
use fence_core::error::FenceError;
use fence_core::MemoryBackend;

use crate::catalog::*;
use crate::region::RegionHandle;

/// The region allocator. Wraps a MemoryBackend and manages a catalog
/// of named regions within the pool.
pub struct RegionAllocator {
    backend: Box<dyn MemoryBackend>,
    /// Base offset where the allocator catalog starts within the pool.
    base: usize,
    /// Total size of the pool managed by this allocator.
    pool_size: usize,
}

/// Error types specific to the allocator.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AllocError {
    /// No free catalog slot available.
    CatalogFull,
    /// A region with this name already exists.
    NameExists { name: String },
    /// Not enough contiguous free space.
    OutOfSpace { requested: u64, available: u64 },
    /// Region not found by name.
    NotFound { name: String },
    /// Invalid allocator state (bad magic, corruption).
    InvalidState { detail: String },
    /// Underlying backend error.
    Backend(FenceError),
}

impl From<FenceError> for AllocError {
    fn from(e: FenceError) -> Self {
        Self::Backend(e)
    }
}

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CatalogFull => write!(f, "catalog full: max {MAX_REGIONS} regions"),
            Self::NameExists { name } => write!(f, "region '{name}' already exists"),
            Self::OutOfSpace { requested, available } => {
                write!(f, "out of space: requested {requested}, available {available}")
            }
            Self::NotFound { name } => write!(f, "region '{name}' not found"),
            Self::InvalidState { detail } => write!(f, "invalid allocator state: {detail}"),
            Self::Backend(e) => write!(f, "backend error: {e}"),
        }
    }
}

impl std::error::Error for AllocError {}

impl RegionAllocator {
    /// Initialize a fresh allocator at `base` offset in the pool.
    ///
    /// Writes the catalog header and zeros all entries.
    pub fn init(
        backend: Box<dyn MemoryBackend>,
        base: usize,
        pool_size: usize,
    ) -> Result<Self, AllocError> {
        let catalog_size = CATALOG_HEADER_SIZE + MAX_REGIONS * CATALOG_ENTRY_SIZE;
        if pool_size < catalog_size {
            return Err(AllocError::OutOfSpace {
                requested: catalog_size as u64,
                available: pool_size as u64,
            });
        }

        let data_start = base + catalog_size;

        unsafe {
            // Write catalog header.
            backend.store_u64(base + CAT_HDR_MAGIC, ALLOC_MAGIC, Ordering::Relaxed);
            backend.store_u64(base + CAT_HDR_REGION_COUNT, 0, Ordering::Relaxed);
            backend.store_u64(base + CAT_HDR_TOTAL_SIZE, pool_size as u64, Ordering::Relaxed);
            backend.store_u64(base + CAT_HDR_DATA_START, data_start as u64, Ordering::Relaxed);
            backend.flush(base, CATALOG_HEADER_SIZE);

            // Zero all entry states.
            for i in 0..MAX_REGIONS {
                let offset = entry_offset(base, i);
                backend.store_u8(offset + ENT_STATE, RegionState::Free as u8, Ordering::Relaxed);
            }
            backend.flush(
                base + CATALOG_HEADER_SIZE,
                MAX_REGIONS * CATALOG_ENTRY_SIZE,
            );
        }

        Ok(Self {
            backend,
            base,
            pool_size,
        })
    }

    /// Open an existing allocator. Validates the magic number.
    pub fn open(
        backend: Box<dyn MemoryBackend>,
        base: usize,
        pool_size: usize,
    ) -> Result<Self, AllocError> {
        let magic = unsafe { backend.load_u64(base + CAT_HDR_MAGIC, Ordering::Relaxed) };
        if magic != ALLOC_MAGIC {
            return Err(AllocError::InvalidState {
                detail: format!("bad magic: expected 0x{ALLOC_MAGIC:016X}, got 0x{magic:016X}"),
            });
        }

        Ok(Self {
            backend,
            base,
            pool_size,
        })
    }

    /// Allocate a named region of `size` bytes.
    ///
    /// Uses a simple bump allocator: new regions are placed after the
    /// highest existing region. This is Phase 3 — no compaction or reuse
    /// of freed space yet.
    pub fn create_region(&self, name: &str, size: u64) -> Result<RegionHandle, AllocError> {
        if name.len() > MAX_NAME_LEN {
            return Err(AllocError::InvalidState {
                detail: format!("name too long: max {MAX_NAME_LEN} bytes"),
            });
        }

        // Check if name already exists.
        if self.find_region(name).is_ok() {
            return Err(AllocError::NameExists {
                name: name.to_string(),
            });
        }

        // Find a free catalog slot.
        let slot = self.find_free_slot().ok_or(AllocError::CatalogFull)?;

        // Compute the offset for this region (after all existing regions).
        let data_start = unsafe {
            self.backend
                .load_u64(self.base + CAT_HDR_DATA_START, Ordering::Relaxed)
        };
        let region_offset = self.next_free_offset(data_start as u64);

        // Check if there's enough space.
        let pool_end = (self.base + self.pool_size) as u64;
        if region_offset + size > pool_end {
            return Err(AllocError::OutOfSpace {
                requested: size,
                available: pool_end.saturating_sub(region_offset),
            });
        }

        // Write the entry in ALLOCATING state first (crash-safe protocol).
        let entry = CatalogEntry {
            state: RegionState::Allocating,
            name: name.to_string(),
            offset: region_offset,
            size,
            created_epoch: 0,
        };
        unsafe {
            write_entry(&*self.backend, self.base, slot, &entry);
        }

        // Transition to ACTIVE.
        unsafe {
            let ent_offset = entry_offset(self.base, slot);
            self.backend
                .store_u8(ent_offset + ENT_STATE, RegionState::Active as u8, Ordering::Release);
            self.backend.flush(ent_offset, 64);

            // Update region count.
            let count = self
                .backend
                .load_u64(self.base + CAT_HDR_REGION_COUNT, Ordering::Relaxed);
            self.backend.store_u64(
                self.base + CAT_HDR_REGION_COUNT,
                count + 1,
                Ordering::Relaxed,
            );
            self.backend.flush(self.base, CATALOG_HEADER_SIZE);
        }

        Ok(RegionHandle {
            slot,
            name: name.to_string(),
            offset: region_offset,
            size,
        })
    }

    /// Free a region by name.
    pub fn free_region(&self, name: &str) -> Result<(), AllocError> {
        let handle = self.find_region(name)?;
        let ent_offset = entry_offset(self.base, handle.slot);

        unsafe {
            // Mark FREEING (crash here → recovery completes the free).
            self.backend
                .store_u8(ent_offset + ENT_STATE, RegionState::Freeing as u8, Ordering::Release);
            self.backend.flush(ent_offset, 64);

            // Complete: mark FREE.
            self.backend
                .store_u8(ent_offset + ENT_STATE, RegionState::Free as u8, Ordering::Release);
            self.backend.flush(ent_offset, 64);

            // Decrement region count.
            let count = self
                .backend
                .load_u64(self.base + CAT_HDR_REGION_COUNT, Ordering::Relaxed);
            if count > 0 {
                self.backend.store_u64(
                    self.base + CAT_HDR_REGION_COUNT,
                    count - 1,
                    Ordering::Relaxed,
                );
                self.backend.flush(self.base, CATALOG_HEADER_SIZE);
            }
        }

        Ok(())
    }

    /// Find a region by name.
    pub fn find_region(&self, name: &str) -> Result<RegionHandle, AllocError> {
        for i in 0..MAX_REGIONS {
            let entry = unsafe { read_entry(&*self.backend, self.base, i) };
            if entry.state == RegionState::Active && entry.name == name {
                return Ok(RegionHandle {
                    slot: i,
                    name: entry.name,
                    offset: entry.offset,
                    size: entry.size,
                });
            }
        }
        Err(AllocError::NotFound {
            name: name.to_string(),
        })
    }

    /// List all active regions.
    pub fn list_regions(&self) -> Vec<RegionHandle> {
        let mut regions = Vec::new();
        for i in 0..MAX_REGIONS {
            let entry = unsafe { read_entry(&*self.backend, self.base, i) };
            if entry.state == RegionState::Active {
                regions.push(RegionHandle {
                    slot: i,
                    name: entry.name,
                    offset: entry.offset,
                    size: entry.size,
                });
            }
        }
        regions
    }

    /// Run crash recovery on the catalog.
    ///
    /// - ALLOCATING → roll back to FREE (incomplete allocation).
    /// - FREEING → complete to FREE (incomplete free).
    ///
    /// Returns the number of entries fixed.
    pub fn recover(&self) -> usize {
        let mut fixed = 0;
        for i in 0..MAX_REGIONS {
            let entry = unsafe { read_entry(&*self.backend, self.base, i) };
            match entry.state {
                RegionState::Allocating | RegionState::Freeing => {
                    // Roll forward/back to FREE.
                    let ent_offset = entry_offset(self.base, i);
                    unsafe {
                        self.backend.store_u8(
                            ent_offset + ENT_STATE,
                            RegionState::Free as u8,
                            Ordering::Release,
                        );
                        self.backend.flush(ent_offset, 64);
                    }
                    fixed += 1;
                }
                _ => {}
            }
        }
        fixed
    }

    /// Access the underlying backend (for region data read/write).
    pub fn backend(&self) -> &dyn MemoryBackend {
        &*self.backend
    }

    // ── Private helpers ──

    fn find_free_slot(&self) -> Option<usize> {
        for i in 0..MAX_REGIONS {
            let entry = unsafe { read_entry(&*self.backend, self.base, i) };
            if entry.state == RegionState::Free {
                return Some(i);
            }
        }
        None
    }

    /// Find the next free data offset (after all existing regions).
    fn next_free_offset(&self, data_start: u64) -> u64 {
        let mut max_end = data_start;
        for i in 0..MAX_REGIONS {
            let entry = unsafe { read_entry(&*self.backend, self.base, i) };
            if entry.state == RegionState::Active || entry.state == RegionState::Allocating {
                let end = entry.offset + entry.size;
                if end > max_end {
                    max_end = end;
                }
            }
        }
        // Align to 64-byte boundary.
        (max_end + 63) & !63
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fence_core::mmap_backend::MmapBackend;
    use tempfile::NamedTempFile;

    fn create_allocator(pool_size: usize) -> (RegionAllocator, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
        let alloc = RegionAllocator::init(Box::new(backend), 0, pool_size).unwrap();
        (alloc, tmp)
    }

    #[test]
    fn init_and_open() {
        let tmp = NamedTempFile::new().unwrap();
        let pool_size = 1024 * 1024;

        // Init.
        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            RegionAllocator::init(Box::new(backend), 0, pool_size).unwrap();
        }

        // Open.
        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            let alloc = RegionAllocator::open(Box::new(backend), 0, pool_size).unwrap();
            assert_eq!(alloc.list_regions().len(), 0);
        }
    }

    #[test]
    fn create_and_find_region() {
        let (alloc, _tmp) = create_allocator(1024 * 1024);

        let handle = alloc.create_region("my-index", 4096).unwrap();
        assert_eq!(handle.name, "my-index");
        assert_eq!(handle.size, 4096);

        let found = alloc.find_region("my-index").unwrap();
        assert_eq!(found.offset, handle.offset);
        assert_eq!(found.size, 4096);
    }

    #[test]
    fn create_multiple_regions() {
        let (alloc, _tmp) = create_allocator(1024 * 1024);

        let r1 = alloc.create_region("region-a", 1024).unwrap();
        let r2 = alloc.create_region("region-b", 2048).unwrap();
        let r3 = alloc.create_region("region-c", 512).unwrap();

        // Regions should not overlap.
        assert!(r2.offset >= r1.offset + r1.size);
        assert!(r3.offset >= r2.offset + r2.size);

        let all = alloc.list_regions();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn duplicate_name_rejected() {
        let (alloc, _tmp) = create_allocator(1024 * 1024);

        alloc.create_region("dup", 1024).unwrap();
        let result = alloc.create_region("dup", 2048);
        assert!(matches!(result, Err(AllocError::NameExists { .. })));
    }

    #[test]
    fn free_region_works() {
        let (alloc, _tmp) = create_allocator(1024 * 1024);

        alloc.create_region("temp", 1024).unwrap();
        assert_eq!(alloc.list_regions().len(), 1);

        alloc.free_region("temp").unwrap();
        assert_eq!(alloc.list_regions().len(), 0);

        // Should not be findable.
        assert!(matches!(
            alloc.find_region("temp"),
            Err(AllocError::NotFound { .. })
        ));
    }

    #[test]
    fn free_nonexistent_errors() {
        let (alloc, _tmp) = create_allocator(1024 * 1024);
        let result = alloc.free_region("ghost");
        assert!(matches!(result, Err(AllocError::NotFound { .. })));
    }

    #[test]
    fn out_of_space() {
        // Very small pool.
        let catalog_overhead = CATALOG_HEADER_SIZE + MAX_REGIONS * CATALOG_ENTRY_SIZE;
        let pool_size = catalog_overhead + 100; // Only 100 bytes of data space.
        let (alloc, _tmp) = create_allocator(pool_size);

        let result = alloc.create_region("too-big", 1000);
        assert!(matches!(result, Err(AllocError::OutOfSpace { .. })));
    }

    #[test]
    fn recovery_fixes_allocating_state() {
        let tmp = NamedTempFile::new().unwrap();
        let pool_size = 1024 * 1024;

        // Init and manually put an entry in ALLOCATING state.
        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            let alloc = RegionAllocator::init(Box::new(backend), 0, pool_size).unwrap();
            // Create a region normally.
            alloc.create_region("good", 1024).unwrap();
        }

        // Simulate a crash during allocation by writing ALLOCATING directly.
        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            let _ent_offset = entry_offset(0, 1); // Slot 1
            unsafe {
                let entry = CatalogEntry {
                    state: RegionState::Allocating,
                    name: "crashed".to_string(),
                    offset: 50000,
                    size: 2048,
                    created_epoch: 0,
                };
                write_entry(&backend, 0, 1, &entry);
            }
            drop(backend);
        }

        // Open and recover.
        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            let alloc = RegionAllocator::open(Box::new(backend), 0, pool_size).unwrap();

            let fixed = alloc.recover();
            assert_eq!(fixed, 1);

            // "good" should still be there.
            assert!(alloc.find_region("good").is_ok());
            // "crashed" should be gone.
            assert!(alloc.find_region("crashed").is_err());
        }
    }

    #[test]
    fn recovery_fixes_freeing_state() {
        let tmp = NamedTempFile::new().unwrap();
        let pool_size = 1024 * 1024;

        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            let alloc = RegionAllocator::init(Box::new(backend), 0, pool_size).unwrap();
            alloc.create_region("to-free", 1024).unwrap();
        }

        // Simulate crash during free: manually set state to FREEING.
        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            let ent_offset = entry_offset(0, 0);
            unsafe {
                backend.store_u8(
                    ent_offset + ENT_STATE,
                    RegionState::Freeing as u8,
                    Ordering::Relaxed,
                );
                backend.flush(ent_offset, 64);
            }
            drop(backend);
        }

        // Recovery should complete the free.
        {
            let backend = MmapBackend::open(tmp.path(), pool_size).unwrap();
            let alloc = RegionAllocator::open(Box::new(backend), 0, pool_size).unwrap();

            let fixed = alloc.recover();
            assert_eq!(fixed, 1);
            assert_eq!(alloc.list_regions().len(), 0);
        }
    }

    #[test]
    fn can_reuse_freed_slot() {
        let (alloc, _tmp) = create_allocator(1024 * 1024);

        alloc.create_region("first", 1024).unwrap();
        alloc.free_region("first").unwrap();

        // Should be able to allocate again (reuses the freed slot).
        let handle = alloc.create_region("second", 2048).unwrap();
        assert_eq!(handle.name, "second");
        assert_eq!(handle.slot, 0); // Reused slot 0.
    }
}
