use crate::{
    block_cementer::BlockCementer,
    block_insertion::{BlockInserter, BlockValidatorFactory, PrefetchedBlockData},
    vote_verifier::VoteVerifier,
    AnySet, BlockRollbackPerformer, LedgerConstants, LedgerSet, OwningAnySet, OwningConfirmedSet,
    OwningUnconfirmedSet, RepWeightCache, RepWeightsUpdater, RollbackError,
};
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use rsnano_core::{
    Account, AccountInfo, Amount, Block, BlockHash, Epoch, Link, PublicKey, QualifiedRoot, Root,
    SavedBlock,
};
use rsnano_ledger_store::BtreeStore;
use rsnano_stats::{DetailType, StatType, Stats};

#[derive(PartialEq, Eq, Debug, Clone, Copy, EnumCount, EnumIter, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum BlockError {
    /// Signature was bad, forged or transmission error
    BadSignature,
    /// Already seen and was valid
    Old,
    /// Malicious attempt to spend a negative amount
    NegativeSpend,
    /// Malicious fork based on previous
    Fork,
    /// Source block doesn't exist, has already been received, or requires an account upgrade (epoch blocks)
    Unreceivable,
    /// Block marked as previous is unknown
    GapPrevious,
    /// Block marked as source is unknown
    GapSource,
    /// Block marked as pending blocks required for epoch open block are unknown
    GapEpochOpenPending,
    /// Block attempts to open the burn account
    OpenedBurnAccount,
    /// Balance and amount delta don't match
    BalanceMismatch,
    /// Representative is changed when it is not allowed
    RepresentativeMismatch,
    /// This block cannot follow the previous block
    BlockPosition,
    /// Insufficient work for this block, even though it passed the minimal validation
    InsufficientWork,
    /// The account got updated while this block was processed. This block is ether old or a fork.
    Conflict,
}

impl BlockError {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockError::BadSignature => "Bad signature",
            BlockError::Old => "Old",
            BlockError::NegativeSpend => "Negative spend",
            BlockError::Fork => "Fork",
            BlockError::Unreceivable => "Unreceivable",
            BlockError::GapPrevious => "Gap previous",
            BlockError::GapSource => "Gap source",
            BlockError::GapEpochOpenPending => "Gap epoch open pendign",
            BlockError::OpenedBurnAccount => "Opened burn account",
            BlockError::BalanceMismatch => "Balance mismatch",
            BlockError::RepresentativeMismatch => "Representative mismatch",
            BlockError::BlockPosition => "Block position",
            BlockError::InsufficientWork => "Insufficient work",
            BlockError::Conflict => "Conflict",
        }
    }
}

impl From<BlockError> for DetailType {
    fn from(value: BlockError) -> Self {
        match value {
            BlockError::BadSignature => Self::BadSignature,
            BlockError::Old => Self::Old,
            BlockError::NegativeSpend => Self::NegativeSpend,
            BlockError::Fork => Self::Fork,
            BlockError::Unreceivable => Self::Unreceivable,
            BlockError::GapPrevious => Self::GapPrevious,
            BlockError::GapSource => Self::GapSource,
            BlockError::GapEpochOpenPending => Self::GapEpochOpenPending,
            BlockError::OpenedBurnAccount => Self::OpenedBurnAccount,
            BlockError::BalanceMismatch => Self::BalanceMismatch,
            BlockError::RepresentativeMismatch => Self::RepresentativeMismatch,
            BlockError::BlockPosition => Self::BlockPosition,
            BlockError::InsufficientWork => Self::InsufficientWork,
            BlockError::Conflict => Self::Conflict,
        }
    }
}

/// Profiling (CLINT ticks): [0]=create_validator, [1]=validate, [2]=insert.
#[derive(Clone)]
enum DeferredPendingOp {
    Insert(rsnano_core::PendingInfo),
    DeleteOnDisk,
    CancelledInBatch,
}
pub static LEDGER_PROF: [core::sync::atomic::AtomicU64; 10] = [
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
];

const FAST_BATCH_SLOTS: usize = 4096;
const FAST_BATCH_MASK: usize = FAST_BATCH_SLOTS - 1;
const FAST_BATCH_MAX_INDEXED: usize = 3072;

trait BatchHashKey: Eq + Clone {
    fn fast_hash(&self) -> usize;
}

impl BatchHashKey for BlockHash {
    #[inline(always)]
    fn fast_hash(&self) -> usize {
        u64::from_ne_bytes(self.as_bytes()[0..8].try_into().unwrap()) as usize
    }
}

impl BatchHashKey for Account {
    #[inline(always)]
    fn fast_hash(&self) -> usize {
        u64::from_ne_bytes(self.as_bytes()[0..8].try_into().unwrap()) as usize
    }
}

impl BatchHashKey for rsnano_core::PendingKey {
    #[inline(always)]
    fn fast_hash(&self) -> usize {
        let a = u64::from_ne_bytes(self.receiving_account.as_bytes()[0..8].try_into().unwrap());
        let b = u64::from_ne_bytes(self.send_block_hash.as_bytes()[0..8].try_into().unwrap());
        (a ^ b.rotate_left(17)) as usize
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct BatchGenSlot {
    gen: u16,
    idx: u16,
}

impl BatchGenSlot {
    const EMPTY: Self = Self { gen: 0, idx: 0 };
}

/// Generation-stamped open-addressed hash table backed by a single flat `Vec<(K, V)>`
/// to avoid per-batch `memset` zeroing and per-node heap allocations on Hart 6.
struct FastBatchMap<K: BatchHashKey, V> {
    gen: u16,
    slots: [BatchGenSlot; FAST_BATCH_SLOTS],
    entries: Vec<(K, V)>,
}

impl<K: BatchHashKey, V> FastBatchMap<K, V> {
    #[inline]
    fn with_capacity(cap: usize) -> Self {
        Self {
            gen: 1,
            slots: [BatchGenSlot::EMPTY; FAST_BATCH_SLOTS],
            entries: Vec::with_capacity(cap),
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    fn find_index(&self, key: &K) -> Option<usize> {
        let gen = self.gen;
        let mut h = key.fast_hash() & FAST_BATCH_MASK;
        loop {
            let s = self.slots[h];
            if s.gen != gen {
                break;
            }
            let idx = s.idx as usize;
            if &self.entries[idx].0 == key {
                return Some(idx);
            }
            h = (h + 1) & FAST_BATCH_MASK;
        }
        if self.entries.len() > FAST_BATCH_MAX_INDEXED {
            for idx in FAST_BATCH_MAX_INDEXED..self.entries.len() {
                if &self.entries[idx].0 == key {
                    return Some(idx);
                }
            }
        }
        None
    }

    #[inline]
    fn contains_key(&self, key: &K) -> bool {
        self.find_index(key).is_some()
    }

    #[inline]
    fn get(&self, key: &K) -> Option<&V> {
        self.find_index(key).map(|idx| &self.entries[idx].1)
    }

    #[inline]
    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.find_index(key).map(|idx| &mut self.entries[idx].1)
    }

    #[inline]
    fn insert(&mut self, key: K, val: V) {
        if self.entries.len() < FAST_BATCH_MAX_INDEXED {
            let gen = self.gen;
            let mut h = key.fast_hash() & FAST_BATCH_MASK;
            loop {
                let s = self.slots[h];
                if s.gen != gen {
                    let idx = self.entries.len();
                    self.entries.push((key, val));
                    self.slots[h] = BatchGenSlot {
                        gen,
                        idx: idx as u16,
                    };
                    return;
                }
                let idx = s.idx as usize;
                if self.entries[idx].0 == key {
                    self.entries[idx].1 = val;
                    return;
                }
                h = (h + 1) & FAST_BATCH_MASK;
            }
        } else if let Some(idx) = self.find_index(&key) {
            self.entries[idx].1 = val;
        } else {
            self.entries.push((key, val));
        }
    }

    #[inline]
    fn clear(&mut self) {
        self.entries.clear();
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.slots.fill(BatchGenSlot::EMPTY);
            self.gen = 1;
        }
    }

    #[inline(always)]
    fn entries(&self) -> &[(K, V)] {
        &self.entries
    }

    #[inline(always)]
    fn entries_mut(&mut self) -> &mut [(K, V)] {
        &mut self.entries
    }
}

static STRIPE_DISPATCH_LOCK: spin::Mutex<()> = spin::Mutex::new(());

static LEDGER_B_ONLINE: AtomicBool = AtomicBool::new(false);
static LEDGER_B_JOB_PTR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static LEDGER_B_JOB_DONE: AtomicBool = AtomicBool::new(false);
pub static LEDGER_B_JOBS_COMPLETED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

static LEDGER_C_ONLINE: AtomicBool = AtomicBool::new(false);
static LEDGER_C_JOB_PTR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static LEDGER_C_JOB_DONE: AtomicBool = AtomicBool::new(false);
pub static LEDGER_C_JOBS_COMPLETED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

const PREFETCH_NEED_EXISTS: u8 = 1;
const PREFETCH_NEED_BLOCK: u8 = 2;

enum LedgerStripeJob {
    PrefetchReads {
        store: *const BtreeStore,
        hash_queries: *const [(BlockHash, u8)],
        hash_results: *mut [(bool, Option<SavedBlock>)],
        acct_queries: *const [Account],
        acct_results: *mut [Option<AccountInfo>],
        pend_queries: *const [rsnano_core::PendingKey],
        pend_results: *mut [Option<rsnano_core::PendingInfo>],
    },
    FlushWrites {
        store: *const BtreeStore,
        ext_succ_slice: *const [(BlockHash, BlockHash)],
        pending_slice: *const [(rsnano_core::PendingKey, Option<rsnano_core::PendingInfo>)],
        blocks_slice: *const [(BlockHash, SavedBlock, BlockHash)],
        accounts_slice: *const [(Account, (AccountInfo, AccountInfo))],
    },
    CementPrefetch {
        store: *const BtreeStore,
        target_hashes: *const [BlockHash],
        target_blocks: *mut [Option<SavedBlock>],
    },
    FlushConfHeights {
        store: *const BtreeStore,
        heights_slice: *const [(Account, rsnano_core::ConfirmationHeightInfo)],
    },
}

#[inline]
fn execute_prefetch_slice(
    store: &BtreeStore,
    hash_q: &[(BlockHash, u8)],
    hash_r: &mut [(bool, Option<SavedBlock>)],
    acct_q: &[Account],
    acct_r: &mut [Option<AccountInfo>],
    pend_q: &[rsnano_core::PendingKey],
    pend_r: &mut [Option<rsnano_core::PendingInfo>],
) {
    for ((hash, flags), out) in hash_q.iter().zip(hash_r.iter_mut()) {
        if (flags & PREFETCH_NEED_BLOCK) != 0 {
            let blk = store.block.get(hash);
            let exists =
                blk.is_some() || ((flags & PREFETCH_NEED_EXISTS) != 0 && store.pruned.exists(hash));
            *out = (exists, blk);
        } else {
            let exists = store.block.exists(hash) || store.pruned.exists(hash);
            *out = (exists, None);
        }
    }
    for (acc, out) in acct_q.iter().zip(acct_r.iter_mut()) {
        *out = store.account.get(acc);
    }
    for (pk, out) in pend_q.iter().zip(pend_r.iter_mut()) {
        *out = store.pending.get(pk);
    }
}

#[inline]
fn execute_cement_prefetch_slice(
    store: &BtreeStore,
    hashes: &[BlockHash],
    blocks: &mut [Option<SavedBlock>],
) {
    for (h, out) in hashes.iter().zip(blocks.iter_mut()) {
        *out = store.block.get(h);
    }
}

#[inline]
fn execute_conf_heights_slice(
    store: &BtreeStore,
    heights: &[(Account, rsnano_core::ConfirmationHeightInfo)],
) {
    for (account, conf_height) in heights {
        store.confirmation_height.put(account, conf_height);
    }
}

/// SAFETY: Caller must guarantee all raw pointers inside `job` remain valid and exclusively
/// partitioned across harts until completion is acknowledged.
#[inline]
unsafe fn execute_stripe_job(job: &LedgerStripeJob) {
    match *job {
        LedgerStripeJob::PrefetchReads {
            store,
            hash_queries,
            hash_results,
            acct_queries,
            acct_results,
            pend_queries,
            pend_results,
        } => {
            execute_prefetch_slice(
                &*store,
                &*hash_queries,
                &mut *hash_results,
                &*acct_queries,
                &mut *acct_results,
                &*pend_queries,
                &mut *pend_results,
            );
        }
        LedgerStripeJob::FlushWrites {
            store,
            ext_succ_slice,
            pending_slice,
            blocks_slice,
            accounts_slice,
        } => {
            flush_shard_range(
                &*store,
                &*ext_succ_slice,
                &*pending_slice,
                &*blocks_slice,
                &*accounts_slice,
            );
        }
        LedgerStripeJob::CementPrefetch {
            store,
            target_hashes,
            target_blocks,
        } => {
            execute_cement_prefetch_slice(&*store, &*target_hashes, &mut *target_blocks);
        }
        LedgerStripeJob::FlushConfHeights {
            store,
            heights_slice,
        } => {
            execute_conf_heights_slice(&*store, &*heights_slice);
        }
    }
}

pub(crate) fn prefetch_cement_blocks_parallel(
    store: &BtreeStore,
    sorted_hashes: &[BlockHash],
) -> Vec<Option<SavedBlock>> {
    let n = sorted_hashes.len();
    let mut results = alloc::vec![None; n];
    if n == 0 {
        return results;
    }

    let stripe_guard = STRIPE_DISPATCH_LOCK.try_lock();
    let b_online = stripe_guard.is_some() && LEDGER_B_ONLINE.load(Ordering::Relaxed);
    let c_online = stripe_guard.is_some()
        && LEDGER_C_ONLINE.load(Ordering::Relaxed)
        && !kernel_common::storage::wal::is_wal_bg_pending();

    if b_online && c_online && n >= 12 {
        let cut1 = sorted_hashes.partition_point(|h| (h.as_bytes()[0] >> 2) < 21);
        let cut2 = sorted_hashes.partition_point(|h| (h.as_bytes()[0] >> 2) < 42);
        let (r0, rest) = results.split_at_mut(cut1);
        let (r1, r2) = rest.split_at_mut(cut2 - cut1);

        let job_b = LedgerStripeJob::CementPrefetch {
            store: store as *const BtreeStore,
            target_hashes: &sorted_hashes[cut1..cut2] as *const [BlockHash],
            target_blocks: r1 as *mut [Option<SavedBlock>],
        };
        let job_c = LedgerStripeJob::CementPrefetch {
            store: store as *const BtreeStore,
            target_hashes: &sorted_hashes[cut2..] as *const [BlockHash],
            target_blocks: r2 as *mut [Option<SavedBlock>],
        };
        let job_b_addr = &job_b as *const LedgerStripeJob as usize;
        let job_c_addr = &job_c as *const LedgerStripeJob as usize;
        LEDGER_B_JOB_DONE.store(false, Ordering::Release);
        LEDGER_C_JOB_DONE.store(false, Ordering::Release);
        LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);
        LEDGER_C_JOB_PTR.store(job_c_addr, Ordering::Release);

        execute_cement_prefetch_slice(store, &sorted_hashes[..cut1], r0);

        let stole_b = LEDGER_B_JOB_PTR
            .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if stole_b {
            execute_cement_prefetch_slice(store, &sorted_hashes[cut1..cut2], r1);
        }
        let stole_c = LEDGER_C_JOB_PTR
            .compare_exchange(job_c_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if stole_c {
            execute_cement_prefetch_slice(store, &sorted_hashes[cut2..], r2);
        }
        if !stole_b {
            while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        if !stole_c {
            while !LEDGER_C_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
    } else if b_online && n >= 8 {
        let mid = sorted_hashes.partition_point(|h| (h.as_bytes()[0] >> 2) < 32);
        let (r0, r1) = results.split_at_mut(mid);
        let job_b = LedgerStripeJob::CementPrefetch {
            store: store as *const BtreeStore,
            target_hashes: &sorted_hashes[mid..] as *const [BlockHash],
            target_blocks: r1 as *mut [Option<SavedBlock>],
        };
        let job_b_addr = &job_b as *const LedgerStripeJob as usize;
        LEDGER_B_JOB_DONE.store(false, Ordering::Release);
        LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);

        execute_cement_prefetch_slice(store, &sorted_hashes[..mid], r0);

        if LEDGER_B_JOB_PTR
            .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            execute_cement_prefetch_slice(store, &sorted_hashes[mid..], r1);
        } else {
            while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
    } else {
        execute_cement_prefetch_slice(store, sorted_hashes, &mut results);
    }

    results
}

pub(crate) fn flush_conf_heights_parallel(
    store: &BtreeStore,
    sorted_heights: &[(Account, rsnano_core::ConfirmationHeightInfo)],
) {
    let n = sorted_heights.len();
    if n == 0 {
        return;
    }

    let stripe_guard = STRIPE_DISPATCH_LOCK.try_lock();
    let b_online = stripe_guard.is_some() && LEDGER_B_ONLINE.load(Ordering::Relaxed);
    let c_online = stripe_guard.is_some()
        && LEDGER_C_ONLINE.load(Ordering::Relaxed)
        && !kernel_common::storage::wal::is_wal_bg_pending();

    if b_online && c_online && n >= 12 {
        let cut1 = sorted_heights.partition_point(|(a, _)| (a.as_bytes()[0] >> 2) < 21);
        let cut2 = sorted_heights.partition_point(|(a, _)| (a.as_bytes()[0] >> 2) < 42);
        let job_b = LedgerStripeJob::FlushConfHeights {
            store: store as *const BtreeStore,
            heights_slice: &sorted_heights[cut1..cut2]
                as *const [(Account, rsnano_core::ConfirmationHeightInfo)],
        };
        let job_c = LedgerStripeJob::FlushConfHeights {
            store: store as *const BtreeStore,
            heights_slice: &sorted_heights[cut2..]
                as *const [(Account, rsnano_core::ConfirmationHeightInfo)],
        };
        let job_b_addr = &job_b as *const LedgerStripeJob as usize;
        let job_c_addr = &job_c as *const LedgerStripeJob as usize;
        LEDGER_B_JOB_DONE.store(false, Ordering::Release);
        LEDGER_C_JOB_DONE.store(false, Ordering::Release);
        LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);
        LEDGER_C_JOB_PTR.store(job_c_addr, Ordering::Release);

        execute_conf_heights_slice(store, &sorted_heights[..cut1]);

        let stole_b = LEDGER_B_JOB_PTR
            .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if stole_b {
            execute_conf_heights_slice(store, &sorted_heights[cut1..cut2]);
        }
        let stole_c = LEDGER_C_JOB_PTR
            .compare_exchange(job_c_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if stole_c {
            execute_conf_heights_slice(store, &sorted_heights[cut2..]);
        }
        if !stole_b {
            while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        if !stole_c {
            while !LEDGER_C_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
    } else if b_online && n >= 8 {
        let mid = sorted_heights.partition_point(|(a, _)| (a.as_bytes()[0] >> 2) < 32);
        let job_b = LedgerStripeJob::FlushConfHeights {
            store: store as *const BtreeStore,
            heights_slice: &sorted_heights[mid..]
                as *const [(Account, rsnano_core::ConfirmationHeightInfo)],
        };
        let job_b_addr = &job_b as *const LedgerStripeJob as usize;
        LEDGER_B_JOB_DONE.store(false, Ordering::Release);
        LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);

        execute_conf_heights_slice(store, &sorted_heights[..mid]);

        if LEDGER_B_JOB_PTR
            .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            execute_conf_heights_slice(store, &sorted_heights[mid..]);
        } else {
            while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
    } else {
        execute_conf_heights_slice(store, sorted_heights);
    }
}

/// Marks Hart 5 (`Ledger-B`, Shards `21..=41`, half-open `[21, 42)`) as online so Hart 6 (`Ledger-A`, Shards `0..=20`)
/// dispatches the middle shard stripe of each block batch to Hart 5 across Cluster 1's shared L2.
pub fn set_ledger_b_online(online: bool) {
    LEDGER_B_ONLINE.store(online, Ordering::Release);
}

/// Marks Hart 7 (`Ledger-C`, Shards `42..=63`, half-open `[42, 64)`) as online so Hart 6 (`Ledger-A`, Shards `0..=20`)
/// dispatches the upper shard stripe of each block batch to Hart 7 across Cluster 1's shared L2.
pub fn set_ledger_c_online(online: bool) {
    LEDGER_C_ONLINE.store(online, Ordering::Release);
}

#[inline]
fn flush_shard_range(
    store: &BtreeStore,
    ext_succ_slice: &[(BlockHash, BlockHash)],
    pending_slice: &[(rsnano_core::PendingKey, Option<rsnano_core::PendingInfo>)],
    blocks_slice: &[(BlockHash, SavedBlock, BlockHash)],
    accounts_slice: &[(Account, (AccountInfo, AccountInfo))],
) {
    if !ext_succ_slice.is_empty() {
        store.successors.put_batch(ext_succ_slice);
    }
    if !pending_slice.is_empty() {
        store.pending.flush_batch(pending_slice);
    }
    if !blocks_slice.is_empty() {
        store.block.put_batch_with_successors(blocks_slice);
    }
    if !accounts_slice.is_empty() {
        store.account.flush_batch(accounts_slice);
    }
}

/// Polled by Hart 5 (`Ledger-B`) in Cluster 1 to execute Shards `21..42` B-Tree reads/writes
/// in parallel while Hart 6 (`Ledger-A`) executes Shards `0..21` and Hart 7 (`Ledger-C`) executes `42..64`.
#[inline]
pub fn try_run_ledger_b_job() -> bool {
    let job_addr = LEDGER_B_JOB_PTR.swap(0, Ordering::AcqRel);
    if job_addr == 0 {
        return false;
    }
    // SAFETY: `job_addr` was published by Hart 6 on its stack and Hart 6 spins on
    // `LEDGER_B_JOB_DONE` with `Acquire` ordering before returning from the frame.
    unsafe {
        execute_stripe_job(&*(job_addr as *const LedgerStripeJob));
    }
    LEDGER_B_JOBS_COMPLETED.fetch_add(1, Ordering::Relaxed);
    LEDGER_B_JOB_DONE.store(true, Ordering::Release);
    true
}

/// Polled by Hart 7 (`Ledger-C`) in Cluster 1 to execute Shards `42..64` B-Tree reads/writes
/// in parallel while Hart 6 (`Ledger-A`) executes Shards `0..21` and Hart 5 (`Ledger-B`) executes `21..42`.
#[inline]
pub fn try_run_ledger_c_job() -> bool {
    let job_addr = LEDGER_C_JOB_PTR.swap(0, Ordering::AcqRel);
    if job_addr == 0 {
        return false;
    }
    // SAFETY: `job_addr` was published by Hart 6 on its stack and Hart 6 spins on
    // `LEDGER_C_JOB_DONE` with `Acquire` ordering before returning from the frame.
    unsafe {
        execute_stripe_job(&*(job_addr as *const LedgerStripeJob));
    }
    LEDGER_C_JOBS_COMPLETED.fetch_add(1, Ordering::Relaxed);
    LEDGER_C_JOB_DONE.store(true, Ordering::Release);
    true
}

/// Specialized generation-stamped batch block table storing `(BlockHash, SavedBlock, BlockHash /* successor */)`
/// contiguously so `entries_mut()` can be sorted in-place and passed directly to `BlockStore::put_batch_with_successors`.
struct FastBatchBlockMap {
    gen: u16,
    slots: [BatchGenSlot; FAST_BATCH_SLOTS],
    entries: Vec<(BlockHash, SavedBlock, BlockHash)>,
}

impl FastBatchBlockMap {
    #[inline]
    fn with_capacity(cap: usize) -> Self {
        Self {
            gen: 1,
            slots: [BatchGenSlot::EMPTY; FAST_BATCH_SLOTS],
            entries: Vec::with_capacity(cap),
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    fn clear(&mut self) {
        self.entries.clear();
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.slots.fill(BatchGenSlot::EMPTY);
            self.gen = 1;
        }
    }

    #[inline]
    fn find_index(&self, key: &BlockHash) -> Option<usize> {
        let gen = self.gen;
        let mut h = key.fast_hash() & FAST_BATCH_MASK;
        loop {
            let s = self.slots[h];
            if s.gen != gen {
                break;
            }
            let idx = s.idx as usize;
            if &self.entries[idx].0 == key {
                return Some(idx);
            }
            h = (h + 1) & FAST_BATCH_MASK;
        }
        if self.entries.len() > FAST_BATCH_MAX_INDEXED {
            for idx in FAST_BATCH_MAX_INDEXED..self.entries.len() {
                if &self.entries[idx].0 == key {
                    return Some(idx);
                }
            }
        }
        None
    }

    #[inline]
    fn contains_key(&self, key: &BlockHash) -> bool {
        self.find_index(key).is_some()
    }

    #[inline]
    fn get_block(&self, key: &BlockHash) -> Option<&SavedBlock> {
        self.find_index(key).map(|idx| &self.entries[idx].1)
    }

    #[inline]
    fn set_successor(&mut self, key: &BlockHash, succ: BlockHash) -> bool {
        if let Some(idx) = self.find_index(key) {
            self.entries[idx].2 = succ;
            true
        } else {
            false
        }
    }

    #[inline]
    fn insert(&mut self, key: BlockHash, block: SavedBlock) {
        if self.entries.len() < FAST_BATCH_MAX_INDEXED {
            let gen = self.gen;
            let mut h = key.fast_hash() & FAST_BATCH_MASK;
            loop {
                let s = self.slots[h];
                if s.gen != gen {
                    let idx = self.entries.len();
                    self.entries.push((key, block, BlockHash::zero()));
                    self.slots[h] = BatchGenSlot {
                        gen,
                        idx: idx as u16,
                    };
                    return;
                }
                let idx = s.idx as usize;
                if self.entries[idx].0 == key {
                    self.entries[idx].1 = block;
                    return;
                }
                h = (h + 1) & FAST_BATCH_MASK;
            }
        } else if let Some(idx) = self.find_index(&key) {
            self.entries[idx].1 = block;
        } else {
            self.entries.push((key, block, BlockHash::zero()));
        }
    }

    #[inline(always)]
    fn entries_mut(&mut self) -> &mut [(BlockHash, SavedBlock, BlockHash)] {
        &mut self.entries
    }
}

struct BatchScratchArena {
    failed_hashes: FastBatchMap<BlockHash, ()>,
    deferred_blocks: FastBatchBlockMap,
    external_successors: FastBatchMap<BlockHash, BlockHash>,
    deferred_accounts: FastBatchMap<Account, (AccountInfo, AccountInfo)>,
    deferred_pending: FastBatchMap<rsnano_core::PendingKey, DeferredPendingOp>,
    old_tips: FastBatchMap<Account, BlockHash>,
    seen_accounts: FastBatchMap<Account, (Option<AccountInfo>, Option<Amount>)>,
    seen_pending: FastBatchMap<rsnano_core::PendingKey, rsnano_core::PendingInfo>,
    produced_in_batch: FastBatchMap<BlockHash, Option<Amount>>,
    raw_hashes: Vec<(BlockHash, u8)>,
    hash_queries: Vec<(BlockHash, u8)>,
    hash_results: Vec<(bool, Option<SavedBlock>)>,
    acct_queries: Vec<Account>,
    acct_results: Vec<Option<AccountInfo>>,
    pend_queries: Vec<rsnano_core::PendingKey>,
    pend_results: Vec<Option<rsnano_core::PendingInfo>>,
    sorted_pending: Vec<(rsnano_core::PendingKey, Option<rsnano_core::PendingInfo>)>,
    rep_deltas: Vec<(PublicKey, Amount)>,
}

impl BatchScratchArena {
    fn new(cap: usize) -> Self {
        Self {
            failed_hashes: FastBatchMap::with_capacity(cap),
            deferred_blocks: FastBatchBlockMap::with_capacity(cap),
            external_successors: FastBatchMap::with_capacity(64),
            deferred_accounts: FastBatchMap::with_capacity(256),
            deferred_pending: FastBatchMap::with_capacity(cap),
            old_tips: FastBatchMap::with_capacity(16),
            seen_accounts: FastBatchMap::with_capacity(256),
            seen_pending: FastBatchMap::with_capacity(256),
            produced_in_batch: FastBatchMap::with_capacity(cap),
            raw_hashes: Vec::new(),
            hash_queries: Vec::new(),
            hash_results: Vec::new(),
            acct_queries: Vec::new(),
            acct_results: Vec::new(),
            pend_queries: Vec::new(),
            pend_results: Vec::new(),
            sorted_pending: Vec::with_capacity(cap),
            rep_deltas: Vec::with_capacity(64),
        }
    }

    #[inline]
    fn reset(&mut self) {
        self.failed_hashes.clear();
        self.deferred_blocks.clear();
        self.external_successors.clear();
        self.deferred_accounts.clear();
        self.deferred_pending.clear();
        self.old_tips.clear();
        self.seen_accounts.clear();
        self.seen_pending.clear();
        self.produced_in_batch.clear();
        self.raw_hashes.clear();
        self.hash_queries.clear();
        self.hash_results.clear();
        self.acct_queries.clear();
        self.acct_results.clear();
        self.pend_queries.clear();
        self.pend_results.clear();
        self.sorted_pending.clear();
        self.rep_deltas.clear();
    }
}

static BATCH_SCRATCH: spin::Mutex<Option<alloc::boxed::Box<BatchScratchArena>>> =
    spin::Mutex::new(None);

pub struct Ledger {
    pub store: BtreeStore,
    pub rep_weights_updater: RepWeightsUpdater,
    pub rep_weights: Arc<RepWeightCache>,
    pub constants: LedgerConstants,
    pruning: AtomicBool,
    pub(crate) stats: Arc<Stats>,
}

impl Ledger {
    pub fn new_null() -> Self {
        let mut store = BtreeStore::new();
        let rep_weights = Arc::new(RepWeightCache::new());
        store.set_cache(rep_weights.ledger_cache.clone());
        store.cache.account_count.store(1, Ordering::SeqCst);
        store.cache.block_count.store(1, Ordering::SeqCst);
        store.cache.confirmed_count.store(1, Ordering::SeqCst);
        let rep_weights_updater =
            RepWeightsUpdater::new(store.rep_weight.clone(), Amount::zero(), &rep_weights);
        Self {
            rep_weights,
            rep_weights_updater,
            store,
            constants: LedgerConstants::unit_test(),
            pruning: AtomicBool::new(false),
            stats: Arc::new(Stats::default()),
        }
    }

    pub fn simulate_block_count(&self, count: u64) {
        self.store.cache.block_count.store(count, Ordering::SeqCst);
    }

    pub fn simulate_confirmed_count(&self, count: u64) {
        self.store
            .cache
            .confirmed_count
            .store(count, Ordering::SeqCst);
    }

    pub fn store_vendor(&self) -> &'static str {
        "B-tree"
    }

    pub fn version(&self) -> u32 {
        1
    }

    pub fn pruned_count(&self) -> usize {
        self.store.cache.pruned_count.load(Ordering::Relaxed) as usize
    }

    pub fn genesis(&self) -> &Block {
        &self.constants.genesis_block
    }

    pub(crate) fn new(
        mut store: BtreeStore,
        constants: LedgerConstants,
        min_rep_weight: Amount,
        rep_weights: Arc<RepWeightCache>,
        stats: Arc<Stats>,
        thread_count: usize,
    ) -> anyhow::Result<Self> {
        store.set_cache(rep_weights.ledger_cache.clone());

        let rep_weights_updater =
            RepWeightsUpdater::new(store.rep_weight.clone(), min_rep_weight, &rep_weights);

        let mut ledger = Self {
            rep_weights,
            rep_weights_updater,
            store,
            constants,
            pruning: AtomicBool::new(false),
            stats,
        };

        ledger.initialize(thread_count)?;

        Ok(ledger)
    }

    fn initialize(&mut self, _thread_count: usize) -> anyhow::Result<()> {
        kernel_common::watchdog::kick();
        tracing::info!("[ledger] Initializing ledger...");
        let genesis = self.constants.genesis_block.clone();
        let genesis_account = self.constants.genesis_block.account();

        for attempt in 0..2 {
            kernel_common::watchdog::kick();
            self.store.generate_cache();
            if self.pruned_count() > 0 {
                self.enable_pruning();
            }
            self.store
                .rep_weight
                .for_each(|rep, weight| self.rep_weights_updater.representation_put(rep, weight));

            // 1. Ensure genesis block exists
            let exists = self.any().block_exists_or_pruned(&genesis.hash());
            tracing::debug!("[ledger] Checked genesis block. Exists={}", exists);

            if !exists {
                tracing::info!(
                    "[ledger] Genesis block not found, inserting (attempt {})...",
                    attempt
                );

                if let Err(e) = self.process_one(&genesis) {
                    tracing::error!("[ledger] Failed to process genesis block: {:?}", e);
                    if attempt == 0 {
                        tracing::error!("[ledger] FATAL: Ledger corruption detected during genesis insertion. Wiping database...");
                        self.store.wipe();
                        continue;
                    } else {
                        return Err(anyhow::anyhow!("Could not insert genesis: {:?}", e));
                    }
                } else {
                    tracing::info!("[ledger] Genesis block processed successfully.");
                    // Diagnostic: Verify we can retrieve it immediately
                    if let Some(_found) = self.any().get_block(&genesis.hash()) {
                        tracing::debug!("[ledger] VERIFIED: Genesis block is retrievable from store.");
                    } else {
                        tracing::error!("[ledger] CRITICAL: Genesis block NOT FOUND in store immediately after processing!");
                    }
                }
                self.confirm(genesis.hash());
                let _ = self.store.flush();
            }

            // 2. Ensure account info exists and is consistent
            let info = self.any().get_account(&genesis_account);
            tracing::debug!(
                "Ledger: initial lookup of genesis account: found={}",
                info.is_some()
            );
            match info {
                Some(info) => {
                    // Check if head block exists
                    if !self.any().block_exists_or_pruned(&info.head) {
                        tracing::warn!(
                            "[ledger] Ledger inconsistency: Head block {} NOT found for genesis account. Resetting to genesis...",
                            info.head
                        );
                        // Reset account info to genesis
                        let mut new_info = info.clone();
                        new_info.head = genesis.hash();
                        new_info.balance = self.constants.genesis_amount;
                        new_info.block_count = 1;
                        self.store.account.put(&genesis_account, &new_info);
                        self.store.flush().expect("Could not flush during recovery");
                        tracing::info!("[ledger] Genesis account reset to consistent state.");
                        continue;
                    }
                }
                None => {
                    tracing::warn!(
                        "[ledger] Genesis account info NOT found for account {}! Recreating from genesis block...",
                        genesis_account.encode_account()
                    );
                    let mut info = AccountInfo::default();
                    info.head = genesis.hash();
                    info.open_block = genesis.hash();
                    info.balance = self.constants.genesis_amount;
                    info.block_count = 1;
                    self.store.account.put(&genesis_account, &info);
                    self.store.flush().expect("Could not flush during recovery");
                    tracing::info!("[ledger] Genesis account info recreated successfully.");
                    continue;
                }
            }

            // If we reached here, basic integrity is confirmed
            if attempt > 0 {
                tracing::info!("[ledger] Ledger successfully recovered and initialized.");
            }
            break;
        }

        tracing::debug!("[ledger] Fetching genesis account info...");
        let _info = self.any().get_account(&genesis_account).unwrap();
        tracing::debug!("[ledger] Fetching genesis account info done.");

        tracing::debug!("[ledger] Tracing block successors...");
        let mut current = genesis.hash();
        let mut count = 0;
        while !current.is_zero() && count < 10 {
            kernel_common::watchdog::kick();
            if let Some(_block) = self.any().get_block(&current) {
                tracing::debug!("[ledger]   Block successor: {}", count);
                current = self.any().block_successor(&current).unwrap_or_default();
            } else if self.any().block_exists_or_pruned(&current) {
                tracing::debug!("[ledger]   Block successor {} is pruned.", count);
                break;
            } else {
                tracing::warn!("[ledger]   Block successor NOT FOUND.");
                break;
            }
            count += 1;
        }
        tracing::debug!("[ledger] Tracing block successors done.");

        Ok(())
    }

    pub fn any(&self) -> OwningAnySet<'_> {
        OwningAnySet::new(&self.store, &self.constants)
    }

    pub fn confirmed(&self) -> OwningConfirmedSet<'_> {
        OwningConfirmedSet::new(&self.store)
    }

    pub fn unconfirmed(&self) -> impl LedgerSet + use<'_> {
        OwningUnconfirmedSet::new(&self.store)
    }

    pub fn pruning_enabled(&self) -> bool {
        self.pruning.load(Ordering::SeqCst)
    }

    pub fn enable_pruning(&self) {
        self.pruning.store(true, Ordering::SeqCst);
    }

    pub fn bootstrap_weight_max_blocks(&self) -> u64 {
        self.rep_weights.bootstrap_weight_max_blocks()
    }

    pub fn work_thresholds(&self) -> &rsnano_work::WorkThresholds {
        &self.constants.work
    }

    /// Returns the cached vote weight for the given representative.
    /// If the weight is below the cache limit it returns 0.
    /// During bootstrap it returns the preconfigured bootstrap weights.
    pub fn weight(&self, rep: &PublicKey) -> Amount {
        self.rep_weights.weight(rep)
    }

    pub fn verify_votes(
        &self,
        candidates: VecDeque<(Root, BlockHash)>,
        is_final: bool,
    ) -> VecDeque<(Root, BlockHash)> {
        let verifier = VoteVerifier {
            constants: &self.constants,
            store: &self.store,
        };
        verifier.verify_votes(candidates, is_final)
    }

    pub fn backlog_count(&self) -> usize {
        self.block_count().saturating_sub(self.confirmed_count())
    }

    #[inline(always)]
    pub fn has_unconfirmed_backlog_work(&self) -> bool {
        rsnano_ledger_store::unconfirmed_accounts_has_work()
    }

    #[inline(always)]
    pub fn tracked_unconfirmed_accounts(&self) -> usize {
        rsnano_ledger_store::unconfirmed_accounts_count()
    }

    #[inline(always)]
    pub fn drain_recently_confirmed(&self, max_count: usize) -> Vec<Account> {
        rsnano_ledger_store::drain_recently_confirmed(max_count)
    }

    #[inline(always)]
    pub fn unconfirmed_accounts_next_batch(
        &self,
        start: Account,
        max_count: usize,
    ) -> (Vec<Account>, Option<Account>, bool) {
        rsnano_ledger_store::unconfirmed_accounts_next_batch(start, max_count)
    }

    #[inline(always)]
    pub fn unconfirmed_account_resolve_clean(&self, account: &Account, current_conf_height: u64) {
        rsnano_ledger_store::unconfirmed_account_resolve_clean(account, current_conf_height);
    }

    pub fn account_count(&self) -> usize {
        self.store.cache.account_count.load(Ordering::Relaxed) as usize
    }

    pub fn block_count(&self) -> usize {
        self.store.cache.block_count.load(Ordering::Relaxed) as usize
    }

    pub fn confirmed_count(&self) -> usize {
        self.store.cache.confirmed_count.load(Ordering::Relaxed) as usize
    }

    pub fn is_epoch_link(&self, link: &Link) -> bool {
        self.constants.epochs.is_epoch_link(link)
    }

    pub fn epoch_signer(&self, link: &Link) -> Option<Account> {
        self.constants.epochs.epoch_signer(link)
    }

    pub fn epoch_link(&self, epoch: Epoch) -> Option<Link> {
        self.constants.epochs.link(epoch).cloned()
    }

    pub(crate) fn update_account(
        &self,
        _txn: &mut (),
        account: &Account,
        old_info: &AccountInfo,
        new_info: &AccountInfo,
    ) {
        if !new_info.head.is_zero() {
            if old_info.head.is_zero() && new_info.open_block == new_info.head {
                self.store
                    .cache
                    .account_count
                    .fetch_add(1, Ordering::SeqCst);
            }
            if !old_info.head.is_zero() && old_info.epoch != new_info.epoch {
                self.store.account.del(account);
            }
            self.store.account.put(account, new_info);
        } else {
            self.store.account.del(account);
            self.store
                .cache
                .account_count
                .fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub fn prune_batch(&self, targets: &mut VecDeque<BlockHash>, batch_size: usize) -> usize {
        let mut transaction_write_count = 0;
        if !targets.is_empty() {
            while !targets.is_empty() && transaction_write_count < batch_size {
                let pruning_hash = targets.front().unwrap();
                let account_pruned_count = self.pruning_action(pruning_hash, batch_size as u64);
                transaction_write_count += account_pruned_count as usize;
                targets.pop_front();
            }
        }
        transaction_write_count
    }

    pub fn prune_one(&self, target: &BlockHash, batch_size: usize) -> usize {
        self.pruning_action(target, batch_size as u64) as usize
    }

    fn pruning_action(&self, hash: &BlockHash, _batch_size: u64) -> u64 {
        self.stats.inc(StatType::Pruning, DetailType::PruningTarget);
        let mut pruned_count = 0;
        let mut hash = *hash;
        let genesis_hash = self.constants.genesis_block.hash();
        let any = OwningAnySet::new(&self.store, &self.constants);

        while !hash.is_zero() && hash != genesis_hash {
            if let Some(block) = any.get_block(&hash) {
                self.store.block.del(&hash);
                self.store.pruned.put(&hash);
                hash = block.previous();
                pruned_count += 1;
                // `store.block.del` decrements `cache.block_count`; restore it because in
                // RSNano `cache.block_count` is total blocks (`unpruned + pruned`), while
                // `store.block.count()` is `cache.block_count - cache.pruned_count`.
                self.store.cache.block_count.fetch_add(1, Ordering::SeqCst);
                self.store.cache.pruned_count.fetch_add(1, Ordering::SeqCst);
                if pruned_count % 256 == 0 {
                    kernel_common::watchdog::kick();
                }
            } else if self.store.pruned.exists(&hash) {
                hash = BlockHash::zero();
            } else {
                break;
            }
        }

        self.stats
            .add(StatType::Pruning, DetailType::PrunedCount, pruned_count);

        pruned_count
    }

    pub fn roll_back(&self, block: &BlockHash) -> Result<usize, RollbackError> {
        let result = self.roll_back_batch(&[*block], usize::MAX, |_| true);
        let rolled_back = result.0[0].rolled_back.len();
        result.0[0]
            .error
            .as_ref()
            .map_or(Ok(rolled_back), |e| Err(e.clone()))
    }

    pub fn roll_back_batch<'a, T, F>(
        &self,
        targets: T,
        max_rollbacks: usize,
        mut can_roll_back: F,
    ) -> RollbackResults
    where
        T: IntoIterator<Item = &'a BlockHash>,
        F: FnMut(&BlockHash) -> bool,
    {
        self.stats
            .inc(StatType::BoundedBacklog, DetailType::PerformingRollbacks);

        let mut rolled_back_count = 0;
        let mut results = RollbackResults(Vec::new());

        for hash in targets {
            if !can_roll_back(hash) {
                results.push(RollbackResult {
                    target_hash: *hash,
                    target_root: QualifiedRoot::ZERO,
                    rolled_back: Vec::new(),
                    error: Some(RollbackError::Rejected),
                });
                continue;
            }

            if let Some(block) = self.store.block.get(hash) {
                let (rollback_list, error) = self.roll_back_with_tx(&block.hash());
                rolled_back_count += rollback_list.len();
                results.push(RollbackResult {
                    target_hash: *hash,
                    target_root: block.qualified_root(),
                    rolled_back: rollback_list,
                    error,
                });

                if rolled_back_count >= max_rollbacks {
                    break;
                }
            } else {
                results.push(RollbackResult {
                    target_hash: *hash,
                    target_root: QualifiedRoot::ZERO,
                    rolled_back: Vec::new(),
                    error: Some(RollbackError::BlockNotFound),
                });
            }
        }

        results
    }

    fn roll_back_with_tx(&self, block: &BlockHash) -> (Vec<SavedBlock>, Option<RollbackError>) {
        let mut tx = ();
        let mut performer = BlockRollbackPerformer::new(self, &mut tx);
        match performer.roll_back(block) {
            Ok(()) => (performer.rolled_back, None),
            Err(e) => (performer.rolled_back, Some(e)),
        }
    }

    pub fn process_one(&self, block: &Block) -> Result<SavedBlock, BlockError> {
        let mut result = self.process_batch(core::iter::once(block));
        let mut drain = result.processed.drain(..);
        match drain.next().unwrap() {
            (Ok(_), Some(block)) => Ok(block),
            (Ok(_), None) => unreachable!(),
            (Err(e), _) => Err(e),
        }
    }

    pub fn process_batch<'a>(
        &self,
        batch: impl IntoIterator<Item = &'a Block>,
    ) -> BatchProcessResult {
        self.process_batch_with_verifications(batch.into_iter().map(|b| (b, false)))
    }

    pub fn process_batch_with_verifications<'a>(
        &self,
        batch: impl IntoIterator<Item = (&'a Block, bool)>,
    ) -> BatchProcessResult {
        self.process_batch_with_lazy_verifications(batch, |_, verified| (verified, 0))
    }

    pub fn process_batch_with_lazy_verifications<'a, F>(
        &self,
        batch: impl IntoIterator<Item = (&'a Block, bool)>,
        mut resolve_verified: F,
    ) -> BatchProcessResult
    where
        F: FnMut(usize, bool) -> (bool, u64),
    {
        let items: Vec<(&'a Block, bool)> = batch.into_iter().collect();
        let len = items.len();
        let mut processed = Vec::with_capacity(len);
        let mut scratch_lock = BATCH_SCRATCH.try_lock();
        let mut fallback_scratch;
        let scratch: &mut BatchScratchArena = if let Some(ref mut opt) = scratch_lock {
            let box_ref = opt.get_or_insert_with(|| alloc::boxed::Box::new(BatchScratchArena::new(2048)));
            box_ref.reset();
            &mut **box_ref
        } else {
            fallback_scratch = BatchScratchArena::new(len);
            &mut fallback_scratch
        };
        let BatchScratchArena {
            ref mut failed_hashes,
            ref mut deferred_blocks,
            ref mut external_successors,
            ref mut deferred_accounts,
            ref mut deferred_pending,
            ref mut old_tips,
            ref mut seen_accounts,
            ref mut seen_pending,
            ref mut produced_in_batch,
            ref mut raw_hashes,
            ref mut hash_queries,
            ref mut hash_results,
            ref mut acct_queries,
            ref mut acct_results,
            ref mut pend_queries,
            ref mut pend_results,
            ref mut sorted_pending,
            ref mut rep_deltas,
        } = *scratch;
        let batch_now = rsnano_core::utils::UnixMillisTimestamp::now();
        let mut any_existing_hash = false;

        if len >= 4 {
            let pf_t0 = kernel_common::clint::mtime();

            let has_pruned = self.store.pruned.has_pruned();
            for (idx, &(block, _)) in items.iter().enumerate() {
                if !cfg!(test) && idx + 12 < len {
                    let ahead = items[idx + 12].0;
                    self.store.block.prefetch_may_exist(&ahead.hash());
                    let ahead_prev = ahead.previous();
                    if !ahead_prev.is_zero() {
                        self.store.block.prefetch_may_exist(&ahead_prev);
                        self.store.block.prefetch_hot(&ahead_prev);
                    }
                    if let Some(ahead_acc) = ahead.account_field() {
                        if !ahead_acc.is_zero() {
                            self.store.account.prefetch_hot(&ahead_acc);
                            let ahead_src = ahead.source_or_link();
                            if !ahead_src.is_zero() {
                                self.store.block.prefetch_may_exist(&ahead_src);
                                self.store
                                    .pending
                                    .prefetch_hot(&rsnano_core::PendingKey::new(ahead_acc, ahead_src));
                            }
                        }
                    }
                }
                let h = block.hash();
                if cfg!(test) || has_pruned {
                    raw_hashes.push((h, PREFETCH_NEED_EXISTS));
                } else if self.store.block.may_exist(&h) {
                    if self.store.block.is_hot(&h) {
                        any_existing_hash = true;
                    } else {
                        raw_hashes.push((h, PREFETCH_NEED_EXISTS));
                    }
                }

                let prev = block.previous();
                let in_batch_prev_bal = if !prev.is_zero() {
                    produced_in_batch.get(&prev).copied()
                } else {
                    None
                };

                let acc_opt = block.account_field().filter(|a| !a.is_zero());
                let mut prev_matches_hot_head = false;
                let prev_bal_from_acc = if let Some(acc) = acc_opt {
                    if let Some((hot_info, cached_bal)) = seen_accounts.get_mut(&acc) {
                        if !cfg!(test)
                            && in_batch_prev_bal.is_none()
                            && !prev.is_zero()
                            && hot_info.as_ref().is_some_and(|a| a.head == prev)
                        {
                            prev_matches_hot_head = true;
                        }
                        let prev_b = *cached_bal;
                        if let Some(new_b) = block.balance_field() {
                            *cached_bal = Some(new_b);
                        }
                        prev_b
                    } else if cfg!(test) {
                        acct_queries.push(acc);
                        seen_accounts.insert(acc, (None, block.balance_field()));
                        None
                    } else {
                        let hot_acc = self.store.account.get_hot(&acc);
                        if hot_acc.is_none() && !self.store.account.is_hot_or_absent(&acc) {
                            acct_queries.push(acc);
                        }
                        if in_batch_prev_bal.is_none()
                            && !prev.is_zero()
                            && hot_acc.as_ref().is_some_and(|a| a.head == prev)
                        {
                            prev_matches_hot_head = true;
                        }
                        let prev_b = hot_acc.as_ref().map(|a| a.balance);
                        seen_accounts.insert(acc, (hot_acc, block.balance_field().or(prev_b)));
                        prev_b
                    }
                } else {
                    None
                };

                if !prev.is_zero()
                    && in_batch_prev_bal.is_none()
                    && (!matches!(block, Block::State(_)) || !prev_matches_hot_head)
                    && (cfg!(test)
                        || (self.store.block.may_exist(&prev) && !self.store.block.is_hot(&prev)))
                {
                    raw_hashes.push((prev, PREFETCH_NEED_BLOCK));
                }

                let src = block.source_or_link();
                let is_epoch_link = matches!(
                    block,
                    Block::State(s) if self.constants.epochs.is_epoch_link(&s.link())
                );
                if !src.is_zero() && !is_epoch_link && !produced_in_batch.contains_key(&src) {
                    if cfg!(test) {
                        raw_hashes.push((src, PREFETCH_NEED_EXISTS));
                    }
                    let prev_bal_opt = in_batch_prev_bal.flatten().or(prev_bal_from_acc);
                    let known_send = match (block, prev_bal_opt) {
                        (Block::State(s), Some(prev_bal)) => s.balance() < prev_bal,
                        _ => false,
                    };
                    if !known_send
                        && (cfg!(test) || has_pruned || self.store.block.may_exist(&src))
                    {
                        if let Some(acc) = acc_opt {
                            let pk = rsnano_core::PendingKey::new(acc, src);
                            if cfg!(test) {
                                pend_queries.push(pk);
                            } else if let Some(hot_pend) = self.store.pending.get_hot(&pk) {
                                seen_pending.insert(pk, hot_pend);
                            } else {
                                pend_queries.push(pk);
                            }
                        }
                    }
                }
                produced_in_batch.insert(h, block.balance_field());
            }

            let total_cold_queries =
                raw_hashes.len() + acct_queries.len() + pend_queries.len();
            if total_cold_queries > 0 {
                raw_hashes.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                hash_queries.reserve(raw_hashes.len());
                for &(h, f) in raw_hashes.iter() {
                    if let Some(last) = hash_queries.last_mut() {
                        if last.0 == h {
                            last.1 |= f;
                            continue;
                        }
                    }
                    hash_queries.push((h, f));
                }
                acct_queries.sort_unstable();
                acct_queries.dedup();
                pend_queries.sort_unstable();
                pend_queries.dedup();

                hash_results.resize(hash_queries.len(), (false, None));
                acct_results.resize(acct_queries.len(), None);
                pend_results.resize(pend_queries.len(), None);

                let use_parallel_stripes = cfg!(test) || total_cold_queries >= 8;
                let stripe_guard = if use_parallel_stripes {
                    STRIPE_DISPATCH_LOCK.try_lock()
                } else {
                    None
                };
                let b_online = stripe_guard.is_some() && LEDGER_B_ONLINE.load(Ordering::Relaxed);
                let c_online = stripe_guard.is_some()
                    && LEDGER_C_ONLINE.load(Ordering::Relaxed)
                    && !kernel_common::storage::wal::is_wal_bg_pending();

                if b_online && c_online {
                    let h_cut1 =
                        hash_queries.partition_point(|(k, _)| (k.as_bytes()[0] >> 2) < 21);
                    let h_cut2 =
                        hash_queries.partition_point(|(k, _)| (k.as_bytes()[0] >> 2) < 42);
                    let a_cut1 = acct_queries.partition_point(|k| (k.as_bytes()[0] >> 2) < 21);
                    let a_cut2 = acct_queries.partition_point(|k| (k.as_bytes()[0] >> 2) < 42);
                    let p_cut1 = pend_queries
                        .partition_point(|k| (k.receiving_account.as_bytes()[0] >> 2) < 21);
                    let p_cut2 = pend_queries
                        .partition_point(|k| (k.receiving_account.as_bytes()[0] >> 2) < 42);

                    let (hr0, hr_rest) = hash_results.split_at_mut(h_cut1);
                    let (hr1, hr2) = hr_rest.split_at_mut(h_cut2 - h_cut1);
                    let (ar0, ar_rest) = acct_results.split_at_mut(a_cut1);
                    let (ar1, ar2) = ar_rest.split_at_mut(a_cut2 - a_cut1);
                    let (pr0, pr_rest) = pend_results.split_at_mut(p_cut1);
                    let (pr1, pr2) = pr_rest.split_at_mut(p_cut2 - p_cut1);

                    let job_b = LedgerStripeJob::PrefetchReads {
                        store: &self.store as *const BtreeStore,
                        hash_queries: &hash_queries[h_cut1..h_cut2] as *const [(BlockHash, u8)],
                        hash_results: hr1 as *mut [(bool, Option<SavedBlock>)],
                        acct_queries: &acct_queries[a_cut1..a_cut2] as *const [Account],
                        acct_results: ar1 as *mut [Option<AccountInfo>],
                        pend_queries: &pend_queries[p_cut1..p_cut2]
                            as *const [rsnano_core::PendingKey],
                        pend_results: pr1 as *mut [Option<rsnano_core::PendingInfo>],
                    };
                    let job_c = LedgerStripeJob::PrefetchReads {
                        store: &self.store as *const BtreeStore,
                        hash_queries: &hash_queries[h_cut2..] as *const [(BlockHash, u8)],
                        hash_results: hr2 as *mut [(bool, Option<SavedBlock>)],
                        acct_queries: &acct_queries[a_cut2..] as *const [Account],
                        acct_results: ar2 as *mut [Option<AccountInfo>],
                        pend_queries: &pend_queries[p_cut2..] as *const [rsnano_core::PendingKey],
                        pend_results: pr2 as *mut [Option<rsnano_core::PendingInfo>],
                    };
                    let job_b_addr = &job_b as *const LedgerStripeJob as usize;
                    let job_c_addr = &job_c as *const LedgerStripeJob as usize;
                    LEDGER_B_JOB_DONE.store(false, Ordering::Release);
                    LEDGER_C_JOB_DONE.store(false, Ordering::Release);
                    LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);
                    LEDGER_C_JOB_PTR.store(job_c_addr, Ordering::Release);

                    execute_prefetch_slice(
                        &self.store,
                        &hash_queries[..h_cut1],
                        hr0,
                        &acct_queries[..a_cut1],
                        ar0,
                        &pend_queries[..p_cut1],
                        pr0,
                    );

                    let stole_b = LEDGER_B_JOB_PTR
                        .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok();
                    if stole_b {
                        execute_prefetch_slice(
                            &self.store,
                            &hash_queries[h_cut1..h_cut2],
                            hr1,
                            &acct_queries[a_cut1..a_cut2],
                            ar1,
                            &pend_queries[p_cut1..p_cut2],
                            pr1,
                        );
                    }
                    let stole_c = LEDGER_C_JOB_PTR
                        .compare_exchange(job_c_addr, 0, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok();
                    if stole_c {
                        execute_prefetch_slice(
                            &self.store,
                            &hash_queries[h_cut2..],
                            hr2,
                            &acct_queries[a_cut2..],
                            ar2,
                            &pend_queries[p_cut2..],
                            pr2,
                        );
                    }
                    if !stole_b {
                        while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                            core::hint::spin_loop();
                        }
                    }
                    if !stole_c {
                        while !LEDGER_C_JOB_DONE.load(Ordering::Acquire) {
                            core::hint::spin_loop();
                        }
                    }
                } else if b_online {
                    let h_mid =
                        hash_queries.partition_point(|(k, _)| (k.as_bytes()[0] >> 2) < 32);
                    let a_mid = acct_queries.partition_point(|k| (k.as_bytes()[0] >> 2) < 32);
                    let p_mid = pend_queries
                        .partition_point(|k| (k.receiving_account.as_bytes()[0] >> 2) < 32);
                    let (hr0, hr1) = hash_results.split_at_mut(h_mid);
                    let (ar0, ar1) = acct_results.split_at_mut(a_mid);
                    let (pr0, pr1) = pend_results.split_at_mut(p_mid);

                    let job_b = LedgerStripeJob::PrefetchReads {
                        store: &self.store as *const BtreeStore,
                        hash_queries: &hash_queries[h_mid..] as *const [(BlockHash, u8)],
                        hash_results: hr1 as *mut [(bool, Option<SavedBlock>)],
                        acct_queries: &acct_queries[a_mid..] as *const [Account],
                        acct_results: ar1 as *mut [Option<AccountInfo>],
                        pend_queries: &pend_queries[p_mid..] as *const [rsnano_core::PendingKey],
                        pend_results: pr1 as *mut [Option<rsnano_core::PendingInfo>],
                    };
                    let job_b_addr = &job_b as *const LedgerStripeJob as usize;
                    LEDGER_B_JOB_DONE.store(false, Ordering::Release);
                    LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);

                    execute_prefetch_slice(
                        &self.store,
                        &hash_queries[..h_mid],
                        hr0,
                        &acct_queries[..a_mid],
                        ar0,
                        &pend_queries[..p_mid],
                        pr0,
                    );

                    if LEDGER_B_JOB_PTR
                        .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        execute_prefetch_slice(
                            &self.store,
                            &hash_queries[h_mid..],
                            hr1,
                            &acct_queries[a_mid..],
                            ar1,
                            &pend_queries[p_mid..],
                            pr1,
                        );
                    } else {
                        while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                            core::hint::spin_loop();
                        }
                    }
                } else {
                    execute_prefetch_slice(
                        &self.store,
                        hash_queries,
                        hash_results,
                        acct_queries,
                        acct_results,
                        pend_queries,
                        pend_results,
                    );
                }
            }
            LEDGER_PROF[0].fetch_add(
                kernel_common::clint::mtime() - pf_t0,
                core::sync::atomic::Ordering::Relaxed,
            );
        }

        let mut sum_prof_create = 0u64;
        let mut sum_prof_validate = 0u64;
        for (idx, &(block, signature_verified)) in items.iter().enumerate() {
            if any_existing_hash && idx + 8 < len {
                let (ahead, _) = items[idx + 8];
                self.store.block.prefetch_may_exist(&ahead.hash());
            }
            let hash = block.hash();
            let prev_hash = block.previous();

            if !prev_hash.is_zero() && failed_hashes.contains_key(&prev_hash) {
                failed_hashes.insert(hash, ());
                processed.push((Err(BlockError::GapPrevious), None));
                continue;
            }

            let tp0 = kernel_common::clint::mtime();
            let direct_acc = block.account_field().filter(|a| !a.is_zero());
            let in_batch_account = direct_acc
                .and_then(|acc| deferred_accounts.get(&acc).map(|(_, latest)| latest.clone()));
            let disk_account = if in_batch_account.is_none() && len >= 4 {
                direct_acc.and_then(|acc| {
                    if !acct_queries.is_empty() {
                        if let Ok(i) = acct_queries.binary_search(&acc) {
                            return Some(acct_results[i].clone());
                        }
                    }
                    if !cfg!(test) {
                        seen_accounts
                            .get(&acc)
                            .map(|(hot_info, _)| hot_info.clone())
                    } else {
                        None
                    }
                })
            } else {
                None
            };

            let known_account_head_matches_prev = !cfg!(test)
                && matches!(block, Block::State(_))
                && !prev_hash.is_zero()
                && (in_batch_account
                    .as_ref()
                    .is_some_and(|info| info.head == prev_hash)
                    || disk_account
                        .as_ref()
                        .and_then(|opt| opt.as_ref())
                        .is_some_and(|info| info.head == prev_hash));

            // Fast-path: if the exact predecessor block is absent from both deferred_blocks
            // and the block Cuckoo filter, reject immediately as GapPrevious.
            if !prev_hash.is_zero()
                && !cfg!(test)
                && !known_account_head_matches_prev
                && !deferred_blocks.contains_key(&prev_hash)
                && !self.store.block.may_exist(&prev_hash)
            {
                failed_hashes.insert(hash, ());
                processed.push((Err(BlockError::GapPrevious), None));
                continue;
            }

            tracing::debug!("Ledger: validating block {}", hash);

            let in_batch_exists = deferred_blocks.contains_key(&hash);
            let in_batch_prev = if !prev_hash.is_zero() && !known_account_head_matches_prev {
                deferred_blocks.get_block(&prev_hash).cloned()
            } else {
                None
            };
            let candidate_account = direct_acc
                .or_else(|| in_batch_prev.as_ref().map(|p| p.account()));
            let in_batch_account = if direct_acc.is_some() {
                in_batch_account
            } else {
                candidate_account
                    .and_then(|acc| deferred_accounts.get(&acc).map(|(_, latest)| latest.clone()))
            };
            let source_hash = block.source_or_link();
            let in_batch_source_exists =
                !source_hash.is_zero() && deferred_blocks.contains_key(&source_hash);

            // If this is an epoch open block (or test mode) that queries `receivable_exists`,
            // flush any deferred pending entries first so B-Tree scan sees them.
            let mut flushed_deferred_pending = false;
            if !deferred_pending.is_empty()
                && (cfg!(test)
                    || (block.is_open()
                        && self.constants.epochs.is_epoch_link(&source_hash.into())))
            {
                for (key, op) in deferred_pending.entries() {
                    match op {
                        DeferredPendingOp::Insert(info) => self.store.pending.put(key, info),
                        DeferredPendingOp::DeleteOnDisk => self.store.pending.del(key),
                        DeferredPendingOp::CancelledInBatch => {}
                    }
                }
                deferred_pending.clear();
                flushed_deferred_pending = true;
            }

            let in_batch_pending = match (candidate_account, source_hash.is_zero()) {
                (Some(acc), false) => deferred_pending
                    .get(&rsnano_core::PendingKey::new(acc, source_hash))
                    .map(|op| match op {
                        DeferredPendingOp::Insert(info) => Some(info.clone()),
                        DeferredPendingOp::DeleteOnDisk | DeferredPendingOp::CancelledInBatch => {
                            None
                        }
                    }),
                _ => None,
            };

            let prefetched = if len >= 4 {
                let has_pruned = self.store.pruned.has_pruned();
                let disk_block_exists = if !in_batch_exists {
                    if !cfg!(test) && !has_pruned {
                        if !any_existing_hash && hash_queries.is_empty() {
                            Some(false)
                        } else if !self.store.block.may_exist(&hash) {
                            Some(false)
                        } else if hash_queries.is_empty() {
                            None
                        } else {
                            hash_queries
                                .binary_search_by(|(k, _)| k.cmp(&hash))
                                .ok()
                                .map(|i| hash_results[i].0)
                        }
                    } else if hash_queries.is_empty() {
                        None
                    } else {
                        hash_queries
                            .binary_search_by(|(k, _)| k.cmp(&hash))
                            .ok()
                            .map(|i| hash_results[i].0)
                    }
                } else {
                    None
                };
                let disk_prev_block = if !prev_hash.is_zero()
                    && !known_account_head_matches_prev
                    && in_batch_prev.is_none()
                    && !hash_queries.is_empty()
                {
                    hash_queries
                        .binary_search_by(|(k, _)| k.cmp(&prev_hash))
                        .ok()
                        .and_then(|i| {
                            if (hash_queries[i].1 & PREFETCH_NEED_BLOCK) != 0 {
                                Some(hash_results[i].1.clone())
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };
                let disk_pending = if !flushed_deferred_pending
                    && in_batch_pending.is_none()
                    && !source_hash.is_zero()
                {
                    direct_acc.and_then(|acc| {
                        let pk = rsnano_core::PendingKey::new(acc, source_hash);
                        if !cfg!(test) {
                            if let Some(hot_pend) = seen_pending.get(&pk) {
                                return Some(Some(hot_pend.clone()));
                            }
                        }
                        if !pend_queries.is_empty() {
                            pend_queries
                                .binary_search(&pk)
                                .ok()
                                .map(|i| pend_results[i].clone())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                let disk_source_exists = if !source_hash.is_zero() && !in_batch_source_exists {
                    if !cfg!(test) && !has_pruned && !self.store.block.may_exist(&source_hash) {
                        Some(false)
                    } else if hash_queries.is_empty() {
                        None
                    } else {
                        hash_queries
                            .binary_search_by(|(k, _)| k.cmp(&source_hash))
                            .ok()
                            .map(|i| hash_results[i].0)
                    }
                } else {
                    None
                };
                PrefetchedBlockData {
                    disk_block_exists,
                    disk_prev_block,
                    disk_account,
                    disk_pending,
                    disk_source_exists,
                }
            } else {
                PrefetchedBlockData::default()
            };

            let tp1 = kernel_common::clint::mtime();
            let (signature_verified, waited_ticks) = resolve_verified(idx, signature_verified);
            let any = OwningAnySet::new(&self.store, &self.constants);
            let validator = BlockValidatorFactory::new(&any, &self.constants, block)
                .create_validator_with_batch_cache(
                    signature_verified,
                    in_batch_exists,
                    in_batch_prev,
                    in_batch_account,
                    in_batch_source_exists,
                    in_batch_pending,
                    prefetched,
                    batch_now,
                );
            let result = validator.validate();
            let tp2 = kernel_common::clint::mtime();
            sum_prof_create += tp1 - tp0;
            sum_prof_validate += (tp2 - tp1).saturating_sub(waited_ticks);

            match result {
                Ok(instructions) => {
                    let inserted = BlockInserter::new(self, &mut (), block, &instructions)
                        .insert_with_options(true);
                    if let Some(saved_block) = inserted {
                        if !prev_hash.is_zero() {
                            if !deferred_blocks.set_successor(&prev_hash, hash) {
                                external_successors.insert(prev_hash, hash);
                            }
                        }
                        if let Some(del_key) = &instructions.delete_pending {
                            let next_op = match deferred_pending.get(del_key) {
                                Some(DeferredPendingOp::Insert(_)) => {
                                    DeferredPendingOp::CancelledInBatch
                                }
                                _ => DeferredPendingOp::DeleteOnDisk,
                            };
                            deferred_pending.insert(del_key.clone(), next_op);
                        }
                        if let Some((ins_key, ins_info)) = &instructions.insert_pending {
                            deferred_pending.insert(
                                ins_key.clone(),
                                DeferredPendingOp::Insert(ins_info.clone()),
                            );
                        }
                        if let Some((_, latest)) = deferred_accounts.get_mut(&instructions.account)
                        {
                            *latest = instructions.set_account_info.clone();
                        } else {
                            let stored_old = instructions.old_account_info.clone();
                            deferred_accounts.insert(
                                instructions.account,
                                (stored_old, instructions.set_account_info.clone()),
                            );
                        }
                        deferred_blocks.insert(hash, saved_block.clone());
                        processed.push((Ok(()), Some(saved_block)));
                    } else {
                        failed_hashes.insert(hash, ());
                        tracing::debug!("Ledger: conflict for block {}", block.hash());
                        processed.push((Err(BlockError::Conflict), None));
                    }
                }
                Err(err) => {
                    if err != BlockError::Old {
                        failed_hashes.insert(hash, ());
                    } else if !cfg!(test) {
                        if let Some(acc) = candidate_account {
                            old_tips.insert(acc, hash);
                            if !prev_hash.is_zero() {
                                let _ = deferred_blocks.set_successor(&prev_hash, hash);
                            }
                        }
                    }
                    tracing::debug!(
                        "Ledger: validation failed for block {}! Error: {:?}",
                        hash,
                        err
                    );
                    processed.push((Err(err), None));
                }
            }
        }
        LEDGER_PROF[0].fetch_add(sum_prof_create, core::sync::atomic::Ordering::Relaxed);
        LEDGER_PROF[1].fetch_add(sum_prof_validate, core::sync::atomic::Ordering::Relaxed);

        // Flush coalesced, key-sorted B-Tree writes across Shards 0..21 (Hart 6: Ledger-A),
        // Shards 21..42 (Hart 5: Ledger-B), and Shards 42..64 (Hart 7: Ledger-C) in parallel.
        if !deferred_blocks.is_empty() {
            let tp3 = kernel_common::clint::mtime();
            let sorted_blocks = deferred_blocks.entries_mut();
            sorted_blocks.sort_unstable_by(|a, b| a.0.cmp(&b.0));

            #[cfg(not(test))]
            for (account, (first_old_info, final_new_info)) in deferred_accounts.entries_mut() {
                let disk_info = if !acct_queries.is_empty() {
                    acct_queries
                        .binary_search(account)
                        .ok()
                        .and_then(|i| acct_results[i].clone())
                        .or_else(|| {
                            seen_accounts
                                .get(account)
                                .and_then(|(hot_info, _)| hot_info.clone())
                        })
                        .or_else(|| self.store.account.get_hot(account))
                } else {
                    seen_accounts
                        .get(account)
                        .and_then(|(hot_info, _)| hot_info.clone())
                        .or_else(|| self.store.account.get_hot(account))
                };
                let best_old = match disk_info.as_ref() {
                    Some(d) if d.block_count > first_old_info.block_count => d.clone(),
                    _ => first_old_info.clone(),
                };
                if !best_old.head.is_zero()
                    && best_old.block_count > final_new_info.block_count
                    && (sorted_blocks
                        .binary_search_by(|e| e.0.cmp(&best_old.head))
                        .is_ok()
                        || self.store.block.exists(&best_old.head))
                {
                    *first_old_info = best_old.clone();
                    *final_new_info = best_old;
                }
            }

            let sorted_ext_succ = external_successors.entries_mut();
            sorted_ext_succ.sort_unstable_by(|a, b| a.0.cmp(&b.0));

            for (k, op) in deferred_pending.entries() {
                match op {
                    DeferredPendingOp::Insert(info) => {
                        sorted_pending.push((k.clone(), Some(info.clone())));
                    }
                    DeferredPendingOp::DeleteOnDisk => {
                        sorted_pending.push((k.clone(), None));
                    }
                    DeferredPendingOp::CancelledInBatch => {}
                }
            }
            sorted_pending.sort_unstable_by(|a, b| a.0.cmp(&b.0));

            let sorted_accounts = deferred_accounts.entries_mut();
            sorted_accounts.sort_unstable_by(|a, b| a.0.cmp(&b.0));

            let cut1 = sorted_blocks.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 21);
            let cut2 = sorted_blocks.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 42);
            let s_cut1 = sorted_ext_succ.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 21);
            let s_cut2 = sorted_ext_succ.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 42);
            let p_cut1 = sorted_pending
                .partition_point(|e| (e.0.receiving_account.as_bytes()[0] >> 2) < 21);
            let p_cut2 = sorted_pending
                .partition_point(|e| (e.0.receiving_account.as_bytes()[0] >> 2) < 42);
            let a_cut1 = sorted_accounts.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 21);
            let a_cut2 = sorted_accounts.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 42);

            let stripe_guard = STRIPE_DISPATCH_LOCK.try_lock();
            let b_online = stripe_guard.is_some() && LEDGER_B_ONLINE.load(Ordering::Relaxed);
            let c_online = stripe_guard.is_some()
                && LEDGER_C_ONLINE.load(Ordering::Relaxed)
                && !kernel_common::storage::wal::is_wal_bg_pending();

            if b_online && c_online {
                let job_b = LedgerStripeJob::FlushWrites {
                    store: &self.store as *const BtreeStore,
                    ext_succ_slice: &sorted_ext_succ[s_cut1..s_cut2]
                        as *const [(BlockHash, BlockHash)],
                    pending_slice: &sorted_pending[p_cut1..p_cut2]
                        as *const [(rsnano_core::PendingKey, Option<rsnano_core::PendingInfo>)],
                    blocks_slice: &sorted_blocks[cut1..cut2]
                        as *const [(BlockHash, SavedBlock, BlockHash)],
                    accounts_slice: &sorted_accounts[a_cut1..a_cut2]
                        as *const [(Account, (AccountInfo, AccountInfo))],
                };
                let job_c = LedgerStripeJob::FlushWrites {
                    store: &self.store as *const BtreeStore,
                    ext_succ_slice: &sorted_ext_succ[s_cut2..]
                        as *const [(BlockHash, BlockHash)],
                    pending_slice: &sorted_pending[p_cut2..]
                        as *const [(rsnano_core::PendingKey, Option<rsnano_core::PendingInfo>)],
                    blocks_slice: &sorted_blocks[cut2..]
                        as *const [(BlockHash, SavedBlock, BlockHash)],
                    accounts_slice: &sorted_accounts[a_cut2..]
                        as *const [(Account, (AccountInfo, AccountInfo))],
                };
                let job_b_addr = &job_b as *const LedgerStripeJob as usize;
                let job_c_addr = &job_c as *const LedgerStripeJob as usize;
                LEDGER_B_JOB_DONE.store(false, Ordering::Release);
                LEDGER_C_JOB_DONE.store(false, Ordering::Release);
                LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);
                LEDGER_C_JOB_PTR.store(job_c_addr, Ordering::Release);

                // Hart 6 (Ledger-A) executes Stripe 0 (Shards 0..21) while Hart 5 (Ledger-B)
                // executes Stripe 1 (Shards 21..42) and Hart 7 (Ledger-C) executes Stripe 2 (Shards 42..64).
                flush_shard_range(
                    &self.store,
                    &sorted_ext_succ[..s_cut1],
                    &sorted_pending[..p_cut1],
                    &sorted_blocks[..cut1],
                    &sorted_accounts[..a_cut1],
                );

                let stole_b = LEDGER_B_JOB_PTR
                    .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok();
                if stole_b {
                    flush_shard_range(
                        &self.store,
                        &sorted_ext_succ[s_cut1..s_cut2],
                        &sorted_pending[p_cut1..p_cut2],
                        &sorted_blocks[cut1..cut2],
                        &sorted_accounts[a_cut1..a_cut2],
                    );
                }

                let stole_c = LEDGER_C_JOB_PTR
                    .compare_exchange(job_c_addr, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok();
                if stole_c {
                    flush_shard_range(
                        &self.store,
                        &sorted_ext_succ[s_cut2..],
                        &sorted_pending[p_cut2..],
                        &sorted_blocks[cut2..],
                        &sorted_accounts[a_cut2..],
                    );
                }

                if !stole_b {
                    while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                        core::hint::spin_loop();
                    }
                }
                if !stole_c {
                    while !LEDGER_C_JOB_DONE.load(Ordering::Acquire) {
                        core::hint::spin_loop();
                    }
                }
            } else if b_online {
                let mid = sorted_blocks.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 32);
                let s_mid = sorted_ext_succ.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 32);
                let p_mid = sorted_pending
                    .partition_point(|e| (e.0.receiving_account.as_bytes()[0] >> 2) < 32);
                let a_mid = sorted_accounts.partition_point(|e| (e.0.as_bytes()[0] >> 2) < 32);
                let job_b = LedgerStripeJob::FlushWrites {
                    store: &self.store as *const BtreeStore,
                    ext_succ_slice: &sorted_ext_succ[s_mid..]
                        as *const [(BlockHash, BlockHash)],
                    pending_slice: &sorted_pending[p_mid..]
                        as *const [(rsnano_core::PendingKey, Option<rsnano_core::PendingInfo>)],
                    blocks_slice: &sorted_blocks[mid..]
                        as *const [(BlockHash, SavedBlock, BlockHash)],
                    accounts_slice: &sorted_accounts[a_mid..]
                        as *const [(Account, (AccountInfo, AccountInfo))],
                };
                let job_b_addr = &job_b as *const LedgerStripeJob as usize;
                LEDGER_B_JOB_DONE.store(false, Ordering::Release);
                LEDGER_B_JOB_PTR.store(job_b_addr, Ordering::Release);

                flush_shard_range(
                    &self.store,
                    &sorted_ext_succ[..s_mid],
                    &sorted_pending[..p_mid],
                    &sorted_blocks[..mid],
                    &sorted_accounts[..a_mid],
                );

                if LEDGER_B_JOB_PTR
                    .compare_exchange(job_b_addr, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    flush_shard_range(
                        &self.store,
                        &sorted_ext_succ[s_mid..],
                        &sorted_pending[p_mid..],
                        &sorted_blocks[mid..],
                        &sorted_accounts[a_mid..],
                    );
                } else {
                    while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                        core::hint::spin_loop();
                    }
                }
            } else {
                flush_shard_range(
                    &self.store,
                    sorted_ext_succ,
                    sorted_pending,
                    sorted_blocks,
                    sorted_accounts,
                );
            }

            let mut add_rep_delta = |rep: PublicKey, delta: Amount| {
                if delta.is_zero() {
                    return;
                }
                for (r, d) in rep_deltas.iter_mut() {
                    if *r == rep {
                        *d = d.wrapping_add(delta);
                        return;
                    }
                }
                rep_deltas.push((rep, delta));
            };

            for (_account, (first_old_info, target_info)) in sorted_accounts.iter() {
                if !first_old_info.head.is_zero() {
                    if first_old_info.representative == target_info.representative {
                        if first_old_info.balance != target_info.balance {
                            add_rep_delta(
                                target_info.representative,
                                target_info.balance.wrapping_sub(first_old_info.balance),
                            );
                        }
                    } else {
                        add_rep_delta(
                            first_old_info.representative,
                            Amount::zero().wrapping_sub(first_old_info.balance),
                        );
                        add_rep_delta(target_info.representative, target_info.balance);
                    }
                } else if !target_info.balance.is_zero() {
                    add_rep_delta(target_info.representative, target_info.balance);
                }
            }

            for &(rep, delta) in rep_deltas.iter() {
                if !delta.is_zero() {
                    self.rep_weights_updater.representation_add(&mut (), rep, delta);
                }
            }
            LEDGER_PROF[2].fetch_add(
                kernel_common::clint::mtime() - tp3,
                core::sync::atomic::Ordering::Relaxed,
            );
        }

        #[cfg(not(test))]
        if !old_tips.is_empty() {
            for &(account, old_hash) in old_tips.entries() {
                let current_info = self.store.account.get(&account);
                let needs_heal = match &current_info {
                    None => true,
                    Some(info) => {
                        info.head != old_hash
                            && (!self.store.block.exists(&info.head)
                                || self
                                    .store
                                    .block
                                    .get(&old_hash)
                                    .map(|b| b.height() > info.block_count)
                                    .unwrap_or(false))
                    }
                };
                if needs_heal {
                    self.heal_account_from_block(&account, old_hash, current_info, batch_now);
                }
            }
        }

        BatchProcessResult { processed }
    }

    pub fn heal_account_from_block(
        &self,
        account: &Account,
        start_hash: BlockHash,
        current_info: Option<AccountInfo>,
        now: rsnano_core::utils::UnixMillisTimestamp,
    ) -> Option<AccountInfo> {
        self.heal_account_inner(account, start_hash, current_info, Some(now))
    }

    /// Rebuilds `account`'s `AccountInfo` from local blocks, walking successors from `start_hash`.
    /// `modified = None` uses the tip block's sideband timestamp instead of the current time.
    fn heal_account_inner(
        &self,
        account: &Account,
        start_hash: BlockHash,
        current_info: Option<AccountInfo>,
        modified: Option<rsnano_core::utils::UnixMillisTimestamp>,
    ) -> Option<AccountInfo> {
        let mut tip_hash = start_hash;
        for _ in 0..4096 {
            if let Some(succ) = self.store.successors.get(&tip_hash) {
                if !succ.is_zero() && self.store.block.exists(&succ) {
                    tip_hash = succ;
                    continue;
                }
            }
            break;
        }
        let tip = self.store.block.get(&tip_hash)?;
        if tip.account() != *account {
            return None;
        }
        let open_block = current_info
            .as_ref()
            .map(|i| i.open_block)
            .filter(|h| !h.is_zero() && self.store.block.exists(h))
            .unwrap_or_else(|| {
                let mut curr = tip.clone();
                for _ in 0..256 {
                    if curr.previous().is_zero() || curr.height() <= 1 {
                        return curr.hash();
                    }
                    let Some(prev) = self.store.block.get(&curr.previous()) else {
                        break;
                    };
                    curr = prev;
                }
                curr.hash()
            });
        let rep = tip
            .representative_field()
            .or_else(|| {
                current_info
                    .as_ref()
                    .map(|i| i.representative)
                    .filter(|r| !r.is_zero())
            })
            .unwrap_or_default();
        let healed = AccountInfo {
            head: tip.hash(),
            representative: rep,
            open_block,
            balance: tip.balance(),
            modified: modified.unwrap_or_else(|| tip.timestamp()).into(),
            block_count: tip.height(),
            epoch: tip.epoch(),
        };
        let was_none = current_info.is_none();
        self.store.account.put(account, &healed);
        if was_none {
            self.store.cache.account_count.fetch_add(1, Ordering::SeqCst);
        }
        Some(healed)
    }

    /// Returns `(head, confirmed_frontier)` for `account`, self-healing cross-shard
    /// `TABLE_ACCOUNTS` / `TABLE_CONFIRMATION_HEIGHT` vs `TABLE_BLOCKS` desynchronization
    /// if the recorded frontier block is missing from `TABLE_BLOCKS` or behind stored successors.
    pub fn get_healed_account_frontiers(&self, account: &Account) -> (BlockHash, BlockHash) {
        let account_info = self.store.account.get(account);
        let mut head = account_info.as_ref().map(|i| i.head).unwrap_or_default();
        let conf_info = self.store.confirmation_height.get(account);
        let mut confirmed_frontier = conf_info.as_ref().map(|c| c.frontier).unwrap_or_default();

        #[cfg(not(test))]
        {
            let old_conf_height = conf_info.as_ref().map(|c| c.height).unwrap_or(0);
            let adjust_confirmed_count = |old_h: u64, new_h: u64| {
                if new_h > old_h {
                    self.store
                        .cache
                        .confirmed_count
                        .fetch_add(new_h - old_h, Ordering::SeqCst);
                } else if old_h > new_h {
                    let diff = old_h - new_h;
                    let _ = self.store.cache.confirmed_count.fetch_update(
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                        |c| Some(c.saturating_sub(diff)),
                    );
                }
            };

            let mut conf_missing = false;
            if !confirmed_frontier.is_zero() && !self.store.block.exists(&confirmed_frontier) {
                confirmed_frontier = BlockHash::zero();
                conf_missing = true;
            } else if conf_info.is_none()
                && account_info
                    .as_ref()
                    .map(|i| {
                        let m = i.modified.as_u64();
                        m >= 1_000_000_000 && m < 1_760_000_000
                    })
                    .unwrap_or(false)
            {
                // Pre-snapshot account (Unix epoch timestamp from the copied LMDB) whose
                // `TABLE_CONFIRMATION_HEIGHT` row was lost during the LMDB copy.
                conf_missing = true;
            }

            if !head.is_zero() {
                let head_exists = self.store.block.exists(&head);
                if head_exists && !conf_missing {
                    let has_valid_succ = self
                        .store
                        .successors
                        .get(&head)
                        .filter(|s| !s.is_zero() && *s != head)
                        .map(|s| self.store.block.exists(&s))
                        .unwrap_or(false);
                    if !has_valid_succ {
                        return (head, confirmed_frontier);
                    }
                }
                let mut start = if head_exists {
                    head
                } else if !confirmed_frontier.is_zero() {
                    confirmed_frontier
                } else {
                    account_info
                        .as_ref()
                        .map(|i| i.open_block)
                        .filter(|h| !h.is_zero() && self.store.block.exists(h))
                        .unwrap_or_default()
                };

                if !start.is_zero() {
                    for _ in 0..4096 {
                        if let Some(succ) = self.store.successors.get(&start) {
                            if !succ.is_zero() {
                                if self.store.block.exists(&succ) {
                                    start = succ;
                                    continue;
                                } else {
                                    self.store.successors.del(&start);
                                }
                            }
                        }
                        break;
                    }
                }

                if start != head {
                    if start.is_zero() {
                        self.store.account.del(account);
                        self.store.cache.account_count.fetch_sub(1, Ordering::SeqCst);
                        if conf_info.is_some() {
                            self.store.confirmation_height.del(account);
                            adjust_confirmed_count(old_conf_height, 0);
                        }
                    } else if let Some(healed) = self.heal_account_from_block(
                        account,
                        start,
                        account_info,
                        rsnano_core::utils::UnixMillisTimestamp::now(),
                    ) {
                        let chain_ended = self
                            .store
                            .successors
                            .get(&healed.head)
                            .filter(|s| !s.is_zero() && *s != healed.head)
                            .map(|s| !self.store.block.exists(&s))
                            .unwrap_or(true);
                        if conf_missing && (chain_ended || healed.block_count >= old_conf_height) {
                            self.store.confirmation_height.put(
                                account,
                                &rsnano_core::ConfirmationHeightInfo::new(
                                    healed.block_count,
                                    healed.head,
                                ),
                            );
                            adjust_confirmed_count(old_conf_height, healed.block_count);
                            confirmed_frontier = healed.head;
                        }
                    }
                    head = start;
                } else if conf_missing {
                    if let Some(tip) = self.store.block.get(&head) {
                        self.store.confirmation_height.put(
                            account,
                            &rsnano_core::ConfirmationHeightInfo::new(tip.height(), tip.hash()),
                        );
                        adjust_confirmed_count(old_conf_height, tip.height());
                        confirmed_frontier = tip.hash();
                    } else {
                        self.store.confirmation_height.del(account);
                        adjust_confirmed_count(old_conf_height, 0);
                    }
                }
            } else if conf_missing {
                self.store.confirmation_height.del(account);
                adjust_confirmed_count(old_conf_height, 0);
            }
        }

        (head, confirmed_frontier)
    }

    /// Drives one chunk of the ledger recount. In repair mode, rebuilds the account records of
    /// orphan confirmation-height rows from local blocks (no network data, no consensus change),
    /// and once the pass completes sets the cached counters from the recounted totals.
    pub fn recount_step(
        &self,
        rc: &mut crate::LedgerRecount,
        budget: usize,
        now_tick: u64,
    ) -> bool {
        let more = rc.step(&self.store, budget, now_tick);
        if rc.repair && !rc.repair_queue.is_empty() {
            let mut queue = core::mem::take(&mut rc.repair_queue);
            for &(account, height, frontier) in queue.iter() {
                if self.store.account.get(&account).is_some() {
                    continue; // healed concurrently by another path
                }
                if frontier.is_zero() || !self.store.block.exists(&frontier) {
                    // Both the `TABLE_ACCOUNTS` row and the `frontier` block are missing from disk.
                    // Remove the dangling confirmation-height entry and enqueue the account for bootstrap pull.
                    self.store.confirmation_height.del(&account);
                    rc.removed_orphan_conf_entries += 1;
                    rc.removed_orphan_conf_height += height;
                    rc.repair_failed += 1;
                    rc.bootstrap_accounts.push(account);
                    rc.bootstrap_seeded += 1;
                    continue;
                }
                match self.heal_account_inner(&account, frontier, None, None) {
                    Some(info) => {
                        if info.block_count > height {
                            self.store.confirmation_height.put(
                                &account,
                                &rsnano_core::ConfirmationHeightInfo::new(info.block_count, info.head),
                            );
                            rc.repaired_missing_conf_blocks += info.block_count - height;
                        }
                        rc.repaired_accounts += 1;
                        rc.repaired_blocks += info.block_count;
                    }
                    None => {
                        self.store.confirmation_height.del(&account);
                        rc.removed_orphan_conf_entries += 1;
                        rc.removed_orphan_conf_height += height;
                        rc.repair_failed += 1;
                        rc.bootstrap_accounts.push(account);
                        rc.bootstrap_seeded += 1;
                    }
                }
            }
            queue.clear();
            rc.repair_queue = queue;
        }
        if rc.repair && !rc.conf_over_queue.is_empty() {
            let mut co_queue = core::mem::take(&mut rc.conf_over_queue);
            for &(account, block_count, head, conf_height, frontier) in co_queue.iter() {
                if !frontier.is_zero() && self.store.block.exists(&frontier) {
                    let cur_info = self.store.account.get(&account);
                    if let Some(info) = self.heal_account_inner(&account, frontier, cur_info, None) {
                        rc.repaired_accounts += 1;
                        rc.repaired_blocks += info.block_count.saturating_sub(block_count);
                        if info.block_count > conf_height {
                            self.store.confirmation_height.put(
                                &account,
                                &rsnano_core::ConfirmationHeightInfo::new(info.block_count, info.head),
                            );
                            rc.repaired_missing_conf_blocks += info.block_count - conf_height;
                        }
                    }
                } else if block_count > 0 && !head.is_zero() && self.store.block.exists(&head) {
                    self.store.confirmation_height.put(
                        &account,
                        &rsnano_core::ConfirmationHeightInfo::new(block_count, head),
                    );
                    rc.removed_orphan_conf_height += conf_height.saturating_sub(block_count);
                    rc.bootstrap_accounts.push(account);
                    rc.bootstrap_seeded += 1;
                } else {
                    let _ = self.get_healed_account_frontiers(&account);
                    let new_h = self
                        .store
                        .confirmation_height
                        .get(&account)
                        .map(|c| c.height)
                        .unwrap_or(0);
                    rc.removed_orphan_conf_height += conf_height.saturating_sub(new_h);
                    rc.repair_failed += 1;
                    rc.bootstrap_accounts.push(account);
                    rc.bootstrap_seeded += 1;
                }
            }
            co_queue.clear();
            rc.conf_over_queue = co_queue;
        }
        if rc.repair && !rc.missing_conf_queue.is_empty() {
            let mut mqueue = core::mem::take(&mut rc.missing_conf_queue);
            for &(account, block_count, head, old_conf_height) in mqueue.iter() {
                let cur_h = self
                    .store
                    .confirmation_height
                    .get(&account)
                    .map(|c| c.height)
                    .unwrap_or(0);
                if cur_h >= block_count {
                    continue;
                }
                if block_count > 0 && self.store.block.exists(&head) {
                    self.store.confirmation_height.put(
                        &account,
                        &rsnano_core::ConfirmationHeightInfo::new(block_count, head),
                    );
                    rc.repaired_missing_conf_accounts += 1;
                    rc.repaired_missing_conf_blocks +=
                        block_count.saturating_sub(old_conf_height.max(cur_h));
                } else {
                    let _ = self.get_healed_account_frontiers(&account);
                    rc.repair_failed += 1;
                    rc.bootstrap_accounts.push(account);
                    rc.bootstrap_seeded += 1;
                }
            }
            mqueue.clear();
            rc.missing_conf_queue = mqueue;
        }
        if rc.done && rc.repair && !rc.counters_applied {
            let cache = &self.store.cache;
            let drift = |now: u64, start: u64| now as i128 - start as i128;
            let clamp = |v: i128| if v < 0 { 0 } else { v as u64 };
            let now_b = cache.block_count.load(Ordering::SeqCst);
            let now_c = cache.confirmed_count.load(Ordering::SeqCst);
            let now_a = cache.account_count.load(Ordering::SeqCst);
            // Blocks / confirmations inserted while the scan ran are added back as drift
            // (bounded by the few blocks inserted during the pass).
            let blocks = clamp(
                (rc.sum_block_count + rc.repaired_blocks) as i128
                    + drift(now_b, rc.start_block_count).max(0),
            );
            let confirmed = clamp(
                (rc.sum_conf_height + rc.repaired_missing_conf_blocks) as i128
                    - rc.removed_orphan_conf_height as i128
                    + drift(now_c, rc.start_confirmed_count).max(0),
            );
            // `heal_account_inner` already incremented `account_count` for repaired accounts.
            let accounts = clamp(rc.accounts as i128 + drift(now_a, rc.start_account_count).max(0));
            cache.block_count.store(blocks, Ordering::SeqCst);
            cache.confirmed_count.store(confirmed, Ordering::SeqCst);
            cache.account_count.store(accounts, Ordering::SeqCst);
            rc.counters_applied = true;
            rc.applied_block_count = blocks;
            rc.applied_confirmed_count = confirmed;
            rc.applied_account_count = accounts;
            tracing::info!(
                "[recount] repair applied: repaired_accounts={} repaired_blocks={} repaired_conf_accounts={} repaired_conf_blocks={} removed_orphans={} seeded_bootstrap={} failed={} | block_count {} -> {} | confirmed_count {} -> {} | account_count {} -> {}",
                rc.repaired_accounts,
                rc.repaired_blocks,
                rc.repaired_missing_conf_accounts,
                rc.repaired_missing_conf_blocks,
                rc.removed_orphan_conf_entries,
                rc.bootstrap_seeded,
                rc.repair_failed,
                now_b,
                blocks,
                now_c,
                confirmed,
                now_a,
                accounts
            );
        }
        more
    }

    pub fn advance_existing_chain(&self, start: BlockHash, max_steps: usize) -> BlockHash {
        if start.is_zero() || !self.store.block.exists(&start) {
            return BlockHash::zero();
        }
        let mut curr = start;
        for _ in 0..max_steps {
            if let Some(succ) = self.store.successors.get(&curr) {
                if !succ.is_zero() && succ != curr && self.store.block.exists(&succ) {
                    curr = succ;
                    continue;
                }
            }
            break;
        }
        curr
    }

    fn is_chain_break(&self, hash: &BlockHash) -> bool {
        if hash.is_zero() {
            return false;
        }
        match self.store.successors.get(hash) {
            Some(succ) if !succ.is_zero() && succ != *hash => !self.store.block.exists(&succ),
            _ => true,
        }
    }

    pub fn find_forward_gap_start(&self, account: &Account, exclude_head: bool) -> BlockHash {
        let (head, confirmed_frontier) = self.get_healed_account_frontiers(account);
        if !confirmed_frontier.is_zero() && confirmed_frontier != head {
            let walked = self.advance_existing_chain(confirmed_frontier, 16384);
            if !walked.is_zero()
                && walked != head
                && (!exclude_head || self.is_chain_break(&walked))
            {
                return walked;
            }
        }
        if !exclude_head && !head.is_zero() {
            return self.advance_existing_chain(head, 4096);
        }
        if !confirmed_frontier.is_zero() && (!exclude_head || confirmed_frontier != head) {
            let walked = self.advance_existing_chain(confirmed_frontier, 4096);
            if !exclude_head || (walked != head && self.is_chain_break(&walked)) {
                return walked;
            }
        }
        let open_block = self
            .store
            .account
            .get(account)
            .map(|i| i.open_block)
            .filter(|h| !h.is_zero() && self.store.block.exists(h))
            .unwrap_or_default();
        if !open_block.is_zero() {
            let walked = self.advance_existing_chain(open_block, 16384);
            if !exclude_head || (walked != head && self.is_chain_break(&walked)) {
                return walked;
            }
        }
        BlockHash::zero()
    }

    pub fn roll_back_competitors<'a, T, F>(&self, blocks: T, mut rolled_back_callback: F)
    where
        T: IntoIterator<Item = &'a Block>,
        F: FnMut(RollbackResults),
    {
        let mut rolled_back = RollbackResults(Vec::new());
        for block in blocks {
            let rolled_back_blocks = self.rollback_competitor(block);
            if !rolled_back_blocks.is_empty() {
                rolled_back.push(RollbackResult {
                    target_hash: block.hash(),
                    target_root: block.qualified_root(),
                    rolled_back: rolled_back_blocks,
                    error: None,
                });
            }
        }
        if !rolled_back.is_empty() {
            rolled_back_callback(rolled_back);
        }
    }

    fn rollback_competitor(&self, fork_block: &Block) -> Vec<SavedBlock> {
        let mut rollback_list = Vec::new();
        let hash = fork_block.hash();
        if let Some(successor) =
            self.block_successor_by_qualified_root(&fork_block.qualified_root())
        {
            if successor != hash {
                let (list, error) = self.roll_back_with_tx(&successor);
                rollback_list = list;
                if error.is_none() {
                    self.stats.inc(StatType::Ledger, DetailType::Rollback);
                } else {
                    self.stats.inc(StatType::Ledger, DetailType::RollbackFailed);
                }
            }
        }
        rollback_list
    }

    fn block_successor_by_qualified_root(&self, root: &QualifiedRoot) -> Option<BlockHash> {
        if !root.previous.is_zero() {
            self.store.successors.get(&root.previous)
        } else {
            self.store
                .account
                .get(&root.root.into())
                .map(|i| i.open_block)
        }
    }

    pub fn confirm(&self, hash: BlockHash) -> Vec<SavedBlock> {
        self.confirm_max(hash, 1024 * 128)
    }

    fn confirm_max(&self, target_hash: BlockHash, max_blocks: usize) -> Vec<SavedBlock> {
        BlockCementer::new(&self.store, &self.constants, &self.stats)
            .confirm(target_hash, max_blocks)
    }

    pub fn confirm_batch<'a, O>(
        &self,
        batch: impl IntoIterator<Item = &'a BlockHash>,
        stopped: &AtomicBool,
        max_blocks: usize,
        cementing_observer: &mut O,
    ) where
        O: CementingObserver,
    {
        BlockCementer::new(&self.store, &self.constants, &self.stats).confirm_batch(
            batch,
            stopped,
            max_blocks,
            cementing_observer,
        );
    }
}

pub struct BatchProcessResult {
    pub processed: Vec<(Result<(), BlockError>, Option<SavedBlock>)>,
}

#[derive(Debug)]
pub struct RollbackResult {
    pub target_hash: BlockHash,
    pub target_root: QualifiedRoot,
    pub rolled_back: Vec<SavedBlock>,
    pub error: Option<RollbackError>,
}

#[derive(Debug, Default)]
pub struct RollbackResults(pub Vec<RollbackResult>);

impl RollbackResults {
    pub fn hashes(&self) -> Vec<BlockHash> {
        self.0
            .iter()
            .flat_map(|r| r.rolled_back.iter().map(|b| b.hash()))
            .collect()
    }

    pub fn roots(&self) -> Vec<rsnano_core::QualifiedRoot> {
        self.0.iter().map(|r| r.target_root.clone()).collect()
    }

    pub fn affected_accounts(&self) -> Vec<rsnano_core::Account> {
        self.0.iter().map(|r| r.target_root.root.into()).collect()
    }

    pub fn iter(&self) -> core::slice::Iter<'_, RollbackResult> {
        self.0.iter()
    }

    pub fn push(&mut self, result: RollbackResult) {
        self.0.push(result);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn extend(&mut self, other: RollbackResults) {
        self.0.extend(other.0);
    }
}

pub trait CementingObserver {
    fn batch_confirmed(&mut self, blocks: Vec<(SavedBlock, BlockHash)>);
}

#[cfg(test)]
mod slotted_btree_tests {
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use kernel_common::storage::bpm::BufferPool;
    use kernel_common::storage::btree::{BTree, KEY_SIZE};
    use kernel_common::storage::StorageContext;
    use rsnano_core::{BlockHash, SavedBlock};
    use spin::Mutex;

    #[test]
    fn slotted_btree_insert_iter_suffix_and_delete() {
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(8)));
        let bpm = BufferPool::new(storage, 256, 1);
        let mut tree = BTree::new(bpm);

        let count = 400usize;
        let mut keys = Vec::with_capacity(count);
        for i in 0..count {
            let mut k = [0u8; KEY_SIZE];
            k[0] = ((i % 4) + 1) as u8;
            let h = ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_be_bytes();
            k[1..9].copy_from_slice(&h);
            k[9..17].copy_from_slice(&(i as u64).to_be_bytes());
            if i % 3 == 0 {
                // 65-byte key (like PendingStore)
                k[33..41].copy_from_slice(&h);
            }
            let val_len = match i % 4 {
                0 => 40,  // ConfirmationHeight
                1 => 129, // AccountInfo
                2 => 248, // SavedBlock + 32B successor
                _ => 65,  // PendingInfo
            };
            let val = alloc::vec![(i & 0xFF) as u8; val_len];
            assert!(tree.insert(k, &val));
            keys.push((k, val));
        }

        // Verify all keys and test in-place suffix updates on 248B entries
        for (i, (k, val)) in keys.iter().enumerate() {
            let got = tree.search(k).expect("key must exist");
            assert_eq!(&got, val);
            if val.len() == 248 {
                let suffix = [(i as u8).wrapping_add(1); 32];
                assert!(tree.update_suffix_32(k, &suffix));
                let updated = tree.search(k).unwrap();
                assert_eq!(&updated[..216], &val[..216]);
                assert_eq!(&updated[216..248], &suffix);

                // Re-insert with preserve_tail_32 = true and zero tail; suffix must be preserved
                assert!(!tree.insert_preserving_suffix_32(*k, val, true));
                let preserved = tree.search(k).unwrap();
                assert_eq!(&preserved[216..248], &suffix);
            }
        }

        // Verify sorted iteration returns exactly `count` keys in strictly ascending order
        let mut iter_keys = Vec::new();
        let mut it = tree.iter([0u8; KEY_SIZE]);
        while let Some(k) = it.next_key() {
            iter_keys.push(k);
        }
        assert_eq!(iter_keys.len(), count);
        for w in iter_keys.windows(2) {
            assert!(w[0] < w[1]);
        }

        // Delete all keys in pseudo-random order and verify remaining keys at steps
        for step in 0..count {
            let idx = (step * 173 + 19) % count;
            let (k, _) = &keys[idx];
            tree.delete(*k);
            assert!(!tree.contains_key(k));
        }
        let mut it_after = tree.iter([0u8; KEY_SIZE]);
        assert_eq!(it_after.next_key(), None);
    }

    /// Serializes tests that install the global `SHARDS` ramdisk B-Trees.
    static SHARD_TEST_LOCK: spin::Mutex<()> = spin::Mutex::new(());

    #[test]
    fn ledger_recount_merge_join_across_chunks() {
        let _guard = SHARD_TEST_LOCK.lock();
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(256)));
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let bpm = BufferPool::new(storage.clone(), 256, 1 + i * 512);
            *kernel_common::storage::SHARDS[i].lock() = Some(BTree::new(bpm));
        }
        let store = rsnano_ledger_store::BtreeStore::new();

        // Accounts spread across all shards; deterministic pseudo-random keys.
        let mut expected_blocks = 0u64;
        let mut expected_conf = 0u64;
        let mut expected_unconfirmed = 0u64;
        let mut expected_missing = 0u64;
        let mut max_gap = 0u64;
        let n = 3000u64;
        for i in 0..n {
            let mut b = [0u8; 32];
            let x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            b[..8].copy_from_slice(&x.to_be_bytes());
            b[8..16].copy_from_slice(&i.to_be_bytes());
            let account = rsnano_core::Account::from_bytes(b);
            let block_count = 1 + (i % 37);
            let mut info = rsnano_core::AccountInfo::default();
            info.block_count = block_count;
            info.head = BlockHash::from(i + 1);
            store.account.put(&account, &info);
            expected_blocks += block_count;
            match i % 5 {
                0 => {
                    // No confirmation height entry
                    expected_missing += 1;
                    expected_unconfirmed += block_count;
                    max_gap = max_gap.max(block_count);
                }
                1 => {
                    let h = block_count / 2;
                    store.confirmation_height.put(
                        &account,
                        &rsnano_core::ConfirmationHeightInfo::new(h, BlockHash::from(1)),
                    );
                    expected_conf += h;
                    expected_unconfirmed += block_count - h;
                    max_gap = max_gap.max(block_count - h);
                }
                _ => {
                    store.confirmation_height.put(
                        &account,
                        &rsnano_core::ConfirmationHeightInfo::new(block_count, info.head),
                    );
                    expected_conf += block_count;
                }
            }
        }
        // Orphan confirmation heights (no account)
        // 50 spread across shards + 400 clustered at the end (exercises the conf cap path).
        for i in 0..450u64 {
            let mut b = [0xFFu8; 32];
            b[24..].copy_from_slice(&i.to_be_bytes());
            if i < 50 {
                b[0] = (i as u8) << 2 | 0x03;
            }
            store.confirmation_height.put(
                &rsnano_core::Account::from_bytes(b),
                &rsnano_core::ConfirmationHeightInfo::new(3, BlockHash::from(1)),
            );
            expected_conf += 3;
        }

        let mut rc = crate::LedgerRecount::new();
        rc.start(&store, 0);
        let mut steps = 0;
        while rc.step(&store, 64, 0) {
            steps += 1;
            assert!(steps < 10_000);
        }
        assert!(rc.done);
        assert!(steps > 10, "budget must force multiple chunks");
        assert_eq!(rc.accounts, n);
        assert_eq!(rc.sum_block_count, expected_blocks);
        assert_eq!(rc.sum_conf_height, expected_conf);
        assert_eq!(rc.orphan_conf_entries, 450);
        assert_eq!(rc.missing_conf_accounts, expected_missing);
        assert_eq!(rc.unconfirmed_blocks, expected_unconfirmed);
        assert_eq!(rc.top[0].gap(), max_gap);

        for i in 0..kernel_common::storage::SHARD_COUNT {
            *kernel_common::storage::SHARDS[i].lock() = None;
        }
    }

    #[test]
    fn ledger_recount_repair_rebuilds_orphan_account_and_counters() {
        let _guard = SHARD_TEST_LOCK.lock();
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(64)));
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let bpm = BufferPool::new(storage.clone(), 64, 1 + i * 128);
            *kernel_common::storage::SHARDS[i].lock() = Some(BTree::new(bpm));
        }
        let ledger = crate::Ledger::new_null();

        // Orphan: block + confirmation height present, account record missing.
        let block = SavedBlock::new_test_instance();
        let account = block.account();
        ledger.store.block.put(&block);
        ledger.store.confirmation_height.put(
            &account,
            &rsnano_core::ConfirmationHeightInfo::new(block.height(), block.hash()),
        );
        assert!(ledger.store.account.get(&account).is_none());

        // Healthy account with an unconfirmed gap (`conf_height = 5 < block_count = 7`) whose head block exists.
        let other_block = SavedBlock::new_test_instance_with_key(42);
        let other = rsnano_core::Account::from_bytes([0x42; 32]);
        ledger.store.block.put(&other_block);
        let mut info = rsnano_core::AccountInfo::default();
        info.head = other_block.hash();
        info.block_count = 7;
        ledger.store.account.put(&other, &info);
        ledger.store.confirmation_height.put(
            &other,
            &rsnano_core::ConfirmationHeightInfo::new(5, BlockHash::from(9)),
        );

        // Unrecoverable orphan (no account row, and frontier block missing from `TABLE_BLOCKS`):
        // should be removed from `TABLE_CONFIRMATION_HEIGHT` and seeded into `bootstrap_accounts`.
        let missing_orphan = rsnano_core::Account::from_bytes([0x99; 32]);
        ledger.store.confirmation_height.put(
            &missing_orphan,
            &rsnano_core::ConfirmationHeightInfo::new(3, BlockHash::from(999)),
        );

        let mut rc = crate::LedgerRecount::new();
        rc.start_repair(&ledger.store, 0);
        while ledger.recount_step(&mut rc, 16, 0) {}

        let healed = ledger.store.account.get(&account).expect("orphan account rebuilt");
        assert_eq!(healed.head, block.hash());
        assert_eq!(healed.block_count, block.height());
        assert_eq!(healed.balance, block.balance());
        assert_eq!(rc.repaired_accounts, 1);
        assert_eq!(rc.repaired_blocks, block.height());
        assert_eq!(rc.repaired_missing_conf_accounts, 1);
        assert_eq!(rc.repaired_missing_conf_blocks, 2);
        assert_eq!(rc.removed_orphan_conf_entries, 1);
        assert_eq!(rc.removed_orphan_conf_height, 3);
        assert_eq!(rc.bootstrap_seeded, 1);
        assert_eq!(rc.bootstrap_accounts, alloc::vec![missing_orphan]);
        assert!(ledger.store.confirmation_height.get(&missing_orphan).is_none());
        assert_eq!(
            ledger.store.confirmation_height.get(&other),
            Some(rsnano_core::ConfirmationHeightInfo::new(7, other_block.hash()))
        );
        assert_eq!(rc.repair_failed, 1);
        assert!(rc.counters_applied);
        assert_eq!(ledger.block_count() as u64, 7 + block.height());
        assert_eq!(ledger.confirmed_count() as u64, 7 + block.height());
        // new_null seeds account_count=1; the scan saw 1 account and the repair added 1.
        assert_eq!(ledger.account_count(), 2);

        for i in 0..kernel_common::storage::SHARD_COUNT {
            *kernel_common::storage::SHARDS[i].lock() = None;
        }
    }

    #[test]
    fn embedded_block_successor_roundtrip() {
        let _guard = SHARD_TEST_LOCK.lock();
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(32)));
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let bpm = BufferPool::new(storage.clone(), 64, 1 + i * 64);
            *kernel_common::storage::SHARDS[i].lock() = Some(BTree::new(bpm));
        }

        let store = rsnano_ledger_store::BtreeStore::new();
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let succ = BlockHash::from(42);

        store.block.put(&block);
        assert_eq!(store.block.get(&hash).as_ref(), Some(&block));
        assert_eq!(store.successors.get(&hash), None);

        store.successors.put(&hash, &succ);
        assert_eq!(store.successors.get(&hash), Some(succ));
        assert_eq!(store.block.get(&hash).as_ref(), Some(&block));

        // Re-putting block with zero successor preserves existing successor
        store.block.put(&block);
        assert_eq!(store.successors.get(&hash), Some(succ));

        // Clearing successor resets tail to None without corrupting block
        store.successors.del(&hash);
        assert_eq!(store.successors.get(&hash), None);
        assert_eq!(store.block.get(&hash).as_ref(), Some(&block));
    }

    #[test]
    fn slotted_btree_rvv_soa_prefix_collisions_and_legacy_upgrade() {
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(8)));
        let bpm = BufferPool::new(storage, 256, 1);
        let mut tree = BTree::new(bpm);

        // 1. Insert 200 keys sharing only 4 distinct 32-bit prefixes (50 keys per prefix)
        // to stress-test RVV `vmsltu.vx`/`vmsleu.vx` + binary search tie-breaking (`h > l + 1`).
        let count = 200usize;
        let mut keys = Vec::with_capacity(count);
        for i in 0..count {
            let mut k = [0u8; KEY_SIZE];
            k[0] = 7; // TABLE_PENDING
            k[1] = 0xAB;
            k[2] = 0xCD;
            k[3] = (i % 4) as u8;
            k[4..12].copy_from_slice(&((i as u64).wrapping_mul(0x517C_C1B7_2722_0A95)).to_be_bytes());
            k[33..41].copy_from_slice(&(i as u64).to_be_bytes());
            let val = alloc::vec![(i & 0xFF) as u8; 49];
            assert!(tree.insert(k, &val));
            keys.push((k, val));
        }

        // 2. Simulate legacy pages loaded from an older NVMe snapshot (`data[7] == 0` and zeroed SoA heads)
        let n_pages = tree.page_count() as u32;
        for pid in 1..n_pages {
            let mut raw = tree.read_page(pid);
            let is_leaf = raw[0] != 0;
            let nk = u16::from_le_bytes([raw[1], raw[2]]) as usize;
            let hb = if is_leaf {
                (8 + nk * 2 + 3) & !3
            } else {
                (12 + nk * 6 + 3) & !3
            };
            raw[7] = 0;
            raw[hb..hb + nk * 4].fill(0);
            tree.write_page(pid, &raw);
        }

        // 3. Verify that read-only lookups lazily upgrade pages in RAM (`page_ensure_soa`)
        // and return exact matches for all 200 keys.
        for (k, val) in &keys {
            assert_eq!(tree.search(k).as_ref(), Some(val));
        }

        // Confirm root page was upgraded in RAM to `PAGE_FLAG_SOA` (`data[7] == 1`)
        for pid in 1..n_pages {
            let upgraded = tree.read_page(pid);
            assert_eq!(upgraded[7], 1);
        }

        // 4. Delete all 200 keys and verify tree is empty
        for (k, _) in &keys {
            tree.delete(*k);
            assert!(!tree.contains_key(k));
        }
        assert_eq!(tree.iter([0u8; KEY_SIZE]).next_key(), None);
    }

    #[test]
    fn exact_unconfirmed_accounts_index_lifecycle() {
        let _guard = SHARD_TEST_LOCK.lock();
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(64)));
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let bpm = BufferPool::new(storage.clone(), 64, 1 + i * 128);
            *kernel_common::storage::SHARDS[i].lock() = Some(BTree::new(bpm));
        }
        let store = rsnano_ledger_store::BtreeStore::new();
        rsnano_ledger_store::unconfirmed_accounts_clear();

        let mut expected_unconfirmed = alloc::collections::BTreeSet::new();
        for i in 0..200u64 {
            let mut b = [0u8; 32];
            b[0] = ((i % 64) as u8) << 2;
            b[24..32].copy_from_slice(&i.to_be_bytes());
            let acc = rsnano_core::Account::from_bytes(b);
            let mut info = rsnano_core::AccountInfo::default();
            info.block_count = 10;
            info.head = BlockHash::from(i + 1);
            store.account.put(&acc, &info);

            if i % 50 == 7 {
                // Unconfirmed gap (conf_height = 6 < block_count = 10)
                store.confirmation_height.put(
                    &acc,
                    &rsnano_core::ConfirmationHeightInfo::new(6, BlockHash::from(i + 1)),
                );
                expected_unconfirmed.insert(acc);
            } else {
                // Fully confirmed (conf_height == block_count == 10)
                store.confirmation_height.put(
                    &acc,
                    &rsnano_core::ConfirmationHeightInfo::new(10, info.head),
                );
            }
        }

        assert_eq!(expected_unconfirmed.len(), 4);
        assert_eq!(
            rsnano_ledger_store::unconfirmed_accounts_count(),
            expected_unconfirmed.len()
        );

        let _ = rsnano_ledger_store::drain_recently_confirmed(1024);
        let (batch, _last, done) =
            rsnano_ledger_store::unconfirmed_accounts_next_batch(rsnano_core::Account::zero(), 64);
        assert!(done);
        let batch_set: alloc::collections::BTreeSet<_> = batch.into_iter().collect();
        assert_eq!(batch_set, expected_unconfirmed);

        // Cement 3 of the 4 unconfirmed accounts and delete the 4th
        let mut iter = expected_unconfirmed.iter().copied();
        for _ in 0..3 {
            let acc = iter.next().unwrap();
            store.confirmation_height.put(
                &acc,
                &rsnano_core::ConfirmationHeightInfo::new(10, BlockHash::from(999)),
            );
        }
        let last_acc = iter.next().unwrap();
        store.account.del(&last_acc);

        assert_eq!(rsnano_ledger_store::unconfirmed_accounts_count(), 0);
        let recently_confirmed: alloc::collections::BTreeSet<_> =
            rsnano_ledger_store::drain_recently_confirmed(64)
                .into_iter()
                .collect();
        assert_eq!(recently_confirmed, expected_unconfirmed);
        assert!(!rsnano_ledger_store::unconfirmed_accounts_has_work());

        for i in 0..kernel_common::storage::SHARD_COUNT {
            *kernel_common::storage::SHARDS[i].lock() = None;
        }
    }
}
