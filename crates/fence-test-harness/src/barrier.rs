//! Thread synchronization barrier for concurrent tests.
//!
//! Wraps `std::sync::Barrier` with convenience methods for the common
//! pattern: N threads start, wait at a barrier, then all proceed simultaneously.

use std::sync::{Arc, Barrier};

/// A test barrier for synchronizing concurrent operations.
///
/// # Example
/// ```no_run
/// use fence_test_harness::TestBarrier;
/// let barrier = TestBarrier::new(4);
/// // Spawn 4 threads, each calls barrier.wait() before starting work.
/// ```
#[derive(Clone)]
pub struct TestBarrier {
    inner: Arc<Barrier>,
    thread_count: usize,
}

impl TestBarrier {
    /// Create a barrier for `n` threads.
    pub fn new(n: usize) -> Self {
        Self {
            inner: Arc::new(Barrier::new(n)),
            thread_count: n,
        }
    }

    /// Wait at the barrier. Returns when all threads have arrived.
    pub fn wait(&self) {
        self.inner.wait();
    }

    /// How many threads this barrier expects.
    pub fn thread_count(&self) -> usize {
        self.thread_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[test]
    fn barrier_synchronizes_threads() {
        let barrier = TestBarrier::new(4);
        let counter = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let b = barrier.clone();
                let c = counter.clone();
                thread::spawn(move || {
                    b.wait();
                    c.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }
}
