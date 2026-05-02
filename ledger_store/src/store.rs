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
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            // kernel_common::uart::print(b"[node] Generating cache...\n");
            // Count blocks
            let mut block_count = 0;
            let mut start_key = [0u8; kernel_common::storage::btree::KEY_SIZE];
            start_key[0] = crate::block_store::BtreeBlockStore::TABLE_ID;
            let mut iter = db.iter(start_key);
            while let Some((k, _)) = iter.next_kv() {
                if k[0] != crate::block_store::BtreeBlockStore::TABLE_ID { break; }
                block_count += 1;
                // if block_count % 1000 == 0 { kernel_common::uart::print(b"."); }
            }
            self.cache.block_count.store(block_count, Ordering::SeqCst);

            // Count accounts
            let mut account_count = 0;
            let mut start_key = [0u8; kernel_common::storage::btree::KEY_SIZE];
            start_key[0] = crate::account_store::BtreeAccountStore::TABLE_ID;
            let mut iter = db.iter(start_key);
            while let Some((k, _)) = iter.next_kv() {
                if k[0] != crate::account_store::BtreeAccountStore::TABLE_ID { break; }
                account_count += 1;
            }
            self.cache.account_count.store(account_count, Ordering::SeqCst);
            // kernel_common::uart::print(b"[node] Cache generated.\n");
        }
    }
    pub fn tx_begin_read(&self) -> () { () }
    pub fn tx_begin_write(&self) -> () { () }
}

use crate::*;
