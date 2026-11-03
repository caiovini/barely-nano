pub mod bpm;
pub mod btree;
pub mod wal;
pub mod wallet_crypto;

#[repr(align(64))]
#[derive(Clone, Copy)]
pub struct AlignedBlock {
    pub bytes: [u8; 4096],
}

impl AlignedBlock {
    pub const fn new() -> Self {
        Self { bytes: [0; 4096] }
    }
}

impl Default for AlignedBlock {
    fn default() -> Self {
        Self::new()
    }
}

use crate::uboot;

use spin::Mutex;
use alloc::sync::Arc;

pub const SHARD_COUNT: usize = 16;

/// Issues a RISC-V `Zicbop` `prefetch.r 0(ptr)` cache-line read prefetch hint.
#[inline(always)]
pub fn prefetch_read(ptr: *const u8) {
    #[cfg(target_arch = "riscv64")]
    {
        // SAFETY: `ori zero, rs1, 1` is the architectural RISC-V Zicbop `prefetch.r 0(rs1)`
        // hint instruction; it writes to `x0`, has no architectural side effects, and never traps.
        unsafe {
            core::arch::asm!(
                "ori zero, {0}, 1",
                in(reg) ptr,
                options(nostack, preserves_flags, readonly)
            );
        }
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = ptr;
    }
}

/// Per-hart shard-lock statistics (CLINT ticks): time waiting for, and holding, shard locks.
pub static LOCK_WAIT: [core::sync::atomic::AtomicU64; 8] = [const { core::sync::atomic::AtomicU64::new(0) }; 8];
pub static LOCK_HOLD: [core::sync::atomic::AtomicU64; 8] = [const { core::sync::atomic::AtomicU64::new(0) }; 8];
pub static LOCK_ACQ: [core::sync::atomic::AtomicU64; 8] = [const { core::sync::atomic::AtomicU64::new(0) }; 8];

#[inline]
fn current_hart() -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        let id: usize;
        // SAFETY: reads the thread-pointer register, which this kernel sets to the hart id on boot;
        // the instruction has no side effects.
        unsafe { core::arch::asm!("mv {0}, tp", out(reg) id) };
        id & 7
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        0
    }
}

/// Spin mutex around one shard that records per-hart wait and hold times.
pub struct ShardMutex {
    inner: Mutex<Option<btree::BTree>>,
}

pub struct ShardGuard<'a> {
    guard: spin::MutexGuard<'a, Option<btree::BTree>>,
    acquired: u64,
    hart: usize,
    site: &'static core::panic::Location<'static>,
}

/// Long lock holds (> 2 ms) aggregated per (hart, call site): (hart, location, total ticks, count).
pub static LONG_HOLDS: Mutex<[Option<(usize, &'static core::panic::Location<'static>, u64, u32)>; 24]> =
    Mutex::new([None; 24]);

impl ShardMutex {
    pub const fn new() -> Self {
        Self { inner: Mutex::new(None) }
    }

    #[track_caller]
    pub fn lock(&self) -> ShardGuard<'_> {
        let site = core::panic::Location::caller();
        let t0 = crate::clint::mtime();
        let guard = self.inner.lock();
        let t1 = crate::clint::mtime();
        let hart = current_hart();
        LOCK_WAIT[hart].fetch_add(t1 - t0, core::sync::atomic::Ordering::Relaxed);
        ShardGuard { guard, acquired: t1, hart, site }
    }

    #[track_caller]
    pub fn try_lock(&self) -> Option<ShardGuard<'_>> {
        let guard = self.inner.try_lock()?;
        Some(ShardGuard { guard, acquired: crate::clint::mtime(), hart: current_hart(), site: core::panic::Location::caller() })
    }

    /// # Safety
    /// Only valid when no guard is alive (used by the boot-time lock reset).
    pub unsafe fn force_unlock(&self) {
        self.inner.force_unlock();
    }
}

impl core::ops::Deref for ShardGuard<'_> {
    type Target = Option<btree::BTree>;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl core::ops::DerefMut for ShardGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for ShardGuard<'_> {
    fn drop(&mut self) {
        let held = crate::clint::mtime() - self.acquired;
        LOCK_HOLD[self.hart].fetch_add(held, core::sync::atomic::Ordering::Relaxed);
        LOCK_ACQ[self.hart].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if held > crate::clint::FREQ / 1000 * 2 {
            if let Some(mut t) = LONG_HOLDS.try_lock() {
                let mut placed = false;
                for slot in t.iter_mut() {
                    match slot {
                        Some((h, loc, total, n)) if *h == self.hart && core::ptr::eq(*loc, self.site) => {
                            *total += held;
                            *n += 1;
                            placed = true;
                            break;
                        }
                        None => {
                            *slot = Some((self.hart, self.site, held, 1));
                            placed = true;
                            break;
                        }
                        _ => {}
                    }
                }
                let _ = placed;
            }
        }
    }
}

pub static SHARDS: [ShardMutex; SHARD_COUNT] = [const { ShardMutex::new() }; SHARD_COUNT];
/// Dedicated B-Tree instance for persistent wallet keys and metadata, physically and logically
/// isolated from the 16 ledger `SHARDS` and the ledger WAL.
pub static WALLET_SHARD: ShardMutex = ShardMutex::new();

pub fn reset_prof() {
    use core::sync::atomic::Ordering::Relaxed;
    for h in 0..8 {
        LOCK_WAIT[h].store(0, Relaxed);
        LOCK_HOLD[h].store(0, Relaxed);
        LOCK_ACQ[h].store(0, Relaxed);
    }
    if let Some(mut t) = LONG_HOLDS.try_lock() {
        *t = [None; 24];
    }
    for counter in bpm::STORAGE_PROF.iter() {
        counter.store(0, Relaxed);
    }
}

pub static SHARD_SIZE_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Helper for legacy code or single-shard access
pub fn db_shard(index: usize) -> &'static ShardMutex {
    &SHARDS[index % SHARD_COUNT]
}

pub fn get_shard_index(key: &[u8]) -> usize {
    if key.len() < 2 { return 0; }
    // We use the high 4 bits of the first byte of the actual key (after Table ID) to shard.
    // This provides Prefix Sharding, which allows range scans to work by visiting shards in order.
    (key[1] >> 4) as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    IoError,
}

pub static GLOBAL_STORAGE: Mutex<Option<Arc<Mutex<StorageContext>>>> = Mutex::new(None);

pub enum StorageBackend {
    UBoot(uboot::DeviceInfo),
    RamDisk(alloc::vec::Vec<u8>),
}

pub const HIGH_DRAM_THRESHOLD: usize = 0x8000_0000;

pub struct StorageContext {
    pub backend: StorageBackend,
    /// 64-byte-aligned 4 KB bounce buffer in Bank 0 (< 0x8000_0000) used when transferring frames
    /// located in High DRAM Bank 1 (>= 0x1_0000_0000) so U-Boot DMA always uses 32-bit addresses.
    dma_bounce: alloc::boxed::Box<AlignedBlock>,
    /// Isolated backing buffer for wallet pages when running on `StorageBackend::RamDisk`
    /// (empty `Vec` with 0 heap allocation on `StorageBackend::UBoot`).
    wallet_ram: alloc::vec::Vec<u8>,
}

pub const STORAGE_BASE_LBA: u64 = 1_048_576; // 512 MB offset to prevent overwriting U-Boot boot partition/SPL
pub const BLOOM_HEADER_LBA: u64 = 524_288;   // 256 MB offset (8 sectors = 4 KB bloom header)
pub const BLOOM_DATA_LBA: u64 = 524_296;     // 256 MB + 4 KB offset (64 MB = 131,072 sectors)

/// Dedicated NVMe LBA region for persistent Wallet B-Tree storage (384 MB offset).
/// Strictly below `STORAGE_BASE_LBA` (512 MB, where the synced ledger + WAL reside) and
/// strictly above the Bloom filter region (`256 MB .. 321 MB + 4 KB`), guaranteeing that
/// wallet operations can NEVER overwrite or collide with a synced ledger.
pub const WALLET_STORAGE_LBA: u64 = 786_432; // 384 MB offset
/// Maximum 4 KB home pages in the dedicated wallet B-Tree region (4,096 pages = 16 MB).
pub const WALLET_MAX_PAGES: usize = 4096;
/// Redo-journal pages immediately following the wallet B-Tree pages (64 pages = 256 KB).
pub const WALLET_JOURNAL_PAGES: usize = 64;
const WALLET_JOURNAL_MAGIC: u64 = 0x5741_4C4C_4A52_4E31; // "WALLJRN1"
const WALLET_JOURNAL_SEED: u64 = 0x5741_4C4C_243F_6A88;

const _: () = assert!(WALLET_STORAGE_LBA >= 657_416);
const _: () = assert!(
    WALLET_STORAGE_LBA + ((WALLET_MAX_PAGES + WALLET_JOURNAL_PAGES) as u64) * 8 <= STORAGE_BASE_LBA
);

fn wallet_journal_checksum(pids: &[u32], pages: &[&[u8; 4096]]) -> u64 {
    let mut h = WALLET_JOURNAL_SEED ^ (pids.len() as u64);
    for &pid in pids {
        h = (h ^ (pid as u64)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 32;
    }
    for page in pages {
        for w in page.chunks_exact(8) {
            let v = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
            h = (h ^ v).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            h ^= h >> 32;
        }
    }
    h
}

impl StorageContext {
    pub fn new_uboot(info: uboot::DeviceInfo) -> Self {
        Self {
            backend: StorageBackend::UBoot(info),
            dma_bounce: alloc::boxed::Box::new(AlignedBlock::new()),
            wallet_ram: alloc::vec::Vec::new(),
        }
    }

    pub fn new_ramdisk(size_mb: usize) -> Self {
        let size = size_mb * 1024 * 1024;
        Self {
            backend: StorageBackend::RamDisk(alloc::vec![0; size]),
            dma_bounce: alloc::boxed::Box::new(AlignedBlock::new()),
            wallet_ram: alloc::vec::Vec::new(),
        }
    }

    /// Reads a 4 KB page from the isolated wallet storage region (`WALLET_STORAGE_LBA`).
    /// Enforces strict bounds so it can never address `< WALLET_STORAGE_LBA` or `>= STORAGE_BASE_LBA`.
    pub fn read_wallet_block(&mut self, wallet_page_id: usize, buf: &mut [u8]) -> Result<(), StorageError> {
        if wallet_page_id >= WALLET_MAX_PAGES + WALLET_JOURNAL_PAGES || buf.len() < 4096 {
            return Err(StorageError::IoError);
        }
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                let lba = WALLET_STORAGE_LBA + (wallet_page_id as u64) * 8;
                if lba < WALLET_STORAGE_LBA || lba + 8 > STORAGE_BASE_LBA {
                    return Err(StorageError::IoError);
                }
                let count = 8;
                if (buf.as_ptr() as usize) >= HIGH_DRAM_THRESHOLD {
                    let ret = uboot::dev_read(info, lba, count, self.dma_bounce.bytes.as_mut_ptr());
                    if ret > 0 {
                        buf[..4096].copy_from_slice(&self.dma_bounce.bytes);
                        Ok(())
                    } else {
                        Err(StorageError::IoError)
                    }
                } else {
                    let ret = uboot::dev_read(info, lba, count, buf.as_mut_ptr());
                    if ret > 0 {
                        Ok(())
                    } else {
                        Err(StorageError::IoError)
                    }
                }
            }
            StorageBackend::RamDisk(_) => {
                let offset = wallet_page_id * 4096;
                if offset + 4096 <= self.wallet_ram.len() {
                    buf[..4096].copy_from_slice(&self.wallet_ram[offset..offset + 4096]);
                } else {
                    buf[..4096].fill(0);
                }
                Ok(())
            }
        }
    }

    /// Writes a 4 KB page to the isolated wallet storage region (`WALLET_STORAGE_LBA`).
    /// Enforces strict bounds so it can never overwrite the synced ledger (`>= STORAGE_BASE_LBA`)
    /// or boot/Bloom partitions (`< WALLET_STORAGE_LBA`).
    pub fn write_wallet_block(&mut self, wallet_page_id: usize, buf: &[u8]) -> Result<(), StorageError> {
        if wallet_page_id >= WALLET_MAX_PAGES + WALLET_JOURNAL_PAGES || buf.len() < 4096 {
            return Err(StorageError::IoError);
        }
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                let lba = WALLET_STORAGE_LBA + (wallet_page_id as u64) * 8;
                if lba < WALLET_STORAGE_LBA || lba + 8 > STORAGE_BASE_LBA {
                    return Err(StorageError::IoError);
                }
                let count = 8;
                let ret = if (buf.as_ptr() as usize) >= HIGH_DRAM_THRESHOLD {
                    self.dma_bounce.bytes.copy_from_slice(&buf[..4096]);
                    uboot::dev_write(info, lba, count, self.dma_bounce.bytes.as_ptr())
                } else {
                    uboot::dev_write(info, lba, count, buf.as_ptr())
                };
                if ret > 0 {
                    Ok(())
                } else {
                    tracing::error!("Storage: FAILED to write wallet block {}", wallet_page_id);
                    Err(StorageError::IoError)
                }
            }
            StorageBackend::RamDisk(_) => {
                let end = (wallet_page_id + 1) * 4096;
                if self.wallet_ram.len() < end {
                    self.wallet_ram.resize(end, 0);
                }
                let offset = wallet_page_id * 4096;
                self.wallet_ram[offset..end].copy_from_slice(&buf[..4096]);
                Ok(())
            }
        }
    }

    /// Atomically persists dirty wallet B-Tree pages using the dedicated wallet redo journal
    /// (`WALLET_MAX_PAGES .. WALLET_MAX_PAGES + WALLET_JOURNAL_PAGES`), then writes home pages
    /// with `pid > 0` first and `pid == 0` (`MetaData`) last before clearing the journal header.
    pub fn flush_wallet_dirty_pages(
        &mut self,
        pages: &mut [(usize, &[u8; 4096])],
    ) -> Result<(), StorageError> {
        if pages.is_empty() {
            return Ok(());
        }
        // Order `pid > 0` (leaf/internal nodes) before `pid == 0` (B-Tree MetaData).
        pages.sort_unstable_by_key(|&(pid, _)| if pid == 0 { usize::MAX } else { pid });

        let max_batch = WALLET_JOURNAL_PAGES - 1;
        for chunk in pages.chunks(max_batch) {
            let mut pids = [0u32; WALLET_JOURNAL_PAGES];
            let mut slice_refs: alloc::vec::Vec<&[u8; 4096]> = alloc::vec::Vec::with_capacity(chunk.len());
            for (idx, &(pid, data)) in chunk.iter().enumerate() {
                if pid >= WALLET_MAX_PAGES {
                    return Err(StorageError::IoError);
                }
                pids[idx] = pid as u32;
                slice_refs.push(data);
                self.write_wallet_block(WALLET_MAX_PAGES + 1 + idx, data)?;
            }
            let sum = wallet_journal_checksum(&pids[..chunk.len()], &slice_refs);
            let mut hdr = AlignedBlock::new();
            hdr.bytes[0..8].copy_from_slice(&WALLET_JOURNAL_MAGIC.to_le_bytes());
            hdr.bytes[8..12].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
            hdr.bytes[16..24].copy_from_slice(&sum.to_le_bytes());
            for (idx, &pid) in pids[..chunk.len()].iter().enumerate() {
                let off = 32 + idx * 4;
                hdr.bytes[off..off + 4].copy_from_slice(&pid.to_le_bytes());
            }
            self.write_wallet_block(WALLET_MAX_PAGES, &hdr.bytes)?;

            for &(pid, data) in chunk {
                self.write_wallet_block(pid, data)?;
            }

            hdr.bytes.fill(0);
            self.write_wallet_block(WALLET_MAX_PAGES, &hdr.bytes)?;
        }
        Ok(())
    }

    /// Recovers any interrupted wallet flush from the dedicated wallet redo journal on boot.
    pub fn recover_wallet_journal(&mut self) -> usize {
        let mut hdr = AlignedBlock::new();
        if self.read_wallet_block(WALLET_MAX_PAGES, &mut hdr.bytes).is_err() {
            return 0;
        }
        let magic = u64::from_le_bytes(hdr.bytes[0..8].try_into().unwrap());
        if magic != WALLET_JOURNAL_MAGIC {
            return 0;
        }
        let count = u32::from_le_bytes(hdr.bytes[8..12].try_into().unwrap()) as usize;
        if count == 0 || count >= WALLET_JOURNAL_PAGES {
            let zero = AlignedBlock::new();
            let _ = self.write_wallet_block(WALLET_MAX_PAGES, &zero.bytes);
            return 0;
        }
        let stored_sum = u64::from_le_bytes(hdr.bytes[16..24].try_into().unwrap());
        let mut pids = alloc::vec![0u32; count];
        for i in 0..count {
            let off = 32 + i * 4;
            let pid = u32::from_le_bytes(hdr.bytes[off..off + 4].try_into().unwrap());
            if (pid as usize) >= WALLET_MAX_PAGES {
                let zero = AlignedBlock::new();
                let _ = self.write_wallet_block(WALLET_MAX_PAGES, &zero.bytes);
                return 0;
            }
            pids[i] = pid;
        }
        let mut blocks = alloc::vec![AlignedBlock::new(); count];
        for i in 0..count {
            if self.read_wallet_block(WALLET_MAX_PAGES + 1 + i, &mut blocks[i].bytes).is_err() {
                return 0;
            }
        }
        let refs: alloc::vec::Vec<&[u8; 4096]> = blocks.iter().map(|b| &b.bytes).collect();
        if wallet_journal_checksum(&pids, &refs) != stored_sum {
            let zero = AlignedBlock::new();
            let _ = self.write_wallet_block(WALLET_MAX_PAGES, &zero.bytes);
            return 0;
        }
        let mut replayed = 0usize;
        for i in 0..count {
            if self.write_wallet_block(pids[i] as usize, &blocks[i].bytes).is_ok() {
                replayed += 1;
            }
        }
        let zero = AlignedBlock::new();
        let _ = self.write_wallet_block(WALLET_MAX_PAGES, &zero.bytes);
        replayed
    }

    pub fn read_block(&mut self, block_id: usize, buf: &mut [u8]) -> Result<(), StorageError> {
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                let lba = (block_id * 8) as u64 + STORAGE_BASE_LBA;
                let count = 8;
                if (buf.as_ptr() as usize) >= HIGH_DRAM_THRESHOLD {
                    let ret = uboot::dev_read(info, lba, count, self.dma_bounce.bytes.as_mut_ptr());
                    if ret > 0 {
                        buf[..4096].copy_from_slice(&self.dma_bounce.bytes);
                        Ok(())
                    } else {
                        Err(StorageError::IoError)
                    }
                } else {
                    let ret = uboot::dev_read(info, lba, count, buf.as_mut_ptr());
                    if ret > 0 {
                        Ok(())
                    } else {
                        Err(StorageError::IoError)
                    }
                }
            }
            StorageBackend::RamDisk(vec) => {
                let offset = block_id * 4096;
                if offset + 4096 <= vec.len() {
                    buf.copy_from_slice(&vec[offset..offset + 4096]);
                    Ok(())
                } else {
                    Err(StorageError::IoError)
                }
            }
        }
    }

    pub fn write_block(&mut self, block_id: usize, buf: &[u8]) -> Result<(), StorageError> {
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                let lba = (block_id * 8) as u64 + STORAGE_BASE_LBA;
                let count = 8;
                let ret = if (buf.as_ptr() as usize) >= HIGH_DRAM_THRESHOLD {
                    self.dma_bounce.bytes.copy_from_slice(&buf[..4096]);
                    uboot::dev_write(info, lba, count, self.dma_bounce.bytes.as_ptr())
                } else {
                    uboot::dev_write(info, lba, count, buf.as_ptr())
                };
                if ret > 0 {
                    Ok(())
                } else {
                    tracing::error!("Storage: FAILED to write block {}", block_id);
                    Err(StorageError::IoError)
                }
            }
            StorageBackend::RamDisk(vec) => {
                let offset = block_id * 4096;
                if offset + 4096 <= vec.len() {
                    vec[offset..offset + 4096].copy_from_slice(buf);
                    Ok(())
                } else {
                    tracing::error!("Storage: FAILED to write RAM block {}", block_id);
                    Err(StorageError::IoError)
                }
            }
        }
    }

    /// Pages transferred per device command by the multi-page helpers (128 KB).
    const IO_CHUNK_PAGES: usize = 32;

    /// Reads `buf.len() / 4096` consecutive pages starting at `block_id`.
    /// `buf` must be a multiple of 4096 bytes and 64-byte aligned (e.g. a slice of `AlignedBlock`s).
    pub fn read_blocks(&mut self, block_id: usize, buf: &mut [u8]) -> Result<(), StorageError> {
        debug_assert!(buf.len() % 4096 == 0 && (buf.as_ptr() as usize) % 64 == 0);
        for (i, chunk) in buf.chunks_mut(Self::IO_CHUNK_PAGES * 4096).enumerate() {
            let first = block_id + i * Self::IO_CHUNK_PAGES;
            match &mut self.backend {
                StorageBackend::UBoot(info) => {
                    let lba = (first * 8) as u64 + STORAGE_BASE_LBA;
                    if uboot::dev_read(info, lba, (chunk.len() / 512) as u64, chunk.as_mut_ptr()) <= 0 {
                        return Err(StorageError::IoError);
                    }
                }
                StorageBackend::RamDisk(vec) => {
                    let offset = first * 4096;
                    if offset + chunk.len() > vec.len() {
                        return Err(StorageError::IoError);
                    }
                    chunk.copy_from_slice(&vec[offset..offset + chunk.len()]);
                }
            }
        }
        Ok(())
    }

    /// Writes `buf.len() / 4096` consecutive pages starting at `block_id` (same alignment rules).
    pub fn write_blocks(&mut self, block_id: usize, buf: &[u8]) -> Result<(), StorageError> {
        debug_assert!(buf.len() % 4096 == 0 && (buf.as_ptr() as usize) % 64 == 0);
        for (i, chunk) in buf.chunks(Self::IO_CHUNK_PAGES * 4096).enumerate() {
            let first = block_id + i * Self::IO_CHUNK_PAGES;
            match &mut self.backend {
                StorageBackend::UBoot(info) => {
                    let lba = (first * 8) as u64 + STORAGE_BASE_LBA;
                    if uboot::dev_write(info, lba, (chunk.len() / 512) as u64, chunk.as_ptr()) <= 0 {
                        tracing::error!("Storage: FAILED to write {} pages at {}", chunk.len() / 4096, first);
                        return Err(StorageError::IoError);
                    }
                }
                StorageBackend::RamDisk(vec) => {
                    let offset = first * 4096;
                    if offset + chunk.len() > vec.len() {
                        return Err(StorageError::IoError);
                    }
                    vec[offset..offset + chunk.len()].copy_from_slice(chunk);
                }
            }
        }
        Ok(())
    }

    pub fn capacity(&self) -> u64 {
        match &self.backend {
            StorageBackend::UBoot(info) => {
                if info.block_count > STORAGE_BASE_LBA {
                    info.block_count - STORAGE_BASE_LBA
                } else {
                    0
                }
            }
            StorageBackend::RamDisk(vec) => (vec.len() / 512) as u64,
        }
    }

    /// Reads `buf.len() / 512` raw 512-byte sectors starting at `lba` on U-Boot block storage.
    /// `buf` must be a multiple of 4096 bytes and 64-byte aligned in low DRAM (< 0x8000_0000).
    pub fn read_raw_lba(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        debug_assert!(buf.len() % 4096 == 0 && (buf.as_ptr() as usize) % 64 == 0);
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                for (i, chunk) in buf.chunks_mut(Self::IO_CHUNK_PAGES * 4096).enumerate() {
                    let cur_lba = lba + (i * Self::IO_CHUNK_PAGES * 8) as u64;
                    let sectors = (chunk.len() / 512) as u64;
                    if uboot::dev_read(info, cur_lba, sectors, chunk.as_mut_ptr()) <= 0 {
                        return Err(StorageError::IoError);
                    }
                }
                Ok(())
            }
            StorageBackend::RamDisk(_) => Err(StorageError::IoError),
        }
    }

    /// Writes `buf.len() / 512` raw 512-byte sectors starting at `lba` on U-Boot block storage.
    /// `buf` must be a multiple of 4096 bytes and 64-byte aligned in low DRAM (< 0x8000_0000).
    pub fn write_raw_lba(&mut self, lba: u64, buf: &[u8]) -> Result<(), StorageError> {
        debug_assert!(buf.len() % 4096 == 0 && (buf.as_ptr() as usize) % 64 == 0);
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                for (i, chunk) in buf.chunks(Self::IO_CHUNK_PAGES * 4096).enumerate() {
                    let cur_lba = lba + (i * Self::IO_CHUNK_PAGES * 8) as u64;
                    let sectors = (chunk.len() / 512) as u64;
                    if uboot::dev_write(info, cur_lba, sectors, chunk.as_ptr()) <= 0 {
                        return Err(StorageError::IoError);
                    }
                }
                Ok(())
            }
            StorageBackend::RamDisk(_) => Err(StorageError::IoError),
        }
    }
}



/// Persists every dirty page of every shard atomically through the write-ahead log.
pub fn flush_all_shards() {
    let _ = wal::checkpoint();
}

pub fn init_uboot_blk() {
    tracing::debug!("[storage] Scanning U-Boot devices for Storage...");

    let mut ssd_info = None;
    let mut fallback_info = None;
    let mut last_info = None;
    let mut max_devices = 32;
    while max_devices > 0 {
        if let Some(info) = uboot::dev_enum(last_info.as_ref()) {
            tracing::debug!(
                "[storage] Discovered Device - Type: {}, Cookie: {:#x}, Blocks: {}",
                info.type_,
                info.cookie as usize,
                info.block_count
            );

            if info.type_ == uboot::DEV_TYP_STOR {
                let if_type = if !info.cookie.is_null() {
                    unsafe { *(info.cookie as *const i32) }
                } else {
                    -1
                };

                tracing::debug!("[storage] -> Block Storage Device found! if_type: {}", if_type);

                // Target NVMe SSD / NVMe/SCSI device, ignore MMC/SD card (if_type 6 and 7)
                if if_type != 6 && if_type != 7 && if_type != -1 {
                    tracing::info!("[storage] -> Selected as NVMe SSD Device!");
                    ssd_info = Some(info);
                    break;
                }

                if fallback_info.is_none() {
                    tracing::debug!("[storage] -> Registered as Fallback Storage (MMC/SD Card).");
                    fallback_info = Some(info);
                }
            }
            last_info = Some(info);
        } else {
            break;
        }
        max_devices -= 1;
    }

    let target_info = if let Some(info) = ssd_info {
        tracing::info!("[storage] Selecting NVMe SSD as primary storage.");
        Some(info)
    } else if let Some(info) = fallback_info {
        tracing::warn!("[storage] WARNING: NVMe SSD not found, falling back to SD card.");
        Some(info)
    } else {
        None
    };

    if let Some(mut info) = target_info {
        tracing::debug!("[storage] Opening U-Boot Storage Device...");
        let res = uboot::dev_open(&mut info);
        if res != 0 {
            tracing::warn!(
                "[storage] WARNING: Failed to open U-Boot Storage Device: {}",
                res
            );
        }
        let storage = StorageContext::new_uboot(info);
        init_shards(storage);
    } else {
        tracing::warn!("[storage] No U-Boot Storage Device found. Allocating 128MB RAM Disk!");
        let storage = StorageContext::new_ramdisk(128);
        init_shards(storage);
    }
}

const STORAGE_MAGIC: u64 = 0x5348415244534834; // "SHARDSH4": header v4 (slotted-page B-Tree + embedded successor)
const HEADER_PAGE: usize = 0;

fn write_header(
    storage: &mut StorageContext,
    shard_size: usize,
    wal_epoch: u64,
    wal_base: usize,
    wal_pages: usize,
) -> Result<(), StorageError> {
    let mut h = AlignedBlock::new();
    h.bytes[0..8].copy_from_slice(&STORAGE_MAGIC.to_le_bytes());
    h.bytes[8..16].copy_from_slice(&(shard_size as u64).to_le_bytes());
    h.bytes[16..20].copy_from_slice(&(SHARD_COUNT as u32).to_le_bytes());
    h.bytes[24..32].copy_from_slice(&wal_epoch.to_le_bytes());
    h.bytes[32..40].copy_from_slice(&(wal_base as u64).to_le_bytes());
    h.bytes[40..48].copy_from_slice(&(wal_pages as u64).to_le_bytes());
    storage.write_block(HEADER_PAGE, &h.bytes)
}

fn init_shards(mut storage: StorageContext) {
    let capacity_sectors = storage.capacity();
    let total_pages = (capacity_sectors / 8) as usize;
    tracing::info!(
        "[storage] Drive Capacity: {} MB ({} pages)",
        total_pages * 4 / 1024,
        total_pages
    );

    // Read header from Page 0
    let mut header_data = AlignedBlock::new();
    let _ = storage.read_block(HEADER_PAGE, &mut header_data.bytes);

    let magic = u64::from_le_bytes(header_data.bytes[0..8].try_into().unwrap());
    let mut shard_size = 0usize;
    let mut wal_epoch = 0u64;
    let mut wal_base = 0usize;
    let mut wal_pages = 0usize;
    if magic == STORAGE_MAGIC {
        shard_size = u64::from_le_bytes(header_data.bytes[8..16].try_into().unwrap()) as usize;
        wal_epoch = u64::from_le_bytes(header_data.bytes[24..32].try_into().unwrap());
        wal_base = u64::from_le_bytes(header_data.bytes[32..40].try_into().unwrap()) as usize;
        wal_pages = u64::from_le_bytes(header_data.bytes[40..48].try_into().unwrap()) as usize;
    }

    let layout_ok = shard_size != 0
        && wal_base == 1 + SHARD_COUNT * (shard_size + 1)
        && wal_pages >= 64
        && wal_pages % 2 == 0
        && wal_base + wal_pages <= total_pages;

    if !layout_ok {
        // First time initialisation, old header format, or invalid header: lay out
        // [header | 16 shards | WAL] and clear every shard's metadata page so stale data from a
        // previous layout is never interpreted as a B-tree.
        // WAL: up to 256 MB (two 128 MB areas), at most 1/8 of a small device.
        wal_pages = core::cmp::min(wal::WAL_PAGES, (total_pages / 8) & !1);
        if wal_pages < 64 || total_pages <= 1 + wal_pages + 2 * SHARD_COUNT {
            tracing::error!("[storage] Drive too small for WAL layout!");
            return;
        }
        shard_size = (total_pages - 1 - wal_pages) / SHARD_COUNT - 1;
        wal_base = 1 + SHARD_COUNT * (shard_size + 1);
        wal_epoch = 1;
        tracing::info!("[storage] Initializing new storage header (WAL layout)...");

        let zero = AlignedBlock::new();
        let _ = storage.write_raw_lba(BLOOM_HEADER_LBA, &zero.bytes);
        for i in 0..SHARD_COUNT {
            let base_page = 1 + i * (shard_size + 1);
            let _ = storage.write_block(base_page, &zero.bytes);
        }
        // Invalidate any stale WAL by writing the (new) header with a fresh epoch.
        if write_header(&mut storage, shard_size, wal_epoch, wal_base, wal_pages).is_err() {
            tracing::error!("[storage] FAILED to write header");
        }
    } else {
        // Crash recovery: re-apply the last committed WAL batch (idempotent), then start a new
        // epoch so every previous batch becomes invalid before any direct write happens.
        let replayed = wal::recover(&mut storage, wal_base, wal_pages / 2, wal_epoch);
        tracing::info!("[storage] WAL recovery: replayed pages={}", replayed);
        wal_epoch += 1;
        if write_header(&mut storage, shard_size, wal_epoch, wal_base, wal_pages).is_err() {
            tracing::error!("[storage] FAILED to write header");
        }
    }

    SHARD_SIZE_PAGES.store(shard_size, core::sync::atomic::Ordering::SeqCst);

    let is_uboot = matches!(storage.backend, StorageBackend::UBoot(_));
    let high_dram_end = bpm::HIGH_DRAM_BASE + SHARD_COUNT * bpm::HIGH_DRAM_MAX_FRAMES * bpm::BLOCK_SIZE;
    let use_high_dram = is_uboot
        && crate::trap::probe_rw_u64(bpm::HIGH_DRAM_BASE)
        && crate::trap::probe_rw_u64(bpm::HIGH_DRAM_BASE + (high_dram_end - bpm::HIGH_DRAM_BASE) / 2)
        && crate::trap::probe_rw_u64(high_dram_end - 8);

    if use_high_dram {
        tracing::info!(
            "[storage] High DRAM Bank 1 verified ({}, {} MB Sector Cache enabled, {} frames/shard)",
            crate::soc::SOC_NAME,
            (SHARD_COUNT * bpm::HIGH_DRAM_TARGET_FRAMES * bpm::BLOCK_SIZE) / (1024 * 1024),
            bpm::HIGH_DRAM_TARGET_FRAMES
        );
    } else {
        tracing::info!("[storage] Using Bank 0 Heap Sector Cache (640 MB, 10240 frames/shard)");
    }

    let shared_storage = Arc::new(Mutex::new(storage));
    *GLOBAL_STORAGE.lock() = Some(shared_storage.clone());
    wal::init(wal_base, wal_pages / 2, wal_epoch);

    for i in 0..SHARD_COUNT {
        // Shards start at Page 1 to avoid colliding with the header
        let base_page = 1 + (i * (shard_size + 1));
        let bpm = if use_high_dram {
            bpm::BufferPool::new_high_dram(shared_storage.clone(), i, base_page)
        } else {
            bpm::BufferPool::new(shared_storage.clone(), 10240, base_page)
        };
        let btree = btree::BTree::new(bpm);
        *SHARDS[i].lock() = Some(btree);
    }

    init_wallet_storage(shared_storage);
}

/// Initializes the dedicated `WALLET_SHARD` B-Tree in the isolated `WALLET_STORAGE_LBA` region
/// (`384 MB` offset, strictly below `STORAGE_BASE_LBA = 512 MB`).
pub fn init_wallet_storage(shared_storage: Arc<Mutex<StorageContext>>) {
    let _ = wallet_crypto::hardware_wallet_key();
    let replayed = shared_storage.lock().recover_wallet_journal();
    if replayed > 0 {
        tracing::info!("[storage] Wallet journal recovery: replayed pages={}", replayed);
    }
    let wallet_bpm = bpm::BufferPool::new_wallet(shared_storage, 64);
    let wallet_btree = btree::BTree::new_with_max_pages(wallet_bpm, WALLET_MAX_PAGES);
    *WALLET_SHARD.lock() = Some(wallet_btree);
}

pub fn process_delegated_requests() {}
