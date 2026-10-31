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
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

static PRE_SNAPSHOT_HOOK: AtomicUsize = AtomicUsize::new(0);
static PRE_COMMIT_HOOK: AtomicUsize = AtomicUsize::new(0);

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
    entries: Vec<DirtyPage>,
    data: Vec<AlignedBlock>,
}

static WAL: Mutex<WalState> = Mutex::new(WalState {
    ready: false,
    base: 0,
    area_pages: 0,
    epoch: 0,
    area: 0,
    pos: 0,
    seq: 1,
    entries: Vec::new(),
    data: Vec::new(),
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

fn make_desc_pages(entries: &[DirtyPage], epoch: u64, seq: u64) -> Vec<AlignedBlock> {
    let n = entries.len();
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
        for (k, e) in entries[first..last].iter().enumerate() {
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
    let entries_cap = st.entries.capacity();
    if entries_cap < MAX_CHECKPOINT_PAGES {
        st.entries.reserve_exact(MAX_CHECKPOINT_PAGES - entries_cap);
    }
    let data_cap = st.data.capacity();
    if data_cap < MAX_CHECKPOINT_PAGES {
        st.data.reserve_exact(MAX_CHECKPOINT_PAGES - data_cap);
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
        crate::uart::print(b"[wal] newest batch failed checksum, ignored\n");
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

    let mut entries = core::mem::take(&mut st.entries);
    let mut data = core::mem::take(&mut st.data);
    super::bpm::NEED_CHECKPOINT.store(false, Ordering::Relaxed);

    let mut total_persisted = 0usize;
    let mut result = Ok(0usize);

    let pre_snap_addr = PRE_SNAPSHOT_HOOK.load(Ordering::Acquire);
    if pre_snap_addr != 0 {
        // SAFETY: `pre_snap_addr` was stored from a valid `fn()` in `set_pre_snapshot_hook`.
        let hook: fn() = unsafe { core::mem::transmute(pre_snap_addr) };
        hook();
    }

    loop {
        entries.clear();
        data.clear();
        let mut more_dirty = false;
        for i in 0..SHARD_COUNT {
            let guard = SHARDS[i].lock();
            if let Some(db) = guard.as_ref() {
                for &fid in db.bpm.dirty_frame_ids() {
                    if entries.len() >= MAX_CHECKPOINT_PAGES {
                        more_dirty = true;
                        break;
                    }
                    let m = db.bpm.frame_meta(fid);
                    if m.is_dirty {
                        if let Some(pid) = m.page_id() {
                            entries.push(DirtyPage {
                                shard: i as u8,
                                page_id: pid,
                                gen: m.gen,
                                home: (pid + db.bpm.base_page) as u64,
                            });
                            data.push(*db.bpm.frame_data(fid));
                        }
                    }
                }
            }
            if more_dirty {
                break;
            }
        }

        let batch_pages = entries.len();
        if batch_pages == 0 {
            break;
        }

        if let Err(e) = write_batch(&mut st, &storage, &mut entries[..], &mut data[..]) {
            result = Err(e);
            break;
        }

        // Mark pages clean only if they were not modified again after the snapshot.
        for i in 0..SHARD_COUNT {
            let mut guard = SHARDS[i].lock();
            if let Some(db) = guard.as_mut() {
                for e in entries.iter().filter(|e| e.shard as usize == i) {
                    db.bpm.mark_clean(e.page_id, e.gen);
                }
                db.bpm.trim_excess();
            }
        }

        total_persisted += batch_pages;
        result = Ok(total_persisted);
        if !more_dirty {
            break;
        }
    }

    WAL_STATS[0].fetch_add(1, Ordering::Relaxed);
    WAL_STATS[3].fetch_add(crate::clint::mtime() - t0, Ordering::Relaxed);
    st.entries = entries;
    st.data = data;
    result
}

fn write_batch(
    st: &mut WalState,
    storage: &alloc::sync::Arc<Mutex<StorageContext>>,
    entries: &mut [DirtyPage],
    data: &mut [AlignedBlock],
) -> Result<(), StorageError> {
    let n = entries.len();
    let dp = desc_pages_for(n);
    let total = dp + n + 1;
    if st.pos + total > st.area_pages {
        st.area ^= 1;
        st.pos = 0;
    }
    let seq = st.seq;
    let desc = make_desc_pages(entries, st.epoch, seq);
    let sum = checksum(data, checksum(&desc, CHECKSUM_SEED));
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
    storage.lock().write_blocks(at, bytes_of(&desc))?;
    const WAL_WRITE_CHUNK: usize = 64;
    let mut written = 0;
    while written < n {
        let chunk = (n - written).min(WAL_WRITE_CHUNK);
        storage
            .lock()
            .write_blocks(at + dp + written, bytes_of(&data[written..written + chunk]))?;
        written += chunk;
    }
    // The commit page goes last: the batch exists only once this write completes.
    storage.lock().write_blocks(at + dp + n, &commit.bytes)?;
    st.pos += total;
    st.seq += 1;
    WAL_STATS[1].fetch_add(n as u64, Ordering::Relaxed);

    // Apply in place in address order. Runs of consecutive home pages (common for freshly
    // allocated B-tree pages) are coalesced into one device command of up to 32 pages.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by_key(|&i| entries[i].home);
    let mut run: Vec<AlignedBlock> = Vec::with_capacity(32);
    let mut k = 0;
    while k < n {
        let first_home = entries[order[k]].home;
        run.clear();
        run.push(data[order[k]]);
        while k + run.len() < n
            && run.len() < 32
            && entries[order[k + run.len()]].home == first_home + run.len() as u64
        {
            run.push(data[order[k + run.len()]]);
        }
        storage.lock().write_blocks(first_home as usize, bytes_of(&run))?;
        WAL_STATS[2].fetch_add(run.len() as u64, Ordering::Relaxed);
        k += run.len();
    }
    Ok(())
}
