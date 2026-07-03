# ⚡ rust-kvdb

A local key-value storage engine written in Rust, based on the **LSM-Tree** architecture. Designed for learning storage engine internals while maintaining production-grade code quality.

[![Rust](https://img.shields.io/badge/Rust-2021-edition-orange.svg)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-84%20passed-brightgreen.svg)]()
[![License](https://img.shields.io/badge/license-MIT-blue.svg)]()

---

## Features

| Category | Feature |
|---|---|
| **Core** | Put / Get / Delete / Scan / Prefix Scan |
| **Durability** | Write-Ahead Log (WAL) with CRC32 integrity checking |
| **Recovery** | Automatic crash recovery via WAL replay on startup |
| **Memory** | Dual-buffer MemTable (active + immutable) with configurable freeze threshold |
| **Storage** | SSTable with prefix compression, bloom filter, and block index |
| **Compaction** | Leveled compaction (L0 → L1 → L2+), automatic tombstone and TTL cleanup |
| **Advanced** | WriteBatch (atomic), Snapshot (MVCC), TTL (time-to-live), Repair |
| **Performance** | LRU Block Cache, Bloom Filter (skip index on miss), prefix-compressed blocks |
| **Concurrency** | Thread-safe `DB` wrapper (`Arc<Mutex<Engine>>`) |
| **Observability** | Atomic metrics (writes/reads/deletes/compactions/flushes), `tracing` logging |
| **HTTP** | Built-in management dashboard with real-time metrics and KV operations |
| **Config** | `Options` struct for memtable size, block size, compaction threshold, etc. |

---

## Quick Start

### As a Library

```rust
use kv_engine::DB;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = DB::open("./mydb")?;

    // Write
    db.put(b"name", b"rust-kvdb")?;

    // Read
    if let Some(value) = db.get(b"name")? {
        println!("name = {}", String::from_utf8_lossy(&value));
    }

    // Delete
    db.delete(b"name")?;

    // Scan range
    let results = db.scan(b"a", b"z")?;
    for (key, value) in &results {
        println!("{} = {}", String::from_utf8_lossy(key), String::from_utf8_lossy(value));
    }

    db.close()?;
    Ok(())
}
```

### HTTP Dashboard

```bash
cargo run --bin kv-server
# Open http://127.0.0.1:8080 in your browser
```

![Dashboard features](https://img.shields.io/badge/Dashboard-dark_theme-1e293b) Real-time metrics · KV operations · Auto-refresh

### CLI Tool

```bash
# Basic operations
cargo run --bin kv-cli -- ./mydb put name rust
cargo run --bin kv-cli -- ./mydb get name
cargo run --bin kv-cli -- ./mydb delete name

# Range scan
cargo run --bin kv-cli -- ./mydb scan a z

# Atomic batch
cargo run --bin kv-cli -- ./mydb batch put k1 v1 put k2 v2 delete k3

# View metrics
cargo run --bin kv-cli -- ./mydb stats
```

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                   Client API (DB)                    │
│         put / get / delete / scan / batch            │
├─────────────────────────────────────────────────────┤
│                     Engine                           │
│  ┌──────────┐  ┌──────────┐  ┌───────────────────┐  │
│  │ MemTable │  │   WAL    │  │   SSTable Layer   │  │
│  │ (active) │  │ (append) │  │ L0: [sst][sst]    │  │
│  │    ↓     │  │          │  │ L1: [sst][sst]    │  │
│  │(immutable)│  │          │  │ L2: [sst]...      │  │
│  └──────────┘  └──────────┘  └───────────────────┘  │
├─────────────────────────────────────────────────────┤
│  Bloom Filter │ Block Cache │ Manifest │ Compaction  │
└─────────────────────────────────────────────────────┘
```

### Data Flow

```
Write Path:   put(k,v) → WAL.append → MemTable.put → [freeze] → flush to SSTable
Read Path:    get(k)   → MemTable(active) → MemTable(immutable) → SSTable(L0→L1→...)
Delete:       delete(k) → put(k, TOMBSTONE) → compaction removes physically
```

### SSTable File Format

```
┌───────────────┐
│  Data Block 0 │  ← prefix-compressed entries
├───────────────┤
│  Data Block 1 │
├───────────────┤
│      ...      │
├───────────────┤
│  Index Block  │  ← (block_offset, last_key) per block
├───────────────┤
│ Bloom Filter  │  ← FNV-1a double-hashing, ~1% false positive
├───────────────┤
│   Footer 8B   │  ← index_offset + bloom_offset
└───────────────┘
```

---

## Configuration

```rust
use kv_engine::{DB, Options};

let opts = Options {
    memtable_size: 4 * 1024 * 1024,   // 4 MiB — freeze threshold
    block_size: 4096,                  // 4 KiB — SSTable data block size
    l0_compaction_threshold: 4,        // compact L0 when ≥ 4 SSTables
    sync_wal: true,                    // fsync after every write
    max_levels: 7,                     // max LSM tree depth
    bloom_filter_bits_per_key: 10,     // ~1% false positive rate
    block_cache_size: 8 * 1024 * 1024, // 8 MiB LRU block cache
};

let db = DB::open_with_options("./mydb", opts)?;
```

---

## API Reference

### Core Operations

| Method | Signature | Description |
|---|---|---|
| `put` | `(&self, key: &[u8], value: &[u8]) -> Result<()>` | Write a key-value pair |
| `get` | `(&self, key: &[u8]) -> Result<Option<Vec<u8>>>` | Read a value by key |
| `delete` | `(&self, key: &[u8]) -> Result<()>` | Delete a key (tombstone) |
| `scan` | `(&self, start: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>` | Range scan `[start, end)` |
| `prefix_scan` | `(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>` | Scan by key prefix |

### Batch & Snapshot

| Method | Description |
|---|---|
| `write_batch(&self, batch: &WriteBatch)` | Atomic batch of put/delete operations |
| `snapshot(&self) -> Snapshot` | Create a point-in-time snapshot |
| `get_at(&self, key, snap)` | Read at a specific snapshot |

### TTL

| Method | Description |
|---|---|
| `put_with_ttl(key, value, Duration)` | Write with time-to-live expiry |

### Lifecycle

| Method | Description |
|---|---|
| `DB::open(path)` | Open or create a database |
| `DB::open_with_options(path, opts)` | Open with custom configuration |
| `DB::repair(path)` | Rebuild manifest from surviving SSTables |
| `db.close()` | Flush all data and save manifest |

### Metrics

```rust
let m = db.metrics();
println!("Writes: {}", m.writes);
println!("Reads:  {}", m.reads);
```

---

## HTTP API

| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/` | Web dashboard |
| `GET` | `/api/health` | Health check + version + uptime |
| `GET` | `/api/metrics` | JSON metrics |
| `GET` | `/api/get/:key` | Read a key |
| `POST` | `/api/put` | Write `{"key":"k","value":"v"}` |
| `POST` | `/api/delete/:key` | Delete a key |
| `POST` | `/api/compact` | Trigger compaction |

```bash
# Examples
curl http://localhost:8080/api/health
curl -X POST http://localhost:8080/api/put -d '{"key":"name","value":"rust"}'
curl http://localhost:8080/api/get/name
```

---

## Project Structure

```
src/
├── engine.rs              # Engine + DB + Options + Metrics (核心)
├── error.rs               # KvError enum (7 variants)
├── types.rs               # Key, Value, SequenceNumber
├── manifest.rs            # SSTable file manifest (binary format)
├── write_batch.rs         # WriteBatch + BatchOp
│
├── memtable/
│   ├── skiplist.rs        # Skip List (unsafe pointers, safe API)
│   └── memtable.rs        # MemTable (active + immutable dual buffer)
│
├── wal/
│   ├── record.rs          # WAL record codec (CRC32 + OpType)
│   └── writer.rs          # WALWriter + WALReader (crash recovery)
│
├── sstable/
│   ├── block.rs           # Data block (prefix compression)
│   ├── builder.rs         # SSTableBuilder (write path)
│   └── reader.rs          # SSTableReader + SSTableIterator (read path)
│
├── filter/
│   └── bloom.rs           # Bloom filter (FNV-1a double-hashing)
│
├── cache/
│   └── block_cache.rs     # LRU block cache (HashMap + VecDeque)
│
├── compaction/
│   ├── merge_iterator.rs  # Multi-way merge iterator (BinaryHeap)
│   └── leveled.rs         # (placeholder for standalone compaction)
│
├── http/
│   ├── mod.rs             # HTTP server entry
│   ├── handler.rs         # Axum route handlers
│   └── dashboard.rs       # HTML dashboard template
│
└── bin/
    ├── cli.rs             # kv-cli command-line tool
    └── server.rs          # kv-server HTTP management server
```

---

## Testing

```bash
# All unit tests (84 tests)
cargo test --lib

# Integration tests (7 tests)
cargo test --test integration

# Property-based tests
cargo test --test property

# Benchmarks
cargo bench

# Lint
cargo clippy -- -D warnings
```

---

## Development Phases

| Phase | Status | Description |
|---|---|---|
| **Phase 1** | ✅ Complete | MVP: Put/Get/Delete/Recovery via WAL |
| **Phase 2** | ✅ Complete | Bloom Filter, Block Cache, Leveled Compaction, SCAN |
| **Phase 3** | ✅ Complete | WriteBatch, Snapshot, TTL, Prefix Scan, Repair |
| **Phase 4** | ✅ Complete | Thread-safe DB, Metrics, Options, HTTP Dashboard, Benchmark |

---

## Performance

Benchmark results (10,000 operations):

| Operation | Throughput | Notes |
|---|---|---|
| Sequential Write | ~382 ops/sec | Limited by fsync per write |
| Random Read | ~34,886 ops/sec | In-memory hit (MemTable / cache) |
| WriteBatch(100) | ~18,291 ops/sec | Batched writes |

> Write throughput is fsync-bound. Disable `sync_wal` for ~10× higher writes (at crash-safety cost).

---

## Dependencies

| Crate | Purpose |
|---|---|
| `crc32fast` | WAL CRC32 integrity |
| `axum` | HTTP framework |
| `tokio` | Async runtime |
| `serde` / `serde_json` | JSON serialization |
| `tracing` | Structured logging |
| `tempfile` | Test isolation (dev) |
| `proptest` | Property-based testing (dev) |

---

## License

MIT

---

## Acknowledgments

- [LevelDB](https://github.com/google/leveldb) — SSTable format and compaction design
- [mini-lsm](https://github.com/skyzh/mini-lsm) — Rust LSM teaching project
- [Rust-rocksdb](https://github.com/rust-rocksdb/rust-rocksdb) — API design reference
