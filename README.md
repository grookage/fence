# Fence

Crash-consistent shared-memory log for CXL memory pools.

## What is this

Multiple physical servers sharing memory over CXL have a nasty failure mode: a host writes data that sits in its CPU cache, updates the shared tail pointer, then crashes. Other hosts follow the pointer and read garbage — the data never left the dead host's cache.

Fence is a write-ahead log that prevents this. Every write flushes payload to the CXL media before advancing any pointer. Recovery after a crash is deterministic and idempotent — any surviving host can fix the log without coordination.

The log uses lock-free slot reservation (`fetch_add`) and a CAS-sweep to advance the committed tail through contiguous completed writes. No cross-host locks. Readers never see in-flight data.

The deeper bet: with CXL, log ordering in shared memory replaces consensus. No Raft, no Paxos, no quorum writes. The memory fabric is the replication layer.

## What's built

The core engine (Phases 1-3 from the plan):

- `fence-core` — MemoryBackend trait, layout, CRC32C, `clwb`/`sfence` primitives
- `fence-runtime` — append, read, trim, recover, per-host metrics
- `fence-alloc` — region allocator with crash-consistent state machine
- `fence-config` — TOML config, CLI/env/default resolution
- `fence-test-harness` — TempPool, crash simulation helpers
- `fenced` — CLI binary (init, status, recover, validate)

~100 tests pass covering concurrency (4-8 threads), crash recovery, compaction, and checksum validation. Builds on x86-64 (real persistence) and ARM (no-op stubs for dev).

All memory access goes through the `MemoryBackend` trait, so the same code runs on a local file (laptop), a DAX device (single Linux box), or a CXL pool (multi-server). Only the path changes.

## What's not built

Engine gaps:
- Segmented log (pool just errors when full today)
- Circular overflow for streaming
- Consumer watermarks
- Epoch increment on recovery
- Real fork+SIGKILL crash tests (current ones are cooperative)

Offering crates (none started):
- `fence-index` — shared-memory hash/btree/inverted indexes
- `fence-streaming` — topics, partitions, consumer groups
- `fence-kv` — key-value with hash index
- `fence-coord` — locks, leases, leader election
- `fence-tsdb` — time series

Longer term: formal verification (TLA+/Verus), TSC latency measurement, shared-memory indexes (currently process-local, rebuilt on open).

## Building

```
cargo build --workspace
cargo test --workspace
```

Rust 1.75+ required (`AtomicU64::from_ptr`).

## License

Apache 2.0, see LICENSE.
