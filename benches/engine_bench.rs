//! Basic benchmark for the KV engine.
//!
//! Run with: cargo bench --bench engine_bench

use std::time::Instant;
use tempfile::tempdir;

fn main() {
    println!("=== KV Engine Benchmark ===\n");

    bench_sequential_write();
    bench_random_read();
    bench_write_batch();
}

fn bench_sequential_write() {
    let dir = tempdir().unwrap();
    let db = kv_engine::DB::open(dir.path()).unwrap();
    let n = 10_000;

    let start = Instant::now();
    for i in 0..n {
        let key = format!("key_{:08}", i);
        let val = format!("val_{:08}", i);
        db.put(key.as_bytes(), val.as_bytes()).unwrap();
    }
    let elapsed = start.elapsed();

    println!(
        "Sequential write: {} ops in {:?} ({:.0} ops/sec)",
        n,
        elapsed,
        n as f64 / elapsed.as_secs_f64()
    );
}

fn bench_random_read() {
    let dir = tempdir().unwrap();
    let db = kv_engine::DB::open(dir.path()).unwrap();
    let n = 10_000;

    // Pre-populate.
    for i in 0..n {
        let key = format!("key_{:08}", i);
        db.put(key.as_bytes(), b"value").unwrap();
    }

    let start = Instant::now();
    for i in 0..n {
        let key = format!("key_{:08}", i);
        let _ = db.get(key.as_bytes()).unwrap();
    }
    let elapsed = start.elapsed();

    println!(
        "Random read:      {} ops in {:?} ({:.0} ops/sec)",
        n,
        elapsed,
        n as f64 / elapsed.as_secs_f64()
    );
}

fn bench_write_batch() {
    let dir = tempdir().unwrap();
    let db = kv_engine::DB::open(dir.path()).unwrap();
    let n = 10_000;
    let batch_size = 100;

    let start = Instant::now();
    for batch_idx in 0..(n / batch_size) {
        let mut batch = kv_engine::WriteBatch::new();
        for i in 0..batch_size {
            let key = format!("batch_{:04}_key_{:04}", batch_idx, i);
            batch.put(key.into_bytes(), b"val".to_vec());
        }
        db.write_batch(&batch).unwrap();
    }
    let elapsed = start.elapsed();

    println!(
        "WriteBatch({}):  {} ops in {:?} ({:.0} ops/sec)",
        batch_size,
        n,
        elapsed,
        n as f64 / elapsed.as_secs_f64()
    );
}
