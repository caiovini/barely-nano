//! Write-ahead log: atomic, batched, sequential persistence of the 16 B-tree shards.
//!
//! Problem solved: the shards used to be flushed independently (and dirty pages were written at
//! eviction time), so a power loss could persist an account head without the block it points to.
//!
//! Design (see also `BufferPool`, which never writes dirty pages on its own any more):
//! 1. A *checkpoint* (called by the single ledger writer between block batches, so the cut is
//!    transaction-consistent) copies every dirty page of every shard.
//! 2. The copies are appended to the WAL as one batch:
//!    `[descriptor pages][data pages][commit page]`. The commit page carries a checksum over all
//!    other pages, so a torn batch is detected and ignored.
//! 3. Only after the batch is durable are the pages written to their home locations (sorted by
//!    address) and marked clean.
//! 4. Recovery re-applies the newest valid batch (idempotent). Everything older was already fully
//!    applied before the newer batch was started.
//!
//! The WAL is two areas used alternately; writes are strictly sequential inside an area. Batches
//! carry the device *epoch* from the header page; the epoch is bumped on every boot, which
//! invalidates all old batches at once (so direct writes after recovery can never be undone by a
//! stale replay). Sequence numbers must be consecutive inside an area, which stops the scan at
//! remnants of older batches that a shorter new batch did not overwrite.

use super::{bpm::DirtyPage, AlignedBlock, StorageContext, StorageError, GLOBAL_STORAGE, SHARDS, SHARD_COUNT};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

static PRE_SNAPSHOT_HOOK: AtomicUsize = AtomicUsize::new(0);
static PRE_COMMIT_HOOK: AtomicUsize = AtomicUsize::new(0);
static WAL_B_ONLINE: AtomicBool = AtomicBool::new(false);
static WAL_B_JOB_PTR: AtomicUsize = AtomicUsize::new(0);
static WAL_B_JOB_DONE: AtomicBool = AtomicBool::new(false);
static WAL_BG_ONLINE: AtomicBool = AtomicBool::new(false);
static WAL_BG_PENDING: AtomicBool = AtomicBool::new(false);

enum WalBJob {
    Snapshot {
        entries_b: *mut Vec<DirtyPage>,
        data_b: *mut Vec<AlignedBlock>,
        scratch_b: *mut Vec<(usize, usize, u64)>,
        more_dirty_b: *mut bool,
    },
    Clean {
        entries_b: *const [DirtyPage],
    },
}

/// Marks Hart 5 (`Ledger-B`, WAL segments 8..15) as online so checkpoints snapshot and clean
/// Shards 8..15 in parallel with Hart 6 (`Ledger-A`, WAL segments 0..7).
pub fn set_wal_b_online(online: bool) {
    WAL_B_ONLINE.store(online, Ordering::Release);
}

/// Marks Hart 7 (`Storage+Mon`) as online so `checkpoint_nonblocking()` can hand off
/// transaction-consistent RAM snapshots to Hart 7 for background NVMe DMA persistence.
pub fn set_wal_bg_online(online: bool) {
    WAL_BG_ONLINE.store(online, Ordering::Release);
}

/// Returns `true` if a snapshotted WAL batch is waiting for Hart 7 to flush to NVMe.
#[inline]
pub fn is_wal_bg_pending() -> bool {
    WAL_BG_PENDING.load(Ordering::Acquire)
}

#[inline]
fn snapshot_shard_range(
    start_shard: usize,
    end_shard: usize,
    max_pages: usize,
    entries: &mut Vec<DirtyPage>,
    data: &mut Vec<AlignedBlock>,
    scratch: &mut Vec<(usize, usize, u64)>,
) -> bool {
    let mut more_dirty = false;
    for i in start_shard..end_shard {
        let guard = SHARDS[i].lock();
        if let Some(db) = guard.as_ref() {
            let dirty_ids = db.bpm.dirty_frame_ids();
            if dirty_ids.is_empty() {
                continue;
            }
            scratch.clear();
            for &fid in dirty_ids {
                let m = db.bpm.frame_meta(fid);
                if m.is_dirty {
                    if let Some(pid) = m.page_id() {
                        scratch.push((pid, fid, m.gen));
                    }
                }
            }
            if scratch.is_empty() {
                continue;
            }
            // Sort by page_id within each shard so `entries` and `data` are emitted in strictly
            // ascending `home` LBA order across the entire shard range without any post-copy sort.
            scratch.sort_unstable_by_key(|&(pid, _, _)| pid);
            let avail = max_pages.saturating_sub(entries.len());
            if scratch.len() > avail {
                scratch.truncate(avail);
                more_dirty = true;
            }
            let base_page = db.bpm.base_page();
            for &(pid, fid, gen) in scratch.iter() {
                entries.push(DirtyPage {
                    shard: i as u8,
                    page_id: pid,
                    gen,
                    home: (pid + base_page) as u64,
                });
                data.push(*db.bpm.frame_data(fid));
            }
        }
        if more_dirty {
            break;
        }
    }
    more_dirty
}

#[inline]
fn clean_shard_slice(entries: &[DirtyPage], start_shard: usize, end_shard: usize) {
    if entries.is_empty() {
        return;
    }
    let mut pos = 0;
    let len = entries.len();
    for i in start_shard..end_shard {
        while pos < len && (entries[pos].shard as usize) < i {
            pos += 1;
        }
        let shard_start = pos;
        while pos < len && (entries[pos].shard as usize) == i {
            pos += 1;
        }
        if shard_start == pos {
            continue;
        }
        let mut guard = SHARDS[i].lock();
        if let Some(db) = guard.as_mut() {
            for e in &entries[shard_start..pos] {
                db.bpm.mark_clean(e.page_id, e.gen);
            }
            db.bpm.trim_excess();
        }
    }
}

/// Polled by Hart 5 (`Ledger-B`) to snapshot or mark clean/trim WAL segments 8..15 (`SHARDS[8..16]`)
/// in parallel with Hart 6 / Hart 7 on WAL segments 0..7 (`SHARDS[0..8]`).
#[inline]
pub fn try_run_wal_b_job() -> bool {
    let job_addr = WAL_B_JOB_PTR.swap(0, Ordering::AcqRel);
    if job_addr == 0 {
        return false;
    }
    // SAFETY: `job_addr` was published on the caller's stack while holding `WAL.lock()` and the
    // publisher spins on `WAL_B_JOB_DONE` with `Acquire` ordering before returning from the scope.
    unsafe {
        match &*(job_addr as *const WalBJob) {
            WalBJob::Snapshot {
                entries_b,
                data_b,
                scratch_b,
                more_dirty_b,
            } => {
                let more = snapshot_shard_range(
                    SHARD_COUNT / 2,
                    SHARD_COUNT,
                    HALF_CHECKPOINT_PAGES,
                    &mut **entries_b,
                    &mut **data_b,
                    &mut **scratch_b,
                );
                **more_dirty_b = more;
            }
            WalBJob::Clean { entries_b } => {
                clean_shard_slice(&**entries_b, SHARD_COUNT / 2, SHARD_COUNT);
            }
        }
    }
    WAL_B_JOB_DONE.store(true, Ordering::Release);
    true
}

/// Registers a callback invoked inside `checkpoint()` before snapshotting dirty shard pages.
pub fn set_pre_snapshot_hook(hook: fn()) {
    PRE_SNAPSHOT_HOOK.store(hook as usize, Ordering::Release);
}

/// Registers a callback invoked inside `checkpoint()` before writing the WAL commit block.
pub fn set_pre_commit_hook(hook: fn(&Mutex<StorageContext>) -> Result<(), StorageError>) {
    PRE_COMMIT_HOOK.store(hook as usize, Ordering::Release);
}

/// Total WAL pages on a full-size device (two areas of 32768 pages = 128 MB each).
pub const WAL_PAGES: usize = 65536;

/// Maximum dirty pages snapshotted in one WAL pass (16 MB pre-allocated at boot so checkpoints
/// never attempt a large contiguous heap allocation at runtime).
pub const MAX_CHECKPOINT_PAGES: usize = 4096;
const HALF_CHECKPOINT_PAGES: usize = MAX_CHECKPOINT_PAGES / 2;
const SCRATCH_CAP: usize = 8192;

const DESC_MAGIC: u64 = 0x5741_4C5F_4445_5343; // "WAL_DESC"
const COMMIT_MAGIC: u64 = 0x5741_4C5F_434D_4954; // "WAL_CMIT"
const DESC_HDR: usize = 32;
const ENTRIES_PER_DESC: usize = (4096 - DESC_HDR) / 8;

/// Statistics: [0]=checkpoints, [1]=pages written to WAL, [2]=pages applied home, [3]=ticks spent.
pub static WAL_STATS: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

struct WalState {
    ready: bool,
    base: usize,
    area_pages: usize,
    epoch: u64,
    area: usize,
    pos: usize,
    seq: u64,
    entries_a: Vec<DirtyPage>,
    data_a: Vec<AlignedBlock>,
    scratch_a: Vec<(usize, usize, u64)>,
    entries_b: Vec<DirtyPage>,
    data_b: Vec<AlignedBlock>,
    scratch_b: Vec<(usize, usize, u64)>,
}

static WAL: Mutex<WalState> = Mutex::new(WalState {
    ready: false,
    base: 0,
    area_pages: 0,
    epoch: 0,
    area: 0,
    pos: 0,
    seq: 1,
    entries_a: Vec::new(),
    data_a: Vec::new(),
    scratch_a: Vec::new(),
    entries_b: Vec::new(),
    data_b: Vec::new(),
    scratch_b: Vec::new(),
});

fn checksum(pages: &[AlignedBlock], mut h: u64) -> u64 {
    for p in pages {
        for w in p.bytes.chunks_exact(8) {
            let v = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
            h = (h ^ v).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            h ^= h >> 32;
        }
    }
    h
}

const CHECKSUM_SEED: u64 = 0x243F_6A88_85A3_08D3;

fn bytes_of(pages: &[AlignedBlock]) -> &[u8] {
    // SAFETY: `AlignedBlock` is `repr(align(64))` holding exactly `[u8; 4096]`, so a slice of N
    // blocks is N*4096 contiguous, initialised bytes with 64-byte alignment; the returned slice
    // borrows `pages`, so it cannot outlive the data.
    unsafe { core::slice::from_raw_parts(pages.as_ptr() as *const u8, pages.len() * 4096) }
}

fn bytes_of_mut(pages: &mut [AlignedBlock]) -> &mut [u8] {
    // SAFETY: same layout argument as `bytes_of`; the exclusive borrow of `pages` is transferred.
    unsafe { core::slice::from_raw_parts_mut(pages.as_mut_ptr() as *mut u8, pages.len() * 4096) }
}

fn desc_pages_for(n: usize) -> usize {
    (n + ENTRIES_PER_DESC - 1) / ENTRIES_PER_DESC
}

fn make_desc_pages(
    entries_a: &[DirtyPage],
    entries_b: &[DirtyPage],
    epoch: u64,
    seq: u64,
) -> Vec<AlignedBlock> {
    let na = entries_a.len();
    let n = na + entries_b.len();
    let dp = desc_pages_for(n);
    let mut pages = alloc::vec![AlignedBlock::new(); dp];
    for (pi, page) in pages.iter_mut().enumerate() {
        page.bytes[0..8].copy_from_slice(&DESC_MAGIC.to_le_bytes());
        page.bytes[8..16].copy_from_slice(&epoch.to_le_bytes());
        page.bytes[16..24].copy_from_slice(&seq.to_le_bytes());
        page.bytes[24..28].copy_from_slice(&(n as u32).to_le_bytes());
        page.bytes[28..32].copy_from_slice(&(pi as u32).to_le_bytes());
        let first = pi * ENTRIES_PER_DESC;
        let last = core::cmp::min(first + ENTRIES_PER_DESC, n);
        for (k, idx) in (first..last).enumerate() {
            let e = if idx < na {
                &entries_a[idx]
            } else {
                &entries_b[idx - na]
            };
            let off = DESC_HDR + k * 8;
            page.bytes[off..off + 8].copy_from_slice(&e.home.to_le_bytes());
        }
    }
    pages
}

fn make_commit_page(epoch: u64, seq: u64, n: usize, sum: u64) -> AlignedBlock {
    let mut c = AlignedBlock::new();
    c.bytes[0..8].copy_from_slice(&COMMIT_MAGIC.to_le_bytes());
    c.bytes[8..16].copy_from_slice(&epoch.to_le_bytes());
    c.bytes[16..24].copy_from_slice(&seq.to_le_bytes());
    c.bytes[24..28].copy_from_slice(&(n as u32).to_le_bytes());
    c.bytes[32..40].copy_from_slice(&sum.to_le_bytes());
    c
}

fn rd_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// Publishes the WAL location; called once after recovery, before any shard is used.
pub fn init(base: usize, area_pages: usize, epoch: u64) {
    let mut st = WAL.lock();
    st.ready = true;
    st.base = base;
    st.area_pages = area_pages;
    st.epoch = epoch;
    st.area = 0;
    st.pos = 0;
    st.seq = 1;
    let entries_a_cap = st.entries_a.capacity();
    if entries_a_cap < HALF_CHECKPOINT_PAGES {
        st.entries_a
            .reserve_exact(HALF_CHECKPOINT_PAGES - entries_a_cap);
    }
    let data_a_cap = st.data_a.capacity();
    if data_a_cap < HALF_CHECKPOINT_PAGES {
        st.data_a.reserve_exact(HALF_CHECKPOINT_PAGES - data_a_cap);
    }
    let scratch_a_cap = st.scratch_a.capacity();
    if scratch_a_cap < SCRATCH_CAP {
        st.scratch_a.reserve_exact(SCRATCH_CAP - scratch_a_cap);
    }
    let entries_b_cap = st.entries_b.capacity();
    if entries_b_cap < HALF_CHECKPOINT_PAGES {
        st.entries_b
            .reserve_exact(HALF_CHECKPOINT_PAGES - entries_b_cap);
    }
    let data_b_cap = st.data_b.capacity();
    if data_b_cap < HALF_CHECKPOINT_PAGES {
        st.data_b.reserve_exact(HALF_CHECKPOINT_PAGES - data_b_cap);
    }
    let scratch_b_cap = st.scratch_b.capacity();
    if scratch_b_cap < SCRATCH_CAP {
        st.scratch_b.reserve_exact(SCRATCH_CAP - scratch_b_cap);
    }
}

struct FoundBatch {
    seq: u64,
    start: usize, // absolute block id of the first descriptor page
    n: usize,
}

/// Walks one area and returns the last structurally valid batch (descriptor + commit headers).
fn scan_area(
    storage: &mut StorageContext,
    area_base: usize,
    area_pages: usize,
    epoch: u64,
) -> Option<FoundBatch> {
    let mut page = AlignedBlock::new();
    let mut pos = 0usize;
    let mut prev_seq: Option<u64> = None;
    let mut last = None;
    while pos + 2 <= area_pages {
        if storage.read_block(area_base + pos, &mut page.bytes).is_err() {
            break;
        }
        if rd_u64(&page.bytes, 0) != DESC_MAGIC || rd_u64(&page.bytes, 8) != epoch || rd_u32(&page.bytes, 28) != 0 {
            break;
        }
        let seq = rd_u64(&page.bytes, 16);
        let n = rd_u32(&page.bytes, 24) as usize;
        if let Some(p) = prev_seq {
            if seq != p + 1 {
                break;
            }
        }
        if n == 0 {
            break;
        }
        let total = desc_pages_for(n) + n + 1;
        if pos + total > area_pages {
            break;
        }
        let commit_at = area_base + pos + desc_pages_for(n) + n;
        if storage.read_block(commit_at, &mut page.bytes).is_err() {
            break;
        }
        if rd_u64(&page.bytes, 0) != COMMIT_MAGIC
            || rd_u64(&page.bytes, 8) != epoch
            || rd_u64(&page.bytes, 16) != seq
            || rd_u32(&page.bytes, 24) as usize != n
        {
            break;
        }
        last = Some(FoundBatch { seq, start: area_base + pos, n });
        prev_seq = Some(seq);
        pos += total;
    }
    last
}

/// Re-applies the newest committed batch of the current epoch. Returns the number of pages applied.
pub fn recover(storage: &mut StorageContext, base: usize, area_pages: usize, epoch: u64) -> usize {
    let a = scan_area(storage, base, area_pages, epoch);
    let b = scan_area(storage, base + area_pages, area_pages, epoch);
    let newest = match (a, b) {
        (Some(x), Some(y)) => Some(if x.seq >= y.seq { x } else { y }),
        (x, y) => x.or(y),
    };
    let Some(batch) = newest else { return 0 };

    let dp = desc_pages_for(batch.n);
    let mut pages = alloc::vec![AlignedBlock::new(); dp + batch.n + 1];
    if storage.read_blocks(batch.start, bytes_of_mut(&mut pages)).is_err() {
        return 0;
    }
    let commit = &pages[dp + batch.n];
    let stored_sum = rd_u64(&commit.bytes, 32);
    if checksum(&pages[..dp + batch.n], CHECKSUM_SEED) != stored_sum {
        // Torn batch: it was never committed, so nothing of it may be applied. The previous
        // batch was fully applied before this one was started.
        tracing::warn!("[wal] newest batch failed checksum, ignored");
        return 0;
    }

    let mut applied = 0;
    for i in 0..batch.n {
        let home = rd_u64(&pages[i / ENTRIES_PER_DESC].bytes, DESC_HDR + (i % ENTRIES_PER_DESC) * 8) as usize;
        let data = &pages[dp + i];
        if storage.write_block(home, &data.bytes).is_ok() {
            applied += 1;
        }
    }
    applied
}

fn snapshot_dirty_shards(
    entries_a: &mut Vec<DirtyPage>,
    data_a: &mut Vec<AlignedBlock>,
    scratch_a: &mut Vec<(usize, usize, u64)>,
    entries_b: &mut Vec<DirtyPage>,
    data_b: &mut Vec<AlignedBlock>,
    scratch_b: &mut Vec<(usize, usize, u64)>,
) -> bool {
    entries_a.clear();
    data_a.clear();
    entries_b.clear();
    data_b.clear();

    let mid_shard = SHARD_COUNT / 2;
    if WAL_B_ONLINE.load(Ordering::Relaxed) {
        let mut more_b = false;
        let job = WalBJob::Snapshot {
            entries_b: entries_b as *mut Vec<DirtyPage>,
            data_b: data_b as *mut Vec<AlignedBlock>,
            scratch_b: scratch_b as *mut Vec<(usize, usize, u64)>,
            more_dirty_b: &mut more_b as *mut bool,
        };
        let job_addr = &job as *const WalBJob as usize;
        WAL_B_JOB_DONE.store(false, Ordering::Release);
        WAL_B_JOB_PTR.store(job_addr, Ordering::Release);

        let more_a = snapshot_shard_range(
            0,
            mid_shard,
            HALF_CHECKPOINT_PAGES,
            entries_a,
            data_a,
            scratch_a,
        );

        if WAL_B_JOB_PTR
            .compare_exchange(job_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            more_b = snapshot_shard_range(
                mid_shard,
                SHARD_COUNT,
                HALF_CHECKPOINT_PAGES,
                entries_b,
                data_b,
                scratch_b,
            );
        } else {
            while !WAL_B_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        more_a || more_b
    } else {
        let more_a = snapshot_shard_range(
            0,
            mid_shard,
            HALF_CHECKPOINT_PAGES,
            entries_a,
            data_a,
            scratch_a,
        );
        let more_b = snapshot_shard_range(
            mid_shard,
            SHARD_COUNT,
            HALF_CHECKPOINT_PAGES,
            entries_b,
            data_b,
            scratch_b,
        );
        more_a || more_b
    }
}

fn clean_shards_parallel(entries_a: &[DirtyPage], entries_b: &[DirtyPage]) {
    let mid_shard = SHARD_COUNT / 2;
    if WAL_B_ONLINE.load(Ordering::Relaxed) && !entries_b.is_empty() {
        let job = WalBJob::Clean {
            entries_b: entries_b as *const [DirtyPage],
        };
        let job_addr = &job as *const WalBJob as usize;
        WAL_B_JOB_DONE.store(false, Ordering::Release);
        WAL_B_JOB_PTR.store(job_addr, Ordering::Release);

        clean_shard_slice(entries_a, 0, mid_shard);

        if WAL_B_JOB_PTR
            .compare_exchange(job_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            clean_shard_slice(entries_b, mid_shard, SHARD_COUNT);
        } else {
            while !WAL_B_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
    } else {
        clean_shard_slice(entries_a, 0, mid_shard);
        clean_shard_slice(entries_b, mid_shard, SHARD_COUNT);
    }
}

/// Non-blocking transaction-boundary WAL snapshot invoked by Hart 6 (`Ledger-A`).
///
/// Captures a transaction-consistent RAM snapshot of dirty pages across Shards `0..7` (Hart 6)
/// and Shards `8..15` (Hart 5) in parallel, then hands the snapshot to Hart 7 (`Storage+Mon`)
/// so synchronous U-Boot NVMe DMA writes execute in the background without stalling Hart 6.
pub fn checkpoint_nonblocking() -> bool {
    if !WAL_BG_ONLINE.load(Ordering::Relaxed) {
        let _ = checkpoint();
        return true;
    }
    if WAL_BG_PENDING.load(Ordering::Acquire) {
        return false;
    }
    let Some(mut st) = WAL.try_lock() else {
        return false;
    };
    if !st.ready {
        return true;
    }
    if WAL_BG_PENDING.load(Ordering::Acquire) {
        return false;
    }

    let t0 = crate::clint::mtime();
    let pre_snap_addr = PRE_SNAPSHOT_HOOK.load(Ordering::Acquire);
    if pre_snap_addr != 0 {
        // SAFETY: `pre_snap_addr` was stored from a valid `fn()` in `set_pre_snapshot_hook`.
        let hook: fn() = unsafe { core::mem::transmute(pre_snap_addr) };
        hook();
    }
    super::bpm::NEED_CHECKPOINT.store(false, Ordering::Relaxed);

    let mut entries_a = core::mem::take(&mut st.entries_a);
    let mut data_a = core::mem::take(&mut st.data_a);
    let mut scratch_a = core::mem::take(&mut st.scratch_a);
    let mut entries_b = core::mem::take(&mut st.entries_b);
    let mut data_b = core::mem::take(&mut st.data_b);
    let mut scratch_b = core::mem::take(&mut st.scratch_b);

    let more_dirty = snapshot_dirty_shards(
        &mut entries_a,
        &mut data_a,
        &mut scratch_a,
        &mut entries_b,
        &mut data_b,
        &mut scratch_b,
    );
    if more_dirty {
        super::bpm::NEED_CHECKPOINT.store(true, Ordering::Relaxed);
    }

    let has_dirty = !entries_a.is_empty() || !entries_b.is_empty();
    st.entries_a = entries_a;
    st.data_a = data_a;
    st.scratch_a = scratch_a;
    st.entries_b = entries_b;
    st.data_b = data_b;
    st.scratch_b = scratch_b;

    WAL_STATS[3].fetch_add(crate::clint::mtime() - t0, Ordering::Relaxed);
    if has_dirty {
        WAL_BG_PENDING.store(true, Ordering::Release);
    }
    true
}

/// Polled by Hart 7 (`Storage+Mon`) to persist a snapshotted WAL batch to NVMe SSD
/// in the background while Harts 5 & 6 continue processing blocks in DRAM.
pub fn try_run_wal_bg_flush() -> bool {
    if !WAL_BG_PENDING.load(Ordering::Acquire) {
        return false;
    }
    let mut st = WAL.lock();
    if !WAL_BG_PENDING.load(Ordering::Acquire) {
        return false;
    }
    let Some(storage) = GLOBAL_STORAGE.lock().clone() else {
        WAL_BG_PENDING.store(false, Ordering::Release);
        return false;
    };
    let t0 = crate::clint::mtime();

    let entries_a = core::mem::take(&mut st.entries_a);
    let data_a = core::mem::take(&mut st.data_a);
    let entries_b = core::mem::take(&mut st.entries_b);
    let data_b = core::mem::take(&mut st.data_b);

    if !entries_a.is_empty() || !entries_b.is_empty() {
        if write_batch_split(&mut st, &storage, &entries_a, &data_a, &entries_b, &data_b).is_ok() {
            clean_shards_parallel(&entries_a, &entries_b);
            WAL_STATS[0].fetch_add(1, Ordering::Relaxed);
        }
    }

    WAL_STATS[3].fetch_add(crate::clint::mtime() - t0, Ordering::Relaxed);
    st.entries_a = entries_a;
    st.data_a = data_a;
    st.entries_b = entries_b;
    st.data_b = data_b;
    WAL_BG_PENDING.store(false, Ordering::Release);
    true
}

/// Writes all dirty pages of all shards atomically (WAL commit, then in-place apply).
/// Must be called by the ledger writer between transactions so the cut is consistent.
/// Returns the number of pages persisted.
pub fn checkpoint() -> Result<usize, StorageError> {
    let mut st = WAL.lock();
    if !st.ready {
        return Ok(0);
    }
    let storage = match GLOBAL_STORAGE.lock().clone() {
        Some(s) => s,
        None => return Ok(0),
    };
    let t0 = crate::clint::mtime();

    let mut entries_a = core::mem::take(&mut st.entries_a);
    let mut data_a = core::mem::take(&mut st.data_a);
    let mut scratch_a = core::mem::take(&mut st.scratch_a);
    let mut entries_b = core::mem::take(&mut st.entries_b);
    let mut data_b = core::mem::take(&mut st.data_b);
    let mut scratch_b = core::mem::take(&mut st.scratch_b);

    let mut total_persisted = 0usize;
    let mut result = Ok(0usize);

    // If a background snapshot was queued just before `checkpoint()` acquired `WAL.lock()`,
    // flush that pending snapshot first.
    if WAL_BG_PENDING.load(Ordering::Acquire) {
        let pending_pages = entries_a.len() + entries_b.len();
        if pending_pages > 0 {
            if let Err(e) =
                write_batch_split(&mut st, &storage, &entries_a, &data_a, &entries_b, &data_b)
            {
                st.entries_a = entries_a;
                st.data_a = data_a;
                st.scratch_a = scratch_a;
                st.entries_b = entries_b;
                st.data_b = data_b;
                st.scratch_b = scratch_b;
                WAL_BG_PENDING.store(false, Ordering::Release);
                return Err(e);
            }
            clean_shards_parallel(&entries_a, &entries_b);
            total_persisted += pending_pages;
            result = Ok(total_persisted);
        }
        WAL_BG_PENDING.store(false, Ordering::Release);
    }

    let pre_snap_addr = PRE_SNAPSHOT_HOOK.load(Ordering::Acquire);
    if pre_snap_addr != 0 {
        // SAFETY: `pre_snap_addr` was stored from a valid `fn()` in `set_pre_snapshot_hook`.
        let hook: fn() = unsafe { core::mem::transmute(pre_snap_addr) };
        hook();
    }
    super::bpm::NEED_CHECKPOINT.store(false, Ordering::Relaxed);

    loop {
        let more_dirty = snapshot_dirty_shards(
            &mut entries_a,
            &mut data_a,
            &mut scratch_a,
            &mut entries_b,
            &mut data_b,
            &mut scratch_b,
        );

        let batch_pages = entries_a.len() + entries_b.len();
        if batch_pages == 0 {
            break;
        }

        if let Err(e) =
            write_batch_split(&mut st, &storage, &entries_a, &data_a, &entries_b, &data_b)
        {
            result = Err(e);
            break;
        }

        clean_shards_parallel(&entries_a, &entries_b);

        total_persisted += batch_pages;
        result = Ok(total_persisted);
        if !more_dirty {
            break;
        }
    }

    if total_persisted > 0 {
        WAL_STATS[0].fetch_add(1, Ordering::Relaxed);
    }
    WAL_STATS[3].fetch_add(crate::clint::mtime() - t0, Ordering::Relaxed);
    st.entries_a = entries_a;
    st.data_a = data_a;
    st.scratch_a = scratch_a;
    st.entries_b = entries_b;
    st.data_b = data_b;
    st.scratch_b = scratch_b;
    result
}

fn write_wal_slice(
    storage: &alloc::sync::Arc<Mutex<StorageContext>>,
    start_block: usize,
    data: &[AlignedBlock],
) -> Result<(), StorageError> {
    const WAL_WRITE_CHUNK: usize = 64;
    let n = data.len();
    let mut written = 0;
    while written < n {
        let chunk = (n - written).min(WAL_WRITE_CHUNK);
        storage.lock().write_blocks(
            start_block + written,
            bytes_of(&data[written..written + chunk]),
        )?;
        written += chunk;
    }
    Ok(())
}

fn apply_sorted_home_runs(
    storage: &alloc::sync::Arc<Mutex<StorageContext>>,
    entries: &[DirtyPage],
    data: &[AlignedBlock],
) -> Result<(), StorageError> {
    const MAX_HOME_RUN: usize = 64;
    let n = entries.len();
    let mut k = 0;
    while k < n {
        let first_home = entries[k].home;
        let mut run_len = 1;
        while k + run_len < n
            && run_len < MAX_HOME_RUN
            && entries[k + run_len].home == first_home + run_len as u64
        {
            run_len += 1;
        }
        storage
            .lock()
            .write_blocks(first_home as usize, bytes_of(&data[k..k + run_len]))?;
        WAL_STATS[2].fetch_add(run_len as u64, Ordering::Relaxed);
        k += run_len;
    }
    Ok(())
}

fn write_batch_split(
    st: &mut WalState,
    storage: &alloc::sync::Arc<Mutex<StorageContext>>,
    entries_a: &[DirtyPage],
    data_a: &[AlignedBlock],
    entries_b: &[DirtyPage],
    data_b: &[AlignedBlock],
) -> Result<(), StorageError> {
    let na = entries_a.len();
    let nb = entries_b.len();
    let n = na + nb;
    let dp = desc_pages_for(n);
    let total = dp + n + 1;
    if st.pos + total > st.area_pages {
        st.area ^= 1;
        st.pos = 0;
    }
    let seq = st.seq;
    let desc = make_desc_pages(entries_a, entries_b, st.epoch, seq);
    let sum = checksum(data_b, checksum(data_a, checksum(&desc, CHECKSUM_SEED)));
    let commit = make_commit_page(st.epoch, seq, n, sum);

    let at = st.base + st.area * st.area_pages + st.pos;
    let hook_addr = PRE_COMMIT_HOOK.load(Ordering::Acquire);
    if hook_addr != 0 {
        // SAFETY: `hook_addr` was stored from a valid `fn(&Mutex<StorageContext>) -> Result<(), StorageError>`
        // in `set_pre_commit_hook`.
        let hook: fn(&Mutex<StorageContext>) -> Result<(), StorageError> =
            unsafe { core::mem::transmute(hook_addr) };
        hook(storage)?;
    }
    crate::watchdog::kick();
    storage.lock().write_blocks(at, bytes_of(&desc))?;
    if na > 0 {
        write_wal_slice(storage, at + dp, data_a)?;
    }
    if nb > 0 {
        write_wal_slice(storage, at + dp + na, data_b)?;
    }
    // The commit page goes last: the batch exists only once this write completes.
    storage.lock().write_blocks(at + dp + n, &commit.bytes)?;
    st.pos += total;
    st.seq += 1;
    WAL_STATS[1].fetch_add(n as u64, Ordering::Relaxed);
    crate::watchdog::kick();

    // Apply in place in ascending LBA order. Since `entries_a` (Shards 0..7) and `entries_b`
    // (Shards 8..15) are already sorted by `(shard, page_id)`, consecutive home runs are
    // already contiguous in `data_a` and `data_b` (zero heap allocation, zero 4 KB page copies).
    if na > 0 {
        apply_sorted_home_runs(storage, entries_a, data_a)?;
    }
    if nb > 0 {
        apply_sorted_home_runs(storage, entries_b, data_b)?;
    }
    crate::watchdog::kick();
    Ok(())
}
