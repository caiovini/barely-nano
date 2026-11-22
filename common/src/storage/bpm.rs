//! Buffer Pool Manager (BPM)
//!
//! In a bare-metal environment without an Operating System (no Linux, no Windows),
//! we don't have access to high-level abstractions like memory-mapped files (mmap)
//! or sophisticated filesystem caches.
//!
//! The BPM provides a middle layer between our storage hardware (e.g., NVMe SSD via U-Boot API)
//! and our high-level data structures (like the B-Tree). It maintains a fixed-size cache
//! of "Frames" in memory to minimize slow hardware I/O.

use crate::storage::StorageContext;
extern crate alloc;
use alloc::vec::Vec;
use alloc::sync::Arc;
use spin::Mutex;

use core::sync::atomic::{AtomicU64, Ordering};
/// Storage profiling (CLINT ticks): [0]=fetches,[1]=fetch ticks (incl. IO),[2]=reads,[3]=read ticks,
/// [4]=writes,[5]=write ticks,[6]=searches,[7]=search ticks,[8]=inserts,[9]=insert ticks
pub static STORAGE_PROF: [AtomicU64; 13] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];

/// The fundamental unit of storage and caching. Matches typical SSD block sizes.
pub const BLOCK_SIZE: usize = 4096;

/// Per-hart Buffer Pool miss attribution: number of misses that went to NVMe, and their
/// total cost in CLINT ticks (victim selection + device read), indexed by hart id.
pub static MISS_N_HART: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
pub static MISS_TICKS_HART: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];

/// A container for a single cached block of data.
pub struct Frame {
    pub data: crate::storage::AlignedBlock,
    pub page_id: Option<usize>,
    pub is_dirty: bool,
    pub in_dirty_list: bool,
    pub ref_bit: bool,
    pub pin_count: u32,
    /// Incremented every time the frame is modified; lets a checkpoint detect pages that were
    /// re-dirtied after its snapshot was taken.
    pub gen: u64,
}

impl Frame {
    pub fn new() -> Self {
        Self {
            data: crate::storage::AlignedBlock::new(),
            page_id: None,
            is_dirty: false,
            in_dirty_list: false,
            ref_bit: false,
            pin_count: 0,
            gen: 0,
        }
    }
}

/// Upper bound on frames per buffer pool in Bank 0 fallback mode.
pub const MAX_FRAMES: usize = 32768;

/// Base physical address of Upper DRAM Bank 1 (`0x1_0000_0000 ..`).
pub const HIGH_DRAM_BASE: usize = crate::soc::HIGH_DRAM_BASE;

/// Target resident 4 KB frames per shard in DRAM Bank 1
/// (`81,920` on K1 8 GB = `5.00 GB` total; `409,600` on K3 32 GB = `25.00 GB` total).
pub const HIGH_DRAM_TARGET_FRAMES: usize = crate::soc::HIGH_DRAM_TARGET_FRAMES;

/// Maximum 4 KB frames per shard in DRAM Bank 1
/// (`86,016` on K1 8 GB = `5.25 GB` max; `425,984` on K3 32 GB = `26.00 GB` max).
pub const HIGH_DRAM_MAX_FRAMES: usize = crate::soc::HIGH_DRAM_MAX_FRAMES;

/// Compact 24-byte metadata per frame kept in Bank 0 L2-friendly memory so Clock eviction sweeps
/// and dirty-frame scans do not stride across 4 KB data pages.
#[derive(Clone, Copy)]
pub struct FrameMeta {
    raw_page_id: u32,
    pub pin_count: u32,
    /// Incremented every time the frame is modified; lets a checkpoint detect pages that were
    /// re-dirtied after its snapshot was taken.
    pub gen: u64,
    pub is_dirty: bool,
    pub in_dirty_list: bool,
    pub ref_count: u8,
}

impl FrameMeta {
    pub const fn new() -> Self {
        Self {
            raw_page_id: u32::MAX,
            pin_count: 0,
            gen: 0,
            is_dirty: false,
            in_dirty_list: false,
            ref_count: 0,
        }
    }

    #[inline]
    pub fn page_id(&self) -> Option<usize> {
        if self.raw_page_id == u32::MAX {
            None
        } else {
            Some(self.raw_page_id as usize)
        }
    }

    #[inline]
    pub fn set_page_id(&mut self, pid: Option<usize>) {
        self.raw_page_id = match pid {
            Some(p) => p as u32,
            None => u32::MAX,
        };
    }
}

/// Two-level page-to-frame table constants:
/// Each leaf chunk maps 1,024 pages (`4,096` bytes = 4 KB slab), and the root directory
/// is pre-allocated at boot with 16,384 slots (covering up to 16M pages = 64 GB per shard,
/// or 1 TB across 16 shards) so runtime page growth never requires a > 4 KB allocation.
const PAGE_CHUNK_SHIFT: usize = 10;
const PAGE_CHUNK_SIZE: usize = 1 << PAGE_CHUNK_SHIFT;
const PAGE_CHUNK_MASK: usize = PAGE_CHUNK_SIZE - 1;
const PAGE_DIR_CAP: usize = 16384;

/// Set when a pool had to grow because every frame was dirty; asks the ledger writer to checkpoint.
pub static NEED_CHECKPOINT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// A dirty page captured for a WAL checkpoint.
pub struct DirtyPage {
    pub shard: u8,
    pub page_id: usize,
    pub gen: u64,
    /// Absolute block id on the storage device.
    pub home: u64,
}

/// Manages a set of memory frames and handles the movement of data between 
/// storage and memory using O(1) direct page indexing and Clock eviction.
pub struct BufferPool {
    storage: Arc<Mutex<StorageContext>>,
    /// The physical page on disk where this buffer pool's virtual "Page 0" starts.
    pub base_page: usize,
    /// Target number of resident frames before Clock eviction of clean pages begins.
    target_frames: usize,
    /// Hard upper bound on frames in `data_ptr`.
    max_frames: usize,
    /// Base pointer to contiguous `max_frames` 4 KB `AlignedBlock`s (in DRAM Bank 1 or `_owned_data`).
    data_ptr: *mut crate::storage::AlignedBlock,
    /// Backing storage when allocated on the Bank 0 heap (empty when using DRAM Bank 1).
    _owned_data: Vec<crate::storage::AlignedBlock>,
    /// Compact per-frame metadata array in Bank 0 (`len <= max_frames`).
    meta: Vec<FrameMeta>,
    /// Two-level O(1) mapping from `page_id` to `frame_id` (`u32::MAX` when not resident).
    page_to_frame: Vec<Option<alloc::boxed::Box<[u32; PAGE_CHUNK_SIZE]>>>,
    /// Unused frame IDs available in O(1) before running Clock eviction.
    free_list: Vec<usize>,
    /// Frame IDs currently marked dirty, so checkpoints don't scan clean/empty frames.
    dirty_frames: Vec<usize>,
    /// Clock hand index into `meta` for Second-Chance replacement.
    clock_hand: usize,
    /// When true, this buffer pool addresses the isolated `WALLET_STORAGE_LBA` region and
    /// never touches ledger blocks (`>= STORAGE_BASE_LBA`) or `NEED_CHECKPOINT`.
    is_wallet: bool,
}

// SAFETY: `BufferPool` is exclusively accessed behind its shard `ShardMutex`; `data_ptr` points to
// either a disjoint static DRAM Bank 1 shard region or `_owned_data` owned by this `BufferPool`.
unsafe impl Send for BufferPool {}
unsafe impl Sync for BufferPool {}

pub struct PageGuard {
    bpm: *mut BufferPool,
    data_ptr: *mut [u8; BLOCK_SIZE],
    frame_id: usize,
    is_dirty: bool,
}

impl PageGuard {
    #[inline]
    pub fn data(&self) -> &[u8; BLOCK_SIZE] {
        // SAFETY: `data_ptr` points to a pinned 4 KB `AlignedBlock` in `BufferPool::data_ptr`.
        unsafe { &*self.data_ptr }
    }

    /// Returns a mutable reference to the in-RAM cached frame without marking the page dirty.
    /// Used for deterministic in-RAM cache enrichment (populating the derived `PAGE_FLAG_SOA`
    /// key-head directory on a legacy slotted page loaded from NVMe).
    #[inline]
    pub fn data_cache_mut(&self) -> &mut [u8; BLOCK_SIZE] {
        // SAFETY: Caller holds the shard's `ShardMutex`, granting exclusive access to the frame.
        unsafe { &mut *self.data_ptr }
    }

    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8; BLOCK_SIZE] {
        self.is_dirty = true;
        // SAFETY: Caller holds the shard's `ShardMutex`, granting exclusive access to the frame.
        unsafe { &mut *self.data_ptr }
    }
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        unsafe {
            (*self.bpm).unpin(self.frame_id, self.is_dirty);
        }
    }
}

impl BufferPool {
    /// Creates a `BufferPool` whose 4 KB page buffers live in DRAM Bank 1 (`0x1_0000_0000 ..`),
    /// providing `81,920` target frames (`320 MB`/shard = `5.00 GB` across 16 shards) with zero
    /// Bank 0 page allocations.
    pub fn new_high_dram(
        storage: Arc<Mutex<StorageContext>>,
        shard_index: usize,
        base_page: usize,
    ) -> Self {
        let shard_bytes = HIGH_DRAM_MAX_FRAMES * BLOCK_SIZE;
        let shard_addr = HIGH_DRAM_BASE + shard_index * shard_bytes;
        let data_ptr = shard_addr as *mut crate::storage::AlignedBlock;

        let mut page_to_frame = Vec::with_capacity(PAGE_DIR_CAP);
        for _ in 0..16 {
            page_to_frame.push(Some(alloc::boxed::Box::new([u32::MAX; PAGE_CHUNK_SIZE])));
        }

        Self {
            storage,
            base_page,
            target_frames: HIGH_DRAM_TARGET_FRAMES,
            max_frames: HIGH_DRAM_MAX_FRAMES,
            data_ptr,
            _owned_data: Vec::new(),
            meta: Vec::with_capacity(HIGH_DRAM_MAX_FRAMES),
            page_to_frame,
            free_list: Vec::new(),
            dirty_frames: Vec::with_capacity(8192),
            clock_hand: 0,
            is_wallet: false,
        }
    }

    /// Fallback constructor allocating frame buffers on the Bank 0 heap (used for RAMDisk or if
    /// DRAM Bank 1 is unavailable).
    pub fn new(storage: Arc<Mutex<StorageContext>>, num_frames: usize, base_page: usize) -> Self {
        let max_frames = core::cmp::max(num_frames, 4096);
        let mut owned_data = alloc::vec![crate::storage::AlignedBlock::new(); max_frames];
        let data_ptr = owned_data.as_mut_ptr();

        let mut page_to_frame = Vec::with_capacity(PAGE_DIR_CAP);
        for _ in 0..32 {
            page_to_frame.push(Some(alloc::boxed::Box::new([u32::MAX; PAGE_CHUNK_SIZE])));
        }

        Self {
            storage,
            base_page,
            target_frames: num_frames,
            max_frames,
            data_ptr,
            _owned_data: owned_data,
            meta: Vec::with_capacity(max_frames),
            page_to_frame,
            free_list: Vec::new(),
            dirty_frames: Vec::with_capacity(max_frames),
            clock_hand: 0,
            is_wallet: false,
        }
    }

    /// Creates a compact Bank 0 `BufferPool` dedicated to the isolated wallet storage region
    /// (`WALLET_STORAGE_LBA`), completely disjoint from the ledger shards and ledger WAL.
    pub fn new_wallet(storage: Arc<Mutex<StorageContext>>, num_frames: usize) -> Self {
        let max_frames = core::cmp::max(num_frames, 16);
        let mut owned_data = alloc::vec![crate::storage::AlignedBlock::new(); max_frames];
        let data_ptr = owned_data.as_mut_ptr();

        let mut page_to_frame = Vec::with_capacity(1);
        page_to_frame.push(Some(alloc::boxed::Box::new([u32::MAX; PAGE_CHUNK_SIZE])));

        Self {
            storage,
            base_page: 0,
            target_frames: max_frames,
            max_frames,
            data_ptr,
            _owned_data: owned_data,
            meta: Vec::with_capacity(max_frames),
            page_to_frame,
            free_list: Vec::new(),
            dirty_frames: Vec::with_capacity(max_frames),
            clock_hand: 0,
            is_wallet: true,
        }
    }

    #[inline]
    pub fn cached_page_count(&self) -> usize {
        self.meta.len().saturating_sub(self.free_list.len())
    }

    #[inline]
    fn get_page_frame(&self, page_id: usize) -> u32 {
        let dir = page_id >> PAGE_CHUNK_SHIFT;
        let off = page_id & PAGE_CHUNK_MASK;
        if let Some(Some(chunk)) = self.page_to_frame.get(dir) {
            chunk[off]
        } else {
            u32::MAX
        }
    }

    #[inline]
    fn set_page_frame(&mut self, page_id: usize, fid: u32) {
        let dir = page_id >> PAGE_CHUNK_SHIFT;
        let off = page_id & PAGE_CHUNK_MASK;
        if dir >= self.page_to_frame.len() {
            self.page_to_frame.resize_with(dir + 1, || None);
        }
        let chunk = self.page_to_frame[dir]
            .get_or_insert_with(|| alloc::boxed::Box::new([u32::MAX; PAGE_CHUNK_SIZE]));
        chunk[off] = fid;
    }

    /// Retrieves a page from memory or loads it from storage if not present.
    pub fn fetch_page(&mut self, page_id: usize) -> Result<PageGuard, crate::storage::StorageError> {
        let frame_id = self.fetch_frame_id(page_id)?;
        // SAFETY: `frame_id < self.meta.len() <= self.max_frames`.
        let data_ptr = unsafe { &raw mut (*self.data_ptr.add(frame_id)).bytes };
        Ok(PageGuard {
            bpm: self as *mut _,
            data_ptr,
            frame_id,
            is_dirty: false,
        })
    }

    #[inline]
    pub fn is_resident(&self, page_id: usize) -> bool {
        self.get_page_frame(page_id) != u32::MAX
    }

    /// Retrieves a page for a sequential scan (`BTree::iter` / `RangeIterator`).
    /// Keeps internal B-Tree nodes pinned with high Clock priority (`4`), while assigning
    /// scanned leaf pages `ref_count = 0` so full-table scans (`FrontierChecker`, `BacklogScan`)
    /// are reclaimed first without evicting hot point-lookup pages (`ref_count = 2`).
    pub fn fetch_page_scan(&mut self, page_id: usize) -> Result<PageGuard, crate::storage::StorageError> {
        let fid = self.get_page_frame(page_id);
        let frame_id = if fid != u32::MAX {
            let frame_id = fid as usize;
            // SAFETY: `frame_id < self.meta.len() <= self.max_frames`.
            let is_internal = unsafe {
                let bytes = &(*self.data_ptr.add(frame_id)).bytes;
                let base = bytes.as_ptr();
                crate::storage::prefetch_read(base);
                crate::storage::prefetch_read(base.add(320));
                crate::storage::prefetch_read(base.add(3840));
                page_id == 0 || bytes[0] == 0
            };
            let m = &mut self.meta[frame_id];
            if is_internal && m.ref_count < 4 {
                m.ref_count = 4;
            }
            m.pin_count += 1;
            frame_id
        } else {
            let t0 = crate::clint::mtime();
            let victim_frame_id = self.claim_victim_frame()?;
            let m = &mut self.meta[victim_frame_id];
            m.set_page_id(Some(page_id));
            m.is_dirty = false;
            m.pin_count = 1;

            let tr = crate::clint::mtime();
            // SAFETY: `victim_frame_id < self.meta.len() <= self.max_frames`.
            let buf = unsafe { &mut (*self.data_ptr.add(victim_frame_id)).bytes };
            if self.is_wallet {
                self.storage.lock().read_wallet_block(page_id, buf)?;
            } else {
                self.storage.lock().read_block(page_id + self.base_page, buf)?;
            }
            self.meta[victim_frame_id].ref_count = if page_id == 0 || buf[0] == 0 { 4 } else { 0 };
            let now = crate::clint::mtime();
            STORAGE_PROF[0].fetch_add(1, Ordering::Relaxed);
            STORAGE_PROF[1].fetch_add(now - t0, Ordering::Relaxed);
            STORAGE_PROF[2].fetch_add(1, Ordering::Relaxed);
            STORAGE_PROF[3].fetch_add(now - tr, Ordering::Relaxed);
            let hart = crate::storage::current_hart();
            MISS_N_HART[hart].fetch_add(1, Ordering::Relaxed);
            MISS_TICKS_HART[hart].fetch_add(now - t0, Ordering::Relaxed);

            self.set_page_frame(page_id, victim_frame_id as u32);
            victim_frame_id
        };
        // SAFETY: `frame_id < self.meta.len() <= self.max_frames`.
        let data_ptr = unsafe { &raw mut (*self.data_ptr.add(frame_id)).bytes };
        Ok(PageGuard {
            bpm: self as *mut _,
            data_ptr,
            frame_id,
            is_dirty: false,
        })
    }

    /// Prefetches a contiguous run of non-resident pages (`start_page_id .. start_page_id + count`)
    /// in a single multi-sector NVMe DMA read (`read_contiguous_into_frames`), assigning scan priority
    /// (`ref_count = 0` for leaf pages, `4` for internal nodes).
    pub fn prefetch_contiguous_scan(&mut self, start_page_id: usize, count: usize) {
        if self.is_wallet || count <= 1 {
            return;
        }
        let max_run = count.min(crate::storage::DMA_BOUNCE_PAGES);
        let mut run_len = 0usize;
        while run_len < max_run {
            if self.get_page_frame(start_page_id + run_len) != u32::MAX {
                break;
            }
            run_len += 1;
        }
        if run_len <= 1 {
            return;
        }

        let t0 = crate::clint::mtime();
        let mut fids = [0usize; crate::storage::DMA_BOUNCE_PAGES];
        let mut ptrs = [core::ptr::null_mut::<[u8; BLOCK_SIZE]>(); crate::storage::DMA_BOUNCE_PAGES];
        let mut claimed = 0usize;
        while claimed < run_len {
            if let Ok(fid) = self.claim_victim_frame() {
                // Temporarily pin while claiming remaining frames in this batch so the Clock hand
                // cannot reclaim an earlier frame from the same batch.
                self.meta[fid].pin_count = 1;
                fids[claimed] = fid;
                // SAFETY: `fid < self.meta.len() <= self.max_frames`.
                ptrs[claimed] = unsafe { &raw mut (*self.data_ptr.add(fid)).bytes };
                claimed += 1;
            } else {
                break;
            }
        }
        if claimed <= 1 {
            for i in 0..claimed {
                let fid = fids[i];
                self.meta[fid].pin_count = 0;
                self.meta[fid].set_page_id(None);
                self.free_list.push(fid);
            }
            return;
        }

        let tr = crate::clint::mtime();
        // SAFETY: Each `ptrs[i]` points to a distinct, valid 4 KB `AlignedBlock` frame in Bank 1.
        let res = unsafe {
            self.storage
                .lock()
                .read_contiguous_into_frames(start_page_id + self.base_page, &ptrs[..claimed])
        };
        if res.is_err() {
            for i in 0..claimed {
                let fid = fids[i];
                self.meta[fid].pin_count = 0;
                self.meta[fid].set_page_id(None);
                self.free_list.push(fid);
            }
            return;
        }

        for i in 0..claimed {
            let pid = start_page_id + i;
            let fid = fids[i];
            // SAFETY: `ptrs[i]` was populated by `read_contiguous_into_frames`.
            let is_internal = pid == 0 || unsafe { (*ptrs[i])[0] == 0 };
            let m = &mut self.meta[fid];
            m.set_page_id(Some(pid));
            m.is_dirty = false;
            m.pin_count = 0;
            m.ref_count = if is_internal { 4 } else { 0 };
            self.set_page_frame(pid, fid as u32);
        }

        let now = crate::clint::mtime();
        STORAGE_PROF[0].fetch_add(claimed as u64, Ordering::Relaxed);
        STORAGE_PROF[1].fetch_add(now - t0, Ordering::Relaxed);
        STORAGE_PROF[2].fetch_add(1, Ordering::Relaxed);
        STORAGE_PROF[3].fetch_add(now - tr, Ordering::Relaxed);
        let hart = crate::storage::current_hart();
        MISS_N_HART[hart].fetch_add(claimed as u64, Ordering::Relaxed);
        MISS_TICKS_HART[hart].fetch_add(now - t0, Ordering::Relaxed);
    }

    /// Allocates or pins a frame for a newly allocated `page_id` without reading uninitialized
    /// disk sectors from NVMe, since the caller will immediately overwrite the 4 KB page.
    pub fn new_page(&mut self, page_id: usize) -> Result<PageGuard, crate::storage::StorageError> {
        let fid = self.get_page_frame(page_id);
        let frame_id = if fid != u32::MAX {
            let frame_id = fid as usize;
            let m = &mut self.meta[frame_id];
            m.pin_count += 1;
            frame_id
        } else {
            let victim_frame_id = self.claim_victim_frame()?;
            let m = &mut self.meta[victim_frame_id];
            m.set_page_id(Some(page_id));
            m.is_dirty = false;
            m.ref_count = 1;
            m.pin_count = 1;
            // SAFETY: `victim_frame_id < self.meta.len() <= self.max_frames`.
            unsafe {
                (*self.data_ptr.add(victim_frame_id)).bytes.fill(0);
            }
            self.set_page_frame(page_id, victim_frame_id as u32);
            victim_frame_id
        };
        // SAFETY: `frame_id < self.meta.len() <= self.max_frames`.
        let data_ptr = unsafe { &raw mut (*self.data_ptr.add(frame_id)).bytes };
        Ok(PageGuard {
            bpm: self as *mut _,
            data_ptr,
            frame_id,
            is_dirty: true,
        })
    }

    #[inline(always)]
    fn page_priority(page_id: usize, bytes: &[u8; BLOCK_SIZE]) -> u8 {
        if page_id == 0 || bytes[0] == 0 {
            // Shard metadata (page 0) or internal B-Tree routing node (`is_leaf == 0`)
            4
        } else {
            let num_keys = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
            let table_id = if num_keys > 0 {
                let cell_off = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
                if cell_off + 3 < BLOCK_SIZE {
                    bytes[cell_off + 3]
                } else {
                    0
                }
            } else {
                0
            };
            if matches!(table_id, 1 | 3 | 7 | 11 | 12) {
                // Hot Account (1), ConfirmationHeight (3), Pending (7), Version (11), or RepWeight (12) leaf
                2
            } else {
                // Cold Block/Successor (2), Pruned (5), Peer (6), FinalVote (8), or OnlineWeight (9) leaf
                1
            }
        }
    }

    #[inline]
    pub fn peek_resident_page(&self, page_id: usize) -> Option<&[u8; BLOCK_SIZE]> {
        let fid = self.get_page_frame(page_id);
        if fid != u32::MAX {
            // SAFETY: `fid` is a valid resident frame index `< self.max_frames`, and caller holds `&self`
            // behind the shard's `ShardMutex`.
            Some(unsafe { &(*self.data_ptr.add(fid as usize)).bytes })
        } else {
            None
        }
    }

    #[inline]
    pub fn prefetch_page(&self, page_id: usize) {
        let fid = self.get_page_frame(page_id);
        if fid != u32::MAX {
            // SAFETY: `fid` is a valid resident frame index `< self.max_frames`.
            unsafe {
                let base = (*self.data_ptr.add(fid as usize)).bytes.as_ptr();
                crate::storage::prefetch_read(base);
                crate::storage::prefetch_read(base.add(128));
                crate::storage::prefetch_read(base.add(3840));
            }
        }
    }

    #[inline]
    pub fn prefetch_page_write(&self, page_id: usize) {
        let fid = self.get_page_frame(page_id);
        if fid != u32::MAX {
            // SAFETY: `fid` is a valid resident frame index `< self.max_frames`.
            unsafe {
                let bytes = &(*self.data_ptr.add(fid as usize)).bytes;
                let base = bytes.as_ptr();
                crate::storage::prefetch_write(base);
                crate::storage::prefetch_write(base.add(128));
                let fe = (u16::from_le_bytes([bytes[3], bytes[4]]) as usize)
                    .saturating_sub(256)
                    .min(BLOCK_SIZE - 64);
                crate::storage::prefetch_write(base.add(fe));
            }
        }
    }

    #[inline]
    fn fetch_frame_id(&mut self, page_id: usize) -> Result<usize, crate::storage::StorageError> {
        // Fast O(1) cache hit path (no BTreeMap lookup, no linear LRU scan, no MMIO timer read)
        let fid = self.get_page_frame(page_id);
        if fid != u32::MAX {
            let frame_id = fid as usize;
            // SAFETY: `frame_id < self.meta.len() <= self.max_frames`.
            let prio = unsafe {
                let bytes = &(*self.data_ptr.add(frame_id)).bytes;
                let base = bytes.as_ptr();
                crate::storage::prefetch_read(base);
                crate::storage::prefetch_read(base.add(320));
                crate::storage::prefetch_read(base.add(3840));
                Self::page_priority(page_id, bytes)
            };
            let m = &mut self.meta[frame_id];
            if m.ref_count < prio {
                m.ref_count = prio;
            }
            m.pin_count += 1;
            return Ok(frame_id);
        }
        self.fetch_frame_id_miss(page_id)
    }

    fn claim_victim_frame(&mut self) -> Result<usize, crate::storage::StorageError> {
        if let Some(fid) = self.free_list.pop() {
            Ok(fid)
        } else if self.meta.len() < self.target_frames {
            let fid = self.meta.len();
            self.meta.push(FrameMeta::new());
            Ok(fid)
        } else {
            let n = self.meta.len();
            let mut found = None;
            for _ in 0..(n * 4) {
                let idx = self.clock_hand % n;
                self.clock_hand = (self.clock_hand + 1) % n;
                let m = &mut self.meta[idx];
                if m.pin_count == 0 && !m.is_dirty {
                    if m.ref_count > 0 {
                        m.ref_count -= 1;
                    } else {
                        found = Some(idx);
                        break;
                    }
                }
            }
            if let Some(fid) = found {
                if let Some(old_pid) = self.meta[fid].page_id() {
                    self.set_page_frame(old_pid, u32::MAX);
                }
                Ok(fid)
            } else {
                if self.meta.len() >= self.max_frames {
                    return Err(crate::storage::StorageError::IoError);
                }
                if !self.is_wallet {
                    NEED_CHECKPOINT.store(true, Ordering::Relaxed);
                }
                let fid = self.meta.len();
                self.meta.push(FrameMeta::new());
                Ok(fid)
            }
        }
    }

    fn fetch_frame_id_miss(&mut self, page_id: usize) -> Result<usize, crate::storage::StorageError> {
        let t0 = crate::clint::mtime();
        let victim_frame_id = self.claim_victim_frame()?;

        let m = &mut self.meta[victim_frame_id];
        m.set_page_id(Some(page_id));
        m.is_dirty = false;
        m.pin_count = 1;

        let tr = crate::clint::mtime();
        // SAFETY: `victim_frame_id < self.meta.len() <= self.max_frames`.
        let buf = unsafe { &mut (*self.data_ptr.add(victim_frame_id)).bytes };
        if self.is_wallet {
            self.storage.lock().read_wallet_block(page_id, buf)?;
        } else {
            self.storage.lock().read_block(page_id + self.base_page, buf)?;
        }
        let prio = Self::page_priority(page_id, buf);
        self.meta[victim_frame_id].ref_count = prio;
        let now = crate::clint::mtime();
        STORAGE_PROF[0].fetch_add(1, Ordering::Relaxed);
        STORAGE_PROF[1].fetch_add(now - t0, Ordering::Relaxed);
        STORAGE_PROF[2].fetch_add(1, Ordering::Relaxed);
        STORAGE_PROF[3].fetch_add(now - tr, Ordering::Relaxed);
        let hart = crate::storage::current_hart();
        MISS_N_HART[hart].fetch_add(1, Ordering::Relaxed);
        MISS_TICKS_HART[hart].fetch_add(now - t0, Ordering::Relaxed);

        self.set_page_frame(page_id, victim_frame_id as u32);
        Ok(victim_frame_id)
    }

    #[inline]
    fn unpin(&mut self, frame_id: usize, is_dirty: bool) {
        let prio = if is_dirty {
            let pid = self.meta[frame_id].raw_page_id as usize;
            // SAFETY: `frame_id < self.meta.len() <= self.max_frames`.
            let bytes = unsafe { &(*self.data_ptr.add(frame_id)).bytes };
            Self::page_priority(pid, bytes)
        } else {
            0
        };
        let m = &mut self.meta[frame_id];
        if m.pin_count > 0 {
            m.pin_count -= 1;
        }
        if is_dirty {
            if m.ref_count < prio {
                m.ref_count = prio;
            }
            m.is_dirty = true;
            m.gen = m.gen.wrapping_add(1);
            if !m.in_dirty_list {
                m.in_dirty_list = true;
                self.dirty_frames.push(frame_id);
                let dirty_limit = (self.target_frames / 8).clamp(2048, 8192);
                if !self.is_wallet && self.dirty_frames.len() >= dirty_limit {
                    NEED_CHECKPOINT.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    #[inline]
    pub fn dirty_frame_ids(&self) -> &[usize] {
        &self.dirty_frames
    }

    #[inline]
    pub fn frame_meta(&self, frame_id: usize) -> &FrameMeta {
        &self.meta[frame_id]
    }

    #[inline]
    pub fn frame_data(&self, frame_id: usize) -> &crate::storage::AlignedBlock {
        // SAFETY: `frame_id < self.meta.len() <= self.max_frames`.
        unsafe { &*self.data_ptr.add(frame_id) }
    }

    #[inline]
    pub fn base_page(&self) -> usize {
        self.base_page
    }

    /// Copies every dirty frame (page id, generation, home block, data) for a WAL checkpoint.
    /// The caller must hold the shard lock so the snapshot is internally consistent.
    pub fn snapshot_dirty(
        &self,
        shard: u8,
        entries: &mut Vec<DirtyPage>,
        data: &mut Vec<crate::storage::AlignedBlock>,
    ) {
        for &fid in &self.dirty_frames {
            let m = &self.meta[fid];
            if m.is_dirty {
                if let Some(pid) = m.page_id() {
                    entries.push(DirtyPage {
                        shard,
                        page_id: pid,
                        gen: m.gen,
                        home: (pid + self.base_page) as u64,
                    });
                    data.push(*self.frame_data(fid));
                }
            }
        }
    }

    /// Marks a page clean after a checkpoint persisted it, unless it was modified again since.
    #[inline]
    pub fn mark_clean(&mut self, page_id: usize, gen: u64) {
        let fid = self.get_page_frame(page_id);
        if fid != u32::MAX {
            let m = &mut self.meta[fid as usize];
            if m.is_dirty && m.gen == gen {
                m.is_dirty = false;
            }
        }
    }

    /// Reclaims any excess clean, unpinned frames above `target_frames`.
    pub fn trim_excess(&mut self) {
        {
            let meta = &mut self.meta;
            self.dirty_frames.retain(|&fid| {
                if meta[fid].is_dirty {
                    true
                } else {
                    meta[fid].in_dirty_list = false;
                    false
                }
            });
        }
        while self.meta.len() > self.target_frames {
            let fid = self.meta.len() - 1;
            let m = &self.meta[fid];
            if m.pin_count != 0 || m.is_dirty || m.in_dirty_list {
                break;
            }
            if let Some(old_pid) = m.page_id() {
                self.set_page_frame(old_pid, u32::MAX);
            }
            self.free_list.retain(|&id| id != fid);
            self.meta.pop();
        }
    }

    /// Persists all dirty frames to the storage device.
    /// For the isolated wallet pool (`self.is_wallet`), writes through the atomic wallet redo
    /// journal in `WALLET_STORAGE_LBA`. For ledger shards, used only during init/wipe before WAL.
    pub fn flush_all(&mut self) -> Result<(), crate::storage::StorageError> {
        let mut storage_guard = self.storage.lock();

        if self.is_wallet {
            let mut batch: Vec<(usize, &[u8; BLOCK_SIZE])> = Vec::with_capacity(self.dirty_frames.len());
            for &fid in &self.dirty_frames {
                let m = &self.meta[fid];
                if m.is_dirty {
                    if let Some(pid) = m.page_id() {
                        // SAFETY: `fid < self.meta.len() <= self.max_frames`.
                        let bytes = unsafe { &(*self.data_ptr.add(fid)).bytes };
                        batch.push((pid, bytes));
                    }
                }
            }
            if !batch.is_empty() {
                storage_guard.flush_wallet_dirty_pages(&mut batch)?;
            }
            for &fid in &self.dirty_frames {
                let m = &mut self.meta[fid];
                m.is_dirty = false;
                m.in_dirty_list = false;
            }
            self.dirty_frames.clear();
            return Ok(());
        }

        for &fid in &self.dirty_frames {
            let m = &mut self.meta[fid];
            if m.is_dirty {
                if let Some(pid) = m.page_id() {
                    // SAFETY: `fid < self.meta.len() <= self.max_frames`.
                    let bytes = unsafe { &(*self.data_ptr.add(fid)).bytes };
                    storage_guard.write_block(pid + self.base_page, bytes)?;
                    m.is_dirty = false;
                }
            }
            m.in_dirty_list = false;
        }
        self.dirty_frames.clear();
        Ok(())
    }

    pub fn fetch_page_read_only(&mut self, page_id: usize) -> Result<PageGuard, crate::storage::StorageError> {
        self.fetch_page(page_id)
    }
}
