//! # fence-runtime
//!
//! The Fence shared-memory log engine. Provides the core append/read/trim/recover
//! API built on top of `fence-core` primitives.
//!
//! ## Quick Start
//!
//! ```no_run
//! use fence_runtime::{FenceRuntime, PoolConfig};
//!
//! let config = PoolConfig {
//!     path: "/tmp/fence-pool".into(),
//!     capacity: 1024,
//!     payload_size: 256,
//!     max_hosts: 4,
//!     host_id: 0,
//!     create: true,
//! };
//!
//! let runtime = FenceRuntime::open(config).unwrap();
//! let index = runtime.append(1, b"hello").unwrap();
//! let record = runtime.read(index).unwrap().unwrap();
//! assert_eq!(record.data, b"hello");
//! ```

pub mod metrics;
pub mod recovery;
pub mod compaction;
pub mod engine;

// Re-export key types at crate root.
pub use engine::{FenceRuntime, PoolConfig, Record};
pub use metrics::PoolStats;
pub use recovery::RecoveryReport;
pub use compaction::TrimResult;
