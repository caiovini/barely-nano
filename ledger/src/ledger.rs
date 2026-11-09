use crate::{
    block_cementer::BlockCementer,
    block_insertion::{BlockInserter, BlockValidatorFactory},
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

const FAST_BATCH_SLOTS: usize = 1024;
const FAST_BATCH_MASK: usize = FAST_BATCH_SLOTS - 1;
const FAST_BATCH_MAX_INDEXED: usize = 768;

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

/// Stack-indexed open-addressed hash table backed by a single flat `Vec<(K, V)>`
/// to avoid per-node heap allocations (`alloc::collections::BTreeMap`) on Hart 6.
struct FastBatchMap<K: BatchHashKey, V> {
    slots: [u16; FAST_BATCH_SLOTS],
    entries: Vec<(K, V)>,
}

impl<K: BatchHashKey, V> FastBatchMap<K, V> {
    #[inline]
    fn with_capacity(cap: usize) -> Self {
        Self {
            slots: [0u16; FAST_BATCH_SLOTS],
            entries: Vec::with_capacity(cap),
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    fn find_index(&self, key: &K) -> Option<usize> {
        let mut h = key.fast_hash() & FAST_BATCH_MASK;
        loop {
            let s = self.slots[h];
            if s == 0 {
                break;
            }
            let idx = (s - 1) as usize;
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
            let mut h = key.fast_hash() & FAST_BATCH_MASK;
            loop {
                let s = self.slots[h];
                if s == 0 {
                    let idx = self.entries.len();
                    self.entries.push((key, val));
                    self.slots[h] = (idx + 1) as u16;
                    return;
                }
                let idx = (s - 1) as usize;
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
        self.slots.fill(0);
        self.entries.clear();
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

static LEDGER_B_ONLINE: AtomicBool = AtomicBool::new(false);
static LEDGER_B_JOB_PTR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static LEDGER_B_JOB_DONE: AtomicBool = AtomicBool::new(false);
pub static LEDGER_B_JOBS_COMPLETED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

struct LedgerBFlushJob {
    store: *const BtreeStore,
    ext_succ: *const [(BlockHash, BlockHash)],
    pending: *const [(rsnano_core::PendingKey, DeferredPendingOp)],
    all_sorted_blocks: *const [(BlockHash, SavedBlock, BlockHash)],
    blocks_upper: *const [(BlockHash, SavedBlock, BlockHash)],
    accounts: *const [(Account, (AccountInfo, AccountInfo))],
}

/// Marks Hart 5 (`Ledger-B`, Shards 8..15) as online so Hart 6 (`Ledger-A`, Shards 0..7)
/// dispatches the upper shard half of each block batch to Hart 5 across Cluster 1's shared L2.
pub fn set_ledger_b_online(online: bool) {
    LEDGER_B_ONLINE.store(online, Ordering::Release);
}

#[inline]
fn flush_shard_half(
    store: &BtreeStore,
    ext_succ: &[(BlockHash, BlockHash)],
    pending: &[(rsnano_core::PendingKey, DeferredPendingOp)],
    all_sorted_blocks: &[(BlockHash, SavedBlock, BlockHash)],
    blocks_slice: &[(BlockHash, SavedBlock, BlockHash)],
    accounts: &[(Account, (AccountInfo, AccountInfo))],
    upper_half: bool,
) {
    for (prev, succ) in ext_succ {
        if ((prev.as_bytes()[0] & 0x80) != 0) == upper_half {
            if all_sorted_blocks.binary_search_by(|e| e.0.cmp(prev)).is_err() {
                store.successors.put(prev, succ);
            }
        }
    }
    for (key, op) in pending {
        if ((key.receiving_account.as_bytes()[0] & 0x80) != 0) == upper_half {
            match op {
                DeferredPendingOp::Insert(info) => store.pending.put(key, info),
                DeferredPendingOp::DeleteOnDisk => store.pending.del(key),
                DeferredPendingOp::CancelledInBatch => {}
            }
        }
    }
    if !blocks_slice.is_empty() {
        store.block.put_batch_with_successors(blocks_slice);
    }
    for (account, (first_old_info, target_info)) in accounts {
        if ((account.as_bytes()[0] & 0x80) != 0) == upper_half {
            if !target_info.head.is_zero() {
                if !first_old_info.head.is_zero() && first_old_info.epoch != target_info.epoch {
                    store.account.del(account);
                }
                store.account.put(account, target_info);
            } else {
                store.account.del(account);
                store.cache.account_count.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

/// Polled by Hart 5 (`Ledger-B`) in Cluster 1 to execute Shards `8..15` B-Tree mutations
/// in parallel while Hart 6 (`Ledger-A`) executes Shards `0..7`.
#[inline]
pub fn try_run_ledger_b_job() -> bool {
    let job_addr = LEDGER_B_JOB_PTR.swap(0, Ordering::AcqRel);
    if job_addr == 0 {
        return false;
    }
    // SAFETY: `job_addr` was published by Hart 6 on its stack and Hart 6 spins on
    // `LEDGER_B_JOB_DONE` with `Acquire` ordering before returning from the frame.
    unsafe {
        let job = &*(job_addr as *const LedgerBFlushJob);
        flush_shard_half(
            &*job.store,
            &*job.ext_succ,
            &*job.pending,
            &*job.all_sorted_blocks,
            &*job.blocks_upper,
            &*job.accounts,
            true,
        );
    }
    LEDGER_B_JOBS_COMPLETED.fetch_add(1, Ordering::Relaxed);
    LEDGER_B_JOB_DONE.store(true, Ordering::Release);
    true
}

/// Specialized open-addressed batch block table storing `(BlockHash, SavedBlock, BlockHash /* successor */)`
/// contiguously so `into_entries()` can be sorted and passed directly to `BlockStore::put_batch_with_successors`.
struct FastBatchBlockMap {
    slots: [u16; FAST_BATCH_SLOTS],
    entries: Vec<(BlockHash, SavedBlock, BlockHash)>,
}

impl FastBatchBlockMap {
    #[inline]
    fn with_capacity(cap: usize) -> Self {
        Self {
            slots: [0u16; FAST_BATCH_SLOTS],
            entries: Vec::with_capacity(cap),
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    fn find_index(&self, key: &BlockHash) -> Option<usize> {
        let mut h = key.fast_hash() & FAST_BATCH_MASK;
        loop {
            let s = self.slots[h];
            if s == 0 {
                break;
            }
            let idx = (s - 1) as usize;
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
            let mut h = key.fast_hash() & FAST_BATCH_MASK;
            loop {
                let s = self.slots[h];
                if s == 0 {
                    let idx = self.entries.len();
                    self.entries.push((key, block, BlockHash::zero()));
                    self.slots[h] = (idx + 1) as u16;
                    return;
                }
                let idx = (s - 1) as usize;
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
    fn into_entries(self) -> Vec<(BlockHash, SavedBlock, BlockHash)> {
        self.entries
    }
}

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
        let items: Vec<(&'a Block, bool)> = batch.into_iter().collect();
        let len = items.len();
        let mut processed = Vec::with_capacity(len);
        let mut failed_hashes: FastBatchMap<BlockHash, ()> = FastBatchMap::with_capacity(len);
        let mut deferred_blocks: FastBatchBlockMap = FastBatchBlockMap::with_capacity(len);
        let mut external_successors: FastBatchMap<BlockHash, BlockHash> =
            FastBatchMap::with_capacity(32);
        let mut deferred_accounts: FastBatchMap<Account, (AccountInfo, AccountInfo)> =
            FastBatchMap::with_capacity(64);
        let mut deferred_pending: FastBatchMap<
            rsnano_core::PendingKey,
            DeferredPendingOp,
        > = FastBatchMap::with_capacity(len);
        let mut old_tips: FastBatchMap<Account, BlockHash> = FastBatchMap::with_capacity(16);
        let batch_now = rsnano_core::utils::UnixMillisTimestamp::now();

        for (idx, &(block, signature_verified)) in items.iter().enumerate() {
            if idx + 4 < len {
                let (ahead, _) = items[idx + 4];
                self.store.block.prefetch_may_exist(&ahead.hash());
                self.store.block.prefetch_may_exist(&ahead.previous());
                self.store.block.prefetch_may_exist(&ahead.source_or_link());
            }
            let hash = block.hash();
            let prev_hash = block.previous();

            // Fast-path: if the exact predecessor block failed earlier in this batch,
            // or is absent from both deferred_blocks and the block Cuckoo filter,
            // this block's predecessor cannot exist in the ledger. Reject immediately as GapPrevious.
            if !prev_hash.is_zero()
                && (failed_hashes.contains_key(&prev_hash)
                    || (!cfg!(test)
                        && !deferred_blocks.contains_key(&prev_hash)
                        && !self.store.block.may_exist(&prev_hash)))
            {
                failed_hashes.insert(hash, ());
                processed.push((Err(BlockError::GapPrevious), None));
                continue;
            }

            tracing::debug!("Ledger: validating block {}", hash);

            let tp0 = kernel_common::clint::mtime();
            let in_batch_exists = deferred_blocks.contains_key(&hash);
            let in_batch_prev = if !prev_hash.is_zero() {
                deferred_blocks.get_block(&prev_hash).cloned()
            } else {
                None
            };
            let candidate_account = block
                .account_field()
                .or_else(|| in_batch_prev.as_ref().map(|p| p.account()));
            let in_batch_account = candidate_account
                .and_then(|acc| deferred_accounts.get(&acc).map(|(_, latest)| latest.clone()));
            let source_hash = block.source_or_link();
            let in_batch_source_exists =
                !source_hash.is_zero() && deferred_blocks.contains_key(&source_hash);

            // If this is an epoch open block (or test mode) that queries `receivable_exists`,
            // flush any deferred pending entries first so B-Tree scan sees them.
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

            let any = OwningAnySet::new(&self.store, &self.constants);
            let validator = BlockValidatorFactory::new(&any, &self.constants, block)
                .create_validator_with_batch_cache(
                    signature_verified,
                    in_batch_exists,
                    in_batch_prev,
                    in_batch_account,
                    in_batch_source_exists,
                    in_batch_pending,
                    batch_now,
                );
            let tp1 = kernel_common::clint::mtime();
            let result = validator.validate();
            let tp2 = kernel_common::clint::mtime();
            LEDGER_PROF[0].fetch_add(tp1 - tp0, core::sync::atomic::Ordering::Relaxed);
            LEDGER_PROF[1].fetch_add(tp2 - tp1, core::sync::atomic::Ordering::Relaxed);

            match result {
                Ok(instructions) => {
                    let tp3 = kernel_common::clint::mtime();
                    let inserted = BlockInserter::new(self, &mut (), block, &instructions)
                        .insert_with_options(true);
                    LEDGER_PROF[2].fetch_add(
                        kernel_common::clint::mtime() - tp3,
                        core::sync::atomic::Ordering::Relaxed,
                    );
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

        // Flush coalesced, key-sorted B-Tree writes across Shards 0..7 (Hart 6: Ledger-A)
        // and Shards 8..15 (Hart 5: Ledger-B) in parallel across Cluster 1's shared L2 cache.
        if !deferred_blocks.is_empty() {
            let tp3 = kernel_common::clint::mtime();
            let mut sorted_blocks = deferred_blocks.into_entries();
            sorted_blocks.sort_unstable_by(|a, b| a.0.cmp(&b.0));

            #[cfg(not(test))]
            for (_, (first_old_info, final_new_info)) in deferred_accounts.entries_mut() {
                if !first_old_info.head.is_zero()
                    && first_old_info.block_count > final_new_info.block_count
                    && (sorted_blocks
                        .binary_search_by(|e| e.0.cmp(&first_old_info.head))
                        .is_ok()
                        || self.store.block.exists(&first_old_info.head))
                {
                    *final_new_info = first_old_info.clone();
                }
            }

            let mid = sorted_blocks.partition_point(|e| (e.0.as_bytes()[0] & 0x80) == 0);
            let ext_succ_slice = external_successors.entries();
            let pending_slice = deferred_pending.entries();
            let accounts_slice = deferred_accounts.entries();

            if LEDGER_B_ONLINE.load(Ordering::Relaxed) {
                let job = LedgerBFlushJob {
                    store: &self.store as *const BtreeStore,
                    ext_succ: ext_succ_slice as *const [(BlockHash, BlockHash)],
                    pending: pending_slice as *const [(rsnano_core::PendingKey, DeferredPendingOp)],
                    all_sorted_blocks: sorted_blocks.as_slice()
                        as *const [(BlockHash, SavedBlock, BlockHash)],
                    blocks_upper: &sorted_blocks[mid..]
                        as *const [(BlockHash, SavedBlock, BlockHash)],
                    accounts: accounts_slice as *const [(Account, (AccountInfo, AccountInfo))],
                };
                let job_addr = &job as *const LedgerBFlushJob as usize;
                LEDGER_B_JOB_DONE.store(false, Ordering::Release);
                LEDGER_B_JOB_PTR.store(job_addr, Ordering::Release);

                // Hart 6 (Ledger-A) executes Shards 0..7 while Hart 5 (Ledger-B) executes Shards 8..15.
                flush_shard_half(
                    &self.store,
                    ext_succ_slice,
                    pending_slice,
                    &sorted_blocks,
                    &sorted_blocks[..mid],
                    accounts_slice,
                    false,
                );

                // If Hart 5 was busy and hasn't claimed the job yet, steal it back and run Shards 8..15
                // on Hart 6 immediately; otherwise wait for Hart 5 to finish Shards 8..15.
                if LEDGER_B_JOB_PTR
                    .compare_exchange(job_addr, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    flush_shard_half(
                        &self.store,
                        ext_succ_slice,
                        pending_slice,
                        &sorted_blocks,
                        &sorted_blocks[mid..],
                        accounts_slice,
                        true,
                    );
                } else {
                    while !LEDGER_B_JOB_DONE.load(Ordering::Acquire) {
                        core::hint::spin_loop();
                    }
                }
            } else {
                flush_shard_half(
                    &self.store,
                    ext_succ_slice,
                    pending_slice,
                    &sorted_blocks,
                    &sorted_blocks[..mid],
                    accounts_slice,
                    false,
                );
                flush_shard_half(
                    &self.store,
                    ext_succ_slice,
                    pending_slice,
                    &sorted_blocks,
                    &sorted_blocks[mid..],
                    accounts_slice,
                    true,
                );
            }

            for (_account, (first_old_info, target_info)) in deferred_accounts.entries() {
                if !first_old_info.head.is_zero() {
                    if first_old_info.representative != target_info.representative
                        || first_old_info.balance != target_info.balance
                    {
                        self.rep_weights_updater.representation_add_dual(
                            &mut (),
                            first_old_info.representative,
                            Amount::zero().wrapping_sub(first_old_info.balance),
                            target_info.representative,
                            target_info.balance,
                        );
                    }
                } else if !target_info.balance.is_zero() {
                    self.rep_weights_updater.representation_add(
                        &mut (),
                        target_info.representative,
                        target_info.balance,
                    );
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

    #[cfg(not(test))]
    fn heal_account_from_block(
        &self,
        account: &Account,
        start_hash: BlockHash,
        current_info: Option<AccountInfo>,
        now: rsnano_core::utils::UnixMillisTimestamp,
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
            modified: now.into(),
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
            let mut conf_missing = false;
            if !confirmed_frontier.is_zero() && !self.store.block.exists(&confirmed_frontier) {
                confirmed_frontier = BlockHash::zero();
                conf_missing = true;
            }

            if !head.is_zero() {
                let head_exists = self.store.block.exists(&head);
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
                        }
                    } else if let Some(healed) = self.heal_account_from_block(
                        account,
                        start,
                        account_info,
                        rsnano_core::utils::UnixMillisTimestamp::now(),
                    ) {
                        if conf_missing
                            || conf_info
                                .as_ref()
                                .map(|c| c.height > healed.block_count)
                                .unwrap_or(false)
                        {
                            self.store.confirmation_height.put(
                                account,
                                &rsnano_core::ConfirmationHeightInfo::new(
                                    healed.block_count,
                                    healed.head,
                                ),
                            );
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
                        confirmed_frontier = tip.hash();
                    } else {
                        self.store.confirmation_height.del(account);
                    }
                }
            } else if conf_missing {
                self.store.confirmation_height.del(account);
            }
        }

        (head, confirmed_frontier)
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

    pub fn find_forward_gap_start(&self, account: &Account, _exclude_head: bool) -> BlockHash {
        let (head, confirmed_frontier) = self.get_healed_account_frontiers(account);
        if !confirmed_frontier.is_zero() && confirmed_frontier != head {
            let walked = self.advance_existing_chain(confirmed_frontier, 16384);
            if !walked.is_zero() && walked != head {
                return walked;
            }
        }
        if !head.is_zero() {
            return self.advance_existing_chain(head, 4096);
        }
        if !confirmed_frontier.is_zero() {
            return self.advance_existing_chain(confirmed_frontier, 4096);
        }
        let open_block = self
            .store
            .account
            .get(account)
            .map(|i| i.open_block)
            .filter(|h| !h.is_zero() && self.store.block.exists(h))
            .unwrap_or_default();
        if !open_block.is_zero() {
            return self.advance_existing_chain(open_block, 16384);
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

    #[test]
    fn embedded_block_successor_roundtrip() {
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(8)));
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
}
