//! Integration test: compaction (trim) behavior.

use fence_test_harness::TempPool;

#[test]
fn trim_frees_records_for_read() {
    let pool = TempPool::builder().capacity(32).build();
    let rt = pool.runtime();

    // Write 10 records.
    for i in 0..10 {
        rt.append(1, format!("msg-{i}").as_bytes()).unwrap();
    }

    // Trim first 5.
    let result = rt.trim(5).unwrap();
    assert_eq!(result.trimmed_count, 5);

    // Records 1-5 should be None (abandoned).
    for idx in 1..=5 {
        assert_eq!(rt.read(idx).unwrap(), None, "record {idx} should be trimmed");
    }

    // Records 6-10 should still be readable.
    for idx in 6..=10 {
        let rec = rt.read(idx).unwrap();
        assert!(rec.is_some(), "record {idx} should still exist");
    }
}

#[test]
fn trim_zero_is_noop() {
    let pool = TempPool::new();
    let rt = pool.runtime();
    rt.append(1, b"data").unwrap();

    let result = rt.trim(0).unwrap();
    assert_eq!(result.trimmed_count, 0);

    // Record still readable.
    assert!(rt.read(1).unwrap().is_some());
}

#[test]
fn trim_then_continue_writing() {
    let pool = TempPool::builder().capacity(16).build();
    let rt = pool.runtime();

    // Fill partially.
    for _ in 0..8 {
        rt.append(1, b"data").unwrap();
    }

    // Trim first 4.
    rt.trim(4).unwrap();

    // Continue writing (slots 8-15 should work).
    for _ in 0..8 {
        rt.append(1, b"more").unwrap();
    }

    // Verify: 16 total slots used, first 4 trimmed.
    assert_eq!(rt.committed_tail(), 16);
    assert_eq!(rt.read(1).unwrap(), None); // trimmed
    assert!(rt.read(5).unwrap().is_some()); // still valid
    assert!(rt.read(16).unwrap().is_some()); // last write
}

#[test]
fn trim_idempotent() {
    let pool = TempPool::builder().capacity(16).build();
    let rt = pool.runtime();

    for _ in 0..5 {
        rt.append(1, b"x").unwrap();
    }

    let r1 = rt.trim(3).unwrap();
    assert_eq!(r1.trimmed_count, 3);

    // Trim same range again — already abandoned, so 0 new trims.
    let r2 = rt.trim(3).unwrap();
    assert_eq!(r2.trimmed_count, 0);
}

#[test]
fn trim_beyond_committed_fails() {
    let pool = TempPool::new();
    let rt = pool.runtime();
    rt.append(1, b"one").unwrap();
    rt.append(1, b"two").unwrap();

    let result = rt.trim(10);
    assert!(result.is_err());
}

#[test]
fn read_range_skips_trimmed() {
    let pool = TempPool::builder().capacity(16).build();
    let rt = pool.runtime();

    for i in 0..8 {
        rt.append(1, format!("r{i}").as_bytes()).unwrap();
    }

    // Trim records 1-3.
    rt.trim(3).unwrap();

    // read_range should skip trimmed records.
    let records = rt.read_range(1, 9).unwrap();
    assert_eq!(records.len(), 5); // Only records 4-8 returned.
    assert_eq!(records[0].index, 4);
}
