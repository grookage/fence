//! # fence-alloc
//!
//! Shared-memory region allocator for the Fence pool.
//!
//! Provides crash-consistent allocation and freeing of named memory regions
//! within a shared-memory pool. The allocator maintains a catalog at a fixed
//! offset and tracks region state transitions atomically.
//!
//! ## Architecture
//!
//! ```text
//! Pool layout:
//! [Catalog Header (64B)] [Catalog Entries...] [Region 0 data] [Region 1 data] ...
//! ```
//!
//! Region lifecycle: FREE → ALLOCATING → ACTIVE → FREEING → FREE

pub mod catalog;
pub mod region;
pub mod allocator;

pub use allocator::RegionAllocator;
pub use catalog::{CatalogHeader, CatalogEntry, RegionState};
pub use region::RegionHandle;
