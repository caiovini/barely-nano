use core::sync::atomic::{AtomicU64, Ordering};
use alloc::sync::Arc;
use crate::{BtreeBlockStore, BtreeAccountStore, BtreePendingStore, BtreeOnlineWeightStore, BtreePrunedStore, BtreeRepWeightStore, BtreePeerStore, BtreeConfirmationHeightStore, BtreeFinalVoteStore, BtreeSuccessorStore, BtreeVersionStore, BtreeWalletStore};

pub struct LedgerCache {
    pub confirmed_count: AtomicU64,
    pub block_count: AtomicU64,
    pub pruned_count: AtomicU64,
    pub account_count: AtomicU64,
}

impl LedgerCache {
    pub fn new() -> Self {
        Self {
            confirmed_count: AtomicU64::new(0),
            block_count: AtomicU64::new(0),
            pruned_count: AtomicU64::new(0),
            account_count: AtomicU64::new(0),
        }
    }

    pub fn reset(&self) {
        self.confirmed_count.store(0, Ordering::Relaxed);
        self.block_count.store(0, Ordering::Relaxed);
        self.pruned_count.store(0, Ordering::Relaxed);
        self.account_count.store(0, Ordering::Relaxed);
    }
}

pub struct BtreeStore {
    pub cache: Arc<LedgerCache>,
    pub block: BtreeBlockStore,
    pub account: BtreeAccountStore,
    pub pending: BtreePendingStore,
    pub online_weight: BtreeOnlineWeightStore,
    pub pruned: BtreePrunedStore,
    pub rep_weight: Arc<BtreeRepWeightStore>,
    pub peer: BtreePeerStore,
    pub confirmation_height: BtreeConfirmationHeightStore,
    pub final_vote: BtreeFinalVoteStore,
    pub successors: BtreeSuccessorStore,
    pub version: BtreeVersionStore,
    pub wallet: BtreeWalletStore,
}

static ACTIVE_CACHE: spin::Mutex<Option<Arc<LedgerCache>>> = spin::Mutex::new(None);
static LAST_PERSISTED_COUNTS: spin::Mutex<(u64, u64, u64, u64)> = spin::Mutex::new((0, 0, 0, 0));

fn flush_cache_counts_pre_snapshot() {
    let counts = {
        let guard = ACTIVE_CACHE.lock();
        let Some(cache) = guard.as_ref() else {
            return;
        };
        (
            cache.account_count.load(Ordering::Relaxed),
            cache.block_count.load(Ordering::Relaxed),
            cache.confirmed_count.load(Ordering::Relaxed),
            cache.pruned_count.load(Ordering::Relaxed),
        )
    };
    if counts.0 == 0 || counts.1 == 0 {
        return;
    }
    let mut last = LAST_PERSISTED_COUNTS.lock();
    if *last == counts {
        return;
    }
    BtreeVersionStore::new().put_cached_counts(counts.0, counts.1, counts.2, counts.3);
    *last = counts;
}

impl BtreeStore {
    pub fn new() -> Self {
        Self::new_with_cache(Arc::new(LedgerCache::new()))
    }

    pub fn new_with_cache(cache: Arc<LedgerCache>) -> Self {
        Self {
            cache: cache.clone(),
            block: BtreeBlockStore::new(cache.clone()),
            account: BtreeAccountStore::new(cache.clone()),
            pending: BtreePendingStore::new(),
            online_weight: BtreeOnlineWeightStore::new(),
            pruned: BtreePrunedStore::new(),
            rep_weight: Arc::new(BtreeRepWeightStore::new()),
            peer: BtreePeerStore::new(),
            confirmation_height: BtreeConfirmationHeightStore::new(),
            final_vote: BtreeFinalVoteStore::new(),
            successors: BtreeSuccessorStore::new(),
            version: BtreeVersionStore::new(),
            wallet: BtreeWalletStore::new(),
        }
    }

    pub fn set_cache(&mut self, cache: Arc<LedgerCache>) {
        self.cache = cache.clone();
        self.block = BtreeBlockStore::new(cache.clone());
        self.account = BtreeAccountStore::new(cache);
    }

    pub fn generate_cache(&self) {
        self.cache.reset();
        *ACTIVE_CACHE.lock() = Some(self.cache.clone());
        kernel_common::storage::wal::set_pre_snapshot_hook(flush_cache_counts_pre_snapshot);

        // Load the persistent 64 MB block bloom filter from NVMe (~150 ms). If not yet persisted
        // on NVMe (first boot after upgrade), build it once from the B-Tree before scanning
        // accounts/confirmation_height so the hot account tables remain warm in the Sector Cache.
        if !self.block.try_load_persisted_bloom() {
            self.block.scan_count();
        }

        let (account_count, block_count, confirmed_count, pruned_count) =
            if let Some((acc, blk, conf, pruned)) = self.version.get_cached_counts() {
                *LAST_PERSISTED_COUNTS.lock() = (acc, blk, conf, pruned);
                if acc <= 250_000 {
                    self.account.populate_bloom();
                } else {
                    self.account.disable_bloom();
                }
                #[cfg(not(test))]
                if pruned > 0 {
                    tracing::info!(
                        "[ledger] Loaded cached counts from B-Tree: accounts={} blocks={} confirmed={} pruned={}",
                        acc,
                        blk,
                        conf,
                        pruned
                    );
                } else {
                    tracing::info!(
                        "[ledger] Loaded cached counts from B-Tree: accounts={} blocks={} confirmed={}",
                        acc,
                        blk,
                        conf
                    );
                }
                (acc, blk, conf, pruned)
            } else {
                #[cfg(not(test))]
                tracing::info!(
                    "[ledger] Scanning B-Tree for initial account/block/confirmed counts (one-time scan)..."
                );
                let (acc, blk) = self.account.scan_counts();
                let conf = self.confirmation_height.sum_heights();
                let pruned = self.pruned.count(&());
                *LAST_PERSISTED_COUNTS.lock() = (0, 0, 0, 0);
                if acc == 0 && blk == 0 {
                    self.block.clear_bloom();
                }
                (acc, blk, conf, pruned)
            };

        if pruned_count > 0 {
            self.pruned.set_has_pruned(true);
        }
        self.block.set_count(block_count.saturating_sub(pruned_count));
        self.cache.block_count.store(block_count, Ordering::Relaxed);
        self.cache.account_count.store(account_count, Ordering::Relaxed);
        self.cache.confirmed_count.store(confirmed_count, Ordering::Relaxed);
        self.cache.pruned_count.store(pruned_count, Ordering::Relaxed);

        self.report_cross_shard_inconsistency();
        kernel_common::storage::reset_prof();
    }

    /// Diagnostic: checks a bounded sample of accounts to verify their head block exists.
    fn report_cross_shard_inconsistency(&self) {
        const CAP: usize = 1_024;
        let mut heads = alloc::vec::Vec::with_capacity(CAP);
        self.account.for_each_while(rsnano_core::Account::zero(), |account, info| {
            heads.push((account, info.head));
            heads.len() < CAP
        });
        let mut missing = 0usize;
        for (idx, (_, head)) in heads.iter().enumerate() {
            if idx % 256 == 0 {
                kernel_common::watchdog::kick();
            }
            if !self.block.exists(head) {
                missing += 1;
            }
        }
        if missing > 0 {
            tracing::warn!(
                "[storage] consistency check: accounts={} head_block_missing={}",
                heads.len(),
                missing
            );
        } else {
            tracing::debug!(
                "[storage] consistency check: accounts={} head_block_missing=0",
                heads.len()
            );
        }
    }

    pub fn tx_begin_read(&self) -> () { () }
    pub fn tx_begin_write(&self) -> () { () }

    /// Atomically persists all dirty pages of every shard through the write-ahead log.
    pub fn flush(&self) -> anyhow::Result<()> {
        kernel_common::storage::wal::checkpoint()
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("WAL checkpoint failed: {:?}", e))
    }

    pub fn wipe(&self) {
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let mut lock = kernel_common::storage::SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                db.wipe();
            }
        }
        self.block.clear_bloom();
        self.account.clear_bloom();
        self.pruned.set_has_pruned(false);
        self.cache.reset();
        *LAST_PERSISTED_COUNTS.lock() = (0, 0, 0, 0);
    }
}

use crate::*;
