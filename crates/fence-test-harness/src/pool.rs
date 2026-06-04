//! TempPool — one-liner pool creation for tests.
//!
//! Creates a temporary file, configures a pool, and provides access to
//! the `FenceRuntime`. The pool is cleaned up when `TempPool` is dropped.

use fence_runtime::{FenceRuntime, PoolConfig};
use tempfile::NamedTempFile;

/// A temporary Fence pool backed by an auto-deleted temp file.
///
/// # Example
/// ```no_run
/// use fence_test_harness::TempPool;
/// let pool = TempPool::new();
/// let idx = pool.runtime().append(1, b"hello").unwrap();
/// ```
pub struct TempPool {
    runtime: FenceRuntime,
    _tmp: NamedTempFile,
}

/// Builder for customizing TempPool parameters.
pub struct TempPoolBuilder {
    capacity: u32,
    payload_size: u32,
    max_hosts: u16,
    host_id: u16,
}

impl Default for TempPoolBuilder {
    fn default() -> Self {
        Self {
            capacity: 1024,
            payload_size: 128,
            max_hosts: 4,
            host_id: 0,
        }
    }
}

impl TempPoolBuilder {
    pub fn capacity(mut self, capacity: u32) -> Self {
        self.capacity = capacity;
        self
    }

    pub fn payload_size(mut self, payload_size: u32) -> Self {
        self.payload_size = payload_size;
        self
    }

    pub fn max_hosts(mut self, max_hosts: u16) -> Self {
        self.max_hosts = max_hosts;
        self
    }

    pub fn host_id(mut self, host_id: u16) -> Self {
        self.host_id = host_id;
        self
    }

    pub fn build(self) -> TempPool {
        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let config = PoolConfig {
            path: tmp.path().to_path_buf(),
            capacity: self.capacity,
            payload_size: self.payload_size,
            max_hosts: self.max_hosts,
            host_id: self.host_id,
            create: true,
        };
        let runtime = FenceRuntime::open(config).expect("failed to open pool");
        TempPool { runtime, _tmp: tmp }
    }
}

impl TempPool {
    /// Create a pool with default settings (1024 slots, 128B payload, 4 hosts).
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Get a builder for customized pool creation.
    pub fn builder() -> TempPoolBuilder {
        TempPoolBuilder::default()
    }

    /// Access the runtime.
    pub fn runtime(&self) -> &FenceRuntime {
        &self.runtime
    }

    /// Consume and return the underlying runtime (temp file stays alive via returned struct).
    pub fn into_runtime(self) -> (FenceRuntime, NamedTempFile) {
        (self.runtime, self._tmp)
    }

    /// Get the path to the backing file (useful for opening a second runtime on same pool).
    pub fn path(&self) -> &std::path::Path {
        self._tmp.path()
    }
}

impl Default for TempPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_pool_creates_and_drops() {
        let pool = TempPool::new();
        let idx = pool.runtime().append(1, b"test").unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn builder_customizes_capacity() {
        let pool = TempPool::builder().capacity(8).build();
        for i in 0..8 {
            pool.runtime().append(1, format!("msg{i}").as_bytes()).unwrap();
        }
        // 9th should fail
        assert!(pool.runtime().append(1, b"overflow").is_err());
    }
}
