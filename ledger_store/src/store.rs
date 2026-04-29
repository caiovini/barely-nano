use core::sync::atomic::{AtomicU64, Ordering};
use alloc::sync::Arc;
use kernel_common::storage::DB;

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
        Self {
            cache: Arc::new(LedgerCache::new()),
            block: BtreeBlockStore::new(),
            account: BtreeAccountStore::new(),
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
}

use crate::*;
