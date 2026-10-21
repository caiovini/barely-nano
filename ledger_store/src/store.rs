use core::sync::atomic::{AtomicU64, Ordering};
use alloc::sync::Arc;
use crate::{BtreeBlockStore, BtreeAccountStore, BtreePendingStore, BtreeOnlineWeightStore, BtreePrunedStore, BtreeRepWeightStore, BtreePeerStore, BtreeConfirmationHeightStore, BtreeFinalVoteStore, BtreeSuccessorStore, BtreeVersionStore};

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
        self.confirmed_count.store(0, Ordering::SeqCst);
        self.block_count.store(0, Ordering::SeqCst);
        self.pruned_count.store(0, Ordering::SeqCst);
        self.account_count.store(0, Ordering::SeqCst);
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
        }
    }
    
    pub fn generate_cache(&self) {
        self.cache.reset();
        
        // Block count
        self.cache.block_count.store(self.block.scan_count() as u64, Ordering::SeqCst);
        
        // Account count
        self.cache.account_count.store(self.account.scan_count() as u64, Ordering::SeqCst);
        
        // Confirmed count
        self.cache.confirmed_count.store(self.confirmation_height.sum_heights(), Ordering::SeqCst);
    }

    pub fn tx_begin_read(&self) -> () { () }
    pub fn tx_begin_write(&self) -> () { () }

    pub fn flush(&self) -> anyhow::Result<()> {
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let mut lock = kernel_common::storage::SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                db.flush().map_err(|e: kernel_common::storage::StorageError| anyhow::anyhow!("DB shard {} flush failed: {:?}", i, e))?;
            }
        }
        Ok(())
    }

    pub fn wipe(&self) {
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let mut lock = kernel_common::storage::SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                db.wipe();
            }
        }
        self.cache.reset();
    }
}

use crate::*;
