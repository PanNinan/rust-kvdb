//! Integration tests — end-to-end verification of the KV engine.

use kv_engine::{DB, Engine, Options, WriteBatch};
use tempfile::tempdir;

#[test]
fn basic_put_get_delete() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    db.put(b"name", b"rust").unwrap();
    assert_eq!(db.get(b"name").unwrap(), Some(b"rust".to_vec()));

    db.delete(b"name").unwrap();
    assert_eq!(db.get(b"name").unwrap(), None);
}

#[test]
fn reopen_recovers_from_wal() {
    let dir = tempdir().unwrap();

    {
        let db = DB::open(dir.path()).unwrap();
        for i in 0..1000 {
            let key = format!("key_{:04}", i);
            let val = format!("val_{:04}", i);
            db.put(key.as_bytes(), val.as_bytes()).unwrap();
        }
        db.close().unwrap();
    }

    {
        let db = DB::open(dir.path()).unwrap();
        assert_eq!(
            db.get(b"key_0500").unwrap(),
            Some(b"val_0500".to_vec())
        );
        assert_eq!(db.get(b"nonexistent").unwrap(), None);
    }
}

#[test]
fn scan_returns_sorted_range() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    for i in 0..100 {
        db.put(format!("k{:03}", i).as_bytes(), b"v").unwrap();
    }

    let results = db.scan(b"k010", b"k020").unwrap();
    assert_eq!(results.len(), 10);
    for (i, (key, _)) in results.iter().enumerate() {
        assert_eq!(key, format!("k{:03}", i + 10).as_bytes());
    }
}

#[test]
fn writebatch_atomicity() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    let mut batch = WriteBatch::new();
    batch.put(b"a".to_vec(), b"1".to_vec());
    batch.put(b"b".to_vec(), b"2".to_vec());
    batch.put(b"c".to_vec(), b"3".to_vec());
    db.write_batch(&batch).unwrap();

    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
    assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
}

#[test]
fn snapshot_read_consistency() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    db.put(b"a", b"1").unwrap();
    db.put(b"b", b"2").unwrap();
    let snap = db.snapshot();

    db.put(b"c", b"3").unwrap();

    // Snapshot sees a and b but not c.
    assert_eq!(db.get_at(b"a", &snap).unwrap(), Some(b"1".to_vec()));
    assert_eq!(db.get_at(b"b", &snap).unwrap(), Some(b"2".to_vec()));
    assert_eq!(db.get_at(b"c", &snap).unwrap(), None);

    // Current sees all.
    assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
}

#[test]
fn overwrite_and_compaction_preserves_latest() {
    let dir = tempdir().unwrap();
    let opts = Options {
        memtable_size: 256, // tiny, fast flush
        ..Default::default()
    };
    let db = DB::open_with_options(dir.path(), opts).unwrap();

    for i in 0..1000 {
        db.put(b"key", format!("v{}", i).as_bytes()).unwrap();
    }

    assert_eq!(db.get(b"key").unwrap(), Some(b"v999".to_vec()));
}

#[test]
fn concurrent_writes_never_lose_data() {
    use std::sync::Arc;
    use std::thread;

    let dir = tempdir().unwrap();
    let db = Arc::new(DB::open(dir.path()).unwrap());

    let mut handles = vec![];
    for t in 0..4 {
        let db = db.clone();
        handles.push(thread::spawn(move || {
            for i in 0..250 {
                let key = format!("t{}_k{:04}", t, i);
                db.put(key.as_bytes(), b"v").unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let mut count = 0;
    for t in 0..4 {
        for i in 0..250 {
            let key = format!("t{}_k{:04}", t, i);
            assert!(db.get(key.as_bytes()).unwrap().is_some());
            count += 1;
        }
    }
    assert_eq!(count, 1000);
}
