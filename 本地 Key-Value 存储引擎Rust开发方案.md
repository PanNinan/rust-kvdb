# 本地 Key-Value 存储引擎 — Rust 开发方案

## 一、技术栈选型

| 类别 | 选择 | 说明 |
|---|---|---|
| 语言 | Rust (edition 2021) | 零成本抽象 + 内存安全 |
| 异步运行时 | `tokio` | WAL fsync / Compaction 异步化，避免阻塞主线程 |
| 序列化 | `bincode` 或手写 | WAL/SSTable 内部编码，手写更快零依赖 |
| CRC 校验 | `crc32fast` | WAL 条目完整性校验 |
| 压缩（Phase 2+） | `lz4_flex` / `snap` | Data block 压缩 |
| 布隆过滤器 | 自行实现 | 教学价值高，核心代码不到 200 行 |
| LRU Cache | `lru` crate | Block Cache |
| 日志 | `tracing` | 结构化日志 |
| 测试 | `#[test]` + `tempfile` | 临时目录隔离测试 |

---

## 二、项目结构设计

```
kv-engine/
├── Cargo.toml
├── src/
│   ├── lib.rs                  # 模块导出
│   ├── engine.rs               # Storage Engine 顶层协调（DB struct）
│   │
│   ├── api/                    # Client API Layer
│   │   ├── mod.rs
│   │   └── db.rs               # DB: put / get / delete / scan
│   │
│   ├── memtable/               # MemTable 模块
│   │   ├── mod.rs
│   │   ├── skiplist.rs          # 跳表实现
│   │   └── memtable.rs          # MemTable 封装（阈值检查、冻结）
│   │
│   ├── wal/                    # Write-Ahead Log
│   │   ├── mod.rs
│   │   ├── record.rs            # WAL 记录编解码
│   │   └── writer.rs            # WAL Writer（append + fsync）
│   │
│   ├── sstable/                # SSTable
│   │   ├── mod.rs
│   │   ├── block.rs             # Data Block / Index Block 读写
│   │   ├── builder.rs           # SSTable 构建器（写入磁盘）
│   │   ├── reader.rs            # SSTable 读取器
│   │   └── iterator.rs          # SSTable 迭代器
│   │
│   ├── compaction/             # Compaction
│   │   ├── mod.rs
│   │   ├── leveled.rs           # Leveled Compaction 策略
│   │   └── merge_iterator.rs    # 多路归并迭代器
│   │
│   ├── filter/                 # 布隆过滤器
│   │   ├── mod.rs
│   │   └── bloom.rs
│   │
│   ├── cache/                  # Block Cache
│   │   ├── mod.rs
│   │   └── block_cache.rs
│   │
│   ├── manifest.rs             # Manifest 文件管理（活跃 SSTable 列表）
│   ├── version.rs              # 版本管理（LSM 各层 SSTable 元数据）
│   ├── types.rs                # 公共类型定义
│   └── error.rs                # 错误类型
│
├── benches/                    # Benchmark
│   └── engine_bench.rs
└── tests/                      # 集成测试
    └── integration.rs
```

---

## 三、分阶段开发计划

### Phase 1 — MVP 核心（预计 2-3 周）

**目标：能跑通 PUT → GET → DELETE → 重启恢复**

#### Step 1.1：公共类型 & 错误处理

- `types.rs`：定义 `Key(Vec<u8>)`, `Value(Vec<u8>)`, `SequenceNumber(u64)`
- `error.rs`：`enum KvError { Io, Corruption, KeyNotFound, ... }`
- 实现 `std::fmt::Display` + `std::error::Error`

#### Step 1.2：Skip List（跳表）

- 文件：`skiplist.rs`
- 核心结构：

```rust
struct Node {
    key: Vec<u8>,
    value: Vec<u8>,
    next: Vec<Option<*mut Node>>,  // next[0]..next[level]
}

pub struct SkipList {
    head: *mut Node,
    max_level: usize,
    size: usize,        // 当前内存占用
}
```

- 方法：`insert(key, value)`, `get(key) -> Option<&[u8]>`, `scan(start, end) -> Iterator`
- `unsafe` 使用范围：跳表内部指针操作，对外暴露 safe API

#### Step 1.3：WAL（Write-Ahead Log）

- 文件：`wal/writer.rs`, `wal/record.rs`
- 记录格式：`[len: u32][crc32: u32][key_len: u32][key][value_len: u32][value]`
- `Writer::append(key, value)` → 写入 + `file.sync_data()`（可配置是否每次 fsync）
- 启动时 `Reader` 顺序扫描 WAL 文件恢复 MemTable

#### Step 1.4：MemTable 封装

- 文件：`memtable/memtable.rs`
- 双缓冲：`active: SkipList` + `immutable: Option<SkipList>`
- `put()` 写 active，超过阈值（默认 4MB）时 freeze 为 immutable 并创建新的 active
- `get()` 先查 active 再查 immutable

#### Step 1.5：SSTable 写入（Builder）

- 文件：`sstable/builder.rs`, `sstable/block.rs`
- `Block`：`Vec<(key, value)>`，按 key 排序，prefix compression
- `SSTableBuilder`：

```rust
pub struct SSTableBuilder {
    file: File,
    data_blocks: Vec<Block>,
    index_entries: Vec<(Vec<u8>, u32)>,  // (last_key, block_offset)
    block_size: usize,                    // 默认 4KB
}
```

- flush 时：immutable MemTable → 遍历 entries → `builder.add(key, value)` → `builder.finish()`

#### Step 1.6：SSTable 读取（Reader）

- 文件：`sstable/reader.rs`
- 读取 Footer → Index Block → 二分定位 Data Block → 二分/顺序查找 key
- `Reader::get(key) -> Result<Option<Vec<u8>>>`

#### Step 1.7：Manifest 文件

- 文件：`manifest.rs`
- JSON 或 bincode 格式，记录 `[level_0: [sst_001.sst, sst_002.sst], level_1: [...]]`
- 启动时加载，每次 SSTable 新增/删除时更新

#### Step 1.8：Storage Engine 组装

- 文件：`engine.rs`, `api/db.rs`
- `DB::open(path)` → 加载 Manifest + 恢复 WAL + 创建 MemTable
- `DB::put(key, value)` → WAL.append → MemTable.put → 满了触发 flush
- `DB::get(key)` → MemTable → Immutable → SSTable (L0 → L1 → ...)
- `DB::delete(key)` → 写 tombstone `value = None`
- `DB::close()` → flush active MemTable → sync → 更新 Manifest

**Phase 1 交付物：**

- [x] 能通过 `cargo test` 的单元测试
- [x] 集成测试：写入 → 关闭 → 重启 → 读取验证
- [x] 基本 CLI 工具：`kv-cli put/get/delete`

---

### Phase 2 — 可靠性与性能（预计 3-4 周）

#### Step 2.1：Bloom Filter（布隆过滤器）

```rust
pub struct BloomFilter {
    bits: Vec<u64>,         // 位数组
    num_hashes: usize,      // 哈希函数数量
    num_bits: usize,
}

impl BloomFilter {
    pub fn build(keys: &[&[u8]]) -> Self;
    pub fn may_contain(&self, key: &[u8]) -> bool;
}
```

- SSTable Builder 在 finish 时生成 Bloom Filter，追加到 SSTable 尾部
- Reader 读取时缓存 Bloom Filter，`get()` 先过滤

#### Step 2.2：Block Cache（LRU 缓存）

```rust
use lru::LruCache;

pub struct BlockCache {
    cache: Mutex<LruCache<(FileId, u32), Arc<Block>>>,  // (sst_id, block_offset) → Block
}
```

- 默认 8MB，可配置
- Reader 读 block 前先查 cache，miss 再读磁盘

#### Step 2.3：Leveled Compaction

- 文件：`compaction/leveled.rs`
- L0 到 L1：L0 可能有 key 重叠，需要和 L1 合并
- L1 到 L2+：SSTable 无重叠，只需 pick 一个 SSTable 和下层合并
- 每层容量 = 上层 × 10（默认）
- 后台线程定时检查各层大小，触发 compaction
- 合并策略：`pick_compaction()` → `merge_sstables()` → 写新 SSTable → 更新 Manifest → 删除旧文件

#### Step 2.4：多路归并迭代器

```rust
pub struct MergeIterator {
    heap: BinaryHeap<HeapEntry>,  // 按 key 排序
    // 每个 HeapEntry 包装一个 SSTable/Block 迭代器
}
```

- 用于 Compaction 和 SCAN

#### Step 2.5：SCAN 范围扫描

```rust
impl DB {
    pub fn scan(&self, start: &[u8], end: &[u8]) -> ScanIterator;
}
```

- 合并 MemTable + Immutable + 各层 SSTable 的迭代器
- tombstone 在迭代时过滤（compaction 中物理删除）

#### Step 2.6：压缩（可选 Data Block 压缩）

```rust
pub enum CompressionType {
    None,
    Lz4,
    Snappy,
}
```

- Block 写入时可选压缩，读取时自动解压
- Index Block 不压缩（需要直接访问）

**Phase 2 交付物：**

- [x] 读写性能接近简易 LevelDB
- [x] Bloom Filter 大幅降低无效磁盘 IO
- [x] Compaction 自动触发，空间可回收
- [x] SCAN 命令可用

---

### Phase 3 — 进阶能力（预计 2-3 周）

#### Step 3.1：WriteBatch（原子批处理）

```rust
pub struct WriteBatch {
    ops: Vec<BatchOp>,
}

impl WriteBatch {
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>);
    pub fn delete(&mut self, key: Vec<u8>);
}

impl DB {
    pub fn write_batch(&self, batch: WriteBatch) -> Result<()>;
}
```

- 整个 batch 写入一条 WAL 记录
- MemTable 要么全应用，要么全不应用（通过 sequence number + 批量标记实现）

#### Step 3.2：Snapshot（快照读）

```rust
pub struct Snapshot {
    sequence: u64,
}

impl DB {
    pub fn snapshot(&self) -> Snapshot;
    pub fn get_at(&self, key: &[u8], snap: &Snapshot) -> Result<Option<Vec<u8>>>;
}
```

- 每条记录带 sequence number
- Snapshot 读取时只看 `seq <= snap.sequence` 的版本

#### Step 3.3：Prefix Iterator

```rust
impl DB {
    pub fn prefix_scan(&self, prefix: &[u8]) -> ScanIterator;
}
```

- SSTable 内部使用前缀压缩，SSTable 元数据记录前缀边界，加速前缀查询

#### Step 3.4：TTL 支持

```rust
impl DB {
    pub fn put_with_ttl(&self, key: Vec<u8>, value: Vec<u8>, ttl: Duration) -> Result<()>;
}
```

- Value 中嵌入过期时间戳
- 读取时检查，过期返回 None
- Compaction 时物理删除过期 key

#### Step 3.5：Repair 工具

```rust
impl DB {
    pub fn repair(path: &Path) -> Result<()>;
}
```

- 扫描所有 SSTable，验证 checksum
- 重建 Manifest
- 清理损坏的 WAL 段

---

### Phase 4 — 工程化（预计 2-3 周）

#### Step 4.1：线程安全

```rust
pub struct DB {
    inner: Arc<RwLock<DBInner>>,  // 读写锁保护
    // 或者更细粒度：
    memtable: RwLock<MemTable>,
    versions: Mutex<VersionSet>,
}
```

- `get()` 持读锁，`put()` 持写锁或细粒度锁
- Compaction 在后台线程运行，用 channel 通知主线程更新版本

#### Step 4.2：指标采集

```rust
pub struct Metrics {
    pub write_latency: Histogram,
    pub read_latency: Histogram,
    pub compaction_count: Counter,
    pub bloom_filter_hit_rate: Gauge,
    pub space_amplification: Gauge,
}
```

- 使用 `hdrhistogram` crate 计算延迟分布
- 通过 `tracing` 暴露或 HTTP 端点查询

#### Step 4.3：配置管理

```rust
pub struct Options {
    pub memtable_size: usize,              // 默认 4MB
    pub block_size: usize,                 // 默认 4KB
    pub max_levels: usize,                 // 默认 7
    pub compression: CompressionType,      // 默认 None
    pub bloom_filter_bits_per_key: usize,  // 默认 10
    pub block_cache_size: usize,           // 默认 8MB
    pub sync_wal: bool,                    // 默认 true
}
```

#### Step 4.4：优雅关闭

`DB::close()` 流程：

1. 停止接受新写入
2. flush active MemTable → WAL sync
3. 等待后台 Compaction 完成
4. 写入 shutdown marker 到 WAL
5. 更新 Manifest

#### Step 4.5：Benchmark

```rust
// benches/engine_bench.rs
#[bench]
fn bench_random_write_1k(b: &mut Bencher) { ... }

#[bench]
fn bench_random_read_1k(b: &mut Bencher) { ... }

#[bench]
fn bench_sequential_write_1k(b: &mut Bencher) { ... }
```

- 目标指标：随机写 QPS、随机读 QPS、P99 延迟、空间放大率

---

## 四、测试方案

### 4.1 测试策略总览

| 测试层级 | 工具 | 覆盖目标 | 执行频率 |
|---|---|---|---|
| 单元测试 | `#[test]` | 每个模块独立逻辑 | 每次提交 |
| 集成测试 | `tests/` + `tempfile` | 多模块协作、端到端流程 | 每次提交 |
| 属性测试 | `proptest` / `quickcheck` | 边界条件、随机输入下的正确性 | 每次提交 |
| 崩溃恢复测试 | 手动注入 + `tempfile` | WAL 恢复、异常断电模拟 | 每周 / CI |
| 并发测试 | `loom`（可选） + 多线程压力 | 数据竞争、死锁检测 | PR 合并前 |
| 压力/模糊测试 | `cargo-fuzz` / 手写压力脚本 | 长时间运行下的稳定性 | 定期 |
| 性能基准 | `criterion` | 读写延迟、吞吐量回归 | 版本发布前 |

### 4.2 单元测试

每个模块在同文件内编写 `#[cfg(test)] mod tests { ... }`，测试命名遵循 `模块_场景_期望结果` 模式。

#### 4.2.1 Skip List 测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skiplist_insert_and_get() {
        let list = SkipList::new(16);
        list.insert(b"key1", b"value1");
        assert_eq!(list.get(b"key1"), Some(b"value1".as_slice()));
    }

    #[test]
    fn skiplist_get_nonexistent_returns_none() {
        let list = SkipList::new(16);
        assert_eq!(list.get(b"missing"), None);
    }

    #[test]
    fn skiplist_update_existing_key() {
        let list = SkipList::new(16);
        list.insert(b"key1", b"v1");
        list.insert(b"key1", b"v2");
        assert_eq!(list.get(b"key1"), Some(b"v2".as_slice()));
    }

    #[test]
    fn skiplist_scan_range() {
        let list = SkipList::new(16);
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            list.insert(key.as_bytes(), format!("val_{}", i).as_bytes());
        }
        let results: Vec<_> = list.scan(b"key_0020", b"key_0030").collect();
        assert_eq!(results.len(), 10);
    }

    #[test]
    fn skiplist_ordering_is_sorted() {
        // 随机顺序插入，验证 scan 返回有序结果
        let list = SkipList::new(16);
        let keys: Vec<String> = (0..1000).map(|i| format!("key_{:06}", i)).collect();
        // 逆序插入
        for key in keys.iter().rev() {
            list.insert(key.as_bytes(), b"v");
        }
        let collected: Vec<_> = list.scan(b"", b"\xff").map(|(k, _)| k).collect();
        for w in collected.windows(2) {
            assert!(w[0] <= w[1], "keys not sorted");
        }
    }

    #[test]
    fn skiplist_size_tracking() {
        let list = SkipList::new(16);
        let before = list.size();
        list.insert(b"key", b"value");
        assert!(list.size() > before);
    }
}
```

#### 4.2.2 WAL 测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn wal_append_and_read_back() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let mut writer = WALWriter::open(&path).unwrap();
        writer.append(b"key1", b"value1").unwrap();
        writer.append(b"key2", b"value2").unwrap();
        writer.flush().unwrap();

        let entries: Vec<_> = WALReader::open(&path).unwrap().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], (b"key1".to_vec(), b"value1".to_vec()));
    }

    #[test]
    fn wal_crc_detects_corruption() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("corrupt.wal");

        let mut writer = WALWriter::open(&path).unwrap();
        writer.append(b"key", b"value").unwrap();
        writer.flush().unwrap();

        // 损坏文件中的某个字节
        let mut data = std::fs::read(&path).unwrap();
        data[8] ^= 0xff;
        std::fs::write(&path, &data).unwrap();

        let mut reader = WALReader::open(&path).unwrap();
        assert!(reader.next().is_err(), "should detect CRC mismatch");
    }

    #[test]
    fn wal_recovery_after_truncation() {
        // 模拟写入中途崩溃：截断文件末尾若干字节
        let dir = tempdir().unwrap();
        let path = dir.path().join("truncated.wal");

        let mut writer = WALWriter::open(&path).unwrap();
        writer.append(b"a", b"1").unwrap();
        writer.append(b"b", b"2").unwrap();
        writer.flush().unwrap();

        let len = std::fs::metadata(&path).unwrap().len();
        // 截断掉最后一条记录的一部分
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(len - 3).unwrap();

        let entries: Vec<_> = WALReader::open(&path)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        // 应恢复出第一条，第二条因不完整被跳过
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"a");
    }
}
```

#### 4.2.3 SSTable 测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sstable_build_and_read_single_block() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        builder.add(b"aaa", b"v1").unwrap();
        builder.add(b"bbb", b"v2").unwrap();
        builder.add(b"ccc", b"v3").unwrap();
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        assert_eq!(reader.get(b"bbb").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(reader.get(b"xxx").unwrap(), None);
    }

    #[test]
    fn sstable_multiple_data_blocks() {
        // block_size 设小，强制跨多个 block
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi_block.sst");

        let mut builder = SSTableBuilder::new(&path, 64).unwrap(); // 64 bytes/block
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let val = format!("value_{:04}", i);
            builder.add(key.as_bytes(), val.as_bytes()).unwrap();
        }
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        assert_eq!(
            reader.get(b"key_0050").unwrap(),
            Some(b"value_0050".to_vec())
        );
    }

    #[test]
    fn sstable_iterator_returns_sorted_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("iter.sst");

        let mut builder = SSTableBuilder::new(&path, 4096).unwrap();
        for i in (0..50).rev() {
            builder.add(format!("k{:03}", i).as_bytes(), b"v").unwrap();
        }
        builder.finish().unwrap();

        let reader = SSTableReader::open(&path).unwrap();
        let keys: Vec<_> = reader.iter().map(|(k, _)| k).collect();
        for w in keys.windows(2) {
            assert!(w[0] <= w[1]);
        }
    }
}
```

#### 4.2.4 Bloom Filter 测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_filter_no_false_negatives() {
        let keys: Vec<&[u8]> = (0..10000).map(|i| format!("key_{}", i).into_bytes()).collect();
        let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let filter = BloomFilter::build(&refs, 10);

        for key in &keys {
            assert!(filter.may_contain(key), "false negative for existing key");
        }
    }

    #[test]
    fn bloom_filter_false_positive_rate() {
        let keys: Vec<Vec<u8>> = (0..10000).map(|i| format!("key_{}", i).into_bytes()).collect();
        let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let filter = BloomFilter::build(&refs, 10);

        let mut false_positives = 0;
        let test_count = 100000;
        for i in 0..test_count {
            let probe = format!("probe_{}", i);
            if filter.may_contain(probe.as_bytes()) {
                false_positives += 1;
            }
        }
        let fp_rate = false_positives as f64 / test_count as f64;
        assert!(fp_rate < 0.01, "false positive rate too high: {}", fp_rate);
    }
}
```

#### 4.2.5 Block Cache 测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_and_eviction() {
        let cache = BlockCache::new(1024); // 1KB

        let block = Arc::new(Block::new(vec![0u8; 512]));
        cache.put(FileId(1), 0, block.clone());

        assert!(cache.get(FileId(1), 0).is_some());

        // 插入超过容量，触发淘汰
        let block2 = Arc::new(Block::new(vec![0u8; 512]));
        let block3 = Arc::new(Block::new(vec![0u8; 512]));
        cache.put(FileId(1), 1, block2);
        cache.put(FileId(1), 2, block3); // total > 1KB，最旧的被淘汰

        assert!(cache.get(FileId(1), 0).is_none());
    }
}
```

#### 4.2.6 Manifest 测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn manifest_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("MANIFEST");

        let mut manifest = Manifest::new();
        manifest.add_sst(0, SSTMeta { id: 1, min_key: b"a".to_vec(), max_key: b"z".to_vec() });
        manifest.add_sst(1, SSTMeta { id: 2, min_key: b"a".to_vec(), max_key: b"z".to_vec() });
        manifest.save(&path).unwrap();

        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.ssts_at_level(0).len(), 1);
        assert_eq!(loaded.ssts_at_level(1).len(), 1);
    }
}
```

### 4.3 集成测试

位于 `tests/integration.rs`，验证完整读写链路。

```rust
// tests/integration.rs
use kv_engine::DB;
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

    // 第一次打开：写入数据
    {
        let db = DB::open(dir.path()).unwrap();
        for i in 0..1000 {
            let key = format!("key_{:04}", i);
            let val = format!("val_{:04}", i);
            db.put(key.as_bytes(), val.as_bytes()).unwrap();
        }
        db.close().unwrap();
    }

    // 第二次打开：验证数据仍在
    {
        let db = DB::open(dir.path()).unwrap();
        assert_eq!(
            db.get(b"key_0500").unwrap(),
            Some(b"val_0500".to_vec())
        );
        assert_eq!(db.get(b"nonexistent").unwrap(), None);
        db.close().unwrap();
    }
}

#[test]
fn scan_returns_sorted_range() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    for i in 0..100 {
        db.put(format!("k{:03}", i).as_bytes(), b"v").unwrap();
    }

    let results: Vec<_> = db.scan(b"k010", b"k020").collect();
    assert_eq!(results.len(), 10);
    for (i, (key, _)) in results.iter().enumerate() {
        assert_eq!(key, format!("k{:03}", i + 10).as_bytes());
    }
}

#[test]
fn writebatch_atomicity() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    let mut batch = db.batch();
    batch.put(b"a", b"1");
    batch.put(b"b", b"2");
    batch.put(b"c", b"3");
    db.write_batch(batch).unwrap();

    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
    assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
}

#[test]
fn snapshot_read_consistency() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    db.put(b"key", b"v1").unwrap();
    let snap = db.snapshot().unwrap();

    db.put(b"key", b"v2").unwrap();

    // snapshot 看到 v1，当前读看到 v2
    assert_eq!(db.get_at(b"key", &snap).unwrap(), Some(b"v1".to_vec()));
    assert_eq!(db.get(b"key").unwrap(), Some(b"v2".to_vec()));
}

#[test]
fn put_with_ttl_expires() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    db.put_with_ttl(b"temp", b"data", Duration::from_secs(1)).unwrap();
    assert_eq!(db.get(b"temp").unwrap(), Some(b"data".to_vec()));

    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(db.get(b"temp").unwrap(), None);
}

#[test]
fn overwite_and_compaction_preserves_latest() {
    let dir = tempdir().unwrap();
    let db = DB::open_with_options(dir.path(), Options {
        memtable_size: 256,  // 极小，快速触发 flush
        ..Default::default()
    }).unwrap();

    // 同一个 key 反复写入，强制 compaction
    for i in 0..1000 {
        db.put(b"key", format!("v{}", i).as_bytes()).unwrap();
    }
    db.flush().unwrap();

    assert_eq!(db.get(b"key").unwrap(), Some(b"v999".to_vec()));
}
```

### 4.4 崩溃恢复测试

模拟异常断电，验证 WAL 和 Manifest 的可靠性。

```rust
// tests/crash_recovery.rs
use kv_engine::DB;
use tempfile::tempdir;

/// 模拟写入过程中进程被 kill，重启后数据应可恢复
#[test]
fn crash_during_write_recovers_committed_data() {
    let dir = tempdir().unwrap();

    let committed_count;
    {
        let db = DB::open(dir.path()).unwrap();
        for i in 0..500 {
            db.put(format!("k{:04}", i).as_bytes(), b"v").unwrap();
            // sync 每 10 条一次，模拟部分 fsync
            if i % 10 == 0 {
                db.sync_wal().unwrap();
            }
        }
        committed_count = 500;
        // 不调用 close()，模拟进程崩溃
        drop(db);
    }

    {
        let db = DB::open(dir.path()).unwrap();
        // 至少恢复到最后一次 sync 的数据
        let mut recovered = 0;
        for i in 0..committed_count {
            if db.get(format!("k{:04}", i).as_bytes()).unwrap().is_some() {
                recovered += 1;
            }
        }
        assert!(recovered > 0, "should recover at least some data");
        assert!(recovered <= committed_count);
        db.close().unwrap();
    }
}

/// 损坏 SSTable 后 Repair 能重建一致状态
#[test]
fn repair_after_sst_corruption() {
    let dir = tempdir().unwrap();
    {
        let db = DB::open(dir.path()).unwrap();
        for i in 0..5000 {
            db.put(format!("k{:04}", i).as_bytes(), b"v").unwrap();
        }
        db.flush().unwrap();
        db.close().unwrap();
    }

    // 损坏一个 SSTable 文件
    let sst_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "sst"))
        .collect();
    if let Some(f) = sst_files.first() {
        std::fs::write(f.path(), b"CORRUPT").unwrap();
    }

    // repair 应该能重建 manifest，排除损坏的 sst
    let report = DB::repair(dir.path()).unwrap();
    assert!(report.corrupted_files > 0);

    // 重新打开，验证剩余数据可读
    let db = DB::open(dir.path()).unwrap();
    let mut readable = 0;
    for i in 0..5000 {
        if db.get(format!("k{:04}", i).as_bytes()).unwrap().is_some() {
            readable += 1;
        }
    }
    assert!(readable > 0);
}
```

### 4.5 属性测试（Property-Based Testing）

使用 `proptest` crate 生成随机操作序列，验证不变量。

```rust
// tests/property.rs
use proptest::prelude::*;
use kv_engine::DB;
use tempfile::tempdir;

/// 生成随机 KV 操作序列
fn kv_ops() -> impl Strategy<Value = Vec<(Vec<u8>, Vec<u8>)>> {
    prop::collection::vec(
        (prop::collection::vec(any::<u8>(), 1..32), prop::collection::vec(any::<u8>(), 0..256)),
        1..500,
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
```

### 4.6 并发测试

验证多线程读写的正确性。

```rust
// tests/concurrency.rs
use std::sync::Arc;
use std::thread;
use kv_engine::DB;
use tempfile::tempdir;

#[test]
fn concurrent_writes_and_reads() {
    let dir = tempdir().unwrap();
    let db = Arc::new(DB::open(dir.path()).unwrap());

    let writer_db = db.clone();
    let writer = thread::spawn(move || {
        for i in 0..10000 {
            writer_db.put(format!("k{:05}", i).as_bytes(), format!("v{}", i).as_bytes()).unwrap();
        }
    });

    let reader_db = db.clone();
    let reader = thread::spawn(move || {
        for i in 0..10000 {
            // 读取结果应为 None 或正确的 value，不能 panic
            let _ = reader_db.get(format!("k{:05}", i).as_bytes());
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();

    // 写入完成后，所有 key 应可读
    for i in 0..10000 {
        assert!(db.get(format!("k{:05}", i).as_bytes()).unwrap().is_some());
    }
}

#[test]
fn concurrent_readers_never_see_corruption() {
    let dir = tempdir().unwrap();
    let db = Arc::new(DB::open(dir.path()).unwrap());

    // 预填充
    for i in 0..1000 {
        db.put(format!("k{:04}", i).as_bytes(), format!("{:064}", i).as_bytes()).unwrap();
    }

    let mut handles = vec![];
    for _ in 0..8 {
        let db = db.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..5000 {
                let key = format!("k{:04}", rand::random::<u32>() % 1000);
                if let Some(v) = db.get(key.as_bytes()).unwrap() {
                    // value 长度固定为 64，不完整则说明损坏
                    assert_eq!(v.len(), 64, "corrupted value for key {}", key);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}
```

> **可选进阶：** 使用 `loom` crate 模拟线程调度，检测内存序问题。将关键的 `MemTable` 和 `VersionSet` 操作在 loom 下运行。

### 4.7 压力测试 & 模糊测试

#### 4.7.1 长时间压力脚本

```rust
// tests/stress.rs
#[test]
#[ignore] // 手动触发: cargo test --ignored stress_
fn stress_million_keys() {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    let n = 1_000_000;
    // 写入
    for i in 0..n {
        db.put(format!("k{:08}", i).as_bytes(), vec![i as u8; 128].as_slice()).unwrap();
    }
    db.flush().unwrap();

    // 读取验证
    for i in 0..n {
        let v = db.get(format!("k{:08}", i).as_bytes()).unwrap();
        assert!(v.is_some(), "missing key k{:08}", i);
    }

    // 删除一半
    for i in (0..n).step_by(2) {
        db.delete(format!("k{:08}", i).as_bytes()).unwrap();
    }

    // 再次验证
    for i in 0..n {
        let v = db.get(format!("k{:08}", i).as_bytes()).unwrap();
        if i % 2 == 0 {
            assert!(v.is_none(), "key k{:08} should be deleted", i);
        } else {
            assert!(v.is_some(), "key k{:08} should exist", i);
        }
    }
    db.close().unwrap();
}
```

#### 4.7.2 cargo-fuzz（模糊测试）

```rust
// fuzz/fuzz_targets/fuzz_engine.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;
use kv_engine::DB;

fuzz_target!(|data: &[u8]| {
    let dir = tempdir().unwrap();
    let db = DB::open(dir.path()).unwrap();

    // 把原始字节解释为操作序列：[op: u8][key_len: u8][key...][value_len: u8][value...]
    let mut cursor = data;
    while cursor.len() >= 2 {
        let op = cursor[0];
        let key_len = cursor[1] as usize;
        cursor = &cursor[2..];
        if cursor.len() < key_len { break; }
        let key = &cursor[..key_len];
        cursor = &cursor[key_len..];

        match op % 3 {
            0 => { let _ = db.get(key); }
            1 => { let _ = db.delete(key); }
            _ => {
                if cursor.is_empty() { break; }
                let val_len = cursor[0] as usize;
                cursor = &cursor[1..];
                if cursor.len() < val_len { break; }
                let val = &cursor[..val_len];
                cursor = &cursor[val_len..];
                let _ = db.put(key, val);
            }
        }
    }

    // 最终不能 panic
    let _ = db.close();
});
```

初始化方式：
```bash
cargo install cargo-fuzz
cargo fuzz init
cargo fuzz run fuzz_engine
```

### 4.8 测试基础设施与辅助工具

#### 4.8.1 通用测试工具模块

```rust
// src/test_utils.rs
use tempfile::TempDir;
use crate::{DB, Options};

/// 用于测试的临时 DB 实例，drop 时自动清理目录
pub struct TestDB {
    pub db: DB,
    pub dir: TempDir,
}

impl TestDB {
    pub fn open() -> Self {
        let dir = TempDir::new().unwrap();
        let db = DB::open(dir.path()).unwrap();
        TestDB { db, dir }
    }

    pub fn open_with_options(opts: Options) -> Self {
        let dir = TempDir::new().unwrap();
        let db = DB::open_with_options(dir.path(), opts).unwrap();
        TestDB { db, dir }
    }

    /// 构造一个会快速触发 flush 的小容量 DB
    pub fn open_small() -> Self {
        Self::open_with_options(Options {
            memtable_size: 256,
            block_size: 64,
            ..Default::default()
        })
    }
}
```

#### 4.8.2 测试夹具（Fixture）

```rust
// tests/fixtures/mod.rs
/// 生成指定数量的有序 KV 对
pub fn generate_kv_pairs(n: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..n)
        .map(|i| (format!("key_{:08}", i).into_bytes(), format!("val_{:08}", i).into_bytes()))
        .collect()
}

/// 生成指定 key 长度和 value 长度的 KV 对
pub fn generate_sized_kv(n: usize, key_len: usize, val_len: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..n)
        .map(|i| {
            let mut key = format!("{}", i).into_bytes();
            key.resize(key_len, b'k');
            let mut val = vec![b'v'; val_len];
            val[0] = i as u8;
            (key, val)
        })
        .collect()
}
```

### 4.9 CI 集成配置

#### GitHub Actions 示例

```yaml
# .github/workflows/test.yml
name: Test Suite

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable

      - name: Cache cargo
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/bin
            ~/.cargo/registry
            target
          key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}

      - name: Unit & Integration Tests
        run: cargo test --all

      - name: Property Tests
        run: cargo test --test property

      - name: Stress Tests (ignored)
        run: cargo test --ignored stress_ -- --test-threads=1

      - name: Clippy Lints
        run: cargo clippy -- -D warnings

      - name: Format Check
        run: cargo fmt -- --check

  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo install cargo-fuzz
      - name: Fuzz (30 minutes)
        run: cargo fuzz run fuzz_engine -- -max_total_time=1800
```

### 4.10 测试覆盖目标

| 模块 | 行覆盖率目标 | 重点覆盖路径 |
|---|---|---|
| skiplist | ≥ 90% | 多级节点插入、删除、边界 key |
| wal | ≥ 95% | 正常写入、CRC 校验、截断恢复、空文件 |
| memtable | ≥ 90% | 阈值触发 freeze、双缓冲切换 |
| sstable | ≥ 90% | 单 block、多 block、prefix compression、边界 key |
| bloom filter | ≥ 95% | build、may_contain、误判率验证 |
| compaction | ≥ 85% | L0→L1 合并、tombstone 清理、多层合并 |
| engine (API) | ≥ 90% | put/get/delete/scan 全链路、open/close 生命周期 |
| manifest | ≥ 90% | save/load 一致性、损坏处理 |

使用 `cargo-tarpaulin` 统计覆盖率：
```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Html --output-dir coverage/
```

---

## 五、关键模块的 Rust 设计要点

| 模块 | Rust 设计要点 |
|---|---|
| **Skip List** | `unsafe` 指针操作 + safe 对外 API；用 `Owned<Node>` 或 raw pointer；考虑 `crossbeam-epoch` 做内存回收 |
| **WAL** | `BufWriter` 减少 syscall；`file.sync_data()` 做 fsync；顺序读用 `BufReader` |
| **SSTable Block** | 前缀压缩用 `(shared_len, suffix, value)` 三元组；Block 用 `&[u8]` 零拷贝读取 |
| **Bloom Filter** | `k` 个独立 hash 用 double-hashing 优化为 2 个 hash：`h(i) = h1 + i * h2` |
| **Compaction** | 后台 `tokio::spawn` 或 `std::thread`；用 `mpsc::channel` 通知版本更新 |
| **并发安全** | 读写锁优先；MemTable 的 immutable 是只读的，无需锁 |
| **错误处理** | 统一 `Result<T, KvError>`；IO 错误直接传播，数据损坏返回 `Corruption` |

---

## 六、开发里程碑与时间线

```
Week 1-2:  Phase 1.1 ~ 1.5  →  Skip List + WAL + MemTable + SSTable Builder
Week 3:    Phase 1.6 ~ 1.8  →  SSTable Reader + Manifest + Engine 组装 + 集成测试
Week 4-5:  Phase 2.1 ~ 2.3  →  Bloom Filter + Block Cache + Leveled Compaction
Week 6:    Phase 2.4 ~ 2.6  →  Merge Iterator + SCAN + 压缩
Week 7-8:  Phase 3 全部      →  WriteBatch + Snapshot + TTL + Prefix + Repair
Week 9-10: Phase 4 全部      →  线程安全 + 指标 + 配置 + Benchmark + 文档
```

---

## 七、参考资源

| 资源 | 用途 |
|---|---|
| [Rust-rocksdb](https://github.com/rust-rocksdb/rust-rocksdb) | API 设计参考 |
| [leveldb (C++)](https://github.com/google/leveldb) | SSTable 格式和 Compaction 算法参考 |
| [mini-lsm (教学项目)](https://github.com/skyzh/mini-lsm) | Rust 写的 LSM 教学实现，非常推荐 |
| [TiKV 文档](https://tikv.org) | 生产级 Rust 存储引擎架构参考 |
