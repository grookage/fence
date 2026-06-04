//! # fence-test-harness
//!
//! Shared test utilities for the Fence workspace. Provides:
//! - `TempPool`: create a temporary pool with a single call.
//! - `CrashSim`: simulate host crashes mid-operation (fork + SIGKILL on Linux, thread abort on macOS).
//! - `Barrier`: synchronize multiple threads/processes for concurrent tests.
//! - Assertion helpers for common pool state checks.

pub mod pool;
pub mod crash_sim;
pub mod barrier;
pub mod assertions;

pub use pool::TempPool;
pub use crash_sim::CrashSim;
pub use barrier::TestBarrier;
