# Fence

A crash-consistent, shared-memory log engine for CXL memory pools.

## Why This Exists

Every existing distributed system — Kafka, Redis, etcd, CockroachDB — was built assuming network is the interconnect between machines. Their entire architecture (replication protocols, serialization formats, consensus algorithms) exists because data must travel over TCP or RDMA between hosts.

CXL eliminates that assumption. With CXL 2.0+, multiple physical servers access the same physical memory pool over PCIe — no network stack, no serialization, no NIC. Data path is CPU → PCIe bus → CXL memory controller. Latency: ~200-400 nanoseconds (vs 10,000-100,000 ns for network).

Fence is built for this world where memory is the interconnect. The log ordering in shared memory IS the consensus — no Raft voting, no Paxos round-trips, no quorum writes. That's not an optimization over existing systems; it's a different category.

## The Problem

In a CXL shared-memory environment, multiple compute hosts connect via PCIe fabric to a central memory pool. The fundamental failure mode is invisible and catastrophic:

1. Host A writes an entry to the shared log.
2. The data sits in Host A's CPU cache — it has not yet traversed the PCIe bus to the CXL memory controller.
3. Host A updates the global tail pointer (visible to all hosts).
4. Host A crashes.
5. Host B reads the updated tail pointer, follows it, and reads garbage. The payload was vaporized with Host A's cache.

This is not a theoretical concern. Any system that exposes shared mutable state across hosts over CXL must solve this, or it silently corrupts data on every hardware fault.

## The Solution

Fence enforces three invariants that make shared-memory writes crash-consistent:

**Payload-Before-Pointer Persistence** — Data is flushed to the CXL media before any pointer advances. Readers never see a pointer without valid data behind it.

**Single-Writer, Multi-Reader Hazard Isolation** — Writers reserve slots via lock-free `fetch_add`. No cross-host locks. Each writer owns its slot exclusively after reservation.

**Deterministic Idempotent Recovery** — After a crash, any surviving host can scan the log and deterministically mark incomplete writes as abandoned without data loss or reordering.

## Architecture

The design separates the write path (WAL for durability + ordering + crash recovery) from the read path (indexed storage materialized from the WAL). The WAL handles the hard problem — safe concurrent writes with crash consistency. Offerings build indexed read structures on top.

```
                 ┌─────────────────────────────────────────────┐
                 │           Offering Crates                    │
                 │  fence-streaming  fence-kv  fence-coord ...  │
                 └──────────────────────┬──────────────────────┘
                                        │
          ┌─────────────────────────────┼─────────────────────┐
          │                             │                     │
          ▼                             ▼                     ▼
   fence-runtime (WAL)          fence-index             fence-alloc
                                (hash/btree)         (region allocator)
          │                             │                     │
          └─────────────────────────────┼─────────────────────┘
                                        │
                                        ▼
                                   fence-core
                        (MemoryBackend trait, layout,
                         CRC32C, persistence primitives)
```

All memory access goes through the `MemoryBackend` trait. The engine never touches raw pointers directly. This makes the system backend-agnostic — the same code runs on a file-backed mmap (your laptop), a Linux DAX device (single server), or a CXL-attached pool (multi-server production). Only the device path changes.

**Implemented crates:**

```
fence-core           MemoryBackend trait, layout constants, CRC32C, persistence primitives
fence-runtime        The WAL engine: append, read, trim, recover, metrics
fence-alloc          Shared-memory region allocator with crash-consistent state transitions
fence-config         TOML configuration parsing and validation
fence-test-harness   TempPool builder, crash simulation, test barriers
fenced               CLI binary (init, status, recover, validate)
```

The runtime is payload-agnostic. It appends bytes, checksums them, flushes them, and commits them. Higher-level semantics (topics, keys, locks) belong to offering crates that build on top.

## Write Protocol

```
1. RESERVE    — slot = reserve_tail.fetch_add(1)
2. MARK       — record.state = WRITING, flush
3. WRITE      — payload + CRC32C checksum
4. FLUSH      — clwb every cacheline, sfence
5. COMMIT     — record.state = COMMITTED, flush
6. ADVANCE    — CAS-sweep committed_tail forward through contiguous COMMITTED slots
```

Readers only see records behind `committed_tail`. They never observe in-flight writes.

## What Works

- Lock-free multi-writer append with CAS-sweep commit
- Read with checksum validation and bounds checking
- Crash recovery: scans in-flight slots, marks incomplete writes ABANDONED, advances committed_tail
- Trim/compaction (flat single-segment model)
- Per-host metrics in shared memory (appends, bytes written, reads, checksum failures, recovery runs)
- `MmapBackend` using `AtomicU64::from_ptr` / `AtomicU8::from_ptr` (Rust 1.75+)
- Region allocator with crash-consistent state machine (FREE → ALLOCATING → ACTIVE → FREEING → FREE)
- TOML-based config with CLI/env/default path resolution
- CLI binary with init/status/recover/validate subcommands
- Full test suite: ~100+ unit and integration tests covering roundtrip, concurrency (4-8 threads), crash simulation, compaction, and recovery idempotency
- Builds on both x86-64 (production persistence via `clwb`/`sfence`) and ARM (development with no-op stubs)

## What Remains

**Near-term (engine hardening):**

- Segmented log overflow — currently the pool returns an error when full; segment rotation is not yet implemented
- Circular overflow strategy for streaming workloads
- Epoch increment during recovery
- Consumer watermarks for safe segment reclamation
- Auto-recovery on open (config flag is parsed but not wired)
- Gap handling after recovery (committed_tail stalls at ABANDONED gaps without manual trim)
- Fork-based SIGKILL crash tests (current crash tests use cooperative simulation)
- Property-based / fuzz testing

**Offering crates (not started):**

- `fence-index` — shared-memory hash maps, B-trees, inverted indexes
- `fence-streaming` — topics, partitions, consumer groups (Kafka-like, no network)
- `fence-kv` — GET/SET/DEL/CAS/TTL with hash index (Redis-like, no network)
- `fence-coord` — distributed locks, leases, leader election (etcd/ZK-like, hardware linearizability)
- `fence-tsdb` — time-series database over the log

**Long-term:**

- TSC-based latency measurement
- Shared-memory indexes (currently process-local, rebuilt from log on open)
- Multi-segment compaction
- Formal verification (TLA+ / Verus) of the append protocol
- `fence-stats` CLI for live dashboard over mmap

## Building

```
cargo build --workspace
cargo test --workspace
```

Requires Rust 1.75+ (for `AtomicU64::from_ptr` stabilization).

## Platform Notes

The software is identical across all deployment tiers:

- **Tier 1 — File-backed mmap (dev laptop)**: No special hardware. Multiple processes share the same file mapping. Protocol logic is fully exercised. Only thing not tested: actual PCIe latency and ordering.
- **Tier 2 — DAX device (single Linux server)**: `/dev/dax0.0` via `ndctl`. Real bypass-page-cache semantics. Same code, different path.
- **Tier 3 — CXL memory pool (multi-server production)**: Multiple physical hosts, CXL switch, shared memory expander. Same code, same trait, same device path interface.

On x86-64, `clwb` + `sfence` provide persistence ordering when the ADR domain covers the CXL controller (Intel Sapphire Rapids and later). On ARM (macOS dev), persistence primitives are no-ops — correctness of the protocol is still testable.

## License

MIT
