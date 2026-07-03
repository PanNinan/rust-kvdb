//! Property-based tests using `proptest`.

use kv_engine::DB;
use proptest::prelude::*;
use tempfile::tempdir;

/// Generate random key-value operation sequences.
fn kv_ops() -> impl Strategy<Value = Vec<(Vec<u8>, Vec<u8>)>> {
    prop::collection::vec(
        (
            prop::collection::vec(any::<u8>(), 1..16),
            prop::collection::vec(any::<u8>(), 0..64),
        ),
        1..50,
    )
}

proptest! {
    #[test]
    fn memtable_never_loses_writes(ops in kv_ops()) {
        let dir = tempdir().unwrap();
        let db = DB::open(dir.path()).unwrap();

        let mut expected = std::collections::BTreeMap::new();
        for (k, v) in &ops {
            db.put(k, v).unwrap();
            expected.insert(k.clone(), v.clone());
        }

        for (k, v) in &expected {
            assert_eq!(db.get(k).unwrap(), Some(v.clone()));
        }
    }

    #[test]
    fn delete_removes_key(ops in kv_ops()) {
        let dir = tempdir().unwrap();
        let db = DB::open(dir.path()).unwrap();

        for (k, v) in &ops {
            db.put(k, v).unwrap();
        }
        for (k, _) in &ops {
            db.delete(k).unwrap();
            assert_eq!(db.get(k).unwrap(), None);
        }
    }

    #[test]
    fn reopen_preserves_last_write(ops in kv_ops()) {
        let dir = tempdir().unwrap();

        let mut expected = std::collections::BTreeMap::new();
        {
            let db = DB::open(dir.path()).unwrap();
            for (k, v) in &ops {
                db.put(k, v).unwrap();
                expected.insert(k.clone(), v.clone());
            }
            db.close().unwrap();
        }

        {
            let db = DB::open(dir.path()).unwrap();
            for (k, v) in &expected {
                assert_eq!(db.get(k).unwrap(), Some(v.clone()));
            }
        }
    }
}
