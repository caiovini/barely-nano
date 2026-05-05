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
        let cache = Arc::new(LedgerCache::new());
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
        let blocks = self.block.iter();
        self.cache.block_count.store(blocks.len() as u64, Ordering::SeqCst);
        
        // Account count
        let accounts = self.account.iter(rsnano_core::Account::zero());
        self.cache.account_count.store(accounts.len() as u64, Ordering::SeqCst);
        
        // Confirmed count
        let conf_heights = self.confirmation_height.iter();
        let total_confirmed: u64 = conf_heights.iter().map(|(_, info)| info.height).sum();
        self.cache.confirmed_count.store(total_confirmed, Ordering::SeqCst);
    }

    pub fn tx_begin_read(&self) -> () { () }
    pub fn tx_begin_write(&self) -> () { () }

    pub fn flush(&self) -> anyhow::Result<()> {
        let mut lock = kernel_common::storage::DB.lock();
        if let Some(db) = lock.as_mut() {
            db.flush().map_err(|e: virtio_drivers::Error| anyhow::anyhow!("DB flush failed: {:?}", e))
        } else {
            Ok(())
        }
    }

    pub fn wipe(&self) {
        let mut lock = kernel_common::storage::DB.lock();
        if let Some(db) = lock.as_mut() {
            db.wipe();
        }
        self.cache.reset();
    }
}

use crate::*;
