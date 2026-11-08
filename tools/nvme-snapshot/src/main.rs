//! Direct Bare-Metal NVMe Ledger Snapshot & Restore Tool (`nvme-snapshot`)
//!
//! Reads a synced `barely-nano` NVMe SSD (e.g. `/dev/sda` over USB-C) and writes a compact,
//! checksummed raw ledger snapshot file containing only the allocated B-Tree pages (`0..next_free_page`)
//! of each of the 16 shards plus the 64 MB `BlockBloom` filter (and optionally the 16 MB hardware-bound
//! wallet partition), with an option to write/restore the raw snapshot file back to any NVMe SSD.
//!
//! ### Key Capabilities
//! 1. **Compact Sparse-Shard Streaming (`~90.4 GiB` unpruned / `~30 GiB` pruned vs `1,000 GB` raw disk):**
//!    Instead of `dd`-ing 1 TB of mostly empty LBA space, `nvme-snapshot` reads each shard's
//!    `MetaData` page (`0xBEEFCB03`) and streams only the allocated pages (`0 .. next_free_page`)
//!    in **16 MB sequential chunks**.
//! 2. **Automatic Non-Destructive WAL Replay on Dump:**
//!    Scans both 128 MB WAL areas (`wal_base` and `wal_base + wal_pages / 2`) using `barely-nano`'s
//!    exact `WAL_DESC` / `WAL_CMIT` + 64-bit checksum verification. If the board was powered off
//!    with a committed WAL batch not yet written to its home shard pages, those pages are overlaid
//!    in memory during the dump so the snapshot is 100% transactionally consistent while keeping
//!    `/dev/sda` opened strictly read-only (`O_RDONLY`).
//! 3. **Cross-Drive Capacity Portability on Restore:**
//!    Restoring a snapshot onto a different NVMe drive (e.g. 1 TB $\rightarrow$ 256 GB or 512 GB)
//!    automatically recalculates `target_shard_size`, `wal_base`, and `wal_pages` for the target
//!    drive and places each shard `0..16` at its proper target `base_page`.
//! 4. **End-to-End 64-Bit Integrity Checksums & Hardware Wallet Protection:**
//!    Every region (64 MB Bloom filter, each of the 16 B-Tree shards, and optional wallet region)
//!    carries a 64-bit checksum in the 4 KB `BNSNAPV1` file header, verified on restore before
//!    committing `STORAGE_MAGIC` (`"SHARDSH4"`). By default, restoring a ledger snapshot never
//!    touches `WALLET_STORAGE_LBA` (`384 MB` mark), preserving the node's eFuse-bound wallet keys.

use std::collections::HashMap;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{FileExt, FileTypeExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// ── Bare-Metal NVMe Layout Constants (from `common/src/storage/mod.rs`) ──────
const SECTOR_SIZE: u64 = 512;
const BLOCK_SIZE: usize = 4096;
const SHARD_COUNT: usize = 16;

const BLOOM_HEADER_LBA: u64 = 524_288; // 256 MB mark (1 page = 4 KB)
const BLOOM_DATA_LBA: u64 = 524_296; // 64 MB = 16,384 pages = 131,072 sectors
const BLOOM_BYTES: usize = 64 * 1024 * 1024;
const BLOOM_MAGIC: u64 = 0x424C_4F4D_5632_0006; // "BLOMV2\x00\x06"

const WALLET_STORAGE_LBA: u64 = 786_432; // 384 MB mark
const WALLET_TOTAL_PAGES: usize = 4096 + 1 + 64; // 4096 B-Tree pages + 1 header + 64 journal pages (16.25 MB)

const STORAGE_BASE_LBA: u64 = 1_048_576; // 512 MB mark
const STORAGE_MAGIC: u64 = 0x5348_4152_4453_4834; // "SHARDSH4"
const BTREE_META_MAGIC: u32 = 0xBEEF_CB03;

// ── WAL Constants (from `common/src/storage/wal.rs`) ─────────────────────────
const WAL_PAGES_DEFAULT: usize = 65_536;
const DESC_MAGIC: u64 = 0x5741_4C5F_4445_5343; // "WAL_DESC"
const COMMIT_MAGIC: u64 = 0x5741_4C5F_434D_4954; // "WAL_CMIT"
const DESC_HDR: usize = 32;
const ENTRIES_PER_DESC: usize = (BLOCK_SIZE - DESC_HDR) / 8; // 508 entries per 4 KB descriptor page
const CHECKSUM_SEED: u64 = 0x243F_6A88_85A3_08D3;

// ── Snapshot File Format (`BNSNAPV1`) ────────────────────────────────────────
/// 4 KB header at byte offset `0` of the snapshot file:
/// - `[0..8]`:     `SNAPSHOT_MAGIC` (`0x424E_534E_4150_5631` = `"BNSNAPV1"`)
/// - `[8..12]`:    `version: u32` (`1`)
/// - `[12..16]`:   `flags: u32` (`bit 0` = has valid Bloom filter, `bit 1` = includes wallet region)
/// - `[16..24]`:   `created_unix_secs: u64`
/// - `[24..32]`:   `source_shard_size_pages: u64`
/// - `[32..40]`:   `cached_accounts: u64`
/// - `[40..48]`:   `cached_blocks: u64`
/// - `[48..56]`:   `cached_confirmed: u64`
/// - `[56..64]`:   `cached_pruned: u64`
/// - `[64..72]`:   `bloom_offset: u64` (byte offset in snapshot file, `4096`)
/// - `[72..80]`:   `bloom_len: u64` (`4096 + 67,108,864 = 67,112,960` bytes: 4 KB header + 64 MB data)
/// - `[80..88]`:   `bloom_checksum: u64`
/// - `[88..96]`:   `wallet_offset: u64` (`0` if not included)
/// - `[96..104]`:  `wallet_len: u64` (`0` or `WALLET_TOTAL_PAGES * 4096`)
/// - `[104..112]`: `wallet_checksum: u64`
/// - `[112..128]`: reserved (`0`)
/// - `[128..640]`: 16 × 32-byte `ShardSnapEntry` records (`shard_idx = 0..16`):
///   - `+0..8`:    `file_offset: u64` (byte offset of shard pages in snapshot file)
///   - `+8..12`:   `next_free_page: u32` (number of 4 KB pages stored for this shard)
///   - `+12..16`:  `root_page_id: u32`
///   - `+16..20`:  `first_free_page: u32`
///   - `+20..24`:  reserved (`0`)
///   - `+24..32`:  `checksum: u64` (64-bit checksum over all `next_free_page * 4096` bytes)
/// - `[4088..4096]`: `header_checksum: u64` (checksum over bytes `0..4088`)
const SNAPSHOT_MAGIC: u64 = 0x424E_534E_4150_5631; // "BNSNAPV1"
const SNAPSHOT_VERSION: u32 = 1;
const FLAG_HAS_BLOOM: u32 = 1 << 0;
const FLAG_HAS_WALLETS: u32 = 1 << 1;

const IO_CHUNK_BYTES: usize = 16 * 1024 * 1024; // 16 MB sequential I/O buffer

#[derive(Clone, Copy, Debug, Default)]
struct ShardSnapEntry {
    file_offset: u64,
    next_free_page: u32,
    root_page_id: u32,
    first_free_page: u32,
    checksum: u64,
}

#[derive(Clone, Debug)]
struct SnapshotHeader {
    version: u32,
    flags: u32,
    created_unix_secs: u64,
    source_shard_size_pages: u64,
    cached_accounts: u64,
    cached_blocks: u64,
    cached_confirmed: u64,
    cached_pruned: u64,
    bloom_offset: u64,
    bloom_len: u64,
    bloom_checksum: u64,
    wallet_offset: u64,
    wallet_len: u64,
    wallet_checksum: u64,
    shards: [ShardSnapEntry; SHARD_COUNT],
}

impl SnapshotHeader {
    fn to_page(&self) -> [u8; BLOCK_SIZE] {
        let mut buf = [0u8; BLOCK_SIZE];
        buf[0..8].copy_from_slice(&SNAPSHOT_MAGIC.to_le_bytes());
        buf[8..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..16].copy_from_slice(&self.flags.to_le_bytes());
        buf[16..24].copy_from_slice(&self.created_unix_secs.to_le_bytes());
        buf[24..32].copy_from_slice(&self.source_shard_size_pages.to_le_bytes());
        buf[32..40].copy_from_slice(&self.cached_accounts.to_le_bytes());
        buf[40..48].copy_from_slice(&self.cached_blocks.to_le_bytes());
        buf[48..56].copy_from_slice(&self.cached_confirmed.to_le_bytes());
        buf[56..64].copy_from_slice(&self.cached_pruned.to_le_bytes());
        buf[64..72].copy_from_slice(&self.bloom_offset.to_le_bytes());
        buf[72..80].copy_from_slice(&self.bloom_len.to_le_bytes());
        buf[80..88].copy_from_slice(&self.bloom_checksum.to_le_bytes());
        buf[88..96].copy_from_slice(&self.wallet_offset.to_le_bytes());
        buf[96..104].copy_from_slice(&self.wallet_len.to_le_bytes());
        buf[104..112].copy_from_slice(&self.wallet_checksum.to_le_bytes());

        for (i, s) in self.shards.iter().enumerate() {
            let off = 128 + i * 32;
            buf[off..off + 8].copy_from_slice(&s.file_offset.to_le_bytes());
            buf[off + 8..off + 12].copy_from_slice(&s.next_free_page.to_le_bytes());
            buf[off + 12..off + 16].copy_from_slice(&s.root_page_id.to_le_bytes());
            buf[off + 16..off + 20].copy_from_slice(&s.first_free_page.to_le_bytes());
            buf[off + 24..off + 32].copy_from_slice(&s.checksum.to_le_bytes());
        }

        let hdr_sum = hash_bytes(&buf[..4088], CHECKSUM_SEED);
        buf[4088..4096].copy_from_slice(&hdr_sum.to_le_bytes());
        buf
    }

    fn from_page(buf: &[u8; BLOCK_SIZE]) -> io::Result<Self> {
        let magic = rd_u64(buf, 0);
        if magic != SNAPSHOT_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Invalid snapshot magic: expected 0x{:016X} (\"BNSNAPV1\"), found 0x{:016X}",
                    SNAPSHOT_MAGIC, magic
                ),
            ));
        }
        let expected_sum = rd_u64(buf, 4088);
        let actual_sum = hash_bytes(&buf[..4088], CHECKSUM_SEED);
        if expected_sum != actual_sum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Snapshot header checksum mismatch: expected 0x{:016X}, computed 0x{:016X}",
                    expected_sum, actual_sum
                ),
            ));
        }
        let version = rd_u32(buf, 8);
        if version != SNAPSHOT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported snapshot version: {}", version),
            ));
        }

        let mut shards = [ShardSnapEntry::default(); SHARD_COUNT];
        for i in 0..SHARD_COUNT {
            let off = 128 + i * 32;
            shards[i] = ShardSnapEntry {
                file_offset: rd_u64(buf, off),
                next_free_page: rd_u32(buf, off + 8),
                root_page_id: rd_u32(buf, off + 12),
                first_free_page: rd_u32(buf, off + 16),
                checksum: rd_u64(buf, off + 24),
            };
        }

        Ok(Self {
            version,
            flags: rd_u32(buf, 12),
            created_unix_secs: rd_u64(buf, 16),
            source_shard_size_pages: rd_u64(buf, 24),
            cached_accounts: rd_u64(buf, 32),
            cached_blocks: rd_u64(buf, 40),
            cached_confirmed: rd_u64(buf, 48),
            cached_pruned: rd_u64(buf, 56),
            bloom_offset: rd_u64(buf, 64),
            bloom_len: rd_u64(buf, 72),
            bloom_checksum: rd_u64(buf, 80),
            wallet_offset: rd_u64(buf, 88),
            wallet_len: rd_u64(buf, 96),
            wallet_checksum: rd_u64(buf, 104),
            shards,
        })
    }
}

#[inline]
fn rd_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

#[inline]
fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// Fast 64-bit word-mixing hash matching `common/src/storage/wal.rs::checksum`.
#[inline]
fn hash_bytes(bytes: &[u8], mut h: u64) -> u64 {
    for w in bytes.chunks_exact(8) {
        let v = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
        h = (h ^ v).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 32;
    }
    h
}

fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = file.read_at(buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("Unexpected EOF reading at byte offset {}", offset),
            ));
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = file.write_at(buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("Failed to write at byte offset {}", offset),
            ));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

/// Queries the size in bytes of a block device or regular file.
fn query_device_or_file_bytes(file: &File) -> io::Result<u64> {
    let meta = file.metadata()?;
    if meta.file_type().is_block_device() {
        // BLKGETSIZE64 on Linux x86_64 = _IOR(0x12, 114, size_t) = 0x80081272
        const BLKGETSIZE64: u64 = 0x8008_1272;
        let mut size_bytes: u64 = 0;
        // SAFETY: `file.as_raw_fd()` is an open valid file descriptor, and `BLKGETSIZE64`
        // writes a single 64-bit unsigned integer into `size_bytes`.
        let rc = unsafe { ioctl(file.as_raw_fd(), BLKGETSIZE64, &mut size_bytes as *mut u64) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(size_bytes)
    } else {
        Ok(meta.len())
    }
}

/// Ensures a target device path is not mounted or backing `/`.
fn verify_target_not_mounted(target_path: &Path) -> io::Result<()> {
    let canon = std::fs::canonicalize(target_path).unwrap_or_else(|_| target_path.to_path_buf());
    let canon_str = canon.to_string_lossy();
    let f = File::open("/proc/mounts")?;
    for line in BufReader::new(f).lines() {
        let line = line?;
        let mut parts = line.split_whitespace();
        if let (Some(dev), Some(mnt)) = (parts.next(), parts.next()) {
            if dev == canon_str || (canon_str.starts_with("/dev/") && dev.starts_with(canon_str.as_ref())) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "Refusing to write to {}: partition {} is currently mounted at {}",
                        canon_str, dev, mnt
                    ),
                ));
            }
        }
    }
    Ok(())
}

// ── Non-Destructive In-Memory WAL Replay ─────────────────────────────────────

fn desc_pages_for(n: usize) -> usize {
    (n + ENTRIES_PER_DESC - 1) / ENTRIES_PER_DESC
}

struct FoundWalBatch {
    seq: u64,
    start_block: usize,
    n: usize,
}

fn scan_wal_area(
    nvme: &File,
    area_base: usize,
    area_pages: usize,
    epoch: u64,
) -> Option<FoundWalBatch> {
    let mut page = [0u8; BLOCK_SIZE];
    let mut pos = 0usize;
    let mut prev_seq: Option<u64> = None;
    let mut last = None;

    while pos + 2 <= area_pages {
        let blk_off = (STORAGE_BASE_LBA + ((area_base + pos) as u64) * 8) * SECTOR_SIZE;
        if read_exact_at(nvme, &mut page, blk_off).is_err() {
            break;
        }
        if rd_u64(&page, 0) != DESC_MAGIC
            || rd_u64(&page, 8) != epoch
            || rd_u32(&page, 28) != 0
        {
            break;
        }
        let seq = rd_u64(&page, 16);
        let n = rd_u32(&page, 24) as usize;
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
        let commit_blk = area_base + pos + desc_pages_for(n) + n;
        let commit_off = (STORAGE_BASE_LBA + (commit_blk as u64) * 8) * SECTOR_SIZE;
        if read_exact_at(nvme, &mut page, commit_off).is_err() {
            break;
        }
        if rd_u64(&page, 0) != COMMIT_MAGIC
            || rd_u64(&page, 8) != epoch
            || rd_u64(&page, 16) != seq
            || rd_u32(&page, 24) as usize != n
        {
            break;
        }
        last = Some(FoundWalBatch {
            seq,
            start_block: area_base + pos,
            n,
        });
        prev_seq = Some(seq);
        pos += total;
    }
    last
}

/// Scans both WAL areas on `nvme` and returns an in-memory map of `home_block_id -> [u8; 4096]`
/// for the newest valid committed WAL batch of `epoch` (if any).
fn collect_wal_overlay(
    nvme: &File,
    wal_base: usize,
    area_pages: usize,
    epoch: u64,
) -> HashMap<usize, [u8; BLOCK_SIZE]> {
    let mut overlay = HashMap::new();
    let a = scan_wal_area(nvme, wal_base, area_pages, epoch);
    let b = scan_wal_area(nvme, wal_base + area_pages, area_pages, epoch);
    let newest = match (a, b) {
        (Some(x), Some(y)) => Some(if x.seq >= y.seq { x } else { y }),
        (x, y) => x.or(y),
    };
    let Some(batch) = newest else {
        return overlay;
    };

    let dp = desc_pages_for(batch.n);
    let total_pages = dp + batch.n + 1;
    let mut raw = vec![0u8; total_pages * BLOCK_SIZE];
    let start_off = (STORAGE_BASE_LBA + (batch.start_block as u64) * 8) * SECTOR_SIZE;
    if read_exact_at(nvme, &mut raw, start_off).is_err() {
        return overlay;
    }

    let commit_slice = &raw[(dp + batch.n) * BLOCK_SIZE..total_pages * BLOCK_SIZE];
    let stored_sum = rd_u64(commit_slice, 32);
    let calc_sum = hash_bytes(&raw[..(dp + batch.n) * BLOCK_SIZE], CHECKSUM_SEED);
    if stored_sum != calc_sum {
        eprintln!("  [WARN] Newest WAL batch (seq={}) failed checksum; ignoring torn batch.", batch.seq);
        return overlay;
    }

    for i in 0..batch.n {
        let desc_idx = i / ENTRIES_PER_DESC;
        let desc_slot = i % ENTRIES_PER_DESC;
        let desc_page = &raw[desc_idx * BLOCK_SIZE..(desc_idx + 1) * BLOCK_SIZE];
        let home = rd_u64(desc_page, DESC_HDR + desc_slot * 8) as usize;
        let mut page_data = [0u8; BLOCK_SIZE];
        page_data.copy_from_slice(&raw[(dp + i) * BLOCK_SIZE..(dp + i + 1) * BLOCK_SIZE]);
        overlay.insert(home, page_data);
    }
    overlay
}

fn read_storage_block_with_overlay(
    nvme: &File,
    block_id: usize,
    overlay: &HashMap<usize, [u8; BLOCK_SIZE]>,
    out: &mut [u8; BLOCK_SIZE],
) -> io::Result<()> {
    if let Some(p) = overlay.get(&block_id) {
        out.copy_from_slice(p);
        return Ok(());
    }
    let off = (STORAGE_BASE_LBA + (block_id as u64) * 8) * SECTOR_SIZE;
    read_exact_at(nvme, out, off)
}

// ── Slotted-Page B-Tree Reader (for extracting `CACHCNTS` from Shard 4) ──────

fn cmp_cell_key(cell_key: &[u8], target: &[u8; 65]) -> std::cmp::Ordering {
    let n = cell_key.len().min(65);
    let cmp = cell_key[..n].cmp(&target[..n]);
    if cmp != std::cmp::Ordering::Equal {
        return cmp;
    }
    if n < 65 && target[n..65].iter().any(|&b| b != 0) {
        std::cmp::Ordering::Less
    } else {
        std::cmp::Ordering::Equal
    }
}

fn btree_lookup<F>(
    root_page: u32,
    target_key: &[u8; 65],
    mut read_shard_page: F,
) -> Option<Vec<u8>>
where
    F: FnMut(u32, &mut [u8; BLOCK_SIZE]) -> io::Result<()>,
{
    if root_page == 0 {
        return None;
    }
    let mut curr = root_page;
    let mut page = [0u8; BLOCK_SIZE];
    for _depth in 0..16 {
        read_shard_page(curr, &mut page).ok()?;
        let is_leaf = page[0] != 0;
        let nk = (u16::from_le_bytes([page[1], page[2]]) as usize).min(64);

        let mut lo = 0usize;
        let mut hi = nk;
        while lo < hi {
            let mid = (lo + hi) >> 1;
            let slot_off = if is_leaf { 8 + mid * 2 } else { 12 + mid * 6 };
            let cell_off = u16::from_le_bytes([page[slot_off], page[slot_off + 1]]) as usize;
            if cell_off + 3 > BLOCK_SIZE {
                return None;
            }
            let klen = page[cell_off] as usize;
            let vlen = u16::from_le_bytes([page[cell_off + 1], page[cell_off + 2]]) as usize;
            if cell_off + 3 + klen + vlen > BLOCK_SIZE {
                return None;
            }
            let ck = &page[cell_off + 3..cell_off + 3 + klen];
            match cmp_cell_key(ck, target_key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    return Some(page[cell_off + 3 + klen..cell_off + 3 + klen + vlen].to_vec());
                }
            }
        }
        if is_leaf {
            return None;
        }
        let child_off = 8 + lo * 6;
        curr = rd_u32(&page, child_off);
        if curr == 0 {
            return None;
        }
    }
    None
}

/// Reads `CACHCNTS` (`TABLE_VERSION = 11`, key `b"CACHCNTS"`) from Shard 4.
fn read_cached_counts_from_shard4<F>(root_page: u32, read_page: F) -> (u64, u64, u64, u64)
where
    F: FnMut(u32, &mut [u8; BLOCK_SIZE]) -> io::Result<()>,
{
    let mut key = [0u8; 65];
    key[0] = 11; // TABLE_VERSION
    key[1..9].copy_from_slice(b"CACHCNTS");
    if let Some(val) = btree_lookup(root_page, &key, read_page) {
        if val.len() >= 32 {
            return (
                rd_u64(&val, 0),
                rd_u64(&val, 8),
                rd_u64(&val, 16),
                rd_u64(&val, 24),
            );
        } else if val.len() >= 24 {
            return (rd_u64(&val, 0), rd_u64(&val, 8), rd_u64(&val, 16), 0);
        }
    }
    (0, 0, 0, 0)
}

// ── Command 1: Dump NVMe -> Raw Snapshot File ────────────────────────────────

fn run_dump(
    nvme_path: &Path,
    out_path: &Path,
    include_wallets: bool,
    dry_run: bool,
) -> io::Result<()> {
    let t0 = Instant::now();
    println!("==================================================================");
    println!(" Barely-Nano NVMe -> Raw Ledger Snapshot Tool (DUMP)");
    println!("==================================================================");
    println!("  Source NVMe  : {}", nvme_path.display());
    println!("  Snapshot Out : {}", out_path.display());
    println!("  Wallets      : {}", if include_wallets { "INCLUDE (LBA 786432)" } else { "SKIP (preserve target NVMe wallets)" });
    println!("  Mode         : {}", if dry_run { "DRY RUN (no file written)" } else { "LIVE DUMP" });
    println!("------------------------------------------------------------------");

    let nvme = File::open(nvme_path)?;
    let dev_bytes = query_device_or_file_bytes(&nvme)?;
    let total_pages = (dev_bytes / (BLOCK_SIZE as u64)) as usize;
    println!(
        "[1/4] Inspecting source NVMe ({:.2} GiB / {} pages)...",
        dev_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
        total_pages
    );

    let mut hdr_page = [0u8; BLOCK_SIZE];
    read_exact_at(&nvme, &mut hdr_page, STORAGE_BASE_LBA * SECTOR_SIZE)?;
    let magic = rd_u64(&hdr_page, 0);
    if magic != STORAGE_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Source NVMe does not contain a valid SHARDSH4 header at LBA {} (found 0x{:016X})",
                STORAGE_BASE_LBA, magic
            ),
        ));
    }

    let shard_size = rd_u64(&hdr_page, 8) as usize;
    let shard_count = rd_u32(&hdr_page, 16) as usize;
    let wal_epoch = rd_u64(&hdr_page, 24);
    let wal_base = rd_u64(&hdr_page, 32) as usize;
    let wal_pages = rd_u64(&hdr_page, 40) as usize;

    if shard_count != SHARD_COUNT || shard_size == 0 || wal_base != 1 + SHARD_COUNT * (shard_size + 1) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Invalid NVMe shard geometry: shard_count={} shard_size={} wal_base={}",
                shard_count, shard_size, wal_base
            ),
        ));
    }

    let wal_overlay = collect_wal_overlay(&nvme, wal_base, wal_pages / 2, wal_epoch);
    println!(
        "      Storage header OK: shard_size={} pages ({:.2} GiB/shard), wal_epoch={}, WAL overlay pages={}",
        shard_size,
        (shard_size as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
        wal_epoch,
        wal_overlay.len()
    );

    // Inspect all 16 shard metadata pages
    let mut shard_entries = [ShardSnapEntry::default(); SHARD_COUNT];
    let mut total_allocated_pages = 0u64;

    for i in 0..SHARD_COUNT {
        let base_page = 1 + i * (shard_size + 1);
        let mut meta_page = [0u8; BLOCK_SIZE];
        read_storage_block_with_overlay(&nvme, base_page, &wal_overlay, &mut meta_page)?;
        let btree_magic = rd_u32(&meta_page, 0);
        if btree_magic != BTREE_META_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Shard {:2} at block {} has invalid B-Tree magic 0x{:08X} (expected 0x{:08X})",
                    i, base_page, btree_magic, BTREE_META_MAGIC
                ),
            ));
        }
        let root_page_id = rd_u32(&meta_page, 4);
        let next_free_page = rd_u32(&meta_page, 8);
        let first_free_page = rd_u32(&meta_page, 12);
        if next_free_page == 0 || (next_free_page as usize) > shard_size + 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Shard {:2} has out-of-range next_free_page={}", i, next_free_page),
            ));
        }
        shard_entries[i] = ShardSnapEntry {
            file_offset: 0,
            next_free_page,
            root_page_id,
            first_free_page,
            checksum: 0,
        };
        total_allocated_pages += next_free_page as u64;
    }

    // Read cached counts from Shard 4
    let shard4_base = 1 + 4 * (shard_size + 1);
    let (cached_accounts, cached_blocks, cached_confirmed, cached_pruned) =
        read_cached_counts_from_shard4(shard_entries[4].root_page_id, |pg, buf| {
            read_storage_block_with_overlay(&nvme, shard4_base + pg as usize, &wal_overlay, buf)
        });

    println!(
        "      Ledger Counts : accounts={} | blocks={} | confirmed={} | pruned={}",
        cached_accounts, cached_blocks, cached_confirmed, cached_pruned
    );
    println!(
        "      Active Pages  : {} pages across 16 shards ({:.2} GiB / {:.1}% of raw disk)",
        total_allocated_pages,
        (total_allocated_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
        (total_allocated_pages as f64 * 100.0) / (total_pages as f64)
    );

    // Check 64 MB BlockBloom filter
    let mut bloom_hdr = [0u8; BLOCK_SIZE];
    read_exact_at(&nvme, &mut bloom_hdr, BLOOM_HEADER_LBA * SECTOR_SIZE)?;
    let has_bloom = rd_u64(&bloom_hdr, 0) == BLOOM_MAGIC;
    println!(
        "      BlockBloom    : {}",
        if has_bloom { "VALID (64 MB at LBA 524288)" } else { "NOT PRESENT" }
    );

    if dry_run {
        println!("[DRY RUN] Source NVMe validation complete. No snapshot file written.");
        return Ok(());
    }

    let out_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(out_path)?;

    let mut cur_file_offset = BLOCK_SIZE as u64; // Reserve first 4 KB for SnapshotHeader
    let mut flags = 0u32;

    // [2/4] Write 64 MB BlockBloom filter (4 KB header + 64 MB data)
    let mut bloom_offset = 0u64;
    let mut bloom_len = 0u64;
    let mut bloom_checksum = 0u64;
    if has_bloom {
        print!("[2/4] Streaming 64 MB BlockBloom filter... ");
        io::stdout().flush().ok();
        flags |= FLAG_HAS_BLOOM;
        bloom_offset = cur_file_offset;
        bloom_len = (BLOCK_SIZE + BLOOM_BYTES) as u64;

        let mut h = hash_bytes(&bloom_hdr, CHECKSUM_SEED);
        write_all_at(&out_file, &bloom_hdr, cur_file_offset)?;
        cur_file_offset += BLOCK_SIZE as u64;

        let mut chunk = vec![0u8; IO_CHUNK_BYTES];
        let mut remaining = BLOOM_BYTES;
        let mut src_off = BLOOM_DATA_LBA * SECTOR_SIZE;
        while remaining > 0 {
            let n = remaining.min(IO_CHUNK_BYTES);
            read_exact_at(&nvme, &mut chunk[..n], src_off)?;
            h = hash_bytes(&chunk[..n], h);
            write_all_at(&out_file, &chunk[..n], cur_file_offset)?;
            src_off += n as u64;
            cur_file_offset += n as u64;
            remaining -= n;
        }
        bloom_checksum = h;
        println!("OK (checksum=0x{:016X})", bloom_checksum);
    } else {
        println!("[2/4] Skipping BlockBloom filter (not present on source NVMe).");
    }

    // Optional: include 16.25 MB hardware-bound wallet partition
    let mut wallet_offset = 0u64;
    let mut wallet_len = 0u64;
    let mut wallet_checksum = 0u64;
    if include_wallets {
        print!("[2b/4] Streaming 16.25 MB Hardware Wallet partition... ");
        io::stdout().flush().ok();
        flags |= FLAG_HAS_WALLETS;
        wallet_offset = cur_file_offset;
        wallet_len = (WALLET_TOTAL_PAGES * BLOCK_SIZE) as u64;

        let mut raw_wallet = vec![0u8; WALLET_TOTAL_PAGES * BLOCK_SIZE];
        read_exact_at(&nvme, &mut raw_wallet, WALLET_STORAGE_LBA * SECTOR_SIZE)?;
        wallet_checksum = hash_bytes(&raw_wallet, CHECKSUM_SEED);
        write_all_at(&out_file, &raw_wallet, cur_file_offset)?;
        cur_file_offset += wallet_len;
        println!("OK (checksum=0x{:016X})", wallet_checksum);
    }

    // [3/4] Stream all 16 shards' allocated pages (`0 .. next_free_page`)
    println!(
        "[3/4] Streaming 16 B-Tree shards ({:.2} GiB total)...",
        (total_allocated_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0)
    );
    let mut io_buf = vec![0u8; IO_CHUNK_BYTES];
    let chunk_pages = IO_CHUNK_BYTES / BLOCK_SIZE;
    let mut pages_done = 0u64;

    for shard_idx in 0..SHARD_COUNT {
        let shard_t0 = Instant::now();
        let base_page = 1 + shard_idx * (shard_size + 1);
        let n_pages = shard_entries[shard_idx].next_free_page as usize;
        shard_entries[shard_idx].file_offset = cur_file_offset;

        let mut shard_hash = CHECKSUM_SEED;
        let mut pg = 0usize;
        while pg < n_pages {
            let batch_pages = (n_pages - pg).min(chunk_pages);
            let batch_bytes = batch_pages * BLOCK_SIZE;
            let src_off = (STORAGE_BASE_LBA + ((base_page + pg) as u64) * 8) * SECTOR_SIZE;
            read_exact_at(&nvme, &mut io_buf[..batch_bytes], src_off)?;

            // Overlay any dirty pages from the WAL batch
            if !wal_overlay.is_empty() {
                for k in 0..batch_pages {
                    let abs_blk = base_page + pg + k;
                    if let Some(over_page) = wal_overlay.get(&abs_blk) {
                        io_buf[k * BLOCK_SIZE..(k + 1) * BLOCK_SIZE].copy_from_slice(over_page);
                    }
                }
            }

            shard_hash = hash_bytes(&io_buf[..batch_bytes], shard_hash);
            write_all_at(&out_file, &io_buf[..batch_bytes], cur_file_offset)?;

            cur_file_offset += batch_bytes as u64;
            pg += batch_pages;
            pages_done += batch_pages as u64;
        }

        shard_entries[shard_idx].checksum = shard_hash;
        let pct = (pages_done as f64 * 100.0) / (total_allocated_pages.max(1) as f64);
        println!(
            "      Shard {:2}/16: {:8} pages ({:6.2} GiB) | root={:7} | csum=0x{:016X} | {:5.2}s [{:5.1}%]",
            shard_idx + 1,
            n_pages,
            (n_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
            shard_entries[shard_idx].root_page_id,
            shard_hash,
            shard_t0.elapsed().as_secs_f64(),
            pct
        );
    }

    // [4/4] Write 4 KB SnapshotHeader at offset 0 and fsync
    print!("[4/4] Writing BNSNAPV1 header and syncing snapshot file to disk... ");
    io::stdout().flush().ok();
    let created_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let snap_hdr = SnapshotHeader {
        version: SNAPSHOT_VERSION,
        flags,
        created_unix_secs,
        source_shard_size_pages: shard_size as u64,
        cached_accounts,
        cached_blocks,
        cached_confirmed,
        cached_pruned,
        bloom_offset,
        bloom_len,
        bloom_checksum,
        wallet_offset,
        wallet_len,
        wallet_checksum,
        shards: shard_entries,
    };
    let hdr_bytes = snap_hdr.to_page();
    write_all_at(&out_file, &hdr_bytes, 0)?;
    out_file.sync_all()?;
    println!("OK!");

    let elapsed = t0.elapsed().as_secs_f64();
    let total_gib = cur_file_offset as f64 / (1024.0 * 1024.0 * 1024.0);
    println!("==================================================================");
    println!(
        " Snapshot Complete in {:.2}s! Size: {:.2} GiB ({:.1} MiB/s)",
        elapsed,
        total_gib,
        (cur_file_offset as f64 / (1024.0 * 1024.0)) / elapsed.max(0.001)
    );
    println!("==================================================================");
    Ok(())
}

// ── Command 2: Restore Raw Snapshot File -> NVMe ─────────────────────────────

fn run_restore(
    snap_path: &Path,
    nvme_path: &Path,
    include_wallets: bool,
    dry_run: bool,
) -> io::Result<()> {
    let t0 = Instant::now();
    println!("==================================================================");
    println!(" Barely-Nano Raw Ledger Snapshot -> NVMe Tool (RESTORE)");
    println!("==================================================================");
    println!("  Snapshot In  : {}", snap_path.display());
    println!("  Target NVMe  : {}", nvme_path.display());
    println!("  Wallets      : {}", if include_wallets { "RESTORE (if present in snapshot)" } else { "PRESERVE target NVMe wallets" });
    println!("  Mode         : {}", if dry_run { "DRY RUN (verify checksums only)" } else { "LIVE RESTORE" });
    println!("------------------------------------------------------------------");

    let snap_file = File::open(snap_path)?;
    let snap_len = snap_file.metadata()?.len();
    let mut hdr_buf = [0u8; BLOCK_SIZE];
    read_exact_at(&snap_file, &mut hdr_buf, 0)?;
    let hdr = SnapshotHeader::from_page(&hdr_buf)?;

    let mut total_snap_pages = 0u64;
    for s in &hdr.shards {
        total_snap_pages += s.next_free_page as u64;
    }

    println!(
        "[1/4] Loaded BNSNAPV1 header (file size: {:.2} GiB):",
        snap_len as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!(
        "      Ledger Counts : accounts={} | blocks={} | confirmed={} | pruned={}",
        hdr.cached_accounts, hdr.cached_blocks, hdr.cached_confirmed, hdr.cached_pruned
    );
    println!(
        "      B-Tree Pages  : {} pages across 16 shards ({:.2} GiB)",
        total_snap_pages,
        (total_snap_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0)
    );

    let nvme_file = if dry_run {
        None
    } else {
        verify_target_not_mounted(nvme_path)?;
        Some(OpenOptions::new().read(true).write(true).open(nvme_path)?)
    };

    let (target_shard_size, wal_base, wal_pages) = if let Some(ref nvme) = nvme_file {
        let dev_bytes = query_device_or_file_bytes(nvme)?;
        let total_sectors = dev_bytes / SECTOR_SIZE;
        if total_sectors <= STORAGE_BASE_LBA + 65536 * 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Target NVMe is too small ({} bytes)", dev_bytes),
            ));
        }
        let usable_sectors = total_sectors - STORAGE_BASE_LBA;
        let total_pages = (usable_sectors / 8) as usize;
        let wp = std::cmp::min(WAL_PAGES_DEFAULT, (total_pages / 8) & !1);
        let ss = (total_pages - 1 - wp) / SHARD_COUNT - 1;
        let wb = 1 + SHARD_COUNT * (ss + 1);
        for (i, s) in hdr.shards.iter().enumerate() {
            if (s.next_free_page as usize) > ss + 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Shard {} requires {} pages, but target NVMe shard capacity is only {} pages",
                        i, s.next_free_page, ss
                    ),
                ));
            }
        }
        println!(
            "      Target NVMe   : {:.2} GiB | shard_size={} pages ({:.2} GiB/shard) | wal_base={}",
            dev_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            ss,
            (ss as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
            wb
        );
        (ss, wb, wp)
    } else {
        let ss = hdr.source_shard_size_pages as usize;
        (ss, 1 + SHARD_COUNT * (ss + 1), WAL_PAGES_DEFAULT)
    };

    // [2/4] Verify & restore 64 MB BlockBloom filter
    if (hdr.flags & FLAG_HAS_BLOOM) != 0 && hdr.bloom_len == (BLOCK_SIZE + BLOOM_BYTES) as u64 {
        print!("[2/4] Verifying & restoring 64 MB BlockBloom filter... ");
        io::stdout().flush().ok();
        let mut bloom_hdr = [0u8; BLOCK_SIZE];
        read_exact_at(&snap_file, &mut bloom_hdr, hdr.bloom_offset)?;
        let mut h = hash_bytes(&bloom_hdr, CHECKSUM_SEED);

        let mut chunk = vec![0u8; IO_CHUNK_BYTES];
        let mut remaining = BLOOM_BYTES;
        let mut src_off = hdr.bloom_offset + BLOCK_SIZE as u64;
        let mut dst_off = BLOOM_DATA_LBA * SECTOR_SIZE;
        while remaining > 0 {
            let n = remaining.min(IO_CHUNK_BYTES);
            read_exact_at(&snap_file, &mut chunk[..n], src_off)?;
            h = hash_bytes(&chunk[..n], h);
            if let Some(ref nvme) = nvme_file {
                write_all_at(nvme, &chunk[..n], dst_off)?;
            }
            src_off += n as u64;
            dst_off += n as u64;
            remaining -= n;
        }
        if h != hdr.bloom_checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "BlockBloom checksum mismatch: expected 0x{:016X}, computed 0x{:016X}",
                    hdr.bloom_checksum, h
                ),
            ));
        }
        if let Some(ref nvme) = nvme_file {
            write_all_at(nvme, &bloom_hdr, BLOOM_HEADER_LBA * SECTOR_SIZE)?;
        }
        println!("OK (checksum=0x{:016X})", h);
    } else {
        println!("[2/4] No BlockBloom filter in snapshot (or skipped).");
    }

    // Optional: restore hardware-bound wallet partition if requested and present
    if include_wallets && (hdr.flags & FLAG_HAS_WALLETS) != 0 && hdr.wallet_len > 0 {
        print!("[2b/4] Verifying & restoring Hardware Wallet partition... ");
        io::stdout().flush().ok();
        let mut raw_wallet = vec![0u8; hdr.wallet_len as usize];
        read_exact_at(&snap_file, &mut raw_wallet, hdr.wallet_offset)?;
        let h = hash_bytes(&raw_wallet, CHECKSUM_SEED);
        if h != hdr.wallet_checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Wallet partition checksum mismatch: expected 0x{:016X}, computed 0x{:016X}",
                    hdr.wallet_checksum, h
                ),
            ));
        }
        if let Some(ref nvme) = nvme_file {
            write_all_at(nvme, &raw_wallet, WALLET_STORAGE_LBA * SECTOR_SIZE)?;
        }
        println!("OK (checksum=0x{:016X})", h);
    }

    // [3/4] Verify & restore all 16 B-Tree shards
    println!(
        "[3/4] Restoring 16 B-Tree shards ({:.2} GiB total)...",
        (total_snap_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0)
    );
    let mut io_buf = vec![0u8; IO_CHUNK_BYTES];
    let chunk_pages = IO_CHUNK_BYTES / BLOCK_SIZE;
    let mut pages_done = 0u64;

    for shard_idx in 0..SHARD_COUNT {
        let shard_t0 = Instant::now();
        let s = &hdr.shards[shard_idx];
        let n_pages = s.next_free_page as usize;
        let base_page = 1 + shard_idx * (target_shard_size + 1);

        let mut shard_hash = CHECKSUM_SEED;
        let mut pg = 0usize;
        let mut src_off = s.file_offset;
        let mut dst_off = (STORAGE_BASE_LBA + (base_page as u64) * 8) * SECTOR_SIZE;

        while pg < n_pages {
            let batch_pages = (n_pages - pg).min(chunk_pages);
            let batch_bytes = batch_pages * BLOCK_SIZE;
            read_exact_at(&snap_file, &mut io_buf[..batch_bytes], src_off)?;
            shard_hash = hash_bytes(&io_buf[..batch_bytes], shard_hash);

            if let Some(ref nvme) = nvme_file {
                write_all_at(nvme, &io_buf[..batch_bytes], dst_off)?;
            }

            src_off += batch_bytes as u64;
            dst_off += batch_bytes as u64;
            pg += batch_pages;
            pages_done += batch_pages as u64;
        }

        if shard_hash != s.checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Shard {} checksum mismatch: expected 0x{:016X}, computed 0x{:016X}",
                    shard_idx, s.checksum, shard_hash
                ),
            ));
        }

        let pct = (pages_done as f64 * 100.0) / (total_snap_pages.max(1) as f64);
        println!(
            "      Shard {:2}/16: {:8} pages ({:6.2} GiB) | root={:7} | csum=0x{:016X} OK | {:5.2}s [{:5.1}%]",
            shard_idx + 1,
            n_pages,
            (n_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
            s.root_page_id,
            shard_hash,
            shard_t0.elapsed().as_secs_f64(),
            pct
        );
    }

    if let Some(ref nvme) = nvme_file {
        print!("[4/4] Zeroing WAL areas, writing SHARDSH4 header, and syncing NVMe... ");
        io::stdout().flush().ok();

        let zero_page = [0u8; BLOCK_SIZE];
        let wal_area0_off = (STORAGE_BASE_LBA + (wal_base as u64) * 8) * SECTOR_SIZE;
        let wal_area1_off =
            (STORAGE_BASE_LBA + ((wal_base + wal_pages / 2) as u64) * 8) * SECTOR_SIZE;
        write_all_at(nvme, &zero_page, wal_area0_off)?;
        write_all_at(nvme, &zero_page, wal_area1_off)?;

        let mut storage_hdr = [0u8; BLOCK_SIZE];
        let wal_epoch = 1u64;
        storage_hdr[0..8].copy_from_slice(&STORAGE_MAGIC.to_le_bytes());
        storage_hdr[8..16].copy_from_slice(&(target_shard_size as u64).to_le_bytes());
        storage_hdr[16..20].copy_from_slice(&(SHARD_COUNT as u32).to_le_bytes());
        storage_hdr[24..32].copy_from_slice(&wal_epoch.to_le_bytes());
        storage_hdr[32..40].copy_from_slice(&(wal_base as u64).to_le_bytes());
        storage_hdr[40..48].copy_from_slice(&(wal_pages as u64).to_le_bytes());
        write_all_at(nvme, &storage_hdr, STORAGE_BASE_LBA * SECTOR_SIZE)?;

        nvme.sync_all()?;
        println!("OK!");
    } else {
        println!("[4/4] [DRY RUN] All 16 shard checksums verified! Target NVMe was not modified.");
    }

    let elapsed = t0.elapsed().as_secs_f64();
    let total_gib = (total_snap_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0);
    println!("==================================================================");
    println!(
        " Restore Complete in {:.2}s! Verified {:.2} GiB ({:.1} MiB/s)",
        elapsed,
        total_gib,
        ((total_snap_pages as f64 * 4096.0) / (1024.0 * 1024.0)) / elapsed.max(0.001)
    );
    println!("==================================================================");
    Ok(())
}

// ── Command 3: Inspect Snapshot File or Live NVMe Device ────────────────────

fn run_info(path: &Path) -> io::Result<()> {
    let f = File::open(path)?;
    let size_bytes = query_device_or_file_bytes(&f)?;
    let mut page0 = [0u8; BLOCK_SIZE];
    read_exact_at(&f, &mut page0, 0)?;

    if rd_u64(&page0, 0) == SNAPSHOT_MAGIC {
        let hdr = SnapshotHeader::from_page(&page0)?;
        let mut total_pages = 0u64;
        for s in &hdr.shards {
            total_pages += s.next_free_page as u64;
        }
        println!("==================================================================");
        println!(" Barely-Nano Snapshot File Info ({})", path.display());
        println!("==================================================================");
        println!("  File Size       : {:.2} GiB ({} bytes)", size_bytes as f64 / (1024.0 * 1024.0 * 1024.0), size_bytes);
        println!("  Format Version  : BNSNAPV1 (v{})", hdr.version);
        println!("  Created (Unix)  : {}", hdr.created_unix_secs);
        println!("  Accounts        : {}", hdr.cached_accounts);
        println!("  Blocks          : {}", hdr.cached_blocks);
        println!("  Confirmed       : {}", hdr.cached_confirmed);
        println!("  Pruned          : {}", hdr.cached_pruned);
        println!("  BlockBloom      : {}", if (hdr.flags & FLAG_HAS_BLOOM) != 0 { "Present (64 MB)" } else { "Absent" });
        println!("  Wallets Region  : {}", if (hdr.flags & FLAG_HAS_WALLETS) != 0 { "Included (16.25 MB)" } else { "Not included" });
        println!("  Total B-Tree    : {} pages ({:.2} GiB)", total_pages, (total_pages as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0));
        println!("------------------------------------------------------------------");
        for (i, s) in hdr.shards.iter().enumerate() {
            println!(
                "  Shard {:2}: {:8} pages ({:6.2} GiB) | root={:7} | free_head={:7} | off=0x{:010X} | csum=0x{:016X}",
                i,
                s.next_free_page,
                (s.next_free_page as f64 * 4096.0) / (1024.0 * 1024.0 * 1024.0),
                s.root_page_id,
                s.first_free_page,
                s.file_offset,
                s.checksum
            );
        }
        println!("==================================================================");
        return Ok(());
    }

    // Otherwise, inspect as a raw NVMe device / image
    run_dump(path, Path::new("/dev/null"), false, true)
}

fn print_usage(prog: &str) {
    eprintln!("Usage:");
    eprintln!("  {prog} dump <nvme_device> <output_snapshot_file> [--include-wallets] [--dry-run]");
    eprintln!("  {prog} restore <input_snapshot_file> <nvme_device> [--include-wallets] [--dry-run]");
    eprintln!("  {prog} info <nvme_device_or_snapshot_file>");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  # 1. Create a raw ledger snapshot file from a synced NVMe drive on /dev/sda:");
    eprintln!("  ./tools/nvme_snapshot.sh dump /dev/sda database/nano_ledger.snap");
    eprintln!();
    eprintln!("  # 2. Inspect a snapshot file or NVMe drive:");
    eprintln!("  ./tools/nvme_snapshot.sh info database/nano_ledger.snap");
    eprintln!();
    eprintln!("  # 3. Restore a snapshot file back onto an NVMe drive on /dev/sda:");
    eprintln!("  ./tools/nvme_snapshot.sh restore database/nano_ledger.snap /dev/sda");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let mut include_wallets = false;
    let mut dry_run = false;
    let mut positional: Vec<&str> = Vec::new();

    for arg in &args[1..] {
        match arg.as_str() {
            "-h" | "--help" | "help" => {
                print_usage(&args[0]);
                return;
            }
            "--include-wallets" | "--wallets" => include_wallets = true,
            "--dry-run" | "--verify-only" | "-n" => dry_run = true,
            _ if arg.starts_with('-') && arg != "--dump" && arg != "--restore" && arg != "--info" => {
                eprintln!("Unknown option: {}", arg);
                print_usage(&args[0]);
                std::process::exit(1);
            }
            _ => positional.push(arg.as_str()),
        }
    }

    if positional.is_empty() {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let res = match positional[0] {
        "dump" | "backup" | "read" | "--dump" => {
            if positional.len() != 3 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            run_dump(
                Path::new(positional[1]),
                Path::new(positional[2]),
                include_wallets,
                dry_run,
            )
        }
        "restore" | "write" | "--restore" => {
            if positional.len() != 3 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            run_restore(
                Path::new(positional[1]),
                Path::new(positional[2]),
                include_wallets,
                dry_run,
            )
        }
        "info" | "inspect" | "--info" => {
            if positional.len() != 2 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            run_info(Path::new(positional[1]))
        }
        _ => {
            eprintln!("Unknown subcommand: {}", positional[0]);
            print_usage(&args[0]);
            std::process::exit(1);
        }
    };

    if let Err(e) = res {
        eprintln!("\n[ERROR] {}", e);
        std::process::exit(1);
    }
}
