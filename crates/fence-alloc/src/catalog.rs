//! Catalog data structures for the region allocator.
//!
//! The catalog lives at a fixed offset in the shared-memory pool and
//! tracks all allocated regions. It is crash-consistent: each state
//! transition is individually flushed.

use fence_core::backend::Ordering;
use fence_core::MemoryBackend;

/// Magic number for the allocator catalog header.
pub const ALLOC_MAGIC: u64 = 0x46454E43455F414C; // "FENCE_AL"

/// Size of the catalog header in bytes (one cacheline).
pub const CATALOG_HEADER_SIZE: usize = 64;

/// Size of each catalog entry in bytes (one cacheline).
pub const CATALOG_ENTRY_SIZE: usize = 64;

/// Maximum region name length in bytes.
pub const MAX_NAME_LEN: usize = 24;

/// Maximum number of regions the catalog can track.
pub const MAX_REGIONS: usize = 256;

/// Region lifecycle states.
///
/// Transitions: FREE → ALLOCATING → ACTIVE → FREEING → FREE
/// Recovery rules:
/// - ALLOCATING → rollback to FREE (allocation was incomplete)
/// - FREEING → complete the free (advance to FREE)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RegionState {
    /// Slot is unused.
    Free = 0,
    /// Allocation in progress (crash here → roll back to Free).
    Allocating = 1,
    /// Region is active and in use.
    Active = 2,
    /// Free in progress (crash here → complete the free).
    Freeing = 3,
}

impl RegionState {
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::Free),
            1 => Some(Self::Allocating),
            2 => Some(Self::Active),
            3 => Some(Self::Freeing),
            _ => None,
        }
    }
}

/// Catalog header (64 bytes, at the start of the allocator region).
///
/// Layout:
/// - [0..8)   magic (ALLOC_MAGIC)
/// - [8..16)  region_count (number of entries allocated, not necessarily active)
/// - [16..24) total_pool_size (total bytes managed)
/// - [24..32) data_start_offset (where region data begins, after catalog)
/// - [32..64) reserved/padding
#[derive(Debug, Clone, Copy)]
pub struct CatalogHeader {
    pub magic: u64,
    pub region_count: u64,
    pub total_pool_size: u64,
    pub data_start_offset: u64,
}

/// Offsets within the catalog header.
pub const CAT_HDR_MAGIC: usize = 0;
pub const CAT_HDR_REGION_COUNT: usize = 8;
pub const CAT_HDR_TOTAL_SIZE: usize = 16;
pub const CAT_HDR_DATA_START: usize = 24;

/// A catalog entry (64 bytes) representing one region.
///
/// Layout:
/// - [0..1)   state (RegionState as u8)
/// - [1..2)   _pad1
/// - [2..4)   name_len (u16, actual bytes used in name)
/// - [4..28)  name (24 bytes, UTF-8, zero-padded)
/// - [28..32) _pad2
/// - [32..40) offset (absolute byte offset of region data in pool)
/// - [40..48) size (region size in bytes)
/// - [48..56) created_epoch (epoch when allocated)
/// - [56..64) _pad3
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub state: RegionState,
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub created_epoch: u64,
}

/// Offsets within a catalog entry (relative to entry start).
pub const ENT_STATE: usize = 0;
pub const ENT_NAME_LEN: usize = 2;
pub const ENT_NAME: usize = 4;
pub const ENT_OFFSET: usize = 32;
pub const ENT_SIZE: usize = 40;
pub const ENT_EPOCH: usize = 48;

/// Compute the absolute offset of catalog entry `index` within the pool.
/// Entries start immediately after the catalog header.
#[inline]
pub fn entry_offset(base: usize, index: usize) -> usize {
    base + CATALOG_HEADER_SIZE + index * CATALOG_ENTRY_SIZE
}

/// Read a catalog entry from the backend.
///
/// # Safety
/// Caller must ensure the backend is valid and `index` is within bounds.
pub unsafe fn read_entry(
    backend: &dyn MemoryBackend,
    base: usize,
    index: usize,
) -> CatalogEntry {
    let offset = entry_offset(base, index);

    let state_byte = backend.load_u8(offset + ENT_STATE, Ordering::Acquire);
    let state = RegionState::from_u8(state_byte).unwrap_or(RegionState::Free);

    let name_len = {
        let mut buf = [0u8; 2];
        backend.read(offset + ENT_NAME_LEN, &mut buf);
        u16::from_le_bytes(buf) as usize
    };

    let name = if name_len > 0 && name_len <= MAX_NAME_LEN {
        let mut buf = [0u8; MAX_NAME_LEN];
        backend.read(offset + ENT_NAME, &mut buf[..name_len]);
        String::from_utf8_lossy(&buf[..name_len]).to_string()
    } else {
        String::new()
    };

    let region_offset = backend.load_u64(offset + ENT_OFFSET, Ordering::Relaxed);
    let size = backend.load_u64(offset + ENT_SIZE, Ordering::Relaxed);
    let created_epoch = backend.load_u64(offset + ENT_EPOCH, Ordering::Relaxed);

    CatalogEntry {
        state,
        name,
        offset: region_offset,
        size,
        created_epoch,
    }
}

/// Write a catalog entry to the backend.
///
/// # Safety
/// Caller must ensure the backend is valid and `index` is within bounds.
pub unsafe fn write_entry(
    backend: &dyn MemoryBackend,
    base: usize,
    index: usize,
    entry: &CatalogEntry,
) {
    let offset = entry_offset(base, index);

    // Write name.
    let name_bytes = entry.name.as_bytes();
    let name_len = name_bytes.len().min(MAX_NAME_LEN);
    let name_len_bytes = (name_len as u16).to_le_bytes();
    backend.write(offset + ENT_NAME_LEN, &name_len_bytes);
    if name_len > 0 {
        backend.write(offset + ENT_NAME, &name_bytes[..name_len]);
    }

    // Write offset, size, epoch.
    backend.store_u64(offset + ENT_OFFSET, entry.offset, Ordering::Relaxed);
    backend.store_u64(offset + ENT_SIZE, entry.size, Ordering::Relaxed);
    backend.store_u64(offset + ENT_EPOCH, entry.created_epoch, Ordering::Relaxed);

    // Write state LAST (makes the entry visible atomically).
    backend.flush(offset, CATALOG_ENTRY_SIZE);
    backend.store_u8(offset + ENT_STATE, entry.state as u8, Ordering::Release);
    backend.flush(offset, 64); // Flush state byte.
}
