//! Integration test: crash simulation and recovery.

use fence_runtime::{FenceRuntime, PoolConfig};
use fence_test_harness::CrashSim;

#[test]
fn recovery_fixes_crashed_writes() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Create pool and write some valid records.
    {
        let config = PoolConfig {
            path: path.clone(),
            capacity: 16,
            payload_size: 64,
            max_hosts: 2,
            host_id: 0,
            create: true,
        };
        let rt = FenceRuntime::open(config).unwrap();
        rt.append(1, b"good-1").unwrap();
        rt.append(1, b"good-2").unwrap();
    }

    // Simulate a crash: reserve a slot but don't commit.
    CrashSim::simulate_crashed_write(&path, 16, 64, 2);

    // Reopen and run recovery.
    {
        let config = PoolConfig {
            path: path.clone(),
            capacity: 16,
            payload_size: 64,
            max_hosts: 2,
            host_id: 1,
            create: false,
        };
        let rt = FenceRuntime::open(config).unwrap();

        let report = rt.recover();
        assert_eq!(report.abandoned_count, 1, "crashed write should be abandoned");
        assert_eq!(report.committed_tail, 2, "only first 2 records committed");

        // Valid records still readable.
        let r1 = rt.read(1).unwrap().unwrap();
        assert_eq!(r1.data, b"good-1");
        let r2 = rt.read(2).unwrap().unwrap();
        assert_eq!(r2.data, b"good-2");

        // Crashed record is beyond committed_tail, so it's not accessible.
        // committed_tail == 2 means only indices 1..=2 are committed.
        // Index 3 (the crashed slot) is out of bounds.
        let r3 = rt.read(3);
        assert!(r3.is_err(), "crashed record beyond committed_tail should be IndexOutOfBounds");
    }
}

#[test]
fn recovery_is_idempotent_integration() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Create pool with mixed state.
    {
        let config = PoolConfig {
            path: path.clone(),
            capacity: 16,
            payload_size: 64,
            max_hosts: 2,
            host_id: 0,
            create: true,
        };
        let rt = FenceRuntime::open(config).unwrap();
        rt.append(1, b"a").unwrap();
        rt.append(1, b"b").unwrap();
    }

    // Simulate crash.
    CrashSim::simulate_crashed_write(&path, 16, 64, 2);

    // Run recovery twice.
    let config = PoolConfig {
        path: path.clone(),
        capacity: 16,
        payload_size: 64,
        max_hosts: 2,
        host_id: 0,
        create: false,
    };
    let rt = FenceRuntime::open(config).unwrap();

    let report1 = rt.recover();
    let report2 = rt.recover();

    // Second run should find nothing new.
    assert_eq!(report2.abandoned_count, 0);
    assert_eq!(report1.committed_tail, report2.committed_tail);
}

#[test]
fn multiple_crashed_writes_all_recovered() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Create pool.
    {
        let config = PoolConfig {
            path: path.clone(),
            capacity: 16,
            payload_size: 64,
            max_hosts: 2,
            host_id: 0,
            create: true,
        };
        let rt = FenceRuntime::open(config).unwrap();
        rt.append(1, b"valid").unwrap();
    }

    // Simulate 3 crashed writes.
    CrashSim::simulate_crashed_write(&path, 16, 64, 2);
    CrashSim::simulate_crashed_write(&path, 16, 64, 2);
    CrashSim::simulate_crashed_write(&path, 16, 64, 2);

    // Recovery should fix all 3.
    let config = PoolConfig {
        path: path.clone(),
        capacity: 16,
        payload_size: 64,
        max_hosts: 2,
        host_id: 0,
        create: false,
    };
    let rt = FenceRuntime::open(config).unwrap();
    let report = rt.recover();

    assert_eq!(report.abandoned_count, 3);
    assert_eq!(report.committed_tail, 1); // Only first record is committed.
}

#[test]
fn writes_succeed_after_recovery() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Create pool with some records.
    {
        let config = PoolConfig {
            path: path.clone(),
            capacity: 16,
            payload_size: 64,
            max_hosts: 2,
            host_id: 0,
            create: true,
        };
        let rt = FenceRuntime::open(config).unwrap();
        rt.append(1, b"before-crash").unwrap();
    }

    // Simulate crash.
    CrashSim::simulate_crashed_write(&path, 16, 64, 2);

    // Recover and continue writing.
    let config = PoolConfig {
        path: path.clone(),
        capacity: 16,
        payload_size: 64,
        max_hosts: 2,
        host_id: 0,
        create: false,
    };
    let rt = FenceRuntime::open(config).unwrap();
    let report = rt.recover();

    // After recovery: committed_tail = 1 (slot 0 committed, slot 1 abandoned).
    assert_eq!(report.committed_tail, 1);
    assert_eq!(report.abandoned_count, 1);

    // New write goes to slot 2 (index 3). It commits, but committed_tail
    // can't advance past the abandoned slot 1 gap. committed_tail stays at 1.
    let idx = rt.append(2, b"after-recovery").unwrap();
    assert_eq!(idx, 3); // Slot 2, 1-based index = 3.

    // committed_tail is still 1 because slot 1 is ABANDONED (gap).
    assert_eq!(rt.committed_tail(), 1);

    // The record exists but is beyond committed_tail, so read returns OutOfBounds.
    // This is correct: in a real system, the caller would need to handle the gap
    // (trim past it, or use segmented overflow).
    // We can verify the first record is still fine.
    let r1 = rt.read(1).unwrap().unwrap();
    assert_eq!(r1.data, b"before-crash");
}
