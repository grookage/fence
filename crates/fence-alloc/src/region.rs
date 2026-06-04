//! Region handle — a reference to an allocated region in shared memory.

/// A handle to an allocated region in the shared-memory pool.
///
/// Provides the offset and size needed to access the region's data
/// through the MemoryBackend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionHandle {
    /// Catalog slot index for this region.
    pub slot: usize,
    /// Region name.
    pub name: String,
    /// Absolute byte offset of the region data in the pool.
    pub offset: u64,
    /// Size of the region in bytes.
    pub size: u64,
}

impl RegionHandle {
    /// Check if an offset is within this region's bounds.
    pub fn contains(&self, byte_offset: u64, len: u64) -> bool {
        byte_offset >= self.offset && byte_offset + len <= self.offset + self.size
    }

    /// Convert a region-relative offset to an absolute pool offset.
    pub fn absolute_offset(&self, relative: u64) -> u64 {
        self.offset + relative
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_check() {
        let handle = RegionHandle {
            slot: 0,
            name: "test".into(),
            offset: 1000,
            size: 500,
        };

        assert!(handle.contains(1000, 100));
        assert!(handle.contains(1000, 500));
        assert!(!handle.contains(1000, 501)); // Exceeds
        assert!(!handle.contains(999, 1)); // Before start
    }

    #[test]
    fn absolute_offset_calc() {
        let handle = RegionHandle {
            slot: 0,
            name: "test".into(),
            offset: 4096,
            size: 1024,
        };

        assert_eq!(handle.absolute_offset(0), 4096);
        assert_eq!(handle.absolute_offset(100), 4196);
    }
}
