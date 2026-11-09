use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    BlockHash, SavedBlock,
};
use kernel_common::storage::{
    SHARDS, get_shard_index, SHARD_COUNT, BLOOM_HEADER_LBA, BLOOM_DATA_LBA, StorageContext, StorageError,
};
use kernel_common::storage::btree::KEY_SIZE;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::LedgerCache;

#[cfg(not(test))]
const BLOOM_BUCKETS: usize = 1 << 21; // 2,097,152 4-word buckets = 8,388,608 u64s = 64 MB
#[cfg(test)]
const BLOOM_BUCKETS: usize = 1 << 10; // 1,024 4-word buckets = 32 KB

const CHUNK_WORDS: usize = 16_384; // 16,384 u64s = 131,072 bytes = 128 KB (256 sectors)
const CHUNK_BYTES: usize = CHUNK_WORDS * 8;
const DIRTY_GROUPS: usize = 8; // 8 * 64 = 512 chunks of 128 KB = 64 MB
const BLOOM_MAGIC: u64 = 0x424C_4F4D_5632_0006; // "BLOMV2\0\x06"
const BLOOM_DELTA_LBA: u64 = BLOOM_DATA_LBA + (64 * 1024 * 1024 / 512); // right after 64 MB bloom data
const DELTA_ENTRIES_PER_PAGE: usize = 512; // 512 * 8 bytes = 4,096 bytes (1 AlignedBlock)
#[cfg(not(test))]
const MAX_DELTA_PAGES: usize = 256; // 256 * 512 = 131,072 block insertions (1 MB) before chunk compaction
#[cfg(test)]
const MAX_DELTA_PAGES: usize = 4;

struct BloomDelta {
    pages: Box<[kernel_common::storage::AlignedBlock]>,
    entry_count: usize,
    flushed_full_pages: usize,
}

impl BloomDelta {
    fn new() -> Self {
        let layout = alloc::alloc::Layout::from_size_align(
            MAX_DELTA_PAGES * core::mem::size_of::<kernel_common::storage::AlignedBlock>(),
            64,
        )
        .expect("valid bloom delta layout");
        // SAFETY: `layout` has non-zero size (`MAX_DELTA_PAGES * 4096`) and 64-byte alignment;
        // `AlignedBlock` is `#[repr(align(64))] struct { bytes: [u8; 4096] }`, for which all-zeros is valid.
        let pages = unsafe {
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut kernel_common::storage::AlignedBlock;
            if ptr.is_null() {
                alloc::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(core::slice::from_raw_parts_mut(ptr, MAX_DELTA_PAGES))
        };
        Self {
            pages,
            entry_count: 0,
            flushed_full_pages: 0,
        }
    }

    fn reset(&mut self) {
        let used_pages = ((self.entry_count + DELTA_ENTRIES_PER_PAGE - 1) / DELTA_ENTRIES_PER_PAGE)
            .min(self.pages.len());
        for p in &mut self.pages[..used_pages] {
            p.bytes.fill(0);
        }
        self.entry_count = 0;
        self.flushed_full_pages = 0;
    }
}

struct BlockBloomFilter {
    words: Box<[AtomicU64]>,
    dirty_chunks: [AtomicU64; DIRTY_GROUPS],
    delta: spin::Mutex<BloomDelta>,
}

impl BlockBloomFilter {
    fn new() -> Self {
        let len = BLOOM_BUCKETS * 4;
        let layout = alloc::alloc::Layout::from_size_align(
            len * core::mem::size_of::<AtomicU64>(),
            64,
        )
        .expect("valid bloom layout");
        // SAFETY: `layout` has non-zero size (`len * 8`) and 64-byte alignment; `AtomicU64` has the
        // exact same in-memory representation as `u64`, for which all-zeros is valid.
        let words = unsafe {
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut AtomicU64;
            if ptr.is_null() {
                alloc::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(core::slice::from_raw_parts_mut(ptr, len))
        };
        Self {
            words,
            dirty_chunks: [const { AtomicU64::new(0) }; DIRTY_GROUPS],
            delta: spin::Mutex::new(BloomDelta::new()),
        }
    }

    #[inline]
    fn probes(hash_bytes: &[u8; 32]) -> (usize, u64, u64, u64, u64) {
        let w0 = u64::from_le_bytes(hash_bytes[0..8].try_into().unwrap());
        let w1 = u64::from_le_bytes(hash_bytes[8..16].try_into().unwrap());
        let base = ((w0 as usize) & (BLOOM_BUCKETS - 1)) * 4;
        let m0 = (1u64 << (w1 & 63)) | (1u64 << ((w1 >> 6) & 63));
        let m1 = (1u64 << ((w1 >> 12) & 63)) | (1u64 << ((w1 >> 18) & 63));
        let m2 = 1u64 << ((w1 >> 24) & 63);
        let m3 = 1u64 << ((w1 >> 30) & 63);
        (base, m0, m1, m2, m3)
    }

    #[inline]
    fn pack_probe(hash_bytes: &[u8; 32]) -> u64 {
        let w0 = u64::from_le_bytes(hash_bytes[0..8].try_into().unwrap());
        let w1 = u64::from_le_bytes(hash_bytes[8..16].try_into().unwrap());
        let bucket = (w0 as usize) & (BLOOM_BUCKETS - 1);
        (1u64 << 63) | ((bucket as u64) << 36) | (w1 & 0xF_FFFF_FFFF)
    }

    #[inline]
    fn unpack_probe(packed: u64) -> (usize, u64, u64, u64, u64) {
        let bucket = ((packed >> 36) as usize) & (BLOOM_BUCKETS - 1);
        let base = bucket * 4;
        let w1 = packed & 0xF_FFFF_FFFF;
        let m0 = (1u64 << (w1 & 63)) | (1u64 << ((w1 >> 6) & 63));
        let m1 = (1u64 << ((w1 >> 12) & 63)) | (1u64 << ((w1 >> 18) & 63));
        let m2 = 1u64 << ((w1 >> 24) & 63);
        let m3 = 1u64 << ((w1 >> 30) & 63);
        (base, m0, m1, m2, m3)
    }

    #[inline]
    fn insert(&self, hash_bytes: &[u8; 32]) {
        let (base, m0, m1, m2, m3) = Self::probes(hash_bytes);
        let p0 = self.words[base].fetch_or(m0, Ordering::Relaxed);
        let p1 = self.words[base + 1].fetch_or(m1, Ordering::Relaxed);
        let p2 = self.words[base + 2].fetch_or(m2, Ordering::Relaxed);
        let p3 = self.words[base + 3].fetch_or(m3, Ordering::Relaxed);
        if (p0 & m0 != m0) || (p1 & m1 != m1) || (p2 & m2 != m2) || (p3 & m3 != m3) {
            let chunk = base / CHUNK_WORDS;
            self.dirty_chunks[chunk / 64].fetch_or(1u64 << (chunk % 64), Ordering::Relaxed);
            if BLOOM_PERSISTED.load(Ordering::Relaxed) {
                let packed = Self::pack_probe(hash_bytes);
                let mut delta = self.delta.lock();
                let idx = delta.entry_count;
                let page_idx = idx / DELTA_ENTRIES_PER_PAGE;
                let slot_off = (idx % DELTA_ENTRIES_PER_PAGE) * 8;
                if page_idx < delta.pages.len() {
                    delta.pages[page_idx].bytes[slot_off..slot_off + 8]
                        .copy_from_slice(&packed.to_le_bytes());
                }
                delta.entry_count = idx + 1;
            }
        }
    }

    #[inline]
    fn may_contain(&self, hash_bytes: &[u8; 32]) -> bool {
        let (base, m0, m1, m2, m3) = Self::probes(hash_bytes);
        (self.words[base].load(Ordering::Relaxed) & m0 == m0)
            && (self.words[base + 1].load(Ordering::Relaxed) & m1 == m1)
            && (self.words[base + 2].load(Ordering::Relaxed) & m2 == m2)
            && (self.words[base + 3].load(Ordering::Relaxed) & m3 == m3)
    }

    #[inline]
    fn prefetch(&self, hash_bytes: &[u8; 32]) {
        let w0 = u64::from_le_bytes(hash_bytes[0..8].try_into().unwrap());
        let base = ((w0 as usize) & (BLOOM_BUCKETS - 1)) * 4;
        // SAFETY: `base < BLOOM_BUCKETS * 4 == self.words.len()`.
        let ptr = unsafe { self.words.as_ptr().add(base) as *const u8 };
        kernel_common::storage::prefetch_read(ptr);
    }

    fn clear_dirty(&self) {
        for g in self.dirty_chunks.iter() {
            g.store(0, Ordering::Relaxed);
        }
    }

    fn clear(&self) {
        for w in self.words.iter() {
            w.store(0, Ordering::Relaxed);
        }
        self.clear_dirty();
        self.delta.lock().reset();
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: `self.words` is a contiguous 64-byte-aligned slice of `AtomicU64` (`len * 8` bytes).
        unsafe {
            core::slice::from_raw_parts(self.words.as_ptr() as *const u8, self.words.len() * 8)
        }
    }

    fn load_from_nvme(&self) -> bool {
        let Some(storage) = kernel_common::storage::GLOBAL_STORAGE.lock().clone() else {
            return false;
        };
        let mut header = kernel_common::storage::AlignedBlock::new();
        if storage.lock().read_raw_lba(BLOOM_HEADER_LBA, &mut header.bytes).is_err() {
            return false;
        }
        let magic = u64::from_le_bytes(header.bytes[0..8].try_into().unwrap());
        let buckets = u64::from_le_bytes(header.bytes[8..16].try_into().unwrap()) as usize;
        let delta_pages = (u64::from_le_bytes(header.bytes[16..24].try_into().unwrap()) as usize)
            .min(MAX_DELTA_PAGES);
        if magic != BLOOM_MAGIC || buckets != BLOOM_BUCKETS {
            return false;
        }
        let total_bytes = self.words.len() * 8;
        let num_chunks = total_bytes / CHUNK_BYTES;
        if num_chunks == 0 {
            return false;
        }
        for chunk_idx in 0..num_chunks {
            if chunk_idx % 32 == 0 {
                kernel_common::watchdog::kick();
            }
            let byte_offset = chunk_idx * CHUNK_BYTES;
            let lba = BLOOM_DATA_LBA + (byte_offset / 512) as u64;
            // SAFETY: `self.words` is 64-byte aligned, has length `total_bytes`, and is exclusively
            // populated during single-threaded boot initialization before `BLOOM_ENABLED` is set.
            let chunk_slice = unsafe {
                core::slice::from_raw_parts_mut(
                    (self.words.as_ptr() as *mut u8).add(byte_offset),
                    CHUNK_BYTES,
                )
            };
            if storage.lock().read_raw_lba(lba, chunk_slice).is_err() {
                return false;
            }
        }
        self.clear_dirty();
        {
            let mut delta = self.delta.lock();
            delta.reset();
            if delta_pages > 0 {
                for p in 0..delta_pages {
                    let lba = BLOOM_DELTA_LBA + (p * 8) as u64;
                    if storage
                        .lock()
                        .read_raw_lba(lba, &mut delta.pages[p].bytes)
                        .is_err()
                    {
                        return false;
                    }
                    for s in 0..DELTA_ENTRIES_PER_PAGE {
                        let off = s * 8;
                        let packed = u64::from_le_bytes(
                            delta.pages[p].bytes[off..off + 8].try_into().unwrap(),
                        );
                        if packed & (1u64 << 63) != 0 {
                            let (base, m0, m1, m2, m3) = Self::unpack_probe(packed);
                            self.words[base].fetch_or(m0, Ordering::Relaxed);
                            self.words[base + 1].fetch_or(m1, Ordering::Relaxed);
                            self.words[base + 2].fetch_or(m2, Ordering::Relaxed);
                            self.words[base + 3].fetch_or(m3, Ordering::Relaxed);
                            let chunk = base / CHUNK_WORDS;
                            self.dirty_chunks[chunk / 64]
                                .fetch_or(1u64 << (chunk % 64), Ordering::Relaxed);
                            delta.entry_count = p * DELTA_ENTRIES_PER_PAGE + s + 1;
                        }
                    }
                }
                delta.flushed_full_pages = delta.entry_count / DELTA_ENTRIES_PER_PAGE;
            }
        }
        BLOOM_PERSISTED.store(true, Ordering::Release);
        true
    }

    fn save_to_nvme(&self) -> bool {
        let Some(storage) = kernel_common::storage::GLOBAL_STORAGE.lock().clone() else {
            return false;
        };
        let total_bytes = self.words.len() * 8;
        let num_chunks = total_bytes / CHUNK_BYTES;
        if num_chunks == 0 {
            return false;
        }
        let words_bytes = self.as_bytes();
        for chunk_idx in 0..num_chunks {
            if chunk_idx % 32 == 0 {
                kernel_common::watchdog::kick();
            }
            let byte_offset = chunk_idx * CHUNK_BYTES;
            let lba = BLOOM_DATA_LBA + (byte_offset / 512) as u64;
            if storage
                .lock()
                .write_raw_lba(lba, &words_bytes[byte_offset..byte_offset + CHUNK_BYTES])
                .is_err()
            {
                return false;
            }
        }
        let mut header = kernel_common::storage::AlignedBlock::new();
        header.bytes[0..8].copy_from_slice(&BLOOM_MAGIC.to_le_bytes());
        header.bytes[8..16].copy_from_slice(&(BLOOM_BUCKETS as u64).to_le_bytes());
        header.bytes[16..24].copy_from_slice(&0u64.to_le_bytes());
        if storage.lock().write_raw_lba(BLOOM_HEADER_LBA, &header.bytes).is_err() {
            return false;
        }
        self.clear_dirty();
        self.delta.lock().reset();
        BLOOM_PERSISTED.store(true, Ordering::Release);
        true
    }
}

static BLOCK_BLOOM: spin::Once<BlockBloomFilter> = spin::Once::new();
static BLOOM_ENABLED: AtomicBool = AtomicBool::new(false);
static BLOOM_PERSISTED: AtomicBool = AtomicBool::new(false);
static BLOCK_COUNT: AtomicU64 = AtomicU64::new(0);

fn flush_bloom_pre_commit(storage: &spin::Mutex<StorageContext>) -> Result<(), StorageError> {
    if !BLOOM_PERSISTED.load(Ordering::Acquire) {
        return Ok(());
    }
    let Some(bloom) = BLOCK_BLOOM.get() else {
        return Ok(());
    };
    let mut delta = bloom.delta.lock();
    let max_entries = MAX_DELTA_PAGES * DELTA_ENTRIES_PER_PAGE;
    if delta.entry_count == 0 {
        return Ok(());
    }
    if delta.entry_count <= max_entries {
        let used_pages =
            (delta.entry_count + DELTA_ENTRIES_PER_PAGE - 1) / DELTA_ENTRIES_PER_PAGE;
        let start_page = delta.flushed_full_pages;
        if used_pages > start_page {
            // SAFETY: `delta.pages` is a contiguous slice of `AlignedBlock` (each 4096 bytes).
            let pages_slice = &delta.pages[start_page..used_pages];
            let byte_slice = unsafe {
                core::slice::from_raw_parts(
                    pages_slice.as_ptr() as *const u8,
                    pages_slice.len() * 4096,
                )
            };
            let lba = BLOOM_DELTA_LBA + (start_page * 8) as u64;
            storage.lock().write_raw_lba(lba, byte_slice)?;

            let mut header = kernel_common::storage::AlignedBlock::new();
            header.bytes[0..8].copy_from_slice(&BLOOM_MAGIC.to_le_bytes());
            header.bytes[8..16].copy_from_slice(&(BLOOM_BUCKETS as u64).to_le_bytes());
            header.bytes[16..24].copy_from_slice(&(used_pages as u64).to_le_bytes());
            storage.lock().write_raw_lba(BLOOM_HEADER_LBA, &header.bytes)?;
        }
        delta.flushed_full_pages = delta.entry_count / DELTA_ENTRIES_PER_PAGE;
        return Ok(());
    }

    // Delta log is full (`> 131,072` new block insertions): compact dirty 128 KB chunks to NVMe
    // and reset the delta log. Each 128 KB write acquires `storage.lock()` briefly (<0.5 ms) so
    // concurrent B-Tree page reads on other harts are never stalled.
    let words_bytes = bloom.as_bytes();
    let total_chunks = words_bytes.len() / CHUNK_BYTES;
    for group in 0..DIRTY_GROUPS {
        let mut mask = bloom.dirty_chunks[group].swap(0, Ordering::AcqRel);
        while mask != 0 {
            let bit = mask.trailing_zeros() as usize;
            let chunk_idx = group * 64 + bit;
            if chunk_idx < total_chunks {
                let byte_offset = chunk_idx * CHUNK_BYTES;
                let lba = BLOOM_DATA_LBA + (byte_offset / 512) as u64;
                if let Err(e) = storage
                    .lock()
                    .write_raw_lba(lba, &words_bytes[byte_offset..byte_offset + CHUNK_BYTES])
                {
                    bloom.dirty_chunks[group].fetch_or(mask, Ordering::Release);
                    return Err(e);
                }
            }
            mask &= mask - 1;
        }
    }
    delta.reset();
    let mut header = kernel_common::storage::AlignedBlock::new();
    header.bytes[0..8].copy_from_slice(&BLOOM_MAGIC.to_le_bytes());
    header.bytes[8..16].copy_from_slice(&(BLOOM_BUCKETS as u64).to_le_bytes());
    header.bytes[16..24].copy_from_slice(&0u64.to_le_bytes());
    storage.lock().write_raw_lba(BLOOM_HEADER_LBA, &header.bytes)?;
    Ok(())
}

fn get_block_bloom() -> &'static BlockBloomFilter {
    BLOCK_BLOOM.call_once(BlockBloomFilter::new)
}

const HOT_BLOCK_SLOTS: usize = 64; // 16 shards * 64 slots = 1,024 hot blocks

#[derive(Clone)]
struct HotBlockEntry {
    gen: u32,
    hash: BlockHash,
    block: SavedBlock,
    successor: Option<BlockHash>,
}

static HOT_BLOCKS: [spin::Mutex<[Option<HotBlockEntry>; HOT_BLOCK_SLOTS]>; SHARD_COUNT] =
    [const { spin::Mutex::new([const { None }; HOT_BLOCK_SLOTS]) }; SHARD_COUNT];

#[inline]
fn hot_block_slot(hash: &BlockHash) -> (usize, usize) {
    let b = hash.as_bytes();
    let shard = (b[0] >> 4) as usize;
    let slot = (u16::from_ne_bytes([b[1], b[2]]) as usize) & (HOT_BLOCK_SLOTS - 1);
    (shard, slot)
}

#[inline]
fn hot_block_get(hash: &BlockHash) -> Option<SavedBlock> {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_block_slot(hash);
    let guard = HOT_BLOCKS[shard].lock();
    if let Some(entry) = &guard[slot] {
        if entry.gen == gen && entry.hash == *hash {
            return Some(entry.block.clone());
        }
    }
    None
}

#[inline]
fn hot_block_exists(hash: &BlockHash) -> bool {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_block_slot(hash);
    let guard = HOT_BLOCKS[shard].lock();
    if let Some(entry) = &guard[slot] {
        return entry.gen == gen && entry.hash == *hash;
    }
    false
}

#[inline]
pub(crate) fn hot_block_get_successor(hash: &BlockHash) -> Option<Option<BlockHash>> {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_block_slot(hash);
    let guard = HOT_BLOCKS[shard].lock();
    if let Some(entry) = &guard[slot] {
        if entry.gen == gen && entry.hash == *hash {
            if let Some(succ) = entry.successor {
                return Some(if succ.is_zero() { None } else { Some(succ) });
            }
        }
    }
    None
}

#[inline]
pub(crate) fn hot_block_set_successor(hash: &BlockHash, successor: BlockHash) {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_block_slot(hash);
    let mut guard = HOT_BLOCKS[shard].lock();
    if let Some(entry) = &mut guard[slot] {
        if entry.gen == gen && entry.hash == *hash {
            entry.successor = Some(successor);
        }
    }
}

#[inline]
fn hot_block_put(
    hash: &BlockHash,
    block: &SavedBlock,
    successor: Option<BlockHash>,
) {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_block_slot(hash);
    let mut guard = HOT_BLOCKS[shard].lock();
    let final_succ = match successor {
        Some(s) => Some(s),
        None => guard[slot]
            .as_ref()
            .filter(|e| e.gen == gen && e.hash == *hash)
            .and_then(|e| e.successor),
    };
    guard[slot] = Some(HotBlockEntry {
        gen,
        hash: *hash,
        block: block.clone(),
        successor: final_succ,
    });
}

#[inline]
fn hot_block_del(hash: &BlockHash) {
    let (shard, slot) = hot_block_slot(hash);
    let mut guard = HOT_BLOCKS[shard].lock();
    if let Some(entry) = &guard[slot] {
        if entry.hash == *hash {
            guard[slot] = None;
        }
    }
}

pub struct BtreeBlockStore {
    #[allow(dead_code)]
    cache: Arc<LedgerCache>,
}

impl BtreeBlockStore {
    pub const TABLE_ID: u8 = 2;

    pub fn new(cache: Arc<LedgerCache>) -> Self {
        Self { cache }
    }

    pub fn clear_bloom(&self) {
        kernel_common::storage::STORAGE_GEN.fetch_add(1, Ordering::Release);
        BLOOM_PERSISTED.store(false, Ordering::Release);
        if let Some(bloom) = BLOCK_BLOOM.get() {
            bloom.clear();
        }
        if let Some(storage) = kernel_common::storage::GLOBAL_STORAGE.lock().clone() {
            let zero = kernel_common::storage::AlignedBlock::new();
            let _ = storage.lock().write_raw_lba(BLOOM_HEADER_LBA, &zero.bytes);
        }
        BLOCK_COUNT.store(0, Ordering::Relaxed);
    }

    fn make_key(&self, hash: &BlockHash) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(hash.as_bytes());
        key
    }

    pub fn put(&self, block: &SavedBlock) {
        self.put_with_successor(block, &BlockHash::zero());
    }

    pub fn put_with_successor(&self, block: &SavedBlock, successor: &BlockHash) {
        let hash = block.hash();
        get_block_bloom().insert(hash.as_bytes());
        let key = self.make_key(&hash);
        let mut buffer = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut buffer);
        block.serialize(&mut adapter);
        let len = adapter.bytes_written();
        buffer[len..len + 32].copy_from_slice(successor.as_bytes());
        let total_len = len + 32;

        let shard_idx = get_shard_index(&key);
        tracing::debug!("BlockStore: putting block {} into shard {}", hash, shard_idx);
        let (has_db, is_new) = {
            let mut lock = SHARDS[shard_idx].lock();
            if let Some(db) = lock.as_mut() {
                (
                    true,
                    db.insert_preserving_suffix_32(key, &buffer[0..total_len], successor.is_zero()),
                )
            } else {
                (false, false)
            }
        };
        if is_new {
            BLOCK_COUNT.fetch_add(1, Ordering::Relaxed);
            self.cache.block_count.fetch_add(1, Ordering::Relaxed);
        }
        if has_db && !cfg!(test) {
            let known_succ = if !successor.is_zero() || is_new {
                Some(*successor)
            } else {
                None
            };
            hot_block_put(&hash, block, known_succ);
        }
    }

    /// Inserts a slice of `(hash, saved_block, successor)` sorted by `BlockHash`,
    /// acquiring each shard lock at most once and prefetching B-Tree leaf pages 2 entries ahead.
    pub fn put_batch_with_successors(&self, items: &[(BlockHash, SavedBlock, BlockHash)]) {
        if items.is_empty() {
            return;
        }
        let bloom = get_block_bloom();
        let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
        let mut buffer = [0u8; 512];
        let mut i = 0;
        let n = items.len();
        while i < n {
            let shard_idx = (items[i].0.as_bytes()[0] >> 4) as usize;
            let mut j = i + 1;
            while j < n && ((items[j].0.as_bytes()[0] >> 4) as usize) == shard_idx {
                j += 1;
            }
            let shard_items = &items[i..j];
            let mut new_in_shard = 0u64;
            let mut has_db = false;
            {
                let mut lock = SHARDS[shard_idx].lock();
                if let Some(db) = lock.as_mut() {
                    has_db = true;
                    for (k, (hash, block, successor)) in shard_items.iter().enumerate() {
                        if k + 2 < shard_items.len() {
                            let ahead_key = self.make_key(&shard_items[k + 2].0);
                            db.prefetch_key_write(&ahead_key);
                        }
                        bloom.insert(hash.as_bytes());
                        let key = self.make_key(hash);
                        let mut adapter = MutStreamAdapter::new(&mut buffer);
                        block.serialize(&mut adapter);
                        let len = adapter.bytes_written();
                        buffer[len..len + 32].copy_from_slice(successor.as_bytes());
                        let total_len = len + 32;
                        if db.insert_preserving_suffix_32(
                            key,
                            &buffer[0..total_len],
                            successor.is_zero(),
                        ) {
                            new_in_shard += 1;
                        }
                    }
                }
            }
            if new_in_shard > 0 {
                BLOCK_COUNT.fetch_add(new_in_shard, Ordering::Relaxed);
                self.cache.block_count.fetch_add(new_in_shard, Ordering::Relaxed);
            }
            if has_db && !cfg!(test) {
                let mut hot_guard = HOT_BLOCKS[shard_idx].lock();
                for (hash, block, successor) in shard_items {
                    let b = hash.as_bytes();
                    let slot = (u16::from_ne_bytes([b[1], b[2]]) as usize) & (HOT_BLOCK_SLOTS - 1);
                    hot_guard[slot] = Some(HotBlockEntry {
                        gen,
                        hash: *hash,
                        block: block.clone(),
                        successor: Some(*successor),
                    });
                }
            }
            i = j;
        }
    }

    #[inline]
    pub fn may_exist(&self, hash: &BlockHash) -> bool {
        Self::may_exist_global(hash)
    }

    #[inline]
    pub fn prefetch_may_exist(&self, hash: &BlockHash) {
        if !hash.is_zero() && BLOOM_ENABLED.load(Ordering::Relaxed) {
            if let Some(bloom) = BLOCK_BLOOM.get() {
                bloom.prefetch(hash.as_bytes());
            }
        }
    }

    #[inline]
    pub fn may_exist_global(hash: &BlockHash) -> bool {
        if BLOOM_ENABLED.load(Ordering::Acquire) {
            if let Some(bloom) = BLOCK_BLOOM.get() {
                return bloom.may_contain(hash.as_bytes());
            }
        }
        true
    }

    pub fn get(&self, hash: &BlockHash) -> Option<SavedBlock> {
        if !self.may_exist(hash) {
            return None;
        }
        if !cfg!(test) {
            if let Some(cached) = hot_block_get(hash) {
                return Some(cached);
            }
        }
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(Some((block, succ))) = db.get_with(&key, |value| {
                let mut stream = BufferReader::new(value);
                let block = SavedBlock::deserialize(&mut stream).ok()?;
                let succ = if value.len() >= 32 {
                    BlockHash::from_bytes(value[value.len() - 32..].try_into().unwrap())
                } else {
                    BlockHash::zero()
                };
                Some((block, succ))
            }) {
                drop(lock);
                if !cfg!(test) {
                    hot_block_put(hash, &block, Some(succ));
                }
                tracing::debug!("BlockStore: found block {}", hash);
                return Some(block);
            }
        }
        tracing::debug!("BlockStore: NOT found block {} in shard {}", hash, shard_idx);
        None
    }

    pub fn exists(&self, hash: &BlockHash) -> bool {
        if !self.may_exist(hash) {
            return false;
        }
        if !cfg!(test) && hot_block_exists(hash) {
            return true;
        }
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let t0 = kernel_common::clint::mtime();
        let mut lock = SHARDS[shard_idx].lock();
        let t1 = kernel_common::clint::mtime();
        let r = if let Some(db) = lock.as_mut() {
            db.contains_key(&key)
        } else {
            false
        };
        let t2 = kernel_common::clint::mtime();
        let p = &kernel_common::storage::bpm::STORAGE_PROF;
        p[10].fetch_add(t1 - t0, Ordering::Relaxed); // lock wait
        p[11].fetch_add(t2 - t1, Ordering::Relaxed); // search while locked
        p[12].fetch_add(1, Ordering::Relaxed);
        r
    }

    pub fn del(&self, hash: &BlockHash) {
        hot_block_del(hash);
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if db.contains_key(&key) {
                db.delete(key);
                BLOCK_COUNT.fetch_sub(1, Ordering::Relaxed);
                self.cache.block_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    pub fn set_count(&self, count: u64) {
        BLOCK_COUNT.store(count, Ordering::Relaxed);
    }

    pub fn try_load_persisted_bloom(&self) -> bool {
        kernel_common::storage::wal::set_pre_commit_hook(flush_bloom_pre_commit);
        let bloom = get_block_bloom();
        if bloom.load_from_nvme() {
            BLOOM_ENABLED.store(true, Ordering::Release);
            tracing::info!("[ledger] Loaded 64 MB block bloom filter from NVMe");
            true
        } else {
            false
        }
    }

    pub fn scan_count(&self) -> usize {
        kernel_common::storage::wal::set_pre_commit_hook(flush_bloom_pre_commit);
        let bloom = get_block_bloom();
        bloom.clear();
        #[cfg(not(test))]
        tracing::info!("[ledger] Building 64 MB block bloom filter from B-Tree (one-time scan)...");
        let mut count = 0;
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;
        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some(k) = it.next_key() {
                    if k[0] != Self::TABLE_ID {
                        break;
                    }
                    bloom.insert(k[1..33].try_into().unwrap());
                    count += 1;
                    if count % 1000 == 0 {
                        kernel_common::watchdog::kick();
                    }
                    #[cfg(not(test))]
                    if count % 5_000_000 == 0 {
                        tracing::info!("[ledger] Bloom scan progress: {} blocks", count);
                    }
                }
            }
        }
        if bloom.save_to_nvme() {
            tracing::info!("[ledger] Saved 64 MB block bloom filter to NVMe");
        }
        BLOCK_COUNT.store(count as u64, Ordering::Relaxed);
        BLOOM_ENABLED.store(true, Ordering::Release);
        count
    }

    pub fn count(&self) -> usize {
        let cached = self.cache.block_count.load(Ordering::Relaxed) as usize;
        if cached > 0 {
            let pruned = self.cache.pruned_count.load(Ordering::Relaxed) as usize;
            cached.saturating_sub(pruned)
        } else {
            BLOCK_COUNT.load(Ordering::Relaxed) as usize
        }
    }

    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(SavedBlock),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let mut stream = BufferReader::new(value);
                    Ok(SavedBlock::deserialize(&mut stream).ok())
                }) {
                    match step {
                        Ok(Some(block)) => f(block),
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
    }

    pub fn iter(&self) -> alloc::vec::Vec<SavedBlock> {
        let mut results = alloc::vec::Vec::new();
        self.for_each(|block| results.push(block));
        results
    }

    /// Reads up to `count` blocks starting from `start_hash` (wrapping around shards if needed),
    /// stopping immediately once `count` blocks are collected instead of scanning the entire store.
    pub fn random_blocks(&self, start_hash: BlockHash, count: usize) -> alloc::vec::Vec<SavedBlock> {
        let mut results = alloc::vec::Vec::with_capacity(count);
        if count == 0 {
            return results;
        }
        let start_key = self.make_key(&start_hash);
        let start_shard = get_shard_index(&start_key);

        for offset in 0..SHARD_COUNT {
            let shard_idx = (start_shard + offset) % SHARD_COUNT;
            let seek_key = if offset == 0 {
                start_key
            } else {
                let mut k = [0u8; KEY_SIZE];
                k[0] = Self::TABLE_ID;
                k
            };

            let mut lock = SHARDS[shard_idx].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(seek_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let mut stream = BufferReader::new(value);
                    Ok(SavedBlock::deserialize(&mut stream).ok())
                }) {
                    match step {
                        Ok(Some(block)) => {
                            results.push(block);
                            if results.len() >= count {
                                return results;
                            }
                        }
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
        results
    }
}
