//! Direct LMDB (`data.ldb`) to Barely-Nano NVMe B-Tree Migration Tool
//!
//! Reads a C++ Nano / RSNano `data.ldb` file via sequential leaf-page scanning, transforms
//! records into `barely-nano`'s 16-shard Slotted-Page B-Tree + 64 MB Block Bloom Filter format,
//! and writes sequentially to the target NVMe block device (e.g. `/dev/sda`).
//!
//! Preserves the isolated hardware wallet region at `WALLET_STORAGE_LBA = 786_432` (384 MB).

use std::cmp::Ordering;
use std::env;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Instant;

// ── Linux libc FFI (linked by std) ───────────────────────────────────────────
const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MAP_FAILED: *mut c_void = !0usize as *mut c_void;
const MADV_SEQUENTIAL: i32 = 2;
const BLKGETSIZE64: u64 = 0x8008_1272;

extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

struct MmapFile {
    ptr: *const u8,
    len: usize,
}

unsafe impl Send for MmapFile {}
unsafe impl Sync for MmapFile {}

impl MmapFile {
    fn open(path: &str) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", path, e))?;
        let len = file
            .metadata()
            .map_err(|e| format!("Failed to stat {}: {}", path, e))?
            .len() as usize;
        if len < 8192 {
            return Err(format!("File {} is too small ({} bytes)", path, len));
        }
        let ptr = unsafe { mmap( std::ptr::null_mut(), len, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0) };
        if ptr == MAP_FAILED {
            return Err(format!(
                "mmap failed on {}: {}",
                path,
                std::io::Error::last_os_error()
            ));
        }
        unsafe {
            madvise(ptr, len, MADV_SEQUENTIAL);
        }
        Ok(Self {
            ptr: ptr as *const u8,
            len,
        })
    }

    #[inline(always)]
    fn page(&self, pgno: usize) -> &[u8; 4096] {
        let off = pgno * 4096;
        unsafe { &*(self.ptr.add(off) as *const [u8; 4096]) }
    }
}

impl Drop for MmapFile {
    fn drop(&mut self) {
        if !self.ptr.is_null() && self.len > 0 {
            unsafe {
                munmap(self.ptr as *mut c_void, self.len);
            }
        }
    }
}

// ── Barely-Nano NVMe Physical Layout Constants ───────────────────────────────
const SECTOR_SIZE: u64 = 512;
const BLOCK_SIZE: usize = 4096;
const SHARD_COUNT: usize = 16;

const BLOOM_HEADER_LBA: u64 = 524_288; // 256 MB offset
const BLOOM_DATA_LBA: u64 = 524_296; // 256 MB + 4 KB offset
const BLOOM_MAGIC: u64 = 0x424C_4F4D_5632_0006; // "BLOMV2\0\x06"
const BLOOM_BUCKETS: usize = 1 << 21; // 2,097,152 buckets
const BLOOM_WORDS: usize = BLOOM_BUCKETS * 4; // 8,388,608 u64s = 64 MB

// Note: WALLET_STORAGE_LBA = 786_432 (384 MB) is intentionally untouched.
const STORAGE_BASE_LBA: u64 = 1_048_576; // 512 MB offset
const STORAGE_MAGIC: u64 = 0x5348_4152_4453_4834; // "SHARDSH4"
const WAL_MAX_PAGES: usize = 65_536; // 256 MB

const BTREE_META_MAGIC: u32 = 0xBEEF_CB03;
const PAGE_FLAG_SOA: u8 = 1;
const MAX_KEYS: usize = 64;
const KEY_SIZE: usize = 65;
const VALUE_SIZE: usize = 272;
const MAX_INSERT_BYTES: usize = 12 + 3 + KEY_SIZE + VALUE_SIZE; // 352 bytes

const COUNTS_MAGIC: u64 = 0x4E41_4E4F_434E_5431; // "NANOCNT1"

// ── Table Identifiers in Barely-Nano ─────────────────────────────────────────
const TABLE_ACCOUNTS: u8 = 1;
const TABLE_BLOCKS: u8 = 2;
const TABLE_CONF_HEIGHT: u8 = 3;
const TABLE_PRUNED: u8 = 5;
const TABLE_PEERS: u8 = 6;
const TABLE_PENDING: u8 = 7;
const TABLE_ONLINE_WEIGHT: u8 = 9;
const TABLE_VERSION: u8 = 11;
const TABLE_REP_WEIGHTS: u8 = 12;

// Table ordinals for Pass 1 bucketing (in ascending TABLE_ID order)
const ORD_ACCOUNTS: usize = 0; // TABLE_ID = 1
const ORD_BLOCKS: usize = 1; // TABLE_ID = 2
const ORD_CONF_HEIGHT: usize = 2; // TABLE_ID = 3
const ORD_PRUNED: usize = 3; // TABLE_ID = 5
const ORD_PEERS: usize = 4; // TABLE_ID = 6
const ORD_PENDING: usize = 5; // TABLE_ID = 7
const ORD_ONLINE_WEIGHT: usize = 6; // TABLE_ID = 9
const ORD_REP_WEIGHTS: usize = 7; // TABLE_ID = 12
const NUM_TABLES: usize = 8;

/// Nano Live Network Genesis Block Hash (`991CF190094C00F0B68E2E5F75F6BEE95A2E0BD93CEAA4A6734DB9F19B728948`).
/// Never pruned (`ledger/src/ledger.rs:432`).
const LIVE_GENESIS_HASH: [u8; 32] = [
    0x99, 0x1C, 0xF1, 0x90, 0x09, 0x4C, 0x00, 0xF0, 0xB6, 0x8E, 0x2E, 0x5F, 0x75, 0xF6, 0xBE, 0xE9,
    0x5A, 0x2E, 0x0B, 0xD9, 0x3C, 0xEA, 0xA4, 0xA6, 0x73, 0x4D, 0xB9, 0xF1, 0x9B, 0x72, 0x89, 0x48,
];

// ── Slotted-Page B-Tree Page Primitives (Exact Mirror of common/src/storage/btree.rs) ──

#[inline(always)]
fn key_head(k: &[u8]) -> u64 {
    u64::from_be_bytes([k[0], k[1], k[2], k[3], k[4], k[5], k[6], k[7]])
}

#[inline(always)]
fn key_head32(k: &[u8]) -> u32 {
    u32::from_be_bytes([k[0], k[1], k[2], k[3]])
}

#[inline(always)]
fn key_compact_slice(key: &[u8; KEY_SIZE]) -> &[u8] {
    if key[33..KEY_SIZE] == [0u8; 32] {
        &key[..33]
    } else {
        &key[..KEY_SIZE]
    }
}

#[inline(always)]
fn cmp_cell_key(cell_key: &[u8], target: &[u8; KEY_SIZE], target_head: u64) -> Ordering {
    let ch = key_head(cell_key);
    if ch != target_head {
        return ch.cmp(&target_head);
    }
    let prefix_cmp = cell_key[8..33].cmp(&target[8..33]);
    if prefix_cmp != Ordering::Equal {
        return prefix_cmp;
    }
    if cell_key.len() == KEY_SIZE {
        cell_key[33..KEY_SIZE].cmp(&target[33..KEY_SIZE])
    } else if target[33..KEY_SIZE] == [0u8; 32] {
        Ordering::Equal
    } else {
        Ordering::Less
    }
}

#[inline(always)]
fn page_init(data: &mut [u8; BLOCK_SIZE], is_leaf: bool) {
    data.fill(0);
    data[0] = if is_leaf { 1 } else { 0 };
    data[1..3].copy_from_slice(&0u16.to_le_bytes());
    data[3..5].copy_from_slice(&(BLOCK_SIZE as u16).to_le_bytes());
    data[5..7].copy_from_slice(&0u16.to_le_bytes());
    data[7] = PAGE_FLAG_SOA;
}

#[inline(always)]
fn page_is_leaf(data: &[u8; BLOCK_SIZE]) -> bool {
    data[0] != 0
}

#[inline(always)]
fn page_num_keys(data: &[u8; BLOCK_SIZE]) -> usize {
    (u16::from_le_bytes([data[1], data[2]]) as usize).min(MAX_KEYS)
}

#[inline(always)]
fn page_set_num_keys(data: &mut [u8; BLOCK_SIZE], nk: usize) {
    data[1..3].copy_from_slice(&(nk as u16).to_le_bytes());
}

#[inline(always)]
fn page_free_end(data: &[u8; BLOCK_SIZE]) -> usize {
    let fe = u16::from_le_bytes([data[3], data[4]]) as usize;
    if fe == 0 || fe > BLOCK_SIZE {
        BLOCK_SIZE
    } else {
        fe
    }
}

#[inline(always)]
fn page_set_free_end(data: &mut [u8; BLOCK_SIZE], fe: usize) {
    data[3..5].copy_from_slice(&(fe as u16).to_le_bytes());
}

#[inline(always)]
fn page_heads_base(is_leaf: bool, num_keys: usize) -> usize {
    let raw = if is_leaf {
        8 + num_keys * 2
    } else {
        12 + num_keys * 6
    };
    (raw + 3) & !3
}

#[inline(always)]
fn page_free_start(is_leaf: bool, num_keys: usize) -> usize {
    page_heads_base(is_leaf, num_keys) + num_keys * 4
}

#[inline(always)]
fn page_slot_off(is_leaf: bool, idx: usize) -> usize {
    if is_leaf {
        8 + idx * 2
    } else {
        12 + idx * 6
    }
}

#[inline(always)]
fn page_get_slot(data: &[u8; BLOCK_SIZE], is_leaf: bool, idx: usize) -> usize {
    let o = page_slot_off(is_leaf, idx);
    u16::from_le_bytes([data[o], data[o + 1]]) as usize
}

#[inline(always)]
fn page_set_slot(data: &mut [u8; BLOCK_SIZE], is_leaf: bool, idx: usize, cell_off: usize) {
    let o = page_slot_off(is_leaf, idx);
    data[o..o + 2].copy_from_slice(&(cell_off as u16).to_le_bytes());
}

#[inline(always)]
fn page_get_child(data: &[u8; BLOCK_SIZE], idx: usize) -> u32 {
    let o = 8 + idx * 6;
    u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
}

#[inline(always)]
fn page_set_child(data: &mut [u8; BLOCK_SIZE], idx: usize, child_id: u32) {
    let o = 8 + idx * 6;
    data[o..o + 4].copy_from_slice(&child_id.to_le_bytes());
}

#[inline(always)]
fn page_cell(data: &[u8; BLOCK_SIZE], cell_off: usize) -> (&[u8], &[u8]) {
    let klen = data[cell_off] as usize;
    let vlen = u16::from_le_bytes([data[cell_off + 1], data[cell_off + 2]]) as usize;
    let kstart = cell_off + 3;
    let vstart = kstart + klen;
    (&data[kstart..vstart], &data[vstart..vstart + vlen])
}

#[inline(always)]
fn page_usable_free(data: &[u8; BLOCK_SIZE]) -> usize {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    let fs = page_free_start(is_leaf, nk);
    let fe = page_free_end(data);
    fe.saturating_sub(fs)
}

#[inline(always)]
fn page_is_full(data: &[u8; BLOCK_SIZE]) -> bool {
    page_num_keys(data) >= MAX_KEYS || page_usable_free(data) < MAX_INSERT_BYTES
}

/// Checks whether appending `(key_slice, val)` to `data` leaves `!page_is_full(data)`.
#[inline(always)]
fn page_can_append_staying_nonfull(
    data: &[u8; BLOCK_SIZE],
    key_slice: &[u8],
    val: &[u8],
) -> bool {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    if nk + 1 >= MAX_KEYS {
        return false;
    }
    let dir_bytes = page_free_start(is_leaf, nk + 1) - page_free_start(is_leaf, nk);
    let cell_bytes = 3 + key_slice.len() + val.len();
    let needed = dir_bytes + cell_bytes;
    let avail = page_usable_free(data);
    avail >= needed + MAX_INSERT_BYTES
}

/// Appends `(key_slice, val)` at index `nk` (`pos == num_keys`).
#[inline(always)]
fn page_append_slot(data: &mut [u8; BLOCK_SIZE], key_slice: &[u8], val: &[u8]) {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    let klen = key_slice.len();
    let vlen = val.len().min(VALUE_SIZE);
    let clen = 3 + klen + vlen;
    let fe = page_free_end(data);
    let new_fe = fe - clen;
    debug_assert!(new_fe >= page_free_start(is_leaf, nk + 1));

    data[new_fe] = klen as u8;
    data[new_fe + 1..new_fe + 3].copy_from_slice(&(vlen as u16).to_le_bytes());
    data[new_fe + 3..new_fe + 3 + klen].copy_from_slice(key_slice);
    data[new_fe + 3 + klen..new_fe + clen].copy_from_slice(&val[..vlen]);
    page_set_free_end(data, new_fe);

    let old_hb = page_heads_base(is_leaf, nk);
    let new_hb = page_heads_base(is_leaf, nk + 1);
    if new_hb != old_hb && nk > 0 {
        data.copy_within(old_hb..old_hb + nk * 4, new_hb);
    }
    let h32 = key_head32(key_slice);
    let ho = new_hb + nk * 4;
    data[ho..ho + 4].copy_from_slice(&h32.to_le_bytes());

    page_set_slot(data, is_leaf, nk, new_fe);
    page_set_num_keys(data, nk + 1);
}

/// Pops the last slot (`nk - 1`) from `data` with zero fragmentation (`dead_bytes == 0`).
#[inline(always)]
fn page_pop_last_slot(
    data: &mut [u8; BLOCK_SIZE],
    k_out: &mut [u8; KEY_SIZE],
    v_out: &mut [u8; VALUE_SIZE],
) -> (usize, usize) {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    debug_assert!(nk >= 2);
    let last_idx = nk - 1;
    let off = page_get_slot(data, is_leaf, last_idx);
    debug_assert_eq!(off, page_free_end(data));
    let (ck, cv) = page_cell(data, off);
    let klen = ck.len();
    let vlen = cv.len();
    k_out[..klen].copy_from_slice(ck);
    v_out[..vlen].copy_from_slice(cv);
    let clen = 3 + klen + vlen;

    // Reclaim cell from top of downward-growing cell heap
    data[off..off + clen].fill(0);
    page_set_free_end(data, off + clen);

    let old_hb = page_heads_base(is_leaf, nk);
    let new_hb = page_heads_base(is_leaf, last_idx);

    // Clear directory entry
    let slot_off = page_slot_off(is_leaf, last_idx);
    data[slot_off..slot_off + 2].fill(0);
    if new_hb != old_hb && last_idx > 0 {
        data.copy_within(old_hb..old_hb + last_idx * 4, new_hb);
    }
    data[new_hb + last_idx * 4..old_hb + nk * 4].fill(0);
    page_set_num_keys(data, last_idx);
    (klen, vlen)
}

// ── Streaming Rightmost-Spine B-Tree Builder ─────────────────────────────────

trait PageSink {
    fn write_page(&mut self, page_id: u32, data: &[u8; BLOCK_SIZE]) -> Result<(), String>;
}

struct StreamingBTreeBuilder<S: PageSink> {
    sink: S,
    next_page_id: u32,
    levels: Vec<[u8; BLOCK_SIZE]>,
    total_entries: u64,
}

impl<S: PageSink> StreamingBTreeBuilder<S> {
    fn new(sink: S) -> Self {
        let mut leaf = [0u8; BLOCK_SIZE];
        page_init(&mut leaf, true);
        Self {
            sink,
            next_page_id: 1, // Page 0 is reserved for shard MetaData
            levels: vec![leaf],
            total_entries: 0,
        }
    }

    #[inline(always)]
    fn alloc_and_write_level(&mut self, level: usize) -> Result<u32, String> {
        let id = self.next_page_id;
        self.next_page_id += 1;
        self.sink.write_page(id, &self.levels[level])?;
        Ok(id)
    }

    /// Inserts `(key, val)` in strictly ascending key order.
    #[inline]
    fn push(&mut self, key: &[u8; KEY_SIZE], val: &[u8]) -> Result<(), String> {
        self.total_entries += 1;
        let kslice = key_compact_slice(key);

        if page_can_append_staying_nonfull(&self.levels[0], kslice, val) {
            page_append_slot(&mut self.levels[0], kslice, val);
            return Ok(());
        }

        // Leaf `levels[0]` is full (~88% capacity, `!page_is_full`).
        // Pop the last entry of `levels[0]` to serve as separator for `levels[1]`,
        // flush `levels[0]` to disk, and place the incoming `(kslice, val)` into the new `levels[0]`.
        let mut sep_k = [0u8; KEY_SIZE];
        let mut sep_v = [0u8; VALUE_SIZE];
        let (sep_klen, sep_vlen) = page_pop_last_slot(&mut self.levels[0], &mut sep_k, &mut sep_v);
        debug_assert!(!page_is_full(&self.levels[0]));

        let left_leaf_id = self.alloc_and_write_level(0)?;

        page_init(&mut self.levels[0], true);
        page_append_slot(&mut self.levels[0], kslice, val);

        self.promote(1, &sep_k[..sep_klen], &sep_v[..sep_vlen], left_leaf_id)
    }

    /// Promotes separator `(sep_k, sep_v)` and its completed left subtree `left_child_id` into `levels[level]`.
    fn promote(
        &mut self,
        level: usize,
        sep_k: &[u8],
        sep_v: &[u8],
        left_child_id: u32,
    ) -> Result<(), String> {
        if level == self.levels.len() {
            let mut node = [0u8; BLOCK_SIZE];
            page_init(&mut node, false);
            page_set_child(&mut node, 0, left_child_id);
            page_append_slot(&mut node, sep_k, sep_v);
            self.levels.push(node);
            return Ok(());
        }

        if page_can_append_staying_nonfull(&self.levels[level], sep_k, sep_v) {
            let nk = page_num_keys(&self.levels[level]);
            page_set_child(&mut self.levels[level], nk, left_child_id);
            page_append_slot(&mut self.levels[level], sep_k, sep_v);
            return Ok(());
        }

        // Internal node `levels[level]` is full.
        // Pop its last separator `S_p` (its children `0..=p-1` are already populated!),
        // flush `levels[level]` to disk as `upper_left_id`, re-init `levels[level]` with
        // `child[0] = left_child_id` and `slot[0] = (sep_k, sep_v)`, and promote `S_p` to `level + 1`.
        let mut up_k = [0u8; KEY_SIZE];
        let mut up_v = [0u8; VALUE_SIZE];
        let (up_klen, up_vlen) =
            page_pop_last_slot(&mut self.levels[level], &mut up_k, &mut up_v);
        debug_assert!(!page_is_full(&self.levels[level]));

        let upper_left_id = self.alloc_and_write_level(level)?;

        page_init(&mut self.levels[level], false);
        page_set_child(&mut self.levels[level], 0, left_child_id);
        page_append_slot(&mut self.levels[level], sep_k, sep_v);

        self.promote(level + 1, &up_k[..up_klen], &up_v[..up_vlen], upper_left_id)
    }

    /// Flushes all open rightmost spine nodes from leaf up to root, returning `(root_page_id, next_free_page, sink)`.
    fn finalize(mut self) -> Result<(u32, u32, S), String> {
        let mut child_id = self.alloc_and_write_level(0)?;
        for level in 1..self.levels.len() {
            let nk = page_num_keys(&self.levels[level]);
            page_set_child(&mut self.levels[level], nk, child_id);
            debug_assert!(!page_is_full(&self.levels[level]));
            child_id = self.alloc_and_write_level(level)?;
        }
        Ok((child_id, self.next_page_id, self.sink))
    }
}

// ── Self-Test of StreamingBTreeBuilder ───────────────────────────────────────

fn verify_btree_builder_self_test() -> Result<(), String> {
    struct MemSink {
        pages: Vec<[u8; BLOCK_SIZE]>,
    }
    impl PageSink for MemSink {
        fn write_page(&mut self, page_id: u32, data: &[u8; BLOCK_SIZE]) -> Result<(), String> {
            let idx = page_id as usize;
            if idx >= self.pages.len() {
                self.pages.resize(idx + 1, [0u8; BLOCK_SIZE]);
            }
            self.pages[idx] = *data;
            Ok(())
        }
    }

    let mut builder = StreamingBTreeBuilder::new(MemSink {
        pages: vec![[0u8; BLOCK_SIZE]],
    });

    let count = 5_000usize;
    for i in 0..count {
        let mut k = [0u8; KEY_SIZE];
        k[0] = if i < 2500 { 1 } else { 7 };
        k[1..9].copy_from_slice(&(i as u64).to_be_bytes());
        if i >= 2500 {
            k[33..41].copy_from_slice(&((i * 3) as u64).to_be_bytes());
        }
        let val = [(i & 0xFF) as u8; 267];
        builder.push(&k, &val)?;
    }

    let (root_id, next_free, sink) = builder.finalize()?;
    if next_free as usize != sink.pages.len() {
        return Err("Self-test page count mismatch".into());
    }

    // Verify every page is `!page_is_full` and every key can be looked up via binary search
    for pid in 1..(next_free as usize) {
        if page_is_full(&sink.pages[pid]) {
            return Err(format!("Self-test page {} is marked full", pid));
        }
    }

    for i in 0..count {
        let mut k = [0u8; KEY_SIZE];
        k[0] = if i < 2500 { 1 } else { 7 };
        k[1..9].copy_from_slice(&(i as u64).to_be_bytes());
        if i >= 2500 {
            k[33..41].copy_from_slice(&((i * 3) as u64).to_be_bytes());
        }
        let th = key_head(&k);
        let mut cur = root_id as usize;
        let found = loop {
            let p = &sink.pages[cur];
            let is_leaf = page_is_leaf(p);
            let nk = page_num_keys(p);
            let (mut lo, mut hi) = (0usize, nk);
            let mut eq = None;
            while lo < hi {
                let mid = (lo + hi) >> 1;
                let off = page_get_slot(p, is_leaf, mid);
                let (ck, cv) = page_cell(p, off);
                match cmp_cell_key(ck, &k, th) {
                    Ordering::Less => lo = mid + 1,
                    Ordering::Equal => {
                        eq = Some(cv[0]);
                        break;
                    }
                    Ordering::Greater => hi = mid,
                }
            }
            if let Some(v0) = eq {
                break Some(v0);
            }
            if is_leaf {
                break None;
            }
            cur = page_get_child(p, lo) as usize;
        };
        if found != Some((i & 0xFF) as u8) {
            return Err(format!("Self-test lookup failed at key {}", i));
        }
    }
    Ok(())
}

// ── Buffered Sequential NVMe Shard Page Sink ─────────────────────────────────

struct NvmeShardSink<'a> {
    file: &'a File,
    shard_base_byte_offset: u64,
    shard_max_pages: u32,
    buf: Vec<u8>,
    buf_start_page: u32,
    buf_pages: u32,
}

const WRITE_BUF_PAGES: usize = 4096; // 16 MB sequential write chunks

impl<'a> NvmeShardSink<'a> {
    fn new(file: &'a File, shard_base_byte_offset: u64, shard_max_pages: u32) -> Self {
        Self {
            file,
            shard_base_byte_offset,
            shard_max_pages,
            buf: vec![0u8; WRITE_BUF_PAGES * BLOCK_SIZE],
            buf_start_page: 1,
            buf_pages: 0,
        }
    }

    fn flush_buf(&mut self) -> Result<(), String> {
        if self.buf_pages == 0 {
            return Ok(());
        }
        let byte_off =
            self.shard_base_byte_offset + (self.buf_start_page as u64) * (BLOCK_SIZE as u64);
        let byte_len = (self.buf_pages as usize) * BLOCK_SIZE;
        self.file
            .write_all_at(&self.buf[..byte_len], byte_off)
            .map_err(|e| format!("NVMe write error at offset {}: {}", byte_off, e))?;
        self.buf_start_page += self.buf_pages;
        self.buf_pages = 0;
        Ok(())
    }

    fn write_meta(&mut self, root_page_id: u32, next_free_page: u32) -> Result<(), String> {
        self.flush_buf()?;
        let mut meta = [0u8; BLOCK_SIZE];
        meta[0..4].copy_from_slice(&BTREE_META_MAGIC.to_le_bytes());
        meta[4..8].copy_from_slice(&root_page_id.to_le_bytes());
        meta[8..12].copy_from_slice(&next_free_page.to_le_bytes());
        meta[12..16].copy_from_slice(&0u32.to_le_bytes()); // first_free_page = 0
        self.file
            .write_all_at(&meta, self.shard_base_byte_offset)
            .map_err(|e| format!("NVMe meta write error: {}", e))
    }
}

impl<'a> PageSink for NvmeShardSink<'a> {
    #[inline]
    fn write_page(&mut self, page_id: u32, data: &[u8; BLOCK_SIZE]) -> Result<(), String> {
        if page_id >= self.shard_max_pages {
            return Err(format!(
                "Shard capacity exceeded: page_id={} >= max={}",
                page_id, self.shard_max_pages
            ));
        }
        debug_assert_eq!(page_id, self.buf_start_page + self.buf_pages);
        let dst_off = (self.buf_pages as usize) * BLOCK_SIZE;
        self.buf[dst_off..dst_off + BLOCK_SIZE].copy_from_slice(data);
        self.buf_pages += 1;
        if (self.buf_pages as usize) == WRITE_BUF_PAGES {
            self.flush_buf()?;
        }
        Ok(())
    }
}

// ── Block Bloom Filter (Exact Mirror of ledger_store/src/block_store.rs) ─────

struct BlockBloom {
    words: Vec<u64>,
}

impl BlockBloom {
    fn new() -> Self {
        Self {
            words: vec![0u64; BLOOM_WORDS],
        }
    }

    #[inline(always)]
    fn insert(&mut self, hash: &[u8; 32]) {
        let w0 = u64::from_le_bytes([
            hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        ]);
        let w1 = u64::from_le_bytes([
            hash[8], hash[9], hash[10], hash[11], hash[12], hash[13], hash[14], hash[15],
        ]);
        let base = ((w0 as usize) & (BLOOM_BUCKETS - 1)) * 4;
        let m0 = (1u64 << (w1 & 63)) | (1u64 << ((w1 >> 6) & 63));
        let m1 = (1u64 << ((w1 >> 12) & 63)) | (1u64 << ((w1 >> 18) & 63));
        let m2 = 1u64 << ((w1 >> 24) & 63);
        let m3 = 1u64 << ((w1 >> 30) & 63);
        self.words[base] |= m0;
        self.words[base + 1] |= m1;
        self.words[base + 2] |= m2;
        self.words[base + 3] |= m3;
    }

    fn write_to_nvme(&self, file: &File) -> Result<(), String> {
        let data_byte_off = BLOOM_DATA_LBA * SECTOR_SIZE;
        // Convert u64 words to little-endian byte slice (native on x86_64)
        let byte_slice = unsafe {
            std::slice::from_raw_parts(
                self.words.as_ptr() as *const u8,
                self.words.len() * std::mem::size_of::<u64>(),
            )
        };
        file.write_all_at(byte_slice, data_byte_off)
            .map_err(|e| format!("Failed to write Bloom filter data: {}", e))?;

        let mut hdr = [0u8; BLOCK_SIZE];
        hdr[0..8].copy_from_slice(&BLOOM_MAGIC.to_le_bytes());
        hdr[8..16].copy_from_slice(&(BLOOM_BUCKETS as u64).to_le_bytes());
        hdr[16..24].copy_from_slice(&0u64.to_le_bytes()); // delta_pages = 0
        file.write_all_at(&hdr, BLOOM_HEADER_LBA * SECTOR_SIZE)
            .map_err(|e| format!("Failed to write Bloom filter header: {}", e))
    }
}

// ── LMDB Leaf Page Classifier & Entry Extractor ──────────────────────────────

/// Classifies an LMDB 4KB page if it is a valid `P_LEAF` page belonging to one of our 7 tables.
/// Returns `Some((table_ord, shard_mask: u16))` where bit `s` in `shard_mask` is set if the leaf
/// page contains at least one entry belonging to shard `s` (`0..16`).
#[inline]
fn classify_lmdb_leaf_page(page: &[u8; 4096], expected_pgno: usize) -> Option<(usize, u16)> {
    let mp_pgno = u64::from_le_bytes(page[0..8].try_into().unwrap()) as usize;
    if mp_pgno != expected_pgno {
        return None;
    }
    let mp_flags = u16::from_le_bytes([page[10], page[11]]);
    if mp_flags != 0x02 {
        // Must be P_LEAF (0x02), not P_BRANCH (0x01), P_OVERFLOW (0x04), P_META (0x08), or P_SUBP
        return None;
    }
    let mp_lower = u16::from_le_bytes([page[12], page[13]]) as usize;
    let mp_upper = u16::from_le_bytes([page[14], page[15]]) as usize;
    if mp_lower <= 16 || mp_lower > mp_upper || mp_upper > 4096 || ((mp_lower - 16) & 1) != 0 {
        return None;
    }
    let num_keys = (mp_lower - 16) >> 1;
    let ptr0 = u16::from_le_bytes([page[16], page[17]]) as usize;
    if ptr0 < mp_upper || ptr0 + 8 > 4096 {
        return None;
    }

    let lo = u16::from_le_bytes([page[ptr0], page[ptr0 + 1]]) as usize;
    let hi = u16::from_le_bytes([page[ptr0 + 2], page[ptr0 + 3]]) as usize;
    let dsize = lo | (hi << 16);
    let nflags = u16::from_le_bytes([page[ptr0 + 4], page[ptr0 + 5]]);
    let ksize = u16::from_le_bytes([page[ptr0 + 6], page[ptr0 + 7]]) as usize;
    if nflags != 0 || ptr0 + 8 + ksize + dsize > 4096 {
        return None;
    }

    let val_off = ptr0 + 8 + ksize;
    let table_ord = match (ksize, dsize) {
        (32, 129) => {
            if page[val_off + 128] > 4 {
                return None;
            }
            ORD_ACCOUNTS
        }
        (32, 225) | (32, 233) | (32, 267) => {
            let btype = page[val_off];
            let valid = matches!(
                (btype, dsize),
                (4, 225) | (2 | 3 | 5, 233) | (6, 267)
            );
            if !valid {
                return None;
            }
            ORD_BLOCKS
        }
        (32, 40) => ORD_CONF_HEIGHT,
        (32, 0) => ORD_PRUNED,
        (18, 8) => ORD_PEERS,
        (64, 49) => {
            if page[val_off + 48] > 4 {
                return None;
            }
            ORD_PENDING
        }
        (8, 16) => ORD_ONLINE_WEIGHT,
        (32, 16) => ORD_REP_WEIGHTS,
        _ => return None,
    };

    let first_shard = (page[ptr0 + 8] >> 4) as usize;
    let last_ptr_off = 16 + (num_keys - 1) * 2;
    let last_ptr = u16::from_le_bytes([page[last_ptr_off], page[last_ptr_off + 1]]) as usize;
    if last_ptr < mp_upper || last_ptr + 8 + ksize > 4096 {
        return None;
    }
    let last_shard = (page[last_ptr + 8] >> 4) as usize;

    let mut mask = (1u16 << first_shard) | (1u16 << last_shard);
    if first_shard != last_shard {
        for i in 1..(num_keys - 1) {
            let p_off = 16 + i * 2;
            let p = u16::from_le_bytes([page[p_off], page[p_off + 1]]) as usize;
            if p >= mp_upper && p + 8 + ksize <= 4096 {
                let s = (page[p + 8] >> 4) as usize;
                mask |= 1u16 << s;
            }
        }
    }
    Some((table_ord, mask))
}

#[derive(Clone, Copy)]
struct EntryRef32 {
    key: [u8; 32],
    pgno: u32,
    val_off: u16,
    val_len: u16,
}

#[derive(Clone, Copy)]
struct EntryRef64 {
    key: [u8; 64],
    pgno: u32,
    val_off: u16,
    val_len: u16,
}

/// Rotates the 32-byte `successor` inside a C++ Nano / RSNano LMDB block value
/// from `raw[block_end .. block_end + 32]` to the end (`out[len - 32 .. len]`),
/// matching `barely-nano`'s `BlockStore::put_raw` format (`SavedBlock` + 32B `successor`).
#[inline(always)]
fn convert_lmdb_block_value(raw: &[u8], out: &mut [u8; VALUE_SIZE]) -> Option<usize> {
    let len = raw.len();
    if len < 169 || len > VALUE_SIZE {
        return None;
    }
    let block_end = match (raw[0], len) {
        (2, 233) => 153, // LegacySend
        (3, 233) => 137, // LegacyReceive
        (4, 225) => 169, // LegacyOpen
        (5, 233) => 137, // LegacyChange
        (6, 267) => 217, // State
        _ => return None,
    };
    let sideband_rest = len - (block_end + 32);
    out[..block_end].copy_from_slice(&raw[..block_end]);
    out[block_end..block_end + sideband_rest]
        .copy_from_slice(&raw[block_end + 32..len]);
    out[len - 32..len].copy_from_slice(&raw[block_end..block_end + 32]);
    Some(len)
}

/// Compares two duplicate `EntryRef32` records for the same key across COW leaf pages,
/// returning `true` if `b` is newer than `a`.
#[inline]
fn is_newer_entry32(mmap: &MmapFile, table_ord: usize, a: &EntryRef32, b: &EntryRef32) -> bool {
    let pa = mmap.page(a.pgno as usize);
    let pb = mmap.page(b.pgno as usize);
    let va = &pa[a.val_off as usize..(a.val_off + a.val_len) as usize];
    let vb = &pb[b.val_off as usize..(b.val_off + b.val_len) as usize];

    match table_ord {
        ORD_ACCOUNTS if va.len() == 129 && vb.len() == 129 => {
            let mod_a = u64::from_le_bytes(va[112..120].try_into().unwrap());
            let mod_b = u64::from_le_bytes(vb[112..120].try_into().unwrap());
            if mod_a != mod_b {
                return mod_b > mod_a;
            }
            let bc_a = u64::from_le_bytes(va[120..128].try_into().unwrap());
            let bc_b = u64::from_le_bytes(vb[120..128].try_into().unwrap());
            if bc_a != bc_b {
                return bc_b > bc_a;
            }
            b.pgno > a.pgno
        }
        ORD_BLOCKS => {
            let block_end = match va[0] {
                2 => 153,
                3 | 5 => 137,
                4 => 169,
                6 => 217,
                _ => return b.pgno > a.pgno,
            };
            let zero_a = va[block_end..block_end + 32] == [0u8; 32];
            let zero_b = vb[block_end..block_end + 32] == [0u8; 32];
            if zero_a != zero_b {
                // Non-zero successor is always newer than zero successor
                return !zero_b;
            }
            b.pgno > a.pgno
        }
        ORD_CONF_HEIGHT if va.len() == 40 && vb.len() == 40 => {
            let ha = u64::from_le_bytes(va[0..8].try_into().unwrap());
            let hb = u64::from_le_bytes(vb[0..8].try_into().unwrap());
            if ha != hb {
                return hb > ha;
            }
            b.pgno > a.pgno
        }
        _ => b.pgno > a.pgno,
    }
}

// ── Global Confirmation Height Index & Block Pruning Classifier ──────────────

#[derive(Clone, Copy)]
struct ConfHeightEntry {
    account: [u8; 32],
    height: u64,
    frontier_prefix: u64,
}

struct ConfHeightIndex {
    /// 4,096 range-partitioned buckets indexed by `((account[0] as usize) << 4) | ((account[1] >> 4) as usize)`.
    /// Only retains accounts with `confirmed_height >= 2` (since accounts with `height <= 1` can never have any block pruned).
    buckets: Vec<Vec<ConfHeightEntry>>,
}

impl ConfHeightIndex {
    fn build(mmap: &MmapFile, shard_table_pages: &[Vec<Vec<u32>>]) -> (Self, u64, u64, u64) {
        #[derive(Clone, Copy)]
        struct RawConf {
            account: [u8; 32],
            height: u64,
            frontier_prefix: u64,
            pgno: u32,
        }

        // Process all 16 shards of ORD_CONF_HEIGHT in parallel (1 thread per shard -> 256 sub-buckets per shard = 4,096 buckets total)
        let shard_outputs: Vec<(Vec<Vec<ConfHeightEntry>>, u64, u64, u64)> =
            std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(SHARD_COUNT);
                for shard_idx in 0..SHARD_COUNT {
                    let pages = &shard_table_pages[shard_idx][ORD_CONF_HEIGHT];
                    handles.push(s.spawn(move || {
                        let mut raw_subs: Vec<Vec<RawConf>> =
                            (0..256).map(|_| Vec::new()).collect();
                        if pages.len() > 1_000 {
                            let est = (pages.len() * 20) / 256 + 32;
                            for b in &mut raw_subs {
                                b.reserve(est);
                            }
                        }

                        for &pgno in pages {
                            let p = mmap.page(pgno as usize);
                            let mp_lower = u16::from_le_bytes([p[12], p[13]]) as usize;
                            let mp_upper = u16::from_le_bytes([p[14], p[15]]) as usize;
                            let nk = (mp_lower - 16) >> 1;
                            for i in 0..nk {
                                let p_off = 16 + i * 2;
                                let ptr = u16::from_le_bytes([p[p_off], p[p_off + 1]]) as usize;
                                if ptr < mp_upper || ptr + 8 > 4096 {
                                    continue;
                                }
                                let lo = u16::from_le_bytes([p[ptr], p[ptr + 1]]) as usize;
                                let hi = u16::from_le_bytes([p[ptr + 2], p[ptr + 3]]) as usize;
                                let dsize = lo | (hi << 16);
                                let ksize =
                                    u16::from_le_bytes([p[ptr + 6], p[ptr + 7]]) as usize;
                                if ksize != 32 || dsize != 40 || ptr + 8 + 32 + 40 > 4096 {
                                    continue;
                                }
                                let kslice = &p[ptr + 8..ptr + 8 + 32];
                                if ((kslice[0] >> 4) as usize) != shard_idx {
                                    continue;
                                }
                                let vslice = &p[ptr + 8 + 32..ptr + 8 + 32 + 40];
                                let height =
                                    u64::from_le_bytes(vslice[0..8].try_into().unwrap());
                                let frontier_prefix =
                                    u64::from_be_bytes(vslice[8..16].try_into().unwrap());
                                let mut account = [0u8; 32];
                                account.copy_from_slice(kslice);
                                let sub_idx = (((account[0] & 0x0F) as usize) << 4)
                                    | ((account[1] >> 4) as usize);
                                raw_subs[sub_idx].push(RawConf {
                                    account,
                                    height,
                                    frontier_prefix,
                                    pgno,
                                });
                            }
                        }

                        let mut out_subs: Vec<Vec<ConfHeightEntry>> = Vec::with_capacity(256);
                        let mut shard_acc_cnt = 0u64;
                        let mut shard_multi_cnt = 0u64;
                        let mut shard_conf_sum = 0u64;

                        for mut b in raw_subs {
                            b.sort_unstable_by(|x, y| {
                                x.account
                                    .cmp(&y.account)
                                    .then_with(|| x.height.cmp(&y.height))
                                    .then_with(|| x.pgno.cmp(&y.pgno))
                            });
                            let mut compact = Vec::new();
                            let n = b.len();
                            let mut i = 0;
                            while i < n {
                                let mut best = b[i];
                                i += 1;
                                while i < n && b[i].account == best.account {
                                    best = b[i];
                                    i += 1;
                                }
                                shard_acc_cnt += 1;
                                shard_conf_sum += best.height;
                                if best.height >= 2 {
                                    shard_multi_cnt += 1;
                                    compact.push(ConfHeightEntry {
                                        account: best.account,
                                        height: best.height,
                                        frontier_prefix: best.frontier_prefix,
                                    });
                                }
                            }
                            compact.shrink_to_fit();
                            out_subs.push(compact);
                        }

                        (out_subs, shard_acc_cnt, shard_multi_cnt, shard_conf_sum)
                    }));
                }
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });

        let mut buckets = Vec::with_capacity(4096);
        let mut total_acc = 0u64;
        let mut total_multi = 0u64;
        let mut total_sum = 0u64;
        for (subs, acc, multi, sum) in shard_outputs {
            buckets.extend(subs);
            total_acc += acc;
            total_multi += multi;
            total_sum += sum;
        }
        (Self { buckets }, total_acc, total_multi, total_sum)
    }

    #[inline(always)]
    fn lookup(&self, account: &[u8; 32]) -> Option<(u64, u64)> {
        let b_idx = ((account[0] as usize) << 4) | ((account[1] >> 4) as usize);
        let bucket = &self.buckets[b_idx];
        bucket
            .binary_search_by(|e| e.account.cmp(account))
            .ok()
            .map(|idx| (bucket[idx].height, bucket[idx].frontier_prefix))
    }
}

/// Determines whether a block in `data.ldb` should be pruned (`true` -> move to `TABLE_PRUNED = 5`)
/// or kept in `TABLE_BLOCKS = 2` (`false`), matching RSNano / `barely-nano` pruning semantics.
#[inline]
fn should_prune_block(
    block_hash: &[u8; 32],
    raw: &[u8],
    conf_index: &ConfHeightIndex,
    max_depth: u64,
    cutoff_time_ms: u64,
) -> bool {
    if *block_hash == LIVE_GENESIS_HASH {
        return false;
    }
    let len = raw.len();
    let (block_end, acc_off, height, timestamp_ms) = match (raw[0], len) {
        (2, 233) => (
            153usize,
            185usize,
            u64::from_be_bytes(raw[217..225].try_into().unwrap()),
            u64::from_be_bytes(raw[225..233].try_into().unwrap()),
        ),
        (3 | 5, 233) => (
            137usize,
            169usize,
            u64::from_be_bytes(raw[201..209].try_into().unwrap()),
            u64::from_be_bytes(raw[225..233].try_into().unwrap()),
        ),
        (4, 225) => (
            169usize,
            65usize,
            1u64,
            u64::from_be_bytes(raw[217..225].try_into().unwrap()),
        ),
        (6, 267) => (
            217usize,
            1usize,
            u64::from_be_bytes(raw[249..257].try_into().unwrap()),
            u64::from_be_bytes(raw[257..265].try_into().unwrap()),
        ),
        _ => return false,
    };

    // Fast path: if `successor` in sideband is zero, this block is the head of its account chain.
    // The head block of an account is never below `confirmed_height` and can NEVER be pruned.
    if height == 0 || raw[block_end..block_end + 32] == [0u8; 32] {
        return false;
    }

    let account: &[u8; 32] = raw[acc_off..acc_off + 32].try_into().unwrap();
    if let Some((conf_height, frontier_prefix)) = conf_index.lookup(account) {
        if height < conf_height {
            let hash_prefix = u64::from_be_bytes(block_hash[0..8].try_into().unwrap());
            if hash_prefix != frontier_prefix {
                let beyond_depth = (conf_height - height) >= max_depth;
                let older_than_cutoff =
                    cutoff_time_ms == u64::MAX || timestamp_ms <= cutoff_time_ms;
                return beyond_depth && older_than_cutoff;
            }
        }
    }
    false
}

/// Processes a 32-byte-keyed table (`accounts`, `blocks`, `confirmation_height`, `rep_weights`, `peers`, `online_weight`)
/// for a single shard by partitioning into 256 sub-buckets, sorting in parallel across threads, deduplicating,
/// and streaming into `builder`.
fn process_table_32<S: PageSink>(
    mmap: &MmapFile,
    pages: &[u32],
    shard_idx: usize,
    table_ord: usize,
    table_id: u8,
    expected_ksize: usize,
    builder: &mut StreamingBTreeBuilder<S>,
    bloom: &mut BlockBloom,
    mut prune_ctx: Option<(&ConfHeightIndex, u64, u64, &mut Vec<[u8; 32]>)>,
    num_threads: usize,
) -> Result<(u64, u64), String> {
    if pages.is_empty() {
        return Ok((0, 0));
    }

    // 256 range-ordered sub-buckets within `shard_idx`:
    // Since `key[0] >> 4 == shard_idx`, `((key[0] & 0x0F) << 4) | (key[1] >> 4)` uniformly spans `0..256`!
    let mut sub_buckets: Vec<Vec<EntryRef32>> = (0..256).map(|_| Vec::new()).collect();
    if pages.len() > 10_000 {
        let avg_per_bucket = (pages.len() * 14) / 256 + 64;
        for b in &mut sub_buckets {
            b.reserve(avg_per_bucket);
        }
    }

    for &pgno in pages {
        let p = mmap.page(pgno as usize);
        let mp_lower = u16::from_le_bytes([p[12], p[13]]) as usize;
        let mp_upper = u16::from_le_bytes([p[14], p[15]]) as usize;
        let nk = (mp_lower - 16) >> 1;
        for i in 0..nk {
            let p_off = 16 + i * 2;
            let ptr = u16::from_le_bytes([p[p_off], p[p_off + 1]]) as usize;
            if ptr < mp_upper || ptr + 8 > 4096 {
                continue;
            }
            let lo = u16::from_le_bytes([p[ptr], p[ptr + 1]]) as usize;
            let hi = u16::from_le_bytes([p[ptr + 2], p[ptr + 3]]) as usize;
            let dsize = lo | (hi << 16);
            let ksize = u16::from_le_bytes([p[ptr + 6], p[ptr + 7]]) as usize;
            if ksize != expected_ksize || ptr + 8 + ksize + dsize > 4096 {
                continue;
            }
            let kslice = &p[ptr + 8..ptr + 8 + ksize];
            if ((kslice[0] >> 4) as usize) != shard_idx {
                continue;
            }
            let mut key = [0u8; 32];
            key[..ksize].copy_from_slice(kslice);
            let sub_idx = (((key[0] & 0x0F) as usize) << 4) | ((key[1] >> 4) as usize);
            sub_buckets[sub_idx].push(EntryRef32 {
                key,
                pgno,
                val_off: (ptr + 8 + ksize) as u16,
                val_len: dsize as u16,
            });
        }
    }

    // Parallel sort all 256 sub-buckets using std::thread::scope
    let next_bucket = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let mut rest = sub_buckets.as_mut_slice();
        let chunk_size = (256 + num_threads - 1) / num_threads.max(1);
        let _ = &next_bucket;
        for _ in 0..num_threads {
            if rest.is_empty() {
                break;
            }
            let (head, tail) = rest.split_at_mut(chunk_size.min(rest.len()));
            rest = tail;
            s.spawn(move || {
                for b in head {
                    b.sort_unstable_by(|x, y| x.key.cmp(&y.key).then_with(|| x.pgno.cmp(&y.pgno)));
                }
            });
        }
    });

    let mut count = 0u64;
    let mut sum_metric = 0u64;
    let mut full_key = [0u8; KEY_SIZE];
    full_key[0] = table_id;
    let mut block_buf = [0u8; VALUE_SIZE];

    for b in &sub_buckets {
        let n = b.len();
        let mut i = 0;
        while i < n {
            let mut best = b[i];
            i += 1;
            while i < n && b[i].key == best.key {
                if is_newer_entry32(mmap, table_ord, &best, &b[i]) {
                    best = b[i];
                }
                i += 1;
            }

            full_key[1..33].copy_from_slice(&best.key);
            let page = mmap.page(best.pgno as usize);
            let raw_val = &page[best.val_off as usize..(best.val_off + best.val_len) as usize];

            match table_ord {
                ORD_ACCOUNTS => {
                    if raw_val.len() != 129 {
                        continue;
                    }
                    let bc = u64::from_le_bytes(raw_val[120..128].try_into().unwrap());
                    sum_metric += bc;
                    count += 1;
                    builder.push(&full_key, raw_val)?;
                }
                ORD_BLOCKS => {
                    if let Some(vlen) = convert_lmdb_block_value(raw_val, &mut block_buf) {
                        bloom.insert(&best.key);
                        if let Some((conf_idx, max_depth, cutoff_ms, ref mut pruned_out)) =
                            prune_ctx
                        {
                            if should_prune_block(
                                &best.key,
                                raw_val,
                                conf_idx,
                                max_depth,
                                cutoff_ms,
                            ) {
                                pruned_out.push(best.key);
                                continue;
                            }
                        }
                        count += 1;
                        builder.push(&full_key, &block_buf[..vlen])?;
                    }
                }
                ORD_CONF_HEIGHT => {
                    if raw_val.len() != 40 {
                        continue;
                    }
                    let h = u64::from_le_bytes(raw_val[0..8].try_into().unwrap());
                    sum_metric += h;
                    count += 1;
                    builder.push(&full_key, raw_val)?;
                }
                _ => {
                    count += 1;
                    builder.push(&full_key, raw_val)?;
                }
            }
        }
    }

    Ok((count, sum_metric))
}

/// Streams `TABLE_PRUNED` (`TABLE_ID = 5`, 32-byte block hash key -> 0-byte value) for a single shard,
/// merging any pre-existing `ORD_PRUNED` leaf pages from `data.ldb` with newly pruned block hashes
/// (which are already sorted in ascending `[u8; 32]` order from `ORD_BLOCKS`).
fn process_pruned_table<S: PageSink>(
    mmap: &MmapFile,
    pages: &[u32],
    shard_idx: usize,
    newly_pruned: &[[u8; 32]],
    builder: &mut StreamingBTreeBuilder<S>,
    bloom: &mut BlockBloom,
    num_threads: usize,
) -> Result<u64, String> {
    if pages.is_empty() && newly_pruned.is_empty() {
        return Ok(0);
    }

    let mut existing: Vec<[u8; 32]> = Vec::new();
    if !pages.is_empty() {
        let mut sub_buckets: Vec<Vec<[u8; 32]>> = (0..256).map(|_| Vec::new()).collect();
        for &pgno in pages {
            let p = mmap.page(pgno as usize);
            let mp_lower = u16::from_le_bytes([p[12], p[13]]) as usize;
            let mp_upper = u16::from_le_bytes([p[14], p[15]]) as usize;
            let nk = (mp_lower - 16) >> 1;
            for i in 0..nk {
                let p_off = 16 + i * 2;
                let ptr = u16::from_le_bytes([p[p_off], p[p_off + 1]]) as usize;
                if ptr < mp_upper || ptr + 8 > 4096 {
                    continue;
                }
                let lo = u16::from_le_bytes([p[ptr], p[ptr + 1]]) as usize;
                let hi = u16::from_le_bytes([p[ptr + 2], p[ptr + 3]]) as usize;
                let dsize = lo | (hi << 16);
                let ksize = u16::from_le_bytes([p[ptr + 6], p[ptr + 7]]) as usize;
                if ksize != 32 || dsize != 0 || ptr + 8 + 32 > 4096 {
                    continue;
                }
                let kslice = &p[ptr + 8..ptr + 8 + 32];
                if ((kslice[0] >> 4) as usize) != shard_idx {
                    continue;
                }
                let mut key = [0u8; 32];
                key.copy_from_slice(kslice);
                let sub_idx = (((key[0] & 0x0F) as usize) << 4) | ((key[1] >> 4) as usize);
                sub_buckets[sub_idx].push(key);
            }
        }
        std::thread::scope(|s| {
            let mut rest = sub_buckets.as_mut_slice();
            let chunk_size = (256 + num_threads - 1) / num_threads.max(1);
            for _ in 0..num_threads {
                if rest.is_empty() {
                    break;
                }
                let (head, tail) = rest.split_at_mut(chunk_size.min(rest.len()));
                rest = tail;
                s.spawn(move || {
                    for b in head {
                        b.sort_unstable();
                        b.dedup();
                    }
                });
            }
        });
        for b in sub_buckets {
            existing.extend_from_slice(&b);
        }
    }

    let mut full_key = [0u8; KEY_SIZE];
    full_key[0] = TABLE_PRUNED;
    let mut count = 0u64;

    let mut i = 0usize;
    let mut j = 0usize;
    while i < existing.len() || j < newly_pruned.len() {
        let next_hash = match (existing.get(i), newly_pruned.get(j)) {
            (Some(&a), Some(&b)) => match a.cmp(&b) {
                Ordering::Less => {
                    i += 1;
                    bloom.insert(&a);
                    a
                }
                Ordering::Greater => {
                    j += 1;
                    b
                }
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                    a
                }
            },
            (Some(&a), None) => {
                i += 1;
                bloom.insert(&a);
                a
            }
            (None, Some(&b)) => {
                j += 1;
                b
            }
            (None, None) => unreachable!(),
        };
        full_key[1..33].copy_from_slice(&next_hash);
        builder.push(&full_key, &[])?;
        count += 1;
    }

    Ok(count)
}

/// Processes a 64-byte-keyed table (`pending`, `TABLE_ID = 7`) for a single shard.
fn process_table_64<S: PageSink>(
    mmap: &MmapFile,
    pages: &[u32],
    shard_idx: usize,
    table_id: u8,
    builder: &mut StreamingBTreeBuilder<S>,
    num_threads: usize,
) -> Result<u64, String> {
    if pages.is_empty() {
        return Ok(0);
    }

    let mut sub_buckets: Vec<Vec<EntryRef64>> = (0..256).map(|_| Vec::new()).collect();
    if pages.len() > 10_000 {
        let avg_per_bucket = (pages.len() * 14) / 256 + 64;
        for b in &mut sub_buckets {
            b.reserve(avg_per_bucket);
        }
    }

    for &pgno in pages {
        let p = mmap.page(pgno as usize);
        let mp_lower = u16::from_le_bytes([p[12], p[13]]) as usize;
        let mp_upper = u16::from_le_bytes([p[14], p[15]]) as usize;
        let nk = (mp_lower - 16) >> 1;
        for i in 0..nk {
            let p_off = 16 + i * 2;
            let ptr = u16::from_le_bytes([p[p_off], p[p_off + 1]]) as usize;
            if ptr < mp_upper || ptr + 8 > 4096 {
                continue;
            }
            let lo = u16::from_le_bytes([p[ptr], p[ptr + 1]]) as usize;
            let hi = u16::from_le_bytes([p[ptr + 2], p[ptr + 3]]) as usize;
            let dsize = lo | (hi << 16);
            let ksize = u16::from_le_bytes([p[ptr + 6], p[ptr + 7]]) as usize;
            if ksize != 64 || dsize != 49 || ptr + 8 + 64 + 49 > 4096 {
                continue;
            }
            let kslice = &p[ptr + 8..ptr + 8 + 64];
            if ((kslice[0] >> 4) as usize) != shard_idx {
                continue;
            }
            let mut key = [0u8; 64];
            key.copy_from_slice(kslice);
            let sub_idx = (((key[0] & 0x0F) as usize) << 4) | ((key[1] >> 4) as usize);
            sub_buckets[sub_idx].push(EntryRef64 {
                key,
                pgno,
                val_off: (ptr + 8 + 64) as u16,
                val_len: 49,
            });
        }
    }

    std::thread::scope(|s| {
        let mut rest = sub_buckets.as_mut_slice();
        let chunk_size = (256 + num_threads - 1) / num_threads.max(1);
        for _ in 0..num_threads {
            if rest.is_empty() {
                break;
            }
            let (head, tail) = rest.split_at_mut(chunk_size.min(rest.len()));
            rest = tail;
            s.spawn(move || {
                for b in head {
                    b.sort_unstable_by(|x, y| x.key.cmp(&y.key).then_with(|| x.pgno.cmp(&y.pgno)));
                }
            });
        }
    });

    let mut count = 0u64;
    let mut full_key = [0u8; KEY_SIZE];
    full_key[0] = table_id;

    for b in &sub_buckets {
        let n = b.len();
        let mut i = 0;
        while i < n {
            let mut best = b[i];
            i += 1;
            while i < n && b[i].key == best.key {
                if b[i].pgno > best.pgno {
                    best = b[i];
                }
                i += 1;
            }
            full_key[1..65].copy_from_slice(&best.key);
            let page = mmap.page(best.pgno as usize);
            let raw_val = &page[best.val_off as usize..(best.val_off + best.val_len) as usize];
            count += 1;
            builder.push(&full_key, raw_val)?;
        }
    }

    Ok(count)
}

fn query_device_bytes(file: &mut File) -> Result<u64, String> {
    let mut size_bytes: u64 = 0;
    let rc = unsafe { ioctl(file.as_raw_fd(), BLKGETSIZE64, &mut size_bytes as *mut u64) };
    if rc == 0 && size_bytes > 0 {
        return Ok(size_bytes);
    }
    file.seek(SeekFrom::End(0))
        .map_err(|e| format!("Failed to determine target device/file size: {}", e))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("\n[ERROR] {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    let mut ldb_path = String::from("database/data.ldb");
    let mut nvme_path = String::from("/dev/sda");
    let mut dry_run = false;
    let mut prune = false;
    let mut prune_max_depth: Option<u64> = None;
    let mut prune_age_secs: Option<u64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--ldb" | "-i" => {
                i += 1;
                ldb_path = args.get(i).cloned().ok_or("Missing value for --ldb")?;
            }
            "--nvme" | "--dev" | "-o" => {
                i += 1;
                nvme_path = args.get(i).cloned().ok_or("Missing value for --nvme")?;
            }
            "--prune" | "--pruned" | "--prunned" | "-p" => {
                prune = true;
            }
            "--prune-depth" => {
                i += 1;
                let val_str = args.get(i).ok_or("Missing value for --prune-depth")?;
                let d: u64 = val_str
                    .parse()
                    .map_err(|_| format!("Invalid --prune-depth value: {}", val_str))?;
                prune = true;
                prune_max_depth = Some(d.max(1));
            }
            "--prune-age" => {
                i += 1;
                let val_str = args.get(i).ok_or("Missing value for --prune-age")?;
                let s: u64 = val_str
                    .parse()
                    .map_err(|_| format!("Invalid --prune-age value: {}", val_str))?;
                prune = true;
                prune_age_secs = Some(s);
            }
            "--dry-run" => {
                dry_run = true;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: {} [--ldb <database/data.ldb>] [--nvme </dev/sda>] [--prune] [--prune-depth <1>] [--prune-age <secs>] [--dry-run]",
                    args[0]
                );
                return Ok(());
            }
            other if !other.starts_with('-') && i == 1 => {
                ldb_path = other.to_string();
            }
            other if !other.starts_with('-') && i == 2 => {
                nvme_path = other.to_string();
            }
            other => {
                return Err(format!("Unknown argument: {}", other));
            }
        }
        i += 1;
    }

    let effective_max_depth = match (prune_max_depth, prune_age_secs) {
        (Some(d), _) => d,
        (None, Some(_)) => 1,
        (None, None) => 1,
    };
    let cutoff_time_ms = if let Some(age_s) = prune_age_secs {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        now_ms.saturating_sub(age_s.saturating_mul(1000))
    } else {
        u64::MAX
    };

    println!("==================================================================");
    println!(" Barely-Nano Direct LMDB -> NVMe B-Tree Migration Tool");
    println!("==================================================================");
    println!("  Source LMDB : {}", ldb_path);
    println!(
        "  Target NVMe : {}{}",
        nvme_path,
        if dry_run { " (DRY RUN)" } else { "" }
    );
    if prune {
        println!(
            "  Mode        : PRUNED (max_depth={}, cutoff={})",
            effective_max_depth,
            if let Some(s) = prune_age_secs {
                format!("{}s ago", s)
            } else {
                "all confirmed history below max_depth".to_string()
            }
        );
    } else {
        println!("  Mode        : FULL LEDGER (unpruned)");
    }

    let t0 = Instant::now();
    print!("[1/5] Running B-Tree slotted-page builder self-test... ");
    std::io::stdout().flush().ok();
    verify_btree_builder_self_test()?;
    println!("OK ({:.1} ms)", t0.elapsed().as_secs_f64() * 1000.0);

    // Open source LMDB
    let mmap = MmapFile::open(&ldb_path)?;
    let total_lmdb_pages = mmap.len / BLOCK_SIZE;
    println!(
        "[2/5] Mapped {} ({:.2} GB, {} 4KB pages)",
        ldb_path,
        mmap.len as f64 / (1024.0 * 1024.0 * 1024.0),
        total_lmdb_pages
    );

    // Open target NVMe block device (unless dry_run)
    let (nvme_file, shard_size, wal_base, wal_pages, wal_epoch) = if !dry_run {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&nvme_path)
            .map_err(|e| {
                format!(
                    "Failed to open {} for read/write ({}). Try running with sudo!",
                    nvme_path, e
                )
            })?;
        let dev_bytes = query_device_bytes(&mut f)?;
        let total_sectors = dev_bytes / SECTOR_SIZE;
        if total_sectors <= STORAGE_BASE_LBA + 1024 {
            return Err(format!(
                "Target device {} is too small ({} bytes)",
                nvme_path, dev_bytes
            ));
        }
        let capacity_sectors = total_sectors - STORAGE_BASE_LBA;
        let total_pages = (capacity_sectors / 8) as usize;
        let wal_pages = std::cmp::min(WAL_MAX_PAGES, (total_pages / 8) & !1);
        let shard_size = (total_pages - 1 - wal_pages) / SHARD_COUNT - 1;
        let wal_base = 1 + SHARD_COUNT * (shard_size + 1);

        // Check existing header on NVMe to increment wal_epoch cleanly
        let mut hdr = [0u8; BLOCK_SIZE];
        let _ = f.read_exact_at(&mut hdr, STORAGE_BASE_LBA * SECTOR_SIZE);
        let existing_magic = u64::from_le_bytes(hdr[0..8].try_into().unwrap());
        let existing_epoch = if existing_magic == STORAGE_MAGIC {
            u64::from_le_bytes(hdr[24..32].try_into().unwrap())
        } else {
            0
        };
        let wal_epoch = existing_epoch.wrapping_add(1).max(1);

        println!(
            "      Target NVMe capacity: {:.2} GB ({} sectors, {} storage pages)",
            dev_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            total_sectors,
            total_pages
        );
        println!(
            "      Layout: 16 shards x {} pages ({:.2} GB/shard), WAL base={} pages={} epoch={}",
            shard_size,
            (shard_size as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
            wal_base,
            wal_pages,
            wal_epoch
        );
        println!(
            "      Wallet region at LBA {} (384 MB) will NOT be modified.",
            786_432
        );
        (Some(f), shard_size, wal_base, wal_pages, wal_epoch)
    } else {
        let shard_size = 15_615_908usize;
        let wal_pages = WAL_MAX_PAGES;
        let wal_base = 1 + SHARD_COUNT * (shard_size + 1);
        (None, shard_size, wal_base, wal_pages, 1u64)
    };

    // Pass 1: Parallel sequential leaf-page scan of data.ldb
    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .max(1);
    println!(
        "[3/5] Scanning {} LMDB pages across {} threads...",
        total_lmdb_pages, num_threads
    );
    let scan_start = Instant::now();

    let mut shard_table_pages: Vec<Vec<Vec<u32>>> = (0..SHARD_COUNT)
        .map(|_| (0..NUM_TABLES).map(|_| Vec::new()).collect())
        .collect();

    let chunk_pages = (total_lmdb_pages + num_threads - 1) / num_threads;
    let scanned_counter = AtomicUsize::new(0);

    let thread_results: Vec<Vec<Vec<Vec<u32>>>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(num_threads);
        for tid in 0..num_threads {
            let start_pg = (tid * chunk_pages).max(2);
            let end_pg = ((tid + 1) * chunk_pages).min(total_lmdb_pages);
            let mmap_ref = &mmap;
            let counter_ref = &scanned_counter;
            handles.push(s.spawn(move || {
                let mut local: Vec<Vec<Vec<u32>>> = (0..SHARD_COUNT)
                    .map(|_| (0..NUM_TABLES).map(|_| Vec::new()).collect())
                    .collect();
                if start_pg >= end_pg {
                    return local;
                }
                let mut batch = 0usize;
                for pgno in start_pg..end_pg {
                    let page = mmap_ref.page(pgno);
                    if let Some((t_ord, mut mask)) = classify_lmdb_leaf_page(page, pgno) {
                        while mask != 0 {
                            let s_idx = mask.trailing_zeros() as usize;
                            mask &= mask - 1;
                            local[s_idx][t_ord].push(pgno as u32);
                        }
                    }
                    batch += 1;
                    if batch == 262_144 {
                        let done =
                            counter_ref.fetch_add(batch, AtomicOrdering::Relaxed) + batch;
                        batch = 0;
                        if tid == 0 {
                            let pct = (done as f64 * 100.0) / (total_lmdb_pages as f64);
                            let elapsed = scan_start.elapsed().as_secs_f64();
                            let gbps = (done as f64 * 4096.0)
                                / (1024.0 * 1024.0 * 1024.0 * elapsed.max(0.001));
                            print!(
                                "\r      Pass 1 progress: {:5.1}% ({}/{} pages, {:.2} GB/s)   ",
                                pct, done, total_lmdb_pages, gbps
                            );
                            std::io::stdout().flush().ok();
                        }
                    }
                }
                if batch > 0 {
                    counter_ref.fetch_add(batch, AtomicOrdering::Relaxed);
                }
                local
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for t_local in thread_results {
        for s_idx in 0..SHARD_COUNT {
            for t_ord in 0..NUM_TABLES {
                shard_table_pages[s_idx][t_ord].extend_from_slice(&t_local[s_idx][t_ord]);
            }
        }
    }

    let mut leaf_counts = [0usize; NUM_TABLES];
    for s_idx in 0..SHARD_COUNT {
        for t_ord in 0..NUM_TABLES {
            leaf_counts[t_ord] += shard_table_pages[s_idx][t_ord].len();
        }
    }
    println!(
        "\r      Pass 1 complete in {:.2}s! Classified leaf pages: accounts={} blocks={} conf_height={} pruned={} pending={} rep_weights={} peers={} online_weight={}",
        scan_start.elapsed().as_secs_f64(),
        leaf_counts[ORD_ACCOUNTS],
        leaf_counts[ORD_BLOCKS],
        leaf_counts[ORD_CONF_HEIGHT],
        leaf_counts[ORD_PRUNED],
        leaf_counts[ORD_PENDING],
        leaf_counts[ORD_REP_WEIGHTS],
        leaf_counts[ORD_PEERS],
        leaf_counts[ORD_ONLINE_WEIGHT],
    );

    let conf_index = if prune {
        let idx_t0 = Instant::now();
        print!("      Building global confirmation_height index across 16 shards... ");
        std::io::stdout().flush().ok();
        let (idx, total_conf_acc, multi_acc, conf_sum) =
            ConfHeightIndex::build(&mmap, &shard_table_pages);
        println!(
            "OK in {:.2}s ({} confirmed accounts, {} multi-block prunable accounts, {} total confirmed blocks)",
            idx_t0.elapsed().as_secs_f64(),
            total_conf_acc,
            multi_acc,
            conf_sum
        );
        Some(idx)
    } else {
        None
    };

    if dry_run {
        println!("[DRY RUN] Skipping NVMe writes.");
        return Ok(());
    }

    let file = nvme_file.as_ref().unwrap();

    // Pass 2: Build and write all 16 shards sequentially, processing Shard 4 last so that
    // TABLE_VERSION (11, key b"CACHCNTS" which hashes to Shard 4) receives the exact global counts!
    println!("[4/5] Building & writing 16 Slotted-Page B-Tree shards + 64 MB Block Bloom Filter...");
    let build_start = Instant::now();

    let mut bloom = BlockBloom::new();
    let mut global_account_count = 0u64;
    let mut global_account_block_sum = 0u64;
    let mut global_block_count = 0u64;
    let mut global_pruned_count = 0u64;
    let mut global_confirmed_count = 0u64;
    let mut global_pending_count = 0u64;
    let mut global_rep_weight_count = 0u64;
    let mut total_btree_pages_written = 0u64;

    let shard_order: [usize; SHARD_COUNT] = [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 4];

    for (step_idx, &shard_idx) in shard_order.iter().enumerate() {
        let shard_t0 = Instant::now();
        let base_page = 1 + shard_idx * (shard_size + 1);
        let shard_byte_offset =
            (STORAGE_BASE_LBA + (base_page as u64) * 8) * SECTOR_SIZE;

        let sink = NvmeShardSink::new(file, shard_byte_offset, shard_size as u32);
        let mut builder = StreamingBTreeBuilder::new(sink);

        // 1. TABLE_ACCOUNTS (1)
        let (acc_cnt, acc_blk_sum) = process_table_32(
            &mmap,
            &shard_table_pages[shard_idx][ORD_ACCOUNTS],
            shard_idx,
            ORD_ACCOUNTS,
            TABLE_ACCOUNTS,
            32,
            &mut builder,
            &mut bloom,
            None,
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_ACCOUNTS] = Vec::new();
        global_account_count += acc_cnt;
        global_account_block_sum += acc_blk_sum;

        // 2. TABLE_BLOCKS (2)
        let mut newly_pruned_hashes: Vec<[u8; 32]> = Vec::new();
        let (blk_cnt, _) = process_table_32(
            &mmap,
            &shard_table_pages[shard_idx][ORD_BLOCKS],
            shard_idx,
            ORD_BLOCKS,
            TABLE_BLOCKS,
            32,
            &mut builder,
            &mut bloom,
            conf_index.as_ref().map(|idx| {
                (
                    idx,
                    effective_max_depth,
                    cutoff_time_ms,
                    &mut newly_pruned_hashes,
                )
            }),
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_BLOCKS] = Vec::new();
        global_block_count += blk_cnt;

        // 3. TABLE_CONF_HEIGHT (3)
        let (_ch_cnt, conf_sum) = process_table_32(
            &mmap,
            &shard_table_pages[shard_idx][ORD_CONF_HEIGHT],
            shard_idx,
            ORD_CONF_HEIGHT,
            TABLE_CONF_HEIGHT,
            32,
            &mut builder,
            &mut bloom,
            None,
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_CONF_HEIGHT] = Vec::new();
        global_confirmed_count += conf_sum;

        // 4. TABLE_PRUNED (5)
        let shard_pruned_cnt = process_pruned_table(
            &mmap,
            &shard_table_pages[shard_idx][ORD_PRUNED],
            shard_idx,
            &newly_pruned_hashes,
            &mut builder,
            &mut bloom,
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_PRUNED] = Vec::new();
        drop(newly_pruned_hashes);
        global_pruned_count += shard_pruned_cnt;

        // 5. TABLE_PEERS (6)
        let _ = process_table_32(
            &mmap,
            &shard_table_pages[shard_idx][ORD_PEERS],
            shard_idx,
            ORD_PEERS,
            TABLE_PEERS,
            18,
            &mut builder,
            &mut bloom,
            None,
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_PEERS] = Vec::new();

        // 6. TABLE_PENDING (7)
        let pend_cnt = process_table_64(
            &mmap,
            &shard_table_pages[shard_idx][ORD_PENDING],
            shard_idx,
            TABLE_PENDING,
            &mut builder,
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_PENDING] = Vec::new();
        global_pending_count += pend_cnt;

        // 7. TABLE_ONLINE_WEIGHT (9)
        let _ = process_table_32(
            &mmap,
            &shard_table_pages[shard_idx][ORD_ONLINE_WEIGHT],
            shard_idx,
            ORD_ONLINE_WEIGHT,
            TABLE_ONLINE_WEIGHT,
            8,
            &mut builder,
            &mut bloom,
            None,
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_ONLINE_WEIGHT] = Vec::new();

        // 8. TABLE_VERSION (11) - only in Shard 4 (since b'C' >> 4 == 4), which runs last!
        if shard_idx == 4 {
            let mut counts_key = [0u8; KEY_SIZE];
            counts_key[0] = TABLE_VERSION;
            counts_key[1..9].copy_from_slice(b"CACHCNTS");

            // In barely-nano / RSNano, `cache.block_count` is total blocks (`unpruned + pruned`),
            // while `store.block.count()` is `cache.block_count - cache.pruned_count`.
            let total_blocks_with_pruned = global_block_count + global_pruned_count;
            let mut counts_val = [0u8; 40];
            counts_val[0..8].copy_from_slice(&COUNTS_MAGIC.to_le_bytes());
            counts_val[8..16].copy_from_slice(&global_account_count.to_le_bytes());
            counts_val[16..24].copy_from_slice(&total_blocks_with_pruned.to_le_bytes());
            counts_val[24..32].copy_from_slice(&global_confirmed_count.to_le_bytes());
            counts_val[32..40].copy_from_slice(&global_pruned_count.to_le_bytes());
            builder.push(&counts_key, &counts_val)?;
        }

        // 9. TABLE_REP_WEIGHTS (12)
        let (rw_cnt, _) = process_table_32(
            &mmap,
            &shard_table_pages[shard_idx][ORD_REP_WEIGHTS],
            shard_idx,
            ORD_REP_WEIGHTS,
            TABLE_REP_WEIGHTS,
            32,
            &mut builder,
            &mut bloom,
            None,
            num_threads,
        )?;
        shard_table_pages[shard_idx][ORD_REP_WEIGHTS] = Vec::new();
        global_rep_weight_count += rw_cnt;

        let entries_in_shard = builder.total_entries;
        let height = builder.levels.len();
        let (root_page_id, next_free_page, mut sink) = builder.finalize()?;
        sink.write_meta(root_page_id, next_free_page)?;
        total_btree_pages_written += next_free_page as u64;

        if prune {
            println!(
                "      [{:2}/16] Shard {:2}: {:9} entries (kept_blk={:8}, pruned={:9}) -> {:7} pages ({:5.2} GB, depth={}, root={}) in {:.2}s",
                step_idx + 1,
                shard_idx,
                entries_in_shard,
                blk_cnt,
                shard_pruned_cnt,
                next_free_page,
                (next_free_page as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
                height,
                root_page_id,
                shard_t0.elapsed().as_secs_f64()
            );
        } else {
            println!(
                "      [{:2}/16] Shard {:2}: {:9} entries -> {:7} pages ({:5.2} GB, depth={}, root={}) in {:.2}s",
                step_idx + 1,
                shard_idx,
                entries_in_shard,
                next_free_page,
                (next_free_page as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
                height,
                root_page_id,
                shard_t0.elapsed().as_secs_f64()
            );
        }
    }

    // Write 64 MB Block Bloom Filter, zero WAL headers, and write main storage header
    println!("[5/5] Writing 64 MB Block Bloom Filter, clearing WAL headers, and writing STORAGE_MAGIC...");
    bloom.write_to_nvme(file)?;

    // Zero both WAL area headers so wal::recover sees no batch to replay
    let zero_page = [0u8; BLOCK_SIZE];
    let wal0_off = (STORAGE_BASE_LBA + (wal_base as u64) * 8) * SECTOR_SIZE;
    let wal1_off =
        (STORAGE_BASE_LBA + ((wal_base + wal_pages / 2) as u64) * 8) * SECTOR_SIZE;
    file.write_all_at(&zero_page, wal0_off)
        .map_err(|e| format!("Failed to zero WAL area 0: {}", e))?;
    file.write_all_at(&zero_page, wal1_off)
        .map_err(|e| format!("Failed to zero WAL area 1: {}", e))?;

    // Write HEADER_PAGE (block 0 at STORAGE_BASE_LBA)
    let mut header = [0u8; BLOCK_SIZE];
    header[0..8].copy_from_slice(&STORAGE_MAGIC.to_le_bytes());
    header[8..16].copy_from_slice(&(shard_size as u64).to_le_bytes());
    header[16..20].copy_from_slice(&(SHARD_COUNT as u32).to_le_bytes());
    header[24..32].copy_from_slice(&wal_epoch.to_le_bytes());
    header[32..40].copy_from_slice(&(wal_base as u64).to_le_bytes());
    header[40..48].copy_from_slice(&(wal_pages as u64).to_le_bytes());
    file.write_all_at(&header, STORAGE_BASE_LBA * SECTOR_SIZE)
        .map_err(|e| format!("Failed to write main storage header: {}", e))?;

    file.sync_all()
        .map_err(|e| format!("fsync failed on {}: {}", nvme_path, e))?;

    println!("==================================================================");
    println!(" Migration Completed Successfully in {:.2}s!", t0.elapsed().as_secs_f64());
    println!("------------------------------------------------------------------");
    println!("  Accounts migrated        : {:12}", global_account_count);
    println!(
        "  Full blocks migrated     : {:12} (account block_count sum: {})",
        global_block_count, global_account_block_sum
    );
    println!("  Pruned blocks recorded   : {:12}", global_pruned_count);
    println!(
        "  Total ledger blocks      : {:12}",
        global_block_count + global_pruned_count
    );
    println!("  Confirmed blocks sum     : {:12}", global_confirmed_count);
    println!("  Pending entries migrated : {:12}", global_pending_count);
    println!("  Rep weights migrated     : {:12}", global_rep_weight_count);
    println!(
        "  Total B-Tree pages       : {:12} ({:.2} GB across 16 shards)",
        total_btree_pages_written,
        (total_btree_pages_written as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0)
    );
    println!(
        "  Build & NVMe write time  : {:12.2}s",
        build_start.elapsed().as_secs_f64()
    );
    println!("==================================================================");

    Ok(())
}
