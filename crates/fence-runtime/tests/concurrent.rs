//! Integration test: concurrent append from multiple threads.

use fence_test_harness::{TempPool, TestBarrier};
use std::sync::Arc;
use std::thread;

#[test]
fn concurrent_appends_no_data_loss() {
    let pool = TempPool::builder()
        .capacity(4000)
        .payload_size(64)
        .max_hosts(8)
        .build();

    let rt = Arc::new(pool.into_runtime());
    let barrier = TestBarrier::new(4);
    let records_per_thread = 100;

    let handles: Vec<_> = (0..4)
        .map(|tid| {
            let rt = rt.clone();
            let b = barrier.clone();
            thread::spawn(move || {
                b.wait(); // All threads start simultaneously.
                let mut indices = Vec::with_capacity(records_per_thread);
                for i in 0..records_per_thread {
                    let data = format!("t{tid}-msg{i}");
                    let idx = rt.0.append(1, data.as_bytes()).unwrap();
                    indices.push(idx);
                }
                indices
            })
        })
        .collect();

    let mut all_indices: Vec<u64> = Vec::new();
    for h in handles {
        all_indices.extend(h.join().unwrap());
    }

    // All indices should be unique (no slot collisions).
    all_indices.sort();
    all_indices.dedup();
    assert_eq!(all_indices.len(), 4 * records_per_thread);

    // committed_tail should eventually reach total appends.
    assert_eq!(rt.0.committed_tail(), 400);

    // All records should be readable.
    for idx in &all_indices {
        let rec = rt.0.read(*idx).unwrap();
        assert!(rec.is_some(), "record at index {idx} should be readable");
    }
}

#[test]
fn concurrent_append_and_read() {
    let pool = TempPool::builder()
        .capacity(2000)
        .payload_size(64)
        .build();

    let rt = Arc::new(pool.into_runtime());
    let barrier = TestBarrier::new(3);

    // Writer threads.
    let writer_handles: Vec<_> = (0..2)
        .map(|tid| {
            let rt = rt.clone();
            let b = barrier.clone();
            thread::spawn(move || {
                b.wait();
                for i in 0..500 {
                    let data = format!("w{tid}-{i}");
                    rt.0.append(1, data.as_bytes()).unwrap();
                }
            })
        })
        .collect();

    // Reader thread — reads whatever is committed.
    let reader_rt = rt.clone();
    let reader_barrier = barrier.clone();
    let reader = thread::spawn(move || {
        reader_barrier.wait();
        let mut max_seen = 0u64;
        for _ in 0..2000 {
            let tail = reader_rt.0.committed_tail();
            if tail > max_seen {
                // Read newly committed records.
                for idx in (max_seen + 1)..=tail {
                    let result = reader_rt.0.read(idx);
                    // Should succeed (committed means valid).
                    assert!(result.is_ok(), "read at {idx} failed: {:?}", result);
                }
                max_seen = tail;
            }
            if max_seen >= 1000 {
                break;
            }
            std::thread::yield_now();
        }
        max_seen
    });

    for h in writer_handles {
        h.join().unwrap();
    }
    let final_read = reader.join().unwrap();
    assert!(final_read > 0, "reader should have seen some records");
}

#[test]
fn no_duplicate_indices() {
    let pool = TempPool::builder()
        .capacity(800)
        .payload_size(32)
        .build();

    let rt = Arc::new(pool.into_runtime());
    let barrier = TestBarrier::new(8);

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let rt = rt.clone();
            let b = barrier.clone();
            thread::spawn(move || {
                b.wait();
                (0..100)
                    .map(|_| rt.0.append(1, b"x").unwrap())
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    let mut all: Vec<u64> = Vec::new();
    for h in handles {
        all.extend(h.join().unwrap());
    }

    all.sort();
    let before_dedup = all.len();
    all.dedup();
    assert_eq!(all.len(), before_dedup, "duplicate indices detected!");
}
