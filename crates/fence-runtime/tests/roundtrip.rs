//! Integration test: end-to-end roundtrip (append + read + persistence).

use fence_runtime::{FenceRuntime, PoolConfig};
use fence_test_harness::TempPool;

#[test]
fn single_record_roundtrip() {
    let pool = TempPool::new();
    let rt = pool.runtime();

    let idx = rt.append(1, b"hello world").unwrap();
    assert_eq!(idx, 1);

    let rec = rt.read(idx).unwrap().unwrap();
    assert_eq!(rec.term, 1);
    assert_eq!(rec.index, 1);
    assert_eq!(rec.data, b"hello world");
    assert_eq!(rec.writer_id, 0);
}

#[test]
fn many_records_roundtrip() {
    let pool = TempPool::builder().capacity(256).payload_size(64).build();
    let rt = pool.runtime();

    for i in 0..256u64 {
        let data = format!("record-{i}");
        let idx = rt.append(1, data.as_bytes()).unwrap();
        assert_eq!(idx, i + 1);
    }

    for i in 0..256u64 {
        let rec = rt.read(i + 1).unwrap().unwrap();
        assert_eq!(rec.data, format!("record-{i}").as_bytes());
    }
}

#[test]
fn persistence_across_reopen() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Create and write.
    {
        let config = PoolConfig {
            path: path.clone(),
            capacity: 64,
            payload_size: 128,
            max_hosts: 2,
            host_id: 0,
            create: true,
        };
        let rt = FenceRuntime::open(config).unwrap();
        rt.append(1, b"persistent data").unwrap();
        rt.append(2, b"second record").unwrap();
    }

    // Reopen and verify.
    {
        let config = PoolConfig {
            path: path.clone(),
            capacity: 64,
            payload_size: 128,
            max_hosts: 2,
            host_id: 1,
            create: false,
        };
        let rt = FenceRuntime::open(config).unwrap();
        assert_eq!(rt.committed_tail(), 2);

        let r1 = rt.read(1).unwrap().unwrap();
        assert_eq!(r1.data, b"persistent data");
        assert_eq!(r1.term, 1);

        let r2 = rt.read(2).unwrap().unwrap();
        assert_eq!(r2.data, b"second record");
        assert_eq!(r2.term, 2);
    }
}

#[test]
fn empty_payload_roundtrip() {
    let pool = TempPool::new();
    let rt = pool.runtime();

    let idx = rt.append(5, b"").unwrap();
    let rec = rt.read(idx).unwrap().unwrap();
    assert_eq!(rec.data, b"");
    assert_eq!(rec.term, 5);
}

#[test]
fn max_payload_roundtrip() {
    let pool = TempPool::builder().payload_size(256).build();
    let rt = pool.runtime();

    let data = vec![0xAB; 256];
    let idx = rt.append(1, &data).unwrap();
    let rec = rt.read(idx).unwrap().unwrap();
    assert_eq!(rec.data, data);
}

#[test]
fn read_range_roundtrip() {
    let pool = TempPool::new();
    let rt = pool.runtime();

    for i in 0..10 {
        rt.append(1, format!("msg-{i}").as_bytes()).unwrap();
    }

    let records = rt.read_range(3, 8).unwrap();
    assert_eq!(records.len(), 5);
    assert_eq!(records[0].data, b"msg-2");
    assert_eq!(records[4].data, b"msg-6");
}
