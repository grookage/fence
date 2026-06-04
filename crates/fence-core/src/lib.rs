//! # fence-core
//!
//! Foundational primitives for the Fence shared-memory log engine.
//!
//! This crate provides:
//! - Memory layout constants and geometry (`layout`)
//! - CRC32C checksum computation (`checksum`)
//! - Platform persistence primitives (`persistence`)
//! - The `MemoryBackend` trait abstraction (`backend`)
//! - File-backed mmap implementation (`mmap_backend`)
//! - Shared error types (`error`)
//!
//! Higher-level crates (`fence-runtime`) build the log engine on top of these.

pub mod layout;
pub mod checksum;
pub mod persistence;
pub mod error;
pub mod backend;
pub mod mmap_backend;

// Re-export key types at crate root for convenience.
pub use backend::{MemoryBackend, Ordering, open_mmap_backend};
pub use error::FenceError;
pub use layout::PoolGeometry;
